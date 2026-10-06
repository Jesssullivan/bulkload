//! P21 WALK-RESUME, the walk leg of the S3 headline (OI-1003-Q6, OI-1003-Q10;
//! R25, R-N58; property-test plan §1, row P21).
//!
//! ∀ generated trees and ∀ interleavings of walks (each under a drawn
//! [`HashPolicy`]) and source mutations M between them, over both freshness
//! caches ([`MemoryCache`], [`SqliteCache`]):
//!
//! - a warm `StaleOnly` walk reads **exactly** Σ size of the files whose
//!   content was never completed under their current stat identity: after a
//!   hashed walk that is Σ size(M), and `bytes_reread_on_resume == 0`;
//! - an `Always` walk reads every file, and `bytes_reread_on_resume ==` Σ size
//!   of the files the cache already held a completed digest for (the
//!   unchanged ones);
//! - a stat-only (`Never`) walk reads nothing and never marks a file hashed:
//!   it leaves no digest, so a later `StaleOnly` walk still reads the file;
//! - every hashed row carries `blake3(content)` (a cached digest is never
//!   stale), a stat-only row carries none;
//! - the row of every seat that M did not touch is equal, field for field
//!   (blake3 aside), to its row in the previous walk;
//! - `fresh_skipped` of a hashed walk counts exactly the files with a
//!   completed digest plus the non-file seats an earlier walk recorded.
//!
//! The model is a pure function of the step sequence: which files hold a
//! completed digest under their current identity. A mutation rewrites a
//! file in place (same size or not) and stamps it with an mtime no earlier
//! step used, so its stat identity always moves. A rewrite that leaves the
//! identity unchanged is the racy case (P19), not this property's.
//!
//! **Corpus.** Fixed seed, 16 CI cases ([`prop_config`]). The deep local
//! tier (`BULKLOAD_PROPTEST_DEEP=1`) runs twenty times the cases from the
//! same fixed seed: this file never draws a random seed and never writes a
//! failure-persistence file. `test_support::prop_config` is `#[cfg(test)]
//! pub(crate)`, out of an integration test's reach, so the helper is
//! mirrored here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bulkload_agent::freshness::{FreshnessCache, MemoryCache, SqliteCache};
use bulkload_agent::hash::hash_bytes;
use bulkload_agent::walk::{walk, HashPolicy, WalkOptions};
use bulkload_agent::RowSchema;
use bulkload_proto::FileKind;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

/// `test_support::CI_SEED`, mirrored: every run draws the same cases.
const CI_SEED: u64 = 0x0B01_C0AD_2026_1003;

/// `test_support::DEEP`, mirrored: the switch for the deep local tier.
const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";

/// `test_support::prop_config`, mirrored for an integration test, with the
/// seed fixed in both tiers: `cases` cases in CI, twenty times as many under
/// `BULKLOAD_PROPTEST_DEEP=1`. Nothing persists between runs.
fn prop_config(cases: u32) -> Config {
    let deep = std::env::var_os(DEEP).is_some_and(|value| value == "1");
    Config {
        cases: if deep {
            cases.saturating_mul(20)
        } else {
            cases
        },
        rng_seed: RngSeed::Fixed(CI_SEED),
        failure_persistence: None,
        ..Config::default()
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Directories a file may sit in; the empty prefix is the root.
const DIRECTORIES: [&str; 4] = ["", "d0", "d0/e", "d1"];

/// A generated tree: files by (directory index, length, content seed), and
/// whether a symlink seat sits beside them.
#[derive(Debug, Clone)]
struct Tree {
    files: Vec<(usize, usize, u64)>,
    symlink: bool,
}

/// One step of a run.
#[derive(Debug, Clone)]
enum Step {
    /// A walk under this policy.
    Walk(HashPolicy),
    /// Rewrite every file whose bit is set, in place; the seed draws each
    /// new length (or keeps the old one) and content.
    Mutate(u16, u64),
}

/// Which cache the run consults.
#[derive(Debug, Clone, Copy)]
enum CacheKind {
    Memory,
    Sqlite,
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn length() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(0_usize),
        Just(1_usize),
        2_usize..4_096,
        4_096_usize..65_536,
    ]
}

fn policy() -> impl Strategy<Value = HashPolicy> {
    prop_oneof![
        Just(HashPolicy::Never),
        Just(HashPolicy::StaleOnly),
        Just(HashPolicy::Always),
    ]
}

fn tree() -> impl Strategy<Value = Tree> {
    (
        prop::collection::vec((0..DIRECTORIES.len(), length(), any::<u64>()), 1..=12),
        any::<bool>(),
    )
        .prop_map(|(files, symlink)| Tree { files, symlink })
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    (
        policy(),
        prop::collection::vec(
            prop_oneof![
                policy().prop_map(Step::Walk),
                (any::<u16>(), any::<u64>()).prop_map(|(mask, seed)| Step::Mutate(mask, seed)),
            ],
            1..=8,
        ),
    )
        .prop_map(|(first, rest)| {
            let mut steps = vec![Step::Walk(first)];
            steps.extend(rest);
            steps
        })
}

fn cache_kind() -> impl Strategy<Value = CacheKind> {
    prop_oneof![Just(CacheKind::Memory), Just(CacheKind::Sqlite)]
}

/// A scratch root under the system temp dir, removed on drop.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "bulkload-s3-walk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_path(tree: &Tree, index: usize) -> Vec<u8> {
    let directory = DIRECTORIES[tree.files[index].0];
    if directory.is_empty() {
        format!("f{index}").into_bytes()
    } else {
        format!("{directory}/f{index}").into_bytes()
    }
}

/// Write `content` over the file in place and stamp it with `stamp` seconds
/// past a fixed epoch, so its stat identity moves whatever the clock tick.
fn write_stamped(path: &Path, content: &[u8], stamp: u64) {
    std::fs::write(path, content).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000 + stamp))
        .unwrap();
}

fn without_digest(row: &RowSchema) -> RowSchema {
    RowSchema {
        blake3: None,
        ..row.clone()
    }
}

fn sum(lengths: impl Iterator<Item = usize>) -> u64 {
    lengths.map(|length| length as u64).sum()
}

/// Run one step sequence against the model and assert every walk.
// The model and every walk's assertions read best as one function.
#[allow(clippy::too_many_lines)]
fn check<C: FreshnessCache>(tree: &Tree, steps: &[Step], mut cache: C) {
    let root = Root::new();
    for directory in DIRECTORIES.iter().filter(|directory| !directory.is_empty()) {
        std::fs::create_dir_all(root.0.join(directory)).unwrap();
    }
    let mut contents: Vec<Vec<u8>> = Vec::new();
    let mut stamp = 0_u64;
    for (index, (_, length, seed)) in tree.files.iter().enumerate() {
        let content = noise(*seed, *length);
        let path = root
            .0
            .join(String::from_utf8(file_path(tree, index)).unwrap());
        write_stamped(&path, &content, stamp);
        stamp += 1;
        contents.push(content);
    }
    if tree.symlink {
        std::os::unix::fs::symlink("f0", root.0.join("d1/link")).unwrap();
    }
    let paths: BTreeMap<Vec<u8>, usize> = (0..tree.files.len())
        .map(|index| (file_path(tree, index), index))
        .collect();

    // The model: whether each file holds a completed digest under its
    // current identity, and whether the non-file seats were recorded.
    let mut digested = vec![false; tree.files.len()];
    let mut others_recorded = false;
    // Files touched since the previous walk, and that walk's rows.
    let mut touched = vec![false; tree.files.len()];
    let mut previous: Option<BTreeMap<Vec<u8>, RowSchema>> = None;

    for (number, step) in steps.iter().enumerate() {
        match step {
            Step::Mutate(mask, seed) => {
                for index in 0..tree.files.len() {
                    if mask & (1 << index) == 0 {
                        continue;
                    }
                    let draw = seed
                        .wrapping_add(index as u64)
                        .wrapping_mul(0x2545_F491_4F6C_DD1D);
                    let length = if draw % 3 == 0 {
                        contents[index].len()
                    } else {
                        usize::try_from(draw % 70_000).unwrap()
                    };
                    let content = noise(draw | 1, length);
                    let path = root
                        .0
                        .join(String::from_utf8(file_path(tree, index)).unwrap());
                    write_stamped(&path, &content, stamp);
                    stamp += 1;
                    contents[index] = content;
                    digested[index] = false;
                    touched[index] = true;
                }
            }
            Step::Walk(policy) => {
                let options = WalkOptions {
                    hash_policy: *policy,
                    ..WalkOptions::new(root.0.clone())
                };
                let outcome = walk(&options, &mut cache).unwrap();
                let context = format!("step {number} {policy:?}");
                assert!(
                    outcome.refusals.is_empty(),
                    "{context}: {:?}",
                    outcome.refusals
                );
                assert!(outcome.engine_temporaries.is_empty(), "{context}");

                let others = outcome
                    .rows
                    .iter()
                    .filter(|row| row.kind != FileKind::Regular)
                    .count() as u64;
                let all = sum(contents.iter().map(Vec::len));
                let with_digest = sum(contents
                    .iter()
                    .zip(&digested)
                    .filter(|(_, done)| **done)
                    .map(|(content, _)| content.len()));
                let stats = outcome.stats;
                assert_eq!(
                    stats.seats_seen,
                    tree.files.len() as u64 + others,
                    "{context}"
                );
                assert_eq!(stats.bytes_seen, all, "{context}");
                let (read, reread) = match policy {
                    HashPolicy::Never => (0, 0),
                    HashPolicy::StaleOnly => (all - with_digest, 0),
                    HashPolicy::Always => (all, with_digest),
                };
                assert_eq!(stats.bytes_read, read, "{context}: bytes_read");
                assert_eq!(
                    stats.bytes_reread_on_resume, reread,
                    "{context}: bytes_reread_on_resume"
                );
                if *policy != HashPolicy::Never {
                    let fresh = digested.iter().filter(|done| **done).count() as u64
                        + if others_recorded { others } else { 0 };
                    assert_eq!(stats.fresh_skipped, fresh, "{context}: fresh_skipped");
                }

                let mut rows = BTreeMap::new();
                for row in &outcome.rows {
                    if row.kind == FileKind::Regular {
                        let index = paths[&row.rel_path];
                        let expected =
                            (*policy != HashPolicy::Never).then(|| hash_bytes(&contents[index]));
                        assert_eq!(row.blake3, expected, "{context}: digest of f{index}");
                    }
                    rows.insert(row.rel_path.clone(), without_digest(row));
                }
                assert_eq!(
                    rows.len(),
                    outcome.rows.len(),
                    "{context}: a seat appeared twice"
                );
                if let Some(previous) = &previous {
                    for (path, row) in &rows {
                        let untouched = paths.get(path).is_none_or(|index| !touched[*index]);
                        if untouched {
                            assert_eq!(
                                Some(row),
                                previous.get(path),
                                "{context}: unchanged seat {}",
                                String::from_utf8_lossy(path)
                            );
                        }
                    }
                }
                previous = Some(rows);
                touched.fill(false);
                others_recorded = true;
                if *policy != HashPolicy::Never {
                    digested.fill(true);
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(prop_config(16))]

    /// P21, walk leg: a warm walk reads exactly the changed files, re-reads
    /// nothing under `StaleOnly`, and every unchanged row is stable.
    #[test]
    fn p21_a_warm_walk_reads_exactly_the_changed_files(
        tree in tree(),
        steps in steps(),
        kind in cache_kind(),
    ) {
        match kind {
            CacheKind::Memory => check(&tree, &steps, MemoryCache::new()),
            CacheKind::Sqlite => check(&tree, &steps, SqliteCache::open_in_memory().unwrap()),
        }
    }
}

/// PINNED rows: shapes the generator may not reach in 16 cases.
#[test]
fn p21_pinned_shapes() {
    let tree = Tree {
        files: vec![(0, 5, 1), (2, 70_000, 2), (3, 0, 3), (1, 4_096, 4)],
        symlink: true,
    };
    let pinned: [&[Step]; 4] = [
        // Stat-only census first: it must not poison content resume.
        &[
            Step::Walk(HashPolicy::Never),
            Step::Walk(HashPolicy::StaleOnly),
            Step::Walk(HashPolicy::StaleOnly),
        ],
        // A warm Always walk counts every unchanged byte as re-read.
        &[
            Step::Walk(HashPolicy::StaleOnly),
            Step::Mutate(0b0010, 7),
            Step::Walk(HashPolicy::Always),
        ],
        // N changed files: the resume reads exactly those.
        &[
            Step::Walk(HashPolicy::Always),
            Step::Mutate(0b1011, 9),
            Step::Walk(HashPolicy::StaleOnly),
            Step::Walk(HashPolicy::StaleOnly),
        ],
        // A stat-only walk between a mutation and the resume.
        &[
            Step::Walk(HashPolicy::StaleOnly),
            Step::Mutate(0b0001, 3),
            Step::Walk(HashPolicy::Never),
            Step::Walk(HashPolicy::StaleOnly),
        ],
    ];
    for steps in pinned {
        check(&tree, steps, MemoryCache::new());
        check(&tree, steps, SqliteCache::open_in_memory().unwrap());
    }
}
