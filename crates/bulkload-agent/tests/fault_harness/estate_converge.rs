//! Estate converge crash rows (2026-10-08;
//! `docs/agent-notes/2026-10-08-estate-converge-design.md`, section 13):
//! the `estate.converge.*` fault points.
//!
//! Each scenario lands a standalone checkout from capture `c0`, moves the
//! source (a commit, a branch move, a remote-tracking update and prune, a
//! stash push, replaced, removed, added and re-moded seats, a file turned
//! symlink, a directory turned file), captures `c1`, and restores `c1`
//! afresh into a second destination as the reference. A child (this test
//! binary, re-run) then applies `c1` over the landing with one point armed
//! and ends itself there with `_exit`; nothing here sends it a signal
//! (R-N11). The parent asserts:
//!
//! - **`ComponentOldOrNew`**: after the crash every native ref, HEAD, the
//!   index listing and every seat holds its `c0` value or its `c1` value
//!   (`.bulkload-converge-*` temporaries aside);
//! - **accounted**: closure counts the item unaccounted (`journal-missing`)
//!   until the rerun;
//! - **settles forward**: the rerun applies, leaves no temporary, intent or
//!   index lock, and the checkout equals the fresh restore of `c1`, its refs
//!   too (the local branch deleted at the source excepted: R7 keeps it);
//! - **idempotent**: a second rerun is a no-op.
//!
//! The `ForeignWrite` row crashes after the first exchange, then writes a
//! seat the converge has not reached yet, as a third party would: the rerun
//! refuses `GIT_CONVERGE_INTERRUPTED`, keeps the intent and the third
//! party's bytes, and closure counts the item unaccounted whatever its
//! review. Once the operator puts the seat back, the rerun converges.
//!
//! `_exit` models a process crash only (see the harness header).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use bulkload_agent::estate::{self, Item};
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the scratch base whose landing it converges.
const CHILD_ENV: &str = "BULKLOAD_W7_CONVERGE_CHILD_BASE";

/// What happens around the crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateConverge {
    /// A crash, then the rerun.
    Delta,
    /// A crash, a third party's write to a seat not yet converged, then the
    /// rerun.
    ForeignWrite,
}

impl Scenario for EstateConverge {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth, self);
    }
}

fn git_command(repo: &Path) -> Command {
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
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(repo);
    command
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = git_command(repo).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

impl World {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-converge-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let world = Self {
            base: fs::canonicalize(&base).unwrap(),
        };
        let src = world.src();
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        for index in 1..=4 {
            fs::write(src.join(format!("f{index}")), format!("seat {index}, c0\n")).unwrap();
        }
        fs::write(src.join("gone.txt"), b"removed at c1\n").unwrap();
        fs::write(src.join("kind.txt"), b"a file at c0\n").unwrap();
        fs::create_dir(src.join("d")).unwrap();
        fs::write(src.join("d/x.txt"), b"a directory at c0\n").unwrap();
        fs::write(src.join("exec.sh"), b"#!/bin/sh\n").unwrap();
        fs::write(src.join("keep.txt"), b"unchanged\n").unwrap();
        git(&src, &["add", "."]);
        git(&src, &["commit", "--quiet", "-m", "c0"]);
        git(&src, &["branch", "side"]);
        git(&src, &["branch", "doomed"]);
        git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&src, &["update-ref", "refs/remotes/origin/old", "HEAD"]);
        fs::write(src.join("keep.txt"), b"stashed\n").unwrap();
        git(&src, &["stash", "push", "--quiet", "-m", "one"]);
        for (set, plan) in [("dst", "plan"), ("fresh", "plan-fresh")] {
            estate::add_batch(
                &world.base.join(plan),
                &[Item {
                    source: src.clone(),
                    repository: world.base.join(set),
                    workspace: Some(world.base.join(set)),
                }],
            )
            .unwrap();
        }
        world
    }

    fn src(&self) -> PathBuf {
        self.base.join("src")
    }
    fn dst(&self) -> PathBuf {
        self.base.join("dst")
    }

    fn capture(&self, plan: &str, corpus: &str) {
        estate::capture(
            &self.base.join(plan),
            &self.base.join(format!("{corpus}-state")),
            &self.base.join(corpus),
            1,
            &|_| Ok(()),
        )
        .unwrap();
    }

    fn apply(&self, plan: &str, corpus: &str, state: &str) -> Vec<(&'static str, Option<String>)> {
        let rows = Mutex::new(Vec::new());
        let _ = estate::apply(
            &self.base.join(plan),
            &self.base.join(corpus),
            &self.base.join(state),
            "neo",
            1,
            &|row| {
                rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                Ok(())
            },
        );
        rows.into_inner().unwrap()
    }

    fn unaccounted(&self) -> u64 {
        let ledger = estate::ledger(
            &self.base.join("plan"),
            &self.base.join("corpus"),
            "neo",
            &[self.base.join("applied")],
        )
        .unwrap();
        bulkload_agent::closure::Report::from_ledger(&ledger).unaccounted
    }

    // The source's day: every kind of change the converge carries.
    fn work(&self) {
        let src = self.src();
        for index in 1..=4 {
            fs::write(src.join(format!("f{index}")), format!("seat {index}, c1\n")).unwrap();
        }
        git(&src, &["commit", "--quiet", "-am", "c1"]);
        git(&src, &["update-ref", "refs/heads/side", "HEAD"]);
        git(&src, &["branch", "-D", "doomed"]);
        git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&src, &["update-ref", "-d", "refs/remotes/origin/old"]);
        fs::write(src.join("keep.txt"), b"stashed again\n").unwrap();
        git(&src, &["stash", "push", "--quiet", "-m", "two"]);
        fs::remove_file(src.join("gone.txt")).unwrap();
        fs::remove_file(src.join("kind.txt")).unwrap();
        std::os::unix::fs::symlink("keep.txt", src.join("kind.txt")).unwrap();
        fs::remove_dir_all(src.join("d")).unwrap();
        fs::write(src.join("d"), b"a file at c1\n").unwrap();
        fs::set_permissions(src.join("exec.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(src.join("added.txt"), b"new at c1\n").unwrap();
    }
}

/// What an operator sees of a checkout, per name: seats (with mode and
/// bytes or link), refs, HEAD and the index listing.
fn snapshot(worktree: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == ".git" || name.starts_with(".bulkload-converge-") {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            let key = format!("seat {}", path.strip_prefix(root).unwrap().display());
            let value = if meta.file_type().is_symlink() {
                format!("link {}", fs::read_link(&path).unwrap().display())
            } else if meta.is_dir() {
                walk(root, &path, out);
                "dir".to_owned()
            } else {
                format!(
                    "file {:o} {}",
                    meta.permissions().mode() & 0o777,
                    String::from_utf8_lossy(&fs::read(&path).unwrap())
                )
            };
            out.insert(key, value);
        }
    }
    let mut out = BTreeMap::new();
    walk(worktree, worktree, &mut out);
    for line in git(
        worktree,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    )
    .lines()
    {
        let (value, name) = line.split_once(' ').unwrap();
        if !name.starts_with("refs/carry") {
            out.insert(format!("ref {name}"), value.to_owned());
        }
    }
    out.insert(
        "stash".to_owned(),
        git_command(worktree)
            .args(["reflog", "show", "--format=%H %gs", "refs/stash"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap(),
    );
    let symbolic = git_command(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    out.insert(
        "HEAD".to_owned(),
        format!(
            "{} {}",
            String::from_utf8_lossy(&symbolic.stdout).trim(),
            git(worktree, &["rev-parse", "HEAD"]).trim()
        ),
    );
    out.insert("index".to_owned(), git(worktree, &["ls-files", "--stage"]));
    out
}

/// Run this test binary as a child that applies the corpus over the
/// landing, armed with `fault`: it must end itself at that point.
fn crash(world: &World, fault: &str) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "estate_converge::converge_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &world.base)
        .env(FAULT_ENV, fault)
        .env_remove(super::CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A detached waiter reaps the child; on timeout the test fails and the
    // child is left alone, never signalled (R-N11).
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let child = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!("{fault}: converge child still running ({timeout}); left running, not signalled")
        })
        .unwrap();
    let stderr = String::from_utf8_lossy(&child.stderr).into_owned();
    assert_ne!(
        child.status.code(),
        Some(CHILD_WATCHDOG_EXIT_CODE),
        "{fault}: hung; the watchdog ended it after {CHILD_WATCHDOG:?} ({stderr})"
    );
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{fault}: the child must stop at its fault point ({:?}, {stderr})",
        child.status
    );
}

/// The child half of every converge scenario; a no-op in an ordinary run.
#[test]
fn converge_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("converge child watchdog: not done within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
    let outcome = estate::apply(
        &base.join("plan"),
        &base.join("corpus"),
        &base.join("applied"),
        "neo",
        1,
        &|_| Ok(()),
    );
    panic!("the armed point was not reached: apply returned {outcome:?}");
}

fn temporaries(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with(".bulkload-converge-") || name.starts_with("bulkload-index-") {
                found.push(path.clone());
            }
            if fs::symlink_metadata(&path).unwrap().is_dir() && name != "objects" {
                pending.push(path);
            }
        }
    }
    found
}

fn scenario(point: Point, nth: u64, kind: EstateConverge) {
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::new(&point.name().replace('.', "-"));
    world.capture("plan", "corpus");
    let landed = world.apply("plan", "corpus", "applied");
    assert_eq!(landed, vec![("workspace-restored", None)], "{label}");
    let old = snapshot(&world.dst());
    settle();
    world.work();
    settle();
    world.capture("plan", "corpus");
    world.capture("plan-fresh", "corpus-fresh");
    let fresh = world.apply("plan-fresh", "corpus-fresh", "applied-fresh");
    assert_eq!(fresh, vec![("workspace-restored", None)], "{label}");
    let new = snapshot(&world.base.join("fresh"));

    crash(&world, &format!("{}:{nth}", point.name()));

    // ComponentOldOrNew.
    let crashed = snapshot(&world.dst());
    let keys: std::collections::BTreeSet<&String> =
        old.keys().chain(new.keys()).chain(crashed.keys()).collect();
    for key in keys {
        let found = crashed.get(key);
        assert!(
            found == old.get(key) || found == new.get(key),
            "{label}: {key} is neither old nor new: {found:?} (old {:?}, new {:?})",
            old.get(key),
            new.get(key)
        );
    }
    assert_eq!(
        world.unaccounted(),
        1,
        "{label}: journal-missing before the rerun"
    );

    if kind == EstateConverge::ForeignWrite {
        // A seat the converge has not reached: f4 (replaced last).
        let foreign = world.dst().join("f4");
        fs::write(&foreign, b"a third party's bytes\n").unwrap();
        let rows = world.apply("plan", "corpus", "applied");
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("GIT_CONVERGE_INTERRUPTED"),
            "{label}: {rows:?}"
        );
        assert_eq!(fs::read(&foreign).unwrap(), b"a third party's bytes\n");
        assert!(world
            .dst()
            .join(".git/bulkload/v1/neo/workspace.intent")
            .exists());
        assert_eq!(world.unaccounted(), 1, "{label}: converge-interrupted");
        // The operator puts the seat back as it landed; the rerun settles.
        fs::write(&foreign, b"seat 4, c0\n").unwrap();
    }

    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    assert!(
        temporaries(&world.dst()).is_empty(),
        "{label}: {:?}",
        temporaries(&world.dst())
    );
    assert!(!world.dst().join(".git/index.lock").exists(), "{label}");
    for intent in ["workspace.intent", "refs.intent"] {
        assert!(
            !world
                .dst()
                .join(".git/bulkload/v1/neo")
                .join(intent)
                .exists(),
            "{label}: {intent}"
        );
    }
    let mut converged = snapshot(&world.dst());
    // R7 keeps the local branch the source deleted, and reports it.
    assert!(
        converged.remove("ref refs/heads/doomed").is_some(),
        "{label}"
    );
    assert_eq!(
        converged, new,
        "{label}: the converge equals a fresh restore"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(
        rows,
        vec![("previous-workspace-restoration-not-revalidated", None)],
        "{label}"
    );
}
