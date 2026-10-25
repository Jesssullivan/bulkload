//! SPEC goal 1 and rules 1, 4, 7 and 8: shallow repositories stream into
//! one live estate without inventing parents or hiding destination history.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bulkload_agent::estate::{self, streamed};
use bulkload_agent::git_carry::stream;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    destination: PathBuf,
    upstream: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let upstream = root.path().join("upstream");
        fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "--quiet", "--template=", "-b", "main"]);
        for number in 0..5 {
            fs::write(upstream.join("tracked"), format!("revision {number}\n")).unwrap();
            git(&upstream, &["add", "tracked"]);
            git(
                &upstream,
                &["commit", "--quiet", "-m", &format!("c{number}")],
            );
        }
        let source = root.path().join("neo/git/subject");
        let destination = root.path().join("sting/git/subject");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--depth=2",
                "--no-local",
                &format!("file://{}", upstream.display()),
                source.to_str().unwrap(),
            ],
        );
        Self {
            root,
            source,
            destination,
            upstream,
        }
    }

    fn clone_destination(&self, depth: Option<&str>) {
        let url = format!("file://{}", self.upstream.display());
        let mut args = vec!["clone", "--quiet", "--no-local"];
        if let Some(depth) = depth {
            args.push(depth);
        }
        args.extend([url.as_str(), self.destination.to_str().unwrap()]);
        git(self.root.path(), &args);
    }

    fn capture(&self) -> streamed::ItemSnapshot {
        streamed::capture_source(
            &estate::Item {
                source: self.source.clone(),
                repository: self.destination.clone(),
                workspace: Some(self.destination.clone()),
            },
            1,
            false,
        )
        .unwrap()
    }

    fn state(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn agent(args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
            .arg("--min-free-percent=0")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    fn source_pass(&self) {
        let map = format!(
            "{}={}",
            self.source.parent().unwrap().display(),
            self.destination.parent().unwrap().display()
        );
        let result = Self::agent(&[
            "estate-move-source",
            "--map",
            &map,
            self.state("source-state").to_str().unwrap(),
            self.source.parent().unwrap().to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "source: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn move_pass(&self) -> String {
        let result = Self::agent(&[
            "estate-move",
            "--label",
            "neo-shallow",
            "--source",
            "neo",
            "-",
            self.state("source-state").to_str().unwrap(),
            self.state("destination-state").to_str().unwrap(),
        ]);
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(text.contains("phase=apply outcome=ok"), "{text}");
        assert!(!text.contains("GIT_HAVES_UNPROVABLE"), "{text}");
        assert!(!text.contains("CAPTURE_DRIFTED"), "{text}");
        text
    }
}

#[test]
fn an_absent_shallow_estate_lands_and_an_unchanged_pass_reuses_objects() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("tracked"), "stashed\n").unwrap();
    git(
        &fixture.source,
        &["stash", "push", "--quiet", "-m", "neo stash"],
    );
    fs::write(fixture.source.join("tracked"), "neo dirty\n").unwrap();
    fs::write(fixture.source.join("staged"), "neo staged\n").unwrap();
    git(&fixture.source, &["add", "staged"]);
    git(
        &fixture.source,
        &["worktree", "add", "--quiet", "-b", "lane", "../lane"],
    );
    fs::write(
        fixture.source.parent().unwrap().join("lane/loose"),
        "lane draft\n",
    )
    .unwrap();
    let boundary = fs::read(fixture.source.join(".git/shallow")).unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    let refs = git(
        &fixture.source,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    );
    let config = fs::read(fixture.source.join(".git/config")).unwrap();
    fixture.source_pass();
    fixture.move_pass();
    assert_eq!(
        fs::read(fixture.destination.join(".git/shallow")).unwrap(),
        boundary
    );
    assert_eq!(
        fs::read(fixture.source.join(".git/shallow")).unwrap(),
        boundary
    );
    assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(fixture.source.join(".git/config")).unwrap(),
        config
    );
    assert_eq!(
        git(
            &fixture.source,
            &["for-each-ref", "--format=%(objectname) %(refname)"]
        ),
        refs
    );
    assert_eq!(
        fs::read(fixture.destination.join("tracked")).unwrap(),
        b"neo dirty\n"
    );
    assert_eq!(
        fs::read(fixture.destination.parent().unwrap().join("lane/loose")).unwrap(),
        b"lane draft\n"
    );
    assert_eq!(
        git(&fixture.destination, &["stash", "list"]),
        git(&fixture.source, &["stash", "list"])
    );
    assert_eq!(
        git(&fixture.destination, &["ls-files", "--stage"]),
        git(&fixture.source, &["ls-files", "--stage"])
    );
    git(
        &fixture.destination,
        &["fsck", "--connectivity-only", "--no-dangling"],
    );
    fixture.source_pass();
    let again = fixture.move_pass();
    for line in again
        .lines()
        .filter(|line| line.starts_with("estate-move item=") && line.contains("object_bytes="))
    {
        assert!(line.contains("object_bytes=0 "), "{line}");
    }
    let audit =
        bulkload_agent::state_payload::measure(&fixture.state("destination-state")).unwrap();
    assert_eq!(audit.payload_bytes, 0);
}

const HANDOFF_EXCLUDE: &[u8] = b"# bulkload hand-off (never tracked)\n/CLAUDE.local.md\n";

fn expect_exclude_hold(fixture: &Fixture) {
    let snapshot = fixture.capture();
    let before = fs::read(fixture.destination.join(".git/info/exclude")).unwrap();
    let index = fs::read(fixture.destination.join(".git/index")).unwrap();
    let wrote = std::cell::Cell::new(false);
    let transfer = |_from: &Path, _to: &Path| {
        wrote.set(true);
        Ok(())
    };
    let settings = || {
        Ok(bulkload_agent::transfer::Consolidation {
            maps: bulkload_agent::consolidate::Maps::new(Vec::<(String, String)>::new()),
            ledger: fixture.state("consolidation"),
            plan_only: false,
            git: None,
            archived: Vec::new(),
        })
    };
    let scratch = fixture.state("scratch");
    let reach = streamed::Reach {
        upload_pack: "unused",
        transfer: &transfer,
        scratch: &scratch,
        consolidation: &settings,
    };
    assert_eq!(
        streamed::land(
            &snapshot,
            &fixture.state("private"),
            &[],
            &reach,
            stream::Fetched::default()
        )
        .unwrap_err()
        .code(),
        "GIT_IGNORE_POLICY_CONFLICT"
    );
    assert!(!wrote.get());
    assert_eq!(
        fs::read(fixture.destination.join(".git/info/exclude")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(fixture.destination.join(".git/index")).unwrap(),
        index
    );
}

#[test]
fn a_recorded_handoff_keeps_source_and_destination_exclude_patterns_on_rerun() {
    let fixture = Fixture::new();
    let source_exclude = fixture.source.join(".git/info/exclude");
    let mut source = fs::read(&source_exclude).unwrap();
    source.extend_from_slice(b"neo-only-pattern\n");
    fs::write(&source_exclude, &source).unwrap();
    fixture.source_pass();
    fixture.move_pass();
    let target = fixture.destination.join(".git/info/exclude");
    let first = fs::read(&target).unwrap();
    assert!(first.starts_with(&source));
    let base = first.strip_suffix(HANDOFF_EXCLUDE).unwrap();
    let mut both = base.to_vec();
    both.extend_from_slice(b"sting-only-pattern\n");
    both.extend_from_slice(HANDOFF_EXCLUDE);
    fs::write(&target, &both).unwrap();
    fixture.source_pass();
    let again = fixture.move_pass();
    assert_eq!(fs::read(&target).unwrap(), both);
    assert_eq!(fs::read(&source_exclude).unwrap(), source);
    for line in again
        .lines()
        .filter(|line| line.starts_with("estate-move item=") && line.contains("object_bytes="))
    {
        assert!(line.contains("object_bytes=0 "), "{line}");
    }
}

#[test]
fn an_unrecorded_handoff_lookalike_never_waives_an_exclude_conflict() {
    let fixture = Fixture::new();
    fixture.clone_destination(Some("--depth=2"));
    let target = fixture.destination.join(".git/info/exclude");
    let mut bytes = fs::read(&target).unwrap();
    bytes.extend_from_slice(HANDOFF_EXCLUDE);
    fs::write(target, bytes).unwrap();
    fs::write(
        fixture.destination.join("CLAUDE.local.md"),
        "unrecorded lookalike\n",
    )
    .unwrap();
    expect_exclude_hold(&fixture);
}

#[test]
fn a_source_exclude_edit_is_held_when_the_destination_does_not_contain_it() {
    let fixture = Fixture::new();
    fixture.source_pass();
    fixture.move_pass();
    let target = fixture.source.join(".git/info/exclude");
    let mut bytes = fs::read(&target).unwrap();
    bytes.extend_from_slice(b"new-neo-pattern\n");
    fs::write(target, bytes).unwrap();
    expect_exclude_hold(&fixture);
}

#[test]
fn a_modified_handoff_block_never_waives_an_exclude_conflict() {
    let fixture = Fixture::new();
    fixture.source_pass();
    fixture.move_pass();
    fs::write(
        fixture.destination.join("CLAUDE.local.md"),
        "operator replacement\n",
    )
    .unwrap();
    expect_exclude_hold(&fixture);
}

#[test]
fn a_skipped_exclude_removed_during_transfer_holds_without_restoring_it() {
    let fixture = Fixture::new();
    fixture.source_pass();
    fixture.move_pass();
    let snapshot = fixture.capture();
    let path = fixture.destination.join(".git/info/exclude");
    assert!(fs::read(&path).unwrap().ends_with(HANDOFF_EXCLUDE));
    let private = fixture.state("postcheck-private");
    fs::create_dir(&private).unwrap();
    let transferred = std::cell::Cell::new(false);
    let transfer = |_from: &Path, _to: &Path| {
        transferred.set(true);
        fs::remove_file(&path).unwrap();
        Ok(())
    };
    let settings = || {
        Ok(bulkload_agent::transfer::Consolidation {
            maps: bulkload_agent::consolidate::Maps::new(Vec::<(String, String)>::new()),
            ledger: fixture.state("consolidation"),
            plan_only: false,
            git: None,
            archived: Vec::new(),
        })
    };
    let scratch = fixture.state("scratch");
    let reach = streamed::Reach {
        upload_pack: "unused",
        transfer: &transfer,
        scratch: &scratch,
        consolidation: &settings,
    };
    let result = streamed::land(&snapshot, &private, &[], &reach, stream::Fetched::default());
    assert_eq!(result.unwrap_err().code(), "GIT_IGNORE_POLICY_CONFLICT");
    assert!(transferred.get());
    assert!(
        !path.exists(),
        "the mover must preserve the concurrent deletion"
    );
}

#[test]
fn a_home_mapped_main_keeps_original_handoff_provenance_on_rerun() {
    let fixture = Fixture::new();
    git(
        &fixture.source,
        &["worktree", "add", "--quiet", "-b", "lane", "../lane"],
    );
    let source_home = fixture.root.path().join("neo");
    let dest_home = fixture.root.path().join("sting");
    let original = estate::Item {
        source: fixture.source.clone(),
        repository: fixture.source.clone(),
        workspace: Some(fixture.source.clone()),
    };
    let original_id = estate::item_id(&original).unwrap();
    assert_ne!(
        original_id,
        estate::item_id(&fixture.capture().item).unwrap()
    );
    let run = |args: &[&str], home: &Path| {
        Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
            .arg("--min-free-percent=0")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", home)
            .output()
            .unwrap()
    };
    for pass in 0..2 {
        let sourced = run(
            &[
                "estate-move-source",
                fixture.state("source-state").to_str().unwrap(),
                fixture.source.parent().unwrap().to_str().unwrap(),
            ],
            &source_home,
        );
        assert!(
            sourced.status.success(),
            "{}",
            String::from_utf8_lossy(&sourced.stderr)
        );
        let moved = run(
            &[
                "estate-move",
                "--label",
                "neo-shallow",
                "--source",
                "neo",
                "--dest-home",
                dest_home.to_str().unwrap(),
                "-",
                fixture.state("source-state").to_str().unwrap(),
                fixture.state("destination-state").to_str().unwrap(),
            ],
            &dest_home,
        );
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&moved.stdout),
            String::from_utf8_lossy(&moved.stderr)
        );
        assert!(text.contains("phase=apply outcome=ok"), "{text}");
        assert!(text.contains("phase=hand-off outcome=ok"), "{text}");
        assert!(!text.contains("GIT_IGNORE_POLICY_CONFLICT"), "{text}");
        if pass == 1 {
            for line in text.lines().filter(|line| {
                line.starts_with("estate-move item=") && line.contains("object_bytes=")
            }) {
                assert!(line.contains("object_bytes=0 "), "{line}");
            }
        }
    }
    let record = fs::read(
        fixture
            .destination
            .join(".git/bulkload/v1/neo/handoff.landed"),
    )
    .unwrap();
    assert!(record
        .windows(original_id.len())
        .any(|bytes| bytes == original_id.as_bytes()));
    let source = fs::read(fixture.source.join(".git/info/exclude")).unwrap();
    let target = fs::read(fixture.destination.join(".git/info/exclude")).unwrap();
    assert!(target.starts_with(&source));
    assert!(target.ends_with(HANDOFF_EXCLUDE));
    assert!(fixture
        .destination
        .parent()
        .unwrap()
        .join("lane/.git")
        .is_file());
}

fn assert_fifo_is_held_promptly(relative: &str) {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let fixture = Fixture::new();
    fixture.source_pass();
    fixture.move_pass();
    let snapshot = fixture.capture();
    let path = fixture.destination.join(relative);
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is this fixture's NUL-terminated path and remains alive.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let before = fs::symlink_metadata(&path).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let transfer = |_from: &Path, _to: &Path| Err(bulkload_agent::BulkloadRefusal::WorkerLost);
        let settings = || {
            Ok(bulkload_agent::transfer::Consolidation {
                maps: bulkload_agent::consolidate::Maps::new(Vec::<(String, String)>::new()),
                ledger: fixture.state("consolidation"),
                plan_only: false,
                git: None,
                archived: Vec::new(),
            })
        };
        let scratch = fixture.state("scratch");
        let reach = streamed::Reach {
            upload_pack: "unused",
            transfer: &transfer,
            scratch: &scratch,
            consolidation: &settings,
        };
        let code = streamed::land(
            &snapshot,
            &fixture.state("private"),
            &[],
            &reach,
            stream::Fetched::default(),
        )
        .unwrap_err()
        .code()
        .to_owned();
        let after = fs::symlink_metadata(&path).unwrap();
        sender
            .send((
                code,
                after.file_type().is_fifo(),
                after.ino() == before.ino(),
            ))
            .unwrap();
    });
    assert_eq!(
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        ("GIT_IGNORE_POLICY_CONFLICT".to_owned(), true, true)
    );
}

#[test]
fn an_exclude_replaced_by_a_fifo_is_held_without_waiting_for_a_writer() {
    assert_fifo_is_held_promptly(".git/info/exclude");
}

#[test]
fn a_local_block_replaced_by_a_fifo_is_held_without_waiting_for_a_writer() {
    assert_fifo_is_held_promptly("CLAUDE.local.md");
}

#[test]
fn a_full_destination_keeps_its_deeper_history_and_its_own_refs() {
    let fixture = Fixture::new();
    fixture.clone_destination(None);
    let old_history = git(&fixture.destination, &["rev-list", "--all"]);
    git(&fixture.destination, &["branch", "sting-only", "HEAD~3"]);
    fs::write(fixture.source.join("neo-file"), "neo contribution\n").unwrap();
    git(&fixture.source, &["add", "neo-file"]);
    git(
        &fixture.source,
        &["commit", "--quiet", "-m", "neo contribution"],
    );
    let snapshot = fixture.capture();
    assert_eq!(
        streamed::shallow_decision(&snapshot, &[]).unwrap(),
        "keep-full"
    );
    fixture.source_pass();
    fixture.move_pass();
    assert!(!fixture.destination.join(".git/shallow").exists());
    let after = git(&fixture.destination, &["rev-list", "--all"]);
    for id in old_history.lines() {
        assert!(after.lines().any(|held| held == id), "history lost {id}");
    }
    assert_eq!(
        git(&fixture.destination, &["rev-parse", "sting-only"]),
        git(&fixture.upstream, &["rev-parse", "HEAD~3"])
    );
    assert_eq!(
        fs::read(fixture.destination.join("neo-file")).unwrap(),
        b"neo contribution\n"
    );
    git(
        &fixture.destination,
        &["fsck", "--connectivity-only", "--no-dangling"],
    );
}

#[test]
fn equal_boundaries_preserve_both_estates_and_the_frontier() {
    let fixture = Fixture::new();
    fixture.clone_destination(Some("--depth=2"));
    git(&fixture.destination, &["branch", "sting-only"]);
    fs::write(fixture.destination.join("sting-file"), "sting unique\n").unwrap();
    fs::write(fixture.source.join("neo-file"), "neo unique\n").unwrap();
    let snapshot = fixture.capture();
    assert_eq!(streamed::shallow_decision(&snapshot, &[]).unwrap(), "same");
    let boundary = fs::read(fixture.destination.join(".git/shallow")).unwrap();
    fixture.source_pass();
    fixture.move_pass();
    assert_eq!(
        fs::read(fixture.destination.join(".git/shallow")).unwrap(),
        boundary
    );
    assert_eq!(
        fs::read(fixture.destination.join("sting-file")).unwrap(),
        b"sting unique\n"
    );
    assert_eq!(
        fs::read(fixture.destination.join("neo-file")).unwrap(),
        b"neo unique\n"
    );
    git(
        &fixture.destination,
        &["show-ref", "--verify", "refs/heads/sting-only"],
    );
    git(
        &fixture.destination,
        &["fsck", "--connectivity-only", "--no-dangling"],
    );
}

#[test]
fn differing_boundaries_hold_before_any_destination_write() {
    let fixture = Fixture::new();
    fixture.clone_destination(Some("--depth=1"));
    let snapshot = fixture.capture();
    let boundary = fs::read(fixture.destination.join(".git/shallow")).unwrap();
    let refs = git(&fixture.destination, &["show-ref"]);
    let index = fs::read(fixture.destination.join(".git/index")).unwrap();
    assert_eq!(
        streamed::shallow_decision(&snapshot, &[])
            .unwrap_err()
            .code(),
        "GIT_HAVES_UNPROVABLE"
    );
    let private = fixture.state("private");
    fs::create_dir(&private).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    assert_eq!(
        streamed::fetch_objects(&snapshot, &private, &upload, &[])
            .unwrap_err()
            .code(),
        "GIT_HAVES_UNPROVABLE"
    );
    assert_eq!(
        fs::read(fixture.destination.join(".git/shallow")).unwrap(),
        boundary
    );
    assert_eq!(git(&fixture.destination, &["show-ref"]), refs);
    assert_eq!(
        fs::read(fixture.destination.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(fs::read_dir(private).unwrap().count(), 0);
}

#[test]
fn a_source_that_became_full_holds_instead_of_leaving_old_history_hidden() {
    let fixture = Fixture::new();
    fixture.source_pass();
    fixture.move_pass();
    let boundary = fs::read(fixture.destination.join(".git/shallow")).unwrap();
    let refs = git(&fixture.destination, &["show-ref"]);
    let index = fs::read(fixture.destination.join(".git/index")).unwrap();
    // Only this generated fixture is deepened, never a user's estate.
    git(
        &fixture.source,
        &["fetch", "--quiet", "--unshallow", "origin"],
    );
    assert!(!fixture.source.join(".git/shallow").exists());
    let snapshot = fixture.capture();
    assert!(snapshot.shallow.is_empty());
    assert_eq!(
        streamed::shallow_decision(&snapshot, &[])
            .unwrap_err()
            .code(),
        "GIT_HAVES_UNPROVABLE"
    );
    let private = fixture.state("unshallow-private");
    fs::create_dir(&private).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    assert_eq!(
        streamed::fetch_objects(&snapshot, &private, &upload, &[])
            .unwrap_err()
            .code(),
        "GIT_HAVES_UNPROVABLE"
    );
    fixture.source_pass();
    let result = Fixture::agent(&[
        "estate-move",
        "--label",
        "neo-shallow",
        "--source",
        "neo",
        "-",
        fixture.state("source-state").to_str().unwrap(),
        fixture.state("destination-state").to_str().unwrap(),
    ]);
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(text.contains("GIT_HAVES_UNPROVABLE"), "{text}");
    assert_eq!(
        fs::read(fixture.destination.join(".git/shallow")).unwrap(),
        boundary
    );
    assert_eq!(git(&fixture.destination, &["show-ref"]), refs);
    assert_eq!(
        fs::read(fixture.destination.join(".git/index")).unwrap(),
        index
    );
}

#[test]
fn a_linked_capture_checks_its_own_boundary_before_any_seat_or_index_write() {
    let fixture = Fixture::new();
    fixture.clone_destination(Some("--depth=2"));
    git(
        &fixture.source,
        &["worktree", "add", "--quiet", "-b", "lane", "../lane"],
    );
    // The fixture main's captured boundary is A. A later linked capture
    // sees B after this fixture alone deepens.
    let main = fixture.capture();
    assert_eq!(streamed::shallow_decision(&main, &[]).unwrap(), "same");
    git(
        &fixture.source,
        &["fetch", "--quiet", "--deepen=1", "origin"],
    );
    let live = fixture.destination.parent().unwrap().join("lane");
    let linked = streamed::capture_source(
        &estate::Item {
            source: fixture.source.parent().unwrap().join("lane"),
            repository: fixture.destination.clone(),
            workspace: Some(live.clone()),
        },
        2,
        false,
    )
    .unwrap();
    assert_ne!(main.shallow, linked.shallow);
    let wrote = std::cell::Cell::new(false);
    let transfer = |_from: &Path, _to: &Path| {
        wrote.set(true);
        Ok(())
    };
    let settings = || {
        Ok(bulkload_agent::transfer::Consolidation {
            maps: bulkload_agent::consolidate::Maps::new(Vec::<(String, String)>::new()),
            ledger: fixture.state("consolidation"),
            plan_only: false,
            git: None,
            archived: Vec::new(),
        })
    };
    let scratch = fixture.state("scratch");
    let reach = streamed::Reach {
        upload_pack: "unused",
        transfer: &transfer,
        scratch: &scratch,
        consolidation: &settings,
    };
    let refs = git(&fixture.destination, &["show-ref"]);
    let index = fs::read(fixture.destination.join(".git/index")).unwrap();
    assert_eq!(
        streamed::land(
            &linked,
            &fixture.state("private"),
            &[],
            &reach,
            stream::Fetched::default()
        )
        .unwrap_err()
        .code(),
        "GIT_HAVES_UNPROVABLE"
    );
    assert!(!wrote.get());
    assert!(!live.exists());
    assert_eq!(git(&fixture.destination, &["show-ref"]), refs);
    assert_eq!(
        fs::read(fixture.destination.join(".git/index")).unwrap(),
        index
    );
}

#[test]
fn a_frontier_created_after_full_capture_never_enters_ordinary_seat_transfer() {
    let fixture = Fixture::new();
    let old_boundary = fs::read(fixture.source.join(".git/shallow")).unwrap();
    git(
        &fixture.source,
        &["fetch", "--quiet", "--unshallow", "origin"],
    );
    fixture.clone_destination(None);
    let before = git(&fixture.destination, &["rev-list", "--all"]);
    let snapshot = fixture.capture();
    assert!(snapshot.shallow.is_empty());
    assert!(snapshot.excluded.contains(&b".git/shallow".to_vec()));
    let private = fixture.state("private");
    fs::create_dir(&private).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    let fetched = streamed::fetch_objects(&snapshot, &private, &upload, &[]).unwrap();
    fs::write(fixture.source.join(".git/shallow"), old_boundary).unwrap();
    let source_state = fixture.state("seat-source");
    let destination_state = fixture.state("seat-destination");
    let transfer = |from: &Path, to: &Path| {
        bulkload_agent::transfer::copy(from, to, &source_state, &destination_state).map(|_| ())
    };
    let settings = || {
        Ok(bulkload_agent::transfer::Consolidation {
            maps: bulkload_agent::consolidate::Maps::new(Vec::<(String, String)>::new()),
            ledger: fixture.state("consolidation"),
            plan_only: false,
            git: None,
            archived: Vec::new(),
        })
    };
    let scratch = fixture.state("scratch");
    let reach = streamed::Reach {
        upload_pack: &upload,
        transfer: &transfer,
        scratch: &scratch,
        consolidation: &settings,
    };
    streamed::land(&snapshot, &private, &[], &reach, fetched).unwrap();
    assert!(!fixture.destination.join(".git/shallow").exists());
    assert_eq!(git(&fixture.destination, &["rev-list", "--all"]), before);
}

#[test]
fn a_full_capture_reuses_a_complete_destination_after_source_frontier_changes() {
    let fixture = Fixture::new();
    let old_boundary = fs::read(fixture.source.join(".git/shallow")).unwrap();
    git(
        &fixture.source,
        &["fetch", "--quiet", "--unshallow", "origin"],
    );
    fixture.clone_destination(None);
    let snapshot = fixture.capture();
    let refs = git(&fixture.destination, &["show-ref"]);
    let history = git(&fixture.destination, &["rev-list", "--all"]);
    let index = fs::read(fixture.destination.join(".git/index")).unwrap();
    fs::write(fixture.source.join(".git/shallow"), old_boundary).unwrap();
    let private = fixture.state("private");
    fs::create_dir(&private).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    let fetched = streamed::fetch_objects(&snapshot, &private, &upload, &[]).unwrap();
    assert_eq!(fetched.bytes, 0);
    assert!(stream::graph_complete(
        &fixture.destination.join(".git/objects"),
        &snapshot.snapshot.object_format,
        &snapshot.snapshot.wants,
        &[]
    )
    .unwrap());
    assert!(!fixture.destination.join(".git/shallow").exists());
    assert_eq!(git(&fixture.destination, &["rev-list", "--all"]), history);
    assert_eq!(git(&fixture.destination, &["show-ref"]), refs);
    assert_eq!(
        fs::read(fixture.destination.join(".git/index")).unwrap(),
        index
    );
}

#[test]
fn a_full_capture_cannot_succeed_with_an_incomplete_graph_after_source_truncates() {
    let fixture = Fixture::new();
    let old_boundary = fs::read(fixture.source.join(".git/shallow")).unwrap();
    git(
        &fixture.source,
        &["fetch", "--quiet", "--unshallow", "origin"],
    );
    let snapshot = fixture.capture();
    fs::write(fixture.source.join(".git/shallow"), old_boundary).unwrap();
    let private = fixture.state("private");
    fs::create_dir(&private).unwrap();
    let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
    let fetched = streamed::fetch_objects(&snapshot, &private, &upload, &[]);
    if fetched.is_ok() {
        assert!(
            stream::graph_complete(
                &fixture.destination.join(".git/objects"),
                &snapshot.snapshot.object_format,
                &snapshot.snapshot.wants,
                &[]
            )
            .unwrap(),
            "a full capture needs its whole graph, never a silently dropped boundary"
        );
    }
    assert!(!fixture.destination.join(".git/shallow").exists());
    assert!(!fixture.destination.join(".git/HEAD").exists());
}

#[test]
fn a_frontier_change_invalidates_marks_but_full_source_marks_stay_compatible() {
    let fixture = Fixture::new();
    let snapshot = fixture.capture();
    let digest = stream::fetch_digest(&snapshot.snapshot, &snapshot.shallow);
    assert_ne!(digest, stream::fetch_digest(&snapshot.snapshot, &[]));
    assert_eq!(
        stream::fetch_digest(&snapshot.snapshot, &[]),
        snapshot.snapshot.digest()
    );
    let mut changed = snapshot.shallow.clone();
    changed.extend_from_slice(format!("{}\n", "1".repeat(40)).as_bytes());
    assert_ne!(digest, stream::fetch_digest(&snapshot.snapshot, &changed));
}

#[test]
fn frontier_validation_rejects_malformed_width_and_missing_commits() {
    assert_eq!(
        stream::normalize_frontier(format!("{}\n", "1".repeat(64)).as_bytes(), "sha1")
            .unwrap_err()
            .code(),
        "GIT_INVENTORY_MALFORMED"
    );
    assert_eq!(
        stream::normalize_frontier(b"not-an-id\n", "sha1")
            .unwrap_err()
            .code(),
        "GIT_INVENTORY_MALFORMED"
    );
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join(".git/shallow"),
        format!("{}\n", "1".repeat(40)),
    )
    .unwrap();
    assert_eq!(
        stream::shallow_frontier(&fixture.source)
            .unwrap_err()
            .code(),
        "GIT_HAVES_UNPROVABLE"
    );
}
