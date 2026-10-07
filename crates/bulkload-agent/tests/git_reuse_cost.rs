//! P69 REUSE-COST (Q42 lane L7, OI-1003-Q42, OI-1003-Q45, OI-1003-Q94).
//!
//! **The law.** A changed capture reuses its retained capture's blobs from
//! the capture's `{bundle}.reuse` manifest. When every seat it reuses is a
//! blob the source object store holds (no miss), the pass reads the manifest
//! and nothing of the retained bundle: `read_source_capture_reuse_bytes` is
//! 0, `read_reuse_manifest_bytes` is the manifest's length and
//! `reuse_dirty_misses` is 0. The bundle is fetched only on a miss (a seat
//! whose blob only the retained bundle holds: dirty content that has not
//! moved since), and every miss is counted. A retained capture without a
//! manifest (anything captured before lane L7) is fetched as before.
//!
//! **CPU.** The decision packet measured the fetch at 13.8 s of CPU for a
//! 5-object change against a 179 MB retained bundle, and 0.45 s for the same
//! capture one round later, against a 39 KB retained bundle
//! (`docs/plans/2026-10-04-git-engine-decision.md`, "What the numbers say",
//! item 3). The history-heavy row pins that ratio: the changed capture whose
//! retained capture is the whole history costs at most 1.5 times the same
//! capture against a thin retained capture (rusage user plus system of the
//! verb and every child it reaped; wall time is not read, OI-1003-Q35).
//!
//! The rows are a fixed table (OI-1003-Q7: no fuzzing), one test per row.
//! Each runs the verb binary end to end, so every counter is one process's:
//! a first pass, the change, the pass under test, then `estate-apply` and a
//! byte comparison of the restored workspace. Each row prints one `P69 ...`
//! line with its counters (`--nocapture`), the lane note's measurements.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repo);
    command
}

fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?}");
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = git(repo).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Incompressible bytes, so history is visible in a bundle's size.
fn noise(len: usize, mut state: u32) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

// Noise as hex: twice the bytes, and every one of them inflated and hashed
// by a fetch of the bundle that holds it.
fn hex_noise(len: usize, state: u32) -> Vec<u8> {
    noise(len / 2, state)
        .into_iter()
        .flat_map(|byte| {
            [
                b"0123456789abcdef"[usize::from(byte >> 4)],
                b"0123456789abcdef"[usize::from(byte & 15)],
            ]
        })
        .collect()
}

/// One verb run: its exit, its `counters` line, its stdout and its CPU.
struct Ran {
    ok: bool,
    counters: BTreeMap<String, u64>,
    stdout: String,
    /// rusage user plus system of the verb and every child it reaped.
    cpu: Duration,
}

/// Run one verb in its own process, reaped with `wait4` for its rusage.
fn verb(scratch: &Path, args: &[&std::ffi::OsStr]) -> Ran {
    let out = scratch.join("verb.stdout");
    let err = scratch.join("verb.stderr");
    let child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is a plain C struct of integers; all-zero is a valid
    // value, and wait4 overwrites it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is this test's own child, spawned above and never
        // waited on by std (the handle is only dropped). Both out-pointers
        // are valid, exclusive borrows for the duration of the call.
        let reaped = unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) };
        if reaped == pid {
            break;
        }
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
    let time = |value: libc::timeval| {
        Duration::from_secs(u64::try_from(value.tv_sec).unwrap())
            + Duration::from_micros(u64::try_from(value.tv_usec).unwrap())
    };
    let stderr = std::fs::read_to_string(&err).unwrap();
    let line = stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .unwrap_or_else(|| panic!("no counters line in {stderr}"));
    Ran {
        ok: libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        counters: line
            .split(' ')
            .filter_map(|pair| pair.split_once('='))
            .filter_map(|(key, value)| Some((key.to_owned(), value.parse().ok()?)))
            .collect(),
        stdout: std::fs::read_to_string(&out).unwrap(),
        cpu: time(usage.ru_utime) + time(usage.ru_stime),
    }
}

// Seats must be older than one timestamp tick to be reused by identity (R-N76).
fn settle() {
    std::thread::sleep(Duration::from_millis(2_100));
}

struct Estate {
    root: Root,
    source: PathBuf,
    plan: PathBuf,
    state: PathBuf,
    corpus: PathBuf,
    applied: PathBuf,
    target: PathBuf,
}

const HISTORY: usize = 256 * 1024;
const UNTRACKED: usize = 4 * 1024;

/// A one-item estate over a repository whose history is `history`, with an
/// untracked file beside it when `dirty`.
fn estate(name: &str, history: &[u8], dirty: bool) -> Estate {
    let root = std::env::temp_dir().join(format!(
        "bulkload-reuse-cost-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    run(git(&source).args(["init", "--quiet", "--template=", "-b", "main"]));
    std::fs::write(source.join("history"), history).unwrap();
    std::fs::write(source.join("file"), b"base").unwrap();
    run(git(&source).args(["add", "."]));
    run(git(&source).args(["commit", "--quiet", "-m", "base"]));
    if dirty {
        std::fs::write(source.join("untracked"), noise(UNTRACKED, 11)).unwrap();
    }
    let plan = root.join("plan");
    let target = root.join("target");
    let added = verb(
        &root,
        &[
            "estate-add".as_ref(),
            plan.as_os_str(),
            source.as_os_str(),
            target.as_os_str(),
            target.as_os_str(),
        ],
    );
    assert!(added.ok);
    Estate {
        source,
        plan,
        state: root.join("state"),
        corpus: root.join("corpus"),
        applied: root.join("applied"),
        target,
        root: Root(root),
    }
}

fn capture(estate: &Estate) -> Ran {
    let ran = verb(
        &estate.root.0,
        &[
            "estate-capture".as_ref(),
            estate.plan.as_os_str(),
            estate.state.as_os_str(),
            estate.corpus.as_os_str(),
            "1".as_ref(),
        ],
    );
    assert!(ran.ok, "{} {:?}", ran.stdout, ran.counters);
    ran
}

/// The bytes a capture's receipts say it streamed from source seats.
fn source_bytes_read(stdout: &str) -> u64 {
    stdout
        .split_whitespace()
        .filter_map(|field| field.strip_prefix("source_bytes_read="))
        .map(|value| value.parse::<u64>().unwrap())
        .sum()
}

/// Every `.bundle` in the corpus, newest last.
fn bundles(corpus: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bundle"))
        .map(|path| (std::fs::metadata(&path).unwrap().modified().unwrap(), path))
        .collect();
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

/// A bundle's `{bundle}.reuse` manifest.
fn manifest_of(bundle: &Path) -> PathBuf {
    let mut path = bundle.as_os_str().to_owned();
    path.push(".reuse");
    PathBuf::from(path)
}

/// Restore the corpus and compare the workspace with the source, byte for
/// byte: `HEAD`, the branches and every file but `.git`.
fn apply_and_compare(estate: &Estate) {
    let applied = verb(
        &estate.root.0,
        &[
            "estate-apply".as_ref(),
            estate.plan.as_os_str(),
            estate.corpus.as_os_str(),
            estate.applied.as_os_str(),
            "neo".as_ref(),
            "1".as_ref(),
        ],
    );
    assert!(applied.ok, "{} {:?}", applied.stdout, applied.counters);
    // Apply never packs from a source and never reads a manifest.
    assert_eq!(applied.counters["write_source_pack_bytes"], 0);
    for args in [
        ["rev-parse", "HEAD"].as_slice(),
        [
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
        ]
        .as_slice(),
    ] {
        assert_eq!(
            git_out(&estate.target, args),
            git_out(&estate.source, args),
            "{args:?}"
        );
    }
    let files = |root: &Path| -> BTreeMap<std::ffi::OsString, Vec<u8>> {
        std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_name() != ".git")
            .map(|entry| (entry.file_name(), std::fs::read(entry.path()).unwrap()))
            .collect()
    };
    let (restored, source) = (files(&estate.target), files(&estate.source));
    assert_eq!(
        restored.keys().collect::<Vec<_>>(),
        source.keys().collect::<Vec<_>>()
    );
    assert!(
        restored == source,
        "a restored file differs from its source"
    );
}

/// What sits beside the tracked files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dirt {
    /// Nothing: every seat's blob is in the source object store.
    Clean,
    /// An untracked file the change leaves alone: only the retained bundle
    /// holds its blob.
    UntrackedKept,
    /// An untracked file the change rewrites: its seat moved, so it is read
    /// again and never looked up.
    UntrackedEdited,
}

/// What moves between the retained capture and the pass under test.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    /// A commit that edits `file`.
    Commit,
    /// A new branch at `HEAD`: the key moves and no seat does.
    RefOnly,
}

/// What the pass under test must read to reuse.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reads {
    /// The manifest and nothing of the retained bundle.
    ManifestOnly,
    /// The whole retained bundle, after this many counted misses.
    Bundle { misses: u64 },
}

struct Row {
    name: &'static str,
    dirt: Dirt,
    change: Change,
    /// Remove the retained capture's manifest first: a capture from before
    /// lane L7.
    legacy: bool,
    reads: Reads,
}

/// The bytes the change put under seats: exactly what the pass may read.
fn change(estate: &Estate, row: &Row, round: u32) -> u64 {
    let mut moved = 0u64;
    match row.change {
        Change::Commit => {
            let bytes = format!("changed {round}");
            std::fs::write(estate.source.join("file"), &bytes).unwrap();
            run(git(&estate.source).args(["commit", "--quiet", "-am", "change"]));
            moved += bytes.len() as u64;
        }
        Change::RefOnly => {
            run(git(&estate.source).args(["branch", &format!("extra-{round}")]));
        }
    }
    if row.dirt == Dirt::UntrackedEdited {
        std::fs::write(estate.source.join("untracked"), noise(UNTRACKED, 13)).unwrap();
        moved += UNTRACKED as u64;
    }
    moved
}

fn row(row: &Row) {
    let name = row.name;
    let estate = estate(name, &noise(HISTORY, 7), row.dirt != Dirt::Clean);
    settle();
    let first = capture(&estate);
    assert_eq!(first.counters["read_source_capture_reuse_bytes"], 0);
    let retained = bundles(&estate.corpus).pop().unwrap();
    let retained_len = std::fs::metadata(&retained).unwrap().len();
    let manifest = manifest_of(&retained);
    let moved = change(&estate, row, 1);
    let manifest_len = if row.legacy {
        // Written by this engine, then removed: what a pass from before
        // lane L7 left.
        std::fs::remove_file(&manifest)
            .unwrap_or_else(|error| panic!("{name}: no manifest to remove: {error}"));
        0
    } else {
        std::fs::metadata(&manifest).map_or(0, |metadata| metadata.len())
    };
    let second = capture(&estate);
    let counter = |key: &str| second.counters.get(key).copied();
    eprintln!(
        "P69 row={name} retained={retained_len} manifest={manifest_len} \
         reuse_read={:?} manifest_read={:?} dirty_misses={:?} source_bytes_read={} \
         census_walks={:?} cpu_ms={}",
        counter("read_source_capture_reuse_bytes"),
        counter("read_reuse_manifest_bytes"),
        counter("reuse_dirty_misses"),
        source_bytes_read(&second.stdout),
        counter("census_walks"),
        second.cpu.as_millis(),
    );
    match row.reads {
        Reads::ManifestOnly => {
            assert_eq!(
                counter("read_source_capture_reuse_bytes"),
                Some(0),
                "{name}: the pass read the retained bundle ({retained_len} B) with no miss"
            );
            assert!(
                manifest_len > 0,
                "{name}: the retained capture has no manifest"
            );
            assert_eq!(counter("reuse_dirty_misses"), Some(0), "{name}");
        }
        Reads::Bundle { misses } => {
            // The retained capture is self-contained, so its fetch reads its
            // length and completes nothing from the source store.
            assert_eq!(
                counter("read_source_capture_reuse_bytes"),
                Some(retained_len),
                "{name}"
            );
            assert_eq!(counter("reuse_dirty_misses"), Some(misses), "{name}");
        }
    }
    // At most the manifest: exactly its bytes when there is one.
    assert_eq!(
        counter("read_reuse_manifest_bytes"),
        Some(manifest_len),
        "{name}"
    );
    // Reuse ran: only what the change moved was read (R25), and the pass
    // gave no reason for reusing nothing.
    assert_eq!(source_bytes_read(&second.stdout), moved, "{name}");
    assert!(
        !second.stdout.contains("reuse_unavailable"),
        "{name}: {}",
        second.stdout
    );
    // A changed capture still walks four censuses (unchanged by lane L7).
    assert_eq!(counter("census_walks"), Some(4), "{name}");
    // The capture under test publishes its own manifest.
    let head = bundles(&estate.corpus).pop().unwrap();
    assert_ne!(head, retained, "{name}: the pass did not recapture");
    assert!(manifest_of(&head).exists(), "{name}: no manifest published");
    apply_and_compare(&estate);
}

#[test]
fn p69_a_clean_commit_reads_the_manifest_and_no_bundle_byte() {
    row(&Row {
        name: "clean_commit",
        dirt: Dirt::Clean,
        change: Change::Commit,
        legacy: false,
        reads: Reads::ManifestOnly,
    });
}

#[test]
fn p69_a_ref_only_change_reads_the_manifest_and_no_bundle_byte() {
    row(&Row {
        name: "ref_only",
        dirt: Dirt::Clean,
        change: Change::RefOnly,
        legacy: false,
        reads: Reads::ManifestOnly,
    });
}

#[test]
fn p69_an_edited_dirty_seat_is_read_again_without_a_fetch() {
    row(&Row {
        name: "dirty_edited",
        dirt: Dirt::UntrackedEdited,
        change: Change::Commit,
        legacy: false,
        reads: Reads::ManifestOnly,
    });
}

#[test]
fn p69_an_unchanged_dirty_seat_is_a_counted_miss_and_fetches_as_before() {
    row(&Row {
        name: "dirty_kept",
        dirt: Dirt::UntrackedKept,
        change: Change::Commit,
        legacy: false,
        reads: Reads::Bundle { misses: 1 },
    });
}

#[test]
fn p69_a_retained_capture_without_a_manifest_fetches_as_before() {
    row(&Row {
        name: "legacy_no_manifest",
        dirt: Dirt::Clean,
        change: Change::Commit,
        legacy: true,
        reads: Reads::Bundle { misses: 0 },
    });
}

/// 64 MiB of hex in one blob: a fetch of the bundle that holds it inflates
/// and hashes every byte.
const HEAVY: usize = 64 * 1024 * 1024;

/// The CPU ratio the law allows (the decision packet's target: "about an
/// unchanged rerun's CPU, not 13.8 s").
const CPU_FACTOR: u32 = 3;
const CPU_DIVISOR: u32 = 2;

#[test]
fn p69_history_heavy_reuse_costs_no_more_cpu_than_a_thin_rerun() {
    let name = "history_heavy";
    let commit = Row {
        name,
        dirt: Dirt::Clean,
        change: Change::Commit,
        legacy: false,
        reads: Reads::ManifestOnly,
    };
    let estate = estate(name, &hex_noise(HEAVY, 7), false);
    settle();
    capture(&estate);
    let whole = bundles(&estate.corpus).pop().unwrap();
    let whole_len = std::fs::metadata(&whole).unwrap().len();
    assert!(whole_len >= (HEAVY / 4) as u64, "{whole_len}");
    // Informational: an unchanged rerun is a whole-capture hit, one census.
    let hit = capture(&estate);
    assert_eq!(hit.counters["census_walks"], 1);
    // The pass under test: one commit, against the whole-history bundle.
    change(&estate, &commit, 1);
    let heavy = capture(&estate);
    // The same capture, three more times, each against the thin bundle the
    // round before it wrote.
    let thin: Vec<Ran> = (2..=4)
        .map(|round| {
            let retained = bundles(&estate.corpus).pop().unwrap();
            assert!(
                std::fs::metadata(&retained).unwrap().len() < whole_len / 64,
                "round {round}: the retained capture is not thin"
            );
            change(&estate, &commit, round);
            capture(&estate)
        })
        .collect();
    let mut baseline: Vec<Duration> = thin.iter().map(|ran| ran.cpu).collect();
    baseline.sort();
    let baseline = baseline[1];
    eprintln!(
        "P69 row={name} retained={whole_len} heavy_cpu_ms={} thin_cpu_ms={:?} \
         baseline_ms={} hit_cpu_ms={} heavy_reuse_read={:?} heavy_manifest_read={:?}",
        heavy.cpu.as_millis(),
        thin.iter()
            .map(|ran| ran.cpu.as_millis())
            .collect::<Vec<_>>(),
        baseline.as_millis(),
        hit.cpu.as_millis(),
        heavy.counters.get("read_source_capture_reuse_bytes"),
        heavy.counters.get("read_reuse_manifest_bytes"),
    );
    for (round, ran) in std::iter::once(&heavy).chain(&thin).enumerate() {
        assert_eq!(
            ran.counters.get("read_source_capture_reuse_bytes").copied(),
            Some(0),
            "round {}: the pass read its retained bundle with no miss",
            round + 1
        );
        assert_eq!(
            ran.counters.get("reuse_dirty_misses").copied(),
            Some(0),
            "round {}",
            round + 1
        );
    }
    assert!(
        heavy.cpu * CPU_DIVISOR <= baseline * CPU_FACTOR,
        "reuse against the whole-history bundle cost {:?} of CPU, over 1.5 times \
         the {baseline:?} of the same capture against a thin bundle",
        heavy.cpu
    );
    apply_and_compare(&estate);
}
