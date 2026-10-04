//! P66 ESTIMATE-DAG: `git-carry-estimate` equals the upload-pack oracle over
//! random DAGs (OI-1003-Q42, OI-1003-Q44; the estimate leg of P46; R-N74,
//! R-N97, R-N113, R-N116).
//!
//! This is the estimate half of `random_dags_equal_upload_pack` in
//! `tests/git_carry_v2.rs`, rehomed so it survives the deletion of carry v2
//! (architecture review WP2 PR 3). It uses nothing PR 3 deletes: the product
//! code under test is `bulkload_agent::git_carry::estimate::estimate`, and
//! the request the oracle is fed is derived here from the two repositories
//! with plain `git`, not from the sender's first round.
//!
//! The oracle is R-N113's: `upload-pack --stateless-rpc` (protocol v2) fed M1's
//! first round, under the pack pins. The wants are the source's tips that the
//! destination does not hold. The haves are exactly the destination's tips
//! that the source holds, ancestors first (R-N116).
//!
//! [`check`] asserts on every DAG:
//! - **Request.** `haves_used` is the held-tip count, and
//!   `destination_tip_count` counts every destination tip. A destination-only
//!   commit is a tip but not a have.
//! - **Size.** `thin_pack` equals the oracle's pack: the same header count and
//!   the same byte length. An oracle pack of zero objects counts as the
//!   estimate's "no pack" (`ThinPack::default()`). When every want is
//!   reachable from the haves, upload-pack still streams an empty 32-byte
//!   pack, while the estimate builds nothing.
//! - **Object set.** The oracle's pack indexes on its own against the
//!   destination's store (`index-pack --fix-thin` into a throwaway object
//!   directory). The per-type count and `%(objectsize:disk)` tally of exactly
//!   the oids it carries equals `missing`, with nothing unavailable.
//! - **Closure.** `source` is the same tally over the closure of the
//!   source's tips.
//!
//! **Corpus.** CI runs a fixed seed and 12 cases ([`prop_config`]), plus a
//! PINNED table of shapes the generator reaches rarely.
//! `BULKLOAD_PROPTEST_DEEP=1` switches to random seeds and twenty times the
//! cases, as `test_support::prop_config` does. That helper is
//! `#[cfg(test)] pub(crate)`, out of an integration test's reach, so this file
//! mirrors it (same seed, same switch) instead of widening the library API.
//!
//! Every test builds its repositories under the system temp dir, runs the
//! real `git` on `PATH`, and removes them afterwards. Measurements print as
//! `p66 case=<name> key=value ...` lines (`--nocapture`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use bulkload_agent::git_carry::estimate::{estimate, Destination, Tally, ThinPack};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

// ---------------------------------------------------------------------------
// Corpus configuration
// ---------------------------------------------------------------------------

/// `test_support::CI_SEED`, mirrored: every CI run draws the same cases.
const CI_SEED: u64 = 0x0B01_C0AD_2026_1003;

/// `test_support::DEEP`, mirrored: the switch for the deep local tier.
const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";

/// `test_support::prop_config`, mirrored for an integration test: `cases`
/// fixed-seed cases in CI; random seeds and twenty times the cases under
/// `BULKLOAD_PROPTEST_DEEP=1`. Nothing persists between runs; a failing deep
/// run prints its seed, which is pinned as a PINNED row.
fn prop_config(cases: u32) -> Config {
    let deep = std::env::var_os(DEEP).is_some_and(|value| value == "1");
    Config {
        cases: if deep {
            cases.saturating_mul(20)
        } else {
            cases
        },
        rng_seed: if deep {
            RngSeed::Random
        } else {
            RngSeed::Fixed(CI_SEED)
        },
        failure_persistence: None,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// Fixture Git: hooks off (R-N98 allows that in fixtures this test creates
/// and removes), the estimate's pack pins plus `pack.useSparse=false` and
/// `pack.useBitmaps=false` (so upload-pack's `pack-objects` packs exactly the
/// walked set, as the estimate's does), a fixed identity and date, no user or
/// system configuration, and no redirecting variable from the parent
/// environment.
fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_QUARANTINE_PATH",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "pack.threads=2",
            "-c",
            "pack.windowMemory=64m",
            "-c",
            "pack.useSparse=false",
            "-c",
            "pack.useBitmaps=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-C",
        ])
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "bulkload")
        .env("GIT_AUTHOR_EMAIL", "bulkload@invalid")
        .env("GIT_COMMITTER_NAME", "bulkload")
        .env("GIT_COMMITTER_EMAIL", "bulkload@invalid")
        .env("GIT_AUTHOR_DATE", "1790121600 +0000")
        .env("GIT_COMMITTER_DATE", "1790121600 +0000");
    command
}

/// Run `command` with `stdin` written from a thread, so a large answer never
/// deadlocks against an unread request.
fn feed(mut command: Command, stdin: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    let mut pipe = child.stdin.take().expect("stdin");
    let input = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        // A child that exits early closes the pipe; that is its answer.
        let _ = pipe.write_all(&input);
    });
    let output = child.wait_with_output().expect("wait git");
    writer.join().expect("writer");
    output
}

fn ok(output: Output, what: &str) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn run(mut command: Command, what: &str) -> Vec<u8> {
    ok(command.output().expect("spawn git"), what)
}

fn text_of(output: Output, what: &str) -> String {
    String::from_utf8(ok(output, what))
        .expect("utf8")
        .trim_end()
        .to_owned()
}

fn text(mut command: Command, what: &str) -> String {
    text_of(command.output().expect("spawn git"), what)
}

fn args<const N: usize>(repo: &Path, list: [&str; N]) -> Command {
    let mut command = git(repo);
    command.args(list);
    command
}

fn rev(repo: &Path, name: &str) -> String {
    text(
        args(repo, ["rev-parse", "--verify", "-q", name]),
        "rev-parse",
    )
}

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// A temp directory removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!(
            "bulkload-estimate-dag-{name}-{}-{}-{nanos}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("scratch");
        let root = root.canonicalize().expect("canonical scratch");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// A fresh bare repository.
    fn bare(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let mut command = git(&self.root);
        command
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&path);
        run(command, "init");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// xorshift64*: deterministic fixture content with no dependency.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// `count` distinct-looking lines drawn from `seed` (the bytes
/// `tests/git_carry_v2.rs`'s `noise` writes).
fn noise(seed: u64, count: usize) -> String {
    use std::fmt::Write as _;
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761) + 1);
    let mut body = String::new();
    for i in 0..count {
        let _ = writeln!(body, "line {i} {:016x}", rng.next());
    }
    body
}

fn objects_dir(repo: &Path) -> PathBuf {
    let dotgit = repo.join(".git");
    if dotgit.is_dir() {
        dotgit.join("objects")
    } else {
        repo.join("objects")
    }
}

fn has_object(repo: &Path, oid: &str) -> bool {
    args(repo, ["cat-file", "-e", oid])
        .status()
        .expect("cat-file")
        .success()
}

/// Every ref tip and `HEAD` of `repo` (a bare repository has no other
/// worktree `HEAD` for the estimate's probe to add).
fn tips_of(repo: &Path) -> BTreeSet<String> {
    let mut tips: BTreeSet<String> = text(
        args(repo, ["for-each-ref", "--format=%(objectname)"]),
        "for-each-ref",
    )
    .lines()
    .map(str::to_owned)
    .collect();
    let head = args(repo, ["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .expect("rev-parse HEAD");
    if head.status.success() {
        tips.insert(String::from_utf8_lossy(&head.stdout).trim().to_owned());
    }
    tips
}

fn peeled_commit(repo: &Path, oid: &str) -> Option<String> {
    let out = args(
        repo,
        ["rev-parse", "--verify", "-q", &format!("{oid}^{{commit}}")],
    )
    .output()
    .expect("rev-parse peel");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `held` ordered as M1 offers it (R-N116, spike D1): the topological order
/// of the peeled commits, ancestors first, then any tip that peels to no
/// commit.
fn ancestors_first(repo: &Path, held: &BTreeSet<String>) -> Vec<String> {
    let peeled: Vec<(String, Option<String>)> = held
        .iter()
        .map(|tip| (tip.clone(), peeled_commit(repo, tip)))
        .collect();
    let commits: BTreeSet<&String> = peeled
        .iter()
        .filter_map(|(_, commit)| commit.as_ref())
        .collect();
    let mut haves = Vec::new();
    if !commits.is_empty() {
        let mut list = git(repo);
        list.args(["rev-list", "--topo-order", "--reverse"])
            .args(&commits);
        for commit in text(list, "rev-list --topo-order").lines() {
            for (tip, peel) in &peeled {
                if peel.as_deref() == Some(commit) {
                    haves.push(tip.clone());
                }
            }
        }
    }
    haves.extend(
        peeled
            .iter()
            .filter(|(_, peel)| peel.is_none())
            .map(|(tip, _)| tip.clone()),
    );
    assert_eq!(haves.len(), held.len(), "every held tip is offered once");
    haves
}

/// The per-type count and `%(objectsize:disk)` of `oids` in `repo`, shaped
/// as the estimate's [`Tally`].
fn tally_of<'a>(repo: &Path, oids: impl IntoIterator<Item = &'a String>) -> Tally {
    let mut input = String::new();
    for oid in oids {
        input.push_str(oid);
        input.push('\n');
    }
    let mut tally = Tally::default();
    if input.is_empty() {
        return tally;
    }
    let listing = text_of(
        feed(
            args(
                repo,
                [
                    "cat-file",
                    "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
                ],
            ),
            input.as_bytes(),
        ),
        "cat-file sizes",
    );
    for line in listing.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields.len(), 3, "batch-check line: {line}");
        let slot = match fields[1] {
            "commit" => &mut tally.commit,
            "tree" => &mut tally.tree,
            "blob" => &mut tally.blob,
            "tag" => &mut tally.tag,
            other => panic!("unexpected object type {other} in {line}"),
        };
        slot.count += 1;
        slot.bytes_disk += fields[2].parse::<u64>().expect("objectsize:disk");
    }
    tally
}

/// The whole object closure of `tips` in `repo`.
fn closure(repo: &Path, tips: &BTreeSet<String>) -> Vec<String> {
    let mut input = String::new();
    for tip in tips {
        input.push_str(tip);
        input.push('\n');
    }
    text_of(
        feed(
            args(
                repo,
                ["rev-list", "--objects", "--no-object-names", "--stdin"],
            ),
            input.as_bytes(),
        ),
        "rev-list --objects",
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

// ---------------------------------------------------------------------------
// The oracle and the object-set reader
// ---------------------------------------------------------------------------

fn pkt_line(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(line.as_bytes());
}

/// R-N113's exactness oracle: `upload-pack --stateless-rpc` (protocol v2)
/// given exactly `haves`, in that order, under the pack pins. Returns the
/// pack it streams on sideband 1 (empty when it streams none).
fn upload_pack(source: &Path, wants: &BTreeSet<String>, haves: &[String]) -> Vec<u8> {
    let mut request = Vec::new();
    pkt_line(&mut request, "command=fetch\n");
    request.extend_from_slice(b"0001");
    for line in ["thin-pack\n", "ofs-delta\n", "no-progress\n"] {
        pkt_line(&mut request, line);
    }
    for oid in wants {
        pkt_line(&mut request, &format!("want {oid}\n"));
    }
    for oid in haves {
        pkt_line(&mut request, &format!("have {oid}\n"));
    }
    pkt_line(&mut request, "done\n");
    request.extend_from_slice(b"0000");
    let mut command = git(source);
    command
        .args(["upload-pack", "--stateless-rpc", "."])
        .env("GIT_PROTOCOL", "version=2");
    let answer = ok(feed(command, &request), "upload-pack");
    let mut rest = &answer[..];
    let mut in_pack = false;
    let mut pack = Vec::new();
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if length < 4 {
            rest = &rest[4..];
            if length == 0 && in_pack {
                break;
            }
            continue;
        }
        let payload = &rest[4..length];
        rest = &rest[length..];
        if in_pack {
            assert_ne!(
                payload[0],
                3,
                "upload-pack error: {}",
                String::from_utf8_lossy(&payload[1..])
            );
            if payload[0] == 1 {
                pack.extend_from_slice(&payload[1..]);
            }
        } else if payload == b"packfile\n" {
            in_pack = true;
        }
    }
    pack
}

fn header_count(pack: &[u8]) -> u32 {
    if pack.is_empty() {
        return 0;
    }
    assert_eq!(&pack[..4], b"PACK", "pack signature");
    u32::from_be_bytes(pack[8..12].try_into().unwrap())
}

/// The oids a thin pack carries, and proof that it indexes on its own: it is
/// indexed with `--fix-thin` into a throwaway object directory whose only
/// alternate is `destination`'s store, and the set is the `.idx` entries
/// whose offset lies inside the received bytes (`--fix-thin` appends its
/// bases after them).
fn carried(scratch: &Scratch, destination: &Path, pack: &[u8]) -> BTreeSet<String> {
    if header_count(pack) == 0 {
        return BTreeSet::new();
    }
    let quarantine = scratch.path(&format!(
        "quarantine-{}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(quarantine.join("pack")).unwrap();
    let mut command = git(destination);
    command
        .args(["index-pack", "--stdin", "--fix-thin"])
        .env("GIT_OBJECT_DIRECTORY", &quarantine)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects_dir(destination))
        .env("GIT_QUARANTINE_PATH", &quarantine);
    let out = String::from_utf8(ok(feed(command, pack), "index-pack alone")).unwrap();
    let hash = out.trim().strip_prefix("pack\t").unwrap().to_owned();
    let idx = quarantine.join("pack").join(format!("pack-{hash}.idx"));
    let listing = ok(
        feed(args(destination, ["show-index"]), &fs::read(idx).unwrap()),
        "show-index",
    );
    fs::remove_dir_all(&quarantine).unwrap();
    let limit = pack.len() - 20;
    String::from_utf8(listing)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let offset: usize = fields.next()?.parse().ok()?;
            let oid = fields.next()?;
            (offset < limit).then(|| oid.to_owned())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The estimate, judged
// ---------------------------------------------------------------------------

/// Estimate `source` -> `destination` and assert P66 (module docs) against
/// the oracle fed M1's first round, derived here from the repositories.
fn check(scratch: &Scratch, name: &str, source: &Path, destination: &Path) {
    let destination_tips = tips_of(destination);
    let held: BTreeSet<String> = destination_tips
        .iter()
        .filter(|tip| has_object(source, tip))
        .cloned()
        .collect();
    let haves = ancestors_first(source, &held);
    let source_tips = tips_of(source);
    let wants: BTreeSet<String> = source_tips.difference(&held).cloned().collect();

    let pack = upload_pack(source, &wants, &haves);
    let objects = header_count(&pack);
    let oracle = if objects == 0 {
        ThinPack::default()
    } else {
        ThinPack {
            bytes: u64::try_from(pack.len()).unwrap(),
            objects: u64::from(objects),
        }
    };
    let set = carried(scratch, destination, &pack);
    assert_eq!(
        u64::try_from(set.len()).unwrap(),
        oracle.objects,
        "{name}: the oracle pack indexes alone and carries its header count"
    );
    let oracle_tally = tally_of(source, &set);
    let source_tally = tally_of(source, &closure(source, &source_tips));

    let measured = estimate(source, &Destination::Local(destination.to_path_buf()))
        .unwrap_or_else(|refused| panic!("{name}: estimate refused: {refused}"));
    println!(
        "p66 case={name} destination_tips={} held={} wants={} oracle_stream_bytes={} \
         oracle_objects={} oracle_bytes={} estimate_objects={} estimate_bytes={} \
         missing_objects={} missing_bytes_disk={} source_objects={}",
        destination_tips.len(),
        haves.len(),
        wants.len(),
        pack.len(),
        oracle.objects,
        oracle.bytes,
        measured.thin_pack.objects,
        measured.thin_pack.bytes,
        measured.missing.objects(),
        measured.missing.bytes_disk(),
        measured.source.objects(),
    );
    assert_eq!(
        measured.destination_tip_count,
        destination_tips.len(),
        "{name}: every destination tip counted"
    );
    assert_eq!(
        measured.haves_used,
        haves.len(),
        "{name}: haves are exactly the held tips"
    );
    assert_eq!(measured.destination_shallow_count, 0, "{name}: full");
    assert_eq!(measured.source_shallow_count, 0, "{name}: full");
    assert!(!measured.source_partial, "{name}: not partial");
    assert_eq!(measured.stash_entries, 0, "{name}: no stash");
    assert_eq!(
        measured.thin_pack, oracle,
        "{name}: estimate.thin_pack == upload-pack (objects and bytes)"
    );
    assert_eq!(
        measured.missing, oracle_tally,
        "{name}: missing == the oracle's object set, per type and disk size"
    );
    assert_eq!(
        measured.source, source_tally,
        "{name}: source == the closure of the source's tips"
    );
}

// ---------------------------------------------------------------------------
// Random DAGs
// ---------------------------------------------------------------------------

/// Parent indexes, and (file, content seed) edits.
type Commit = (Vec<usize>, Vec<(usize, u64)>);

/// A random history: each commit names up to two earlier parents and edits
/// one to three of four files. `source_tips` and `held` index commits;
/// `stranger` adds a destination-only commit the source never saw.
#[derive(Debug, Clone)]
struct Dag {
    commits: Vec<Commit>,
    source_tips: Vec<usize>,
    held: Vec<usize>,
    stranger: bool,
}

/// `tests/git_carry_v2.rs`'s generator without the sender's segment-cap
/// dimension, which the estimate does not have. The last commit is always a
/// source tip, and the source's `HEAD`.
fn dag() -> impl Strategy<Value = Dag> {
    (2_usize..9).prop_flat_map(|n| {
        let commits = (0..n)
            .map(|i| {
                let parents = if i == 0 {
                    Just(Vec::new()).boxed()
                } else {
                    proptest::collection::btree_set(0..i, 1..=2.min(i))
                        .prop_map(|set| set.into_iter().collect())
                        .boxed()
                };
                let edits = proptest::collection::vec((0_usize..4, any::<u64>()), 1..=3);
                (parents, edits)
            })
            .collect::<Vec<_>>();
        (
            commits,
            proptest::collection::btree_set(0..n, 1..=n),
            proptest::collection::btree_set(0..n, 0..=n),
            any::<bool>(),
        )
            .prop_map(move |(commits, source_tips, held, stranger)| {
                let mut source_tips: Vec<usize> = source_tips.into_iter().collect();
                if !source_tips.contains(&(n - 1)) {
                    source_tips.push(n - 1);
                }
                Dag {
                    commits,
                    source_tips,
                    held: held.into_iter().collect(),
                    stranger,
                }
            })
    })
}

/// Build `dag` in a fresh bare source with one fast-import stream, then give
/// a bare destination exactly the held commits.
fn build(scratch: &Scratch, dag: &Dag) -> (PathBuf, PathBuf) {
    use std::fmt::Write as _;
    let source = scratch.bare("source.git");
    let mut stream = String::new();
    let mut blob_mark = 1_000;
    for (index, (parents, edits)) in dag.commits.iter().enumerate() {
        let mut files = Vec::new();
        for (file, seed) in edits {
            blob_mark += 1;
            let body = noise(*seed, 40 + usize::try_from(seed % 60).unwrap());
            let _ = write!(
                stream,
                "blob\nmark :{blob_mark}\ndata {}\n{body}\n",
                body.len()
            );
            files.push((file, blob_mark));
        }
        let message = format!("c{index}");
        let _ = write!(
            stream,
            "commit refs/fi/c{index}\nmark :{}\ncommitter T <t@invalid> {} +0000\ndata {}\n{message}\n",
            index + 1,
            1_790_000_000 + index * 60,
            message.len()
        );
        let mut parents = parents.iter();
        if let Some(first) = parents.next() {
            let _ = writeln!(stream, "from :{}", first + 1);
        }
        for other in parents {
            let _ = writeln!(stream, "merge :{}", other + 1);
        }
        for (file, mark) in files {
            let _ = writeln!(stream, "M 100644 :{mark} f{file}.txt");
        }
        stream.push('\n');
    }
    ok(
        feed(args(&source, ["fast-import", "--quiet"]), stream.as_bytes()),
        "fast-import",
    );
    let oid = |index: usize| rev(&source, &format!("refs/fi/c{index}"));
    let destination = scratch.bare("destination.git");
    if !dag.held.is_empty() {
        let mut fetch = git(&destination);
        fetch.args(["fetch", "-q"]).arg(&source);
        for index in &dag.held {
            fetch.arg(format!("refs/fi/c{index}:refs/heads/d{index}"));
        }
        run(fetch, "fetch held");
    }
    for index in &dag.source_tips {
        run(
            args(
                &source,
                ["update-ref", &format!("refs/heads/b{index}"), &oid(*index)],
            ),
            "tip",
        );
    }
    // The fast-import refs go; their commits stay in the source's store, so
    // a held commit no source ref names is still a have.
    for index in 0..dag.commits.len() {
        run(
            args(&source, ["update-ref", "-d", &format!("refs/fi/c{index}")]),
            "drop",
        );
    }
    run(
        args(
            &source,
            [
                "symbolic-ref",
                "HEAD",
                &format!("refs/heads/b{}", dag.commits.len() - 1),
            ],
        ),
        "HEAD",
    );
    if dag.stranger {
        let tree = text_of(feed(args(&destination, ["mktree"]), b""), "mktree");
        let stranger = text_of(
            feed(
                args(&destination, ["commit-tree", &tree, "-m", "stranger"]),
                b"",
            ),
            "commit-tree",
        );
        run(
            args(
                &destination,
                ["update-ref", "refs/heads/stranger", &stranger],
            ),
            "stranger",
        );
    }
    (source, destination)
}

fn check_dag(name: &str, dag: &Dag) {
    let scratch = Scratch::new(name);
    let (source, destination) = build(&scratch, dag);
    check(&scratch, name, &source, &destination);
}

/// One edit of file `file` with content seed `seed`.
fn edit(file: usize, seed: u64) -> Vec<(usize, u64)> {
    vec![(file, seed)]
}

/// Shapes the generator reaches rarely in 12 cases, each judged by the same
/// [`check`].
#[test]
fn pinned_dags_equal_upload_pack() {
    let chain = |n: usize| -> Vec<Commit> {
        (0..n)
            .map(|i| {
                let parents = if i == 0 { Vec::new() } else { vec![i - 1] };
                (parents, edit(i % 2, 7 + u64::try_from(i).unwrap()))
            })
            .collect()
    };
    let pinned: [(&str, Dag); 5] = [
        // The thin case: the destination holds the tip's parent, so the
        // pack is the tip's commit, tree and a blob deltified against a
        // have.
        (
            "parent-held",
            Dag {
                commits: chain(4),
                source_tips: vec![3],
                held: vec![2],
                stranger: false,
            },
        ),
        // A want reachable from a have: upload-pack streams an empty pack
        // (zero objects), and the estimate builds none.
        (
            "want-behind-a-have",
            Dag {
                commits: chain(3),
                source_tips: vec![1, 2],
                held: vec![2],
                stranger: false,
            },
        ),
        // Every source tip held: no want, so no pack at all.
        (
            "all-held",
            Dag {
                commits: chain(3),
                source_tips: vec![2],
                held: vec![2],
                stranger: false,
            },
        ),
        // Nothing held, one destination-only tip: no have, the whole
        // closure is missing, and the stranger counts as a tip only.
        (
            "nothing-held-stranger",
            Dag {
                commits: chain(3),
                source_tips: vec![2],
                held: Vec::new(),
                stranger: true,
            },
        ),
        // A merge over two held branch tips, offered ancestors first, plus a
        // held commit no source ref names.
        (
            "merge-over-held-branches",
            Dag {
                commits: vec![
                    (Vec::new(), edit(0, 1)),
                    (vec![0], edit(1, 2)),
                    (vec![0], edit(2, 3)),
                    (vec![1, 2], vec![(0, 4), (3, 5)]),
                    (vec![3], edit(1, 6)),
                ],
                source_tips: vec![4],
                held: vec![0, 1, 2],
                stranger: true,
            },
        ),
    ];
    for (name, dag) in &pinned {
        check_dag(name, dag);
    }
}

proptest! {
    #![proptest_config(prop_config(12))]

    /// On random DAGs, with random source tips, held commits (named or not
    /// by any source ref) and a destination-only tip, the estimate equals
    /// the upload-pack oracle: the thin pack's objects and bytes, and the
    /// missing set's per-type count and disk size.
    #[test]
    fn estimate_equals_upload_pack_over_random_dags(dag in dag()) {
        check_dag("random-dag", &dag);
    }
}
