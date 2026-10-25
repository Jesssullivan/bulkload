//! SPEC rule 8: first-fetch custody survives transport loss without
//! accepting an unrelated directory or overwriting native Git state.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use bulkload_agent::estate::{self, streamed};
use bulkload_agent::first_fetch::{self, Target};
use bulkload_agent::git_carry::stream;
use bulkload_proto::BulkloadRefusal;

const ITEM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct World {
    _directory: tempfile::TempDir,
    source: PathBuf,
    live: PathBuf,
    state: PathBuf,
    private: PathBuf,
}

impl World {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(directory.path()).unwrap();
        let state = base.join("state");
        let private = state.join("private");
        fs::create_dir_all(&private).unwrap();
        Self {
            _directory: directory,
            source: base.join("source"),
            live: base.join("live"),
            state,
            private,
        }
    }

    fn target(&self) -> Target<'_> {
        Target {
            item: ITEM,
            source_label: "neo",
            source_host: "neo",
            source_path: &self.source,
            live_path: &self.live,
            expected_shallow: &[],
        }
    }

    fn marker(&self) -> PathBuf {
        self.private.join(format!("{ITEM}.first-fetch"))
    }
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
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
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_failed_object_fetch_resumes_once_without_a_landing_journal() {
    let world = World::new();
    fs::create_dir(&world.source).unwrap();
    git(
        &world.source,
        &["init", "--quiet", "--template=", "-b", "main"],
    );
    fs::write(world.source.join("work.txt"), b"unique source work\n").unwrap();
    git(&world.source, &["add", "."]);
    git(&world.source, &["commit", "--quiet", "-m", "fixture"]);
    let item = estate::Item {
        source: world.source.clone(),
        repository: world.live.clone(),
        workspace: Some(world.live.clone()),
    };
    let snapshot = streamed::capture_source(&item, 1, false).unwrap();
    first_fetch::begin(&world.private, &world.target()).unwrap();
    assert!(!world.live.join(".git").exists());
    let marker = fs::read(world.marker()).unwrap();
    assert!(streamed::fetch_objects(&snapshot, &world.private, "false", &[]).is_err());
    assert!(world.live.join(".git/objects").is_dir());
    assert!(!world.live.join(".git/HEAD").exists());
    assert!(first_fetch::prove(&world.private, &world.target()).unwrap());
    first_fetch::begin(&world.private, &world.target()).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    let resumed = streamed::fetch_objects(&snapshot, &world.private, &upload, &[]).unwrap();
    assert!(resumed.bytes > 0);
    assert!(first_fetch::prove(&world.private, &world.target()).unwrap());
    let again = streamed::fetch_objects(&snapshot, &world.private, &upload, &[]).unwrap();
    assert_eq!(again.bytes, 0);
    assert_eq!(again.batches, 0);
    assert_eq!(fs::read(world.marker()).unwrap(), marker);
    assert_eq!(
        bulkload_agent::state_payload::measure(&world.state)
            .unwrap()
            .payload_bytes,
        0
    );
}

#[test]
fn a_crash_before_the_marker_leaves_only_an_adoptable_empty_root() {
    let world = World::new();
    fs::create_dir(&world.live).unwrap();
    assert!(!first_fetch::prove(&world.private, &world.target()).unwrap());
    first_fetch::begin(&world.private, &world.target()).unwrap();
    assert!(first_fetch::prove(&world.private, &world.target()).unwrap());
    assert_eq!(fs::read_dir(&world.live).unwrap().count(), 0);
}

#[test]
fn a_pre_marker_object_tree_is_never_claimed() {
    let world = World::new();
    fs::create_dir_all(world.live.join(".git/objects/pack")).unwrap();
    assert!(!first_fetch::prove(&world.private, &world.target()).unwrap());
    assert_eq!(
        first_fetch::begin(&world.private, &world.target()),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert!(!world.marker().exists());
}

#[test]
fn a_replaced_root_keeps_foreign_work_and_the_original_intent() {
    let world = World::new();
    first_fetch::begin(&world.private, &world.target()).unwrap();
    let marker = fs::read(world.marker()).unwrap();
    fs::rename(&world.live, world.live.with_extension("old")).unwrap();
    fs::create_dir(&world.live).unwrap();
    fs::write(world.live.join("foreign.txt"), b"destination work").unwrap();
    assert_eq!(
        first_fetch::prove(&world.private, &world.target()),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert_eq!(
        first_fetch::begin(&world.private, &world.target()),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert_eq!(
        fs::read(world.live.join("foreign.txt")).unwrap(),
        b"destination work"
    );
    assert_eq!(fs::read(world.marker()).unwrap(), marker);
}

#[test]
fn native_metadata_and_working_files_are_never_first_fetch_custody() {
    for path in [
        ".git/HEAD",
        ".git/config",
        ".git/index",
        ".git/refs/heads/work",
        "work.txt",
        ".git/objects/info/alternates",
        ".git/objects/pack/pack-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.promisor",
    ] {
        let world = World::new();
        first_fetch::begin(&world.private, &world.target()).unwrap();
        let foreign = world.live.join(path);
        fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        fs::write(&foreign, b"destination work").unwrap();
        assert_eq!(
            first_fetch::prove(&world.private, &world.target()),
            Err(BulkloadRefusal::GitDestinationOccupied),
            "{path}"
        );
        assert_eq!(fs::read(&foreign).unwrap(), b"destination work");
        assert!(world.marker().exists());
    }
}

#[test]
fn a_git_symlink_never_redirects_an_intent() {
    let world = World::new();
    first_fetch::begin(&world.private, &world.target()).unwrap();
    fs::create_dir(&world.source).unwrap();
    symlink(&world.source, world.live.join(".git")).unwrap();
    assert_eq!(
        first_fetch::prove(&world.private, &world.target()),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert_eq!(fs::read_dir(&world.source).unwrap().count(), 0);
}

#[test]
fn a_different_source_never_inherits_the_intent() {
    let world = World::new();
    first_fetch::begin(&world.private, &world.target()).unwrap();
    let marker = fs::read(world.marker()).unwrap();
    let mut other_host = world.target();
    other_host.source_host = "other-source";
    assert_eq!(
        first_fetch::prove(&world.private, &other_host),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    let other_path = world.source.with_extension("other");
    let mut other_source = world.target();
    other_source.source_path = &other_path;
    assert_eq!(
        first_fetch::prove(&world.private, &other_source),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert_eq!(fs::read(world.marker()).unwrap(), marker);
}

#[test]
fn a_corrupt_marker_is_not_replaced_by_a_new_claim() {
    let world = World::new();
    fs::create_dir(&world.live).unwrap();
    fs::write(world.marker(), b"interrupted foreign record").unwrap();
    assert_eq!(
        first_fetch::begin(&world.private, &world.target()),
        Err(BulkloadRefusal::FrameCodec)
    );
    assert_eq!(
        fs::read(world.marker()).unwrap(),
        b"interrupted foreign record"
    );
    assert_eq!(fs::read_dir(&world.live).unwrap().count(), 0);
}

#[test]
fn a_captured_shallow_frontier_survives_fetch_and_retry() {
    let world = World::new();
    let frontier = b"1111111111111111111111111111111111111111\n";
    let mut target = world.target();
    target.expected_shallow = frontier;
    first_fetch::begin(&world.private, &target).unwrap();
    fs::create_dir_all(world.live.join(".git/objects/pack")).unwrap();
    fs::write(world.live.join(".git/shallow"), frontier).unwrap();
    assert!(first_fetch::prove(&world.private, &target).unwrap());
    first_fetch::begin(&world.private, &target).unwrap();
    assert_eq!(fs::read(world.live.join(".git/shallow")).unwrap(), frontier);
    let marker = fs::read(world.marker()).unwrap();
    target.expected_shallow = b"2222222222222222222222222222222222222222\n";
    assert_eq!(
        first_fetch::prove(&world.private, &target),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert_eq!(fs::read(world.marker()).unwrap(), marker);
}

#[test]
fn an_unexpected_changed_or_symlinked_shallow_frontier_is_held() {
    let frontier = b"1111111111111111111111111111111111111111\n";
    for expected in [&[][..], &frontier[..]] {
        let world = World::new();
        let mut target = world.target();
        target.expected_shallow = expected;
        first_fetch::begin(&world.private, &target).unwrap();
        fs::create_dir(world.live.join(".git")).unwrap();
        let shallow = world.live.join(".git/shallow");
        let different = b"2222222222222222222222222222222222222222\n";
        fs::write(&shallow, different).unwrap();
        assert_eq!(
            first_fetch::prove(&world.private, &target),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert_eq!(fs::read(&shallow).unwrap(), different);
        fs::remove_file(&shallow).unwrap();
        fs::write(&world.source, frontier).unwrap();
        symlink(&world.source, &shallow).unwrap();
        assert_eq!(
            first_fetch::prove(&world.private, &target),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert_eq!(fs::read(&world.source).unwrap(), frontier);
    }
}

#[test]
fn finish_accepts_completed_metadata_and_removes_only_the_bound_marker() {
    let world = World::new();
    first_fetch::begin(&world.private, &world.target()).unwrap();
    fs::create_dir(world.live.join(".git")).unwrap();
    fs::write(world.live.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    first_fetch::finish(&world.private, &world.target()).unwrap();
    assert!(!world.marker().exists());
    assert_eq!(
        fs::read(world.live.join(".git/HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    first_fetch::finish(&world.private, &world.target()).unwrap();
}
