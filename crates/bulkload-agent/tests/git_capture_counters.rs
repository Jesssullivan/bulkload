//! v1 Git capture accounting through the verb's own process counters (WP2,
//! OI-1003-Q15, R-N13).
//!
//! Counters are process scope, so each assertion reads the `counters` line a
//! separate `bulkload-agent` process printed for exactly one verb run: the
//! values are exact, not lower bounds shared with concurrent tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Incompressible bytes, so a full re-pack of history is visible in the pack size.
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

/// Run one verb and return its exit status and its `counters` line as a map.
fn verb(args: &[&std::ffi::OsStr]) -> (bool, BTreeMap<String, u64>) {
    let result = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8(result.stderr).unwrap();
    let line = stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .unwrap_or_else(|| panic!("no counters line in {stderr}"));
    let counters = line
        .split(' ')
        .filter_map(|pair| pair.split_once('='))
        .filter_map(|(key, value)| Some((key.to_owned(), value.parse().ok()?)))
        .collect();
    (result.status.success(), counters)
}

// Seats must be older than one timestamp tick for a whole-capture reuse (R-N76).
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

struct Estate {
    _root: Root,
    source: PathBuf,
    plan: PathBuf,
    state: PathBuf,
    corpus: PathBuf,
    applied: PathBuf,
}

const HISTORY: usize = 256 * 1024;

fn estate(name: &str) -> Estate {
    let root = std::env::temp_dir().join(format!(
        "bulkload-capture-counters-{name}-{}-{}",
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
    std::fs::write(source.join("history"), noise(HISTORY, 7)).unwrap();
    std::fs::write(source.join("file"), b"base").unwrap();
    run(git(&source).args(["add", "."]));
    run(git(&source).args(["commit", "--quiet", "-m", "base"]));
    let plan = root.join("plan");
    let target = root.join("target");
    let (ok, _) = verb(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        source.as_os_str(),
        target.as_os_str(),
        target.as_os_str(),
    ]);
    assert!(ok);
    Estate {
        source,
        plan,
        state: root.join("state"),
        corpus: root.join("corpus"),
        applied: root.join("applied"),
        _root: Root(root),
    }
}

fn capture(estate: &Estate) -> BTreeMap<String, u64> {
    let (ok, counters) = verb(&[
        "estate-capture".as_ref(),
        estate.plan.as_os_str(),
        estate.state.as_os_str(),
        estate.corpus.as_os_str(),
        "1".as_ref(),
    ]);
    assert!(ok, "{counters:?}");
    counters
}

fn bundles(corpus: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bundle"))
        .collect()
}

#[test]
fn a_capture_counts_its_pack_its_censuses_and_its_reuse_reads() {
    let estate = estate("pack");
    settle();
    let first = capture(&estate);
    // The pack holds every object of history, including the incompressible
    // blob; the counter is the bundle git wrote, exactly.
    let written = bundles(&estate.corpus);
    assert_eq!(written.len(), 1);
    let size = std::fs::metadata(&written[0]).unwrap().len();
    assert_eq!(first["write_source_pack_bytes"], size);
    assert!(first["write_source_pack_bytes"] >= HISTORY as u64);
    // commit, root tree, two blobs, plus the capture's own refs' objects.
    assert!(first["write_source_pack_objects"] >= 4, "{first:?}");
    // Nothing was retained, so nothing was fetched for reuse.
    assert_eq!(first["read_source_capture_reuse_bytes"], 0);
    // A changed item costs four metadata censuses today: the pre-pass key,
    // the export's own census before and after its byte pass, and the
    // post-pass key (architecture review finding 5; WP7 reduces it).
    assert_eq!(first["census_walks"], 4, "{first:?}");

    let reused = capture(&estate);
    // A reuse hit is one census and no pack at all.
    assert_eq!(reused["census_walks"], 1, "{reused:?}");
    assert_eq!(reused["write_source_pack_bytes"], 0);
    assert_eq!(reused["write_source_pack_objects"], 0);
    assert_eq!(reused["read_source_pack_readback_bytes"], 0);
    assert_eq!(reused["read_source_file_bytes"], 0);

    std::fs::write(estate.source.join("file"), b"changed").unwrap();
    run(git(&estate.source).args(["commit", "--quiet", "-am", "change"]));
    let changed = capture(&estate);
    // The retained capture is fetched once for blob reuse, and counted.
    assert_eq!(changed["read_source_capture_reuse_bytes"], size);
    assert!(changed["write_source_pack_objects"] >= 1);
    // WP2 PR 2: the rerun declares the retained capture's source-held tips
    // as prerequisites, so it packs only the new commit, its tree and blob,
    // and the capture's own metadata: never the history blob again.
    assert!(
        changed["write_source_pack_bytes"] < (HISTORY / 8) as u64,
        "{changed:?}"
    );
    assert!(changed["write_source_pack_objects"] < first["write_source_pack_objects"] + 8);

    let (ok, applied) = verb(&[
        "estate-apply".as_ref(),
        estate.plan.as_os_str(),
        estate.corpus.as_os_str(),
        estate.applied.as_os_str(),
        "neo".as_ref(),
        "1".as_ref(),
    ]);
    assert!(ok, "{applied:?}");
    // Staging copies the published bundle once, hashing exactly what it reads.
    assert!(applied["read_bundle_stage_bytes"] > 0);
    assert_eq!(
        applied["read_bundle_stage_bytes"],
        applied["blake3_bundle_stage_bytes"]
    );
    assert_eq!(
        applied["read_bundle_stage_bytes"],
        applied["write_bundle_stage_bytes"]
    );
    // Apply never packs from a source.
    assert_eq!(applied["write_source_pack_bytes"], 0);
    // The chained capture restored exactly: HEAD and every byte.
    let head = |repo: &Path| {
        let out = git(repo).args(["rev-parse", "HEAD"]).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let target = estate.source.with_file_name("target");
    assert_eq!(head(&target), head(&estate.source));
    assert_eq!(
        std::fs::read(target.join("history")).unwrap(),
        noise(HISTORY, 7)
    );
    assert_eq!(std::fs::read(target.join("file")).unwrap(), b"changed");
}
