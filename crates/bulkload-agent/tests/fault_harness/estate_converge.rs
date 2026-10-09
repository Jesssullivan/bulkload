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
//! The source's day also switches HEAD to a branch that moves with it, so
//! `set_head` runs its two transactions (`estate.converge.head_branch`
//! between them) and the branch HEAD left converges after it; and it turns
//! a file into a directory, which a crash after its `MkDir` leaves for the
//! settle (the convergence lane's fixes, 2026-10-09). A seat whose kind
//! changes may be absent between its removal and its addition, as
//! EstateConverge.tla's `CrashAtomicity` allows.
//!
//! The `LinkedLanding` row crashes a first landing of a linked worktree
//! after its `git worktree add` (`estate.converge.linked_added`, #216's
//! residual): the rerun finishes it forward from its landing intent, never
//! `GIT_DESTINATION_OCCUPIED`, and it equals a fresh restore.
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
    /// A first landing of a main and its linked worktree, crashed inside
    /// the linked one's, then the rerun.
    LinkedLanding,
    /// A crash after `set_head`, then the operator puts HEAD back on the
    /// branch it left: the rerun refuses and keeps it (the intent records
    /// HEAD at its new value), then converges once HEAD is put forward.
    SwitchBack,
    /// A crash between `set_head`'s branch transaction and its HEAD
    /// transaction, then the operator's `git worktree add` of that branch
    /// (review 4, the blocker): the rerun must never put the landed
    /// checkout on it too; it re-plans HEAD detached.
    DoubleCheckout,
    /// A crash with the index locked, and the lock Git takes on the
    /// converge's scratch index left as a killed `read-tree` leaves it
    /// (review 4): the rerun removes it and converges.
    FreshIndexLock,
    /// The landed refs packed by the operator, a crash with the prune's ref
    /// intent pending, and what a killed delete child leaves: an empty, old
    /// `packed-refs.lock` and a `packed-refs.new` (review 4). The lock is
    /// not provably bulkload's: the rerun refuses and keeps it; once the
    /// operator removes it, the rerun removes the orphan `packed-refs.new`
    /// and converges.
    PackedNew,
    /// A first linked landing crashed after `worktree add`, then the
    /// operator's work in it: a file staged, and a seat holding a prefix of
    /// its blob (review 4). The rerun refuses `GIT_DESTINATION_MODIFIED`
    /// and changes nothing of it.
    LinkedOperator,
}

impl Scenario for EstateConverge {
    fn run(self, point: Point, nth: u64) {
        if matches!(self, Self::LinkedLanding | Self::LinkedOperator) {
            linked_landing(point, nth, self);
        } else {
            scenario(point, nth, self);
        }
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
        Self::with(tag, false)
    }

    /// `linked`: the source has a linked worktree on `feat`, planned with
    /// the main into the same destination repository.
    fn with(tag: &str, linked: bool) -> Self {
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
        fs::write(src.join("todir"), b"a file at c0, a directory at c1\n").unwrap();
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
        if linked {
            let feat = world.base.join("src-feat");
            git(
                &src,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "feat",
                    feat.to_str().unwrap(),
                ],
            );
            fs::write(feat.join("feat.txt"), b"the linked worktree's own\n").unwrap();
        }
        for (set, plan) in [("dst", "plan"), ("fresh", "plan-fresh")] {
            let mut items = vec![Item {
                source: src.clone(),
                repository: world.base.join(set),
                workspace: Some(world.base.join(set)),
            }];
            if linked {
                items.push(Item {
                    source: world.base.join("src-feat"),
                    repository: world.base.join(set),
                    workspace: Some(world.base.join(format!("{set}-feat"))),
                });
            }
            estate::add_batch(&world.base.join(plan), &items).unwrap();
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
        // HEAD moves to a branch that moves with it: set_head's two
        // transactions, and `main` converged after HEAD has left it.
        git(&src, &["switch", "--quiet", "side"]);
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
        fs::remove_file(src.join("todir")).unwrap();
        fs::create_dir(src.join("todir")).unwrap();
        fs::write(src.join("todir/inner"), b"a directory at c1\n").unwrap();
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

#[allow(clippy::too_many_lines)]
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
    if kind == EstateConverge::PackedNew {
        git(&world.dst(), &["pack-refs", "--all"]);
    }

    crash(&world, &format!("{}:{nth}", point.name()));

    // ComponentOldOrNew.
    let crashed = snapshot(&world.dst());
    let keys: std::collections::BTreeSet<&String> =
        old.keys().chain(new.keys()).chain(crashed.keys()).collect();
    let kind_of = |value: Option<&String>| {
        value.map(|value| value.split(' ').next().unwrap_or("").to_owned())
    };
    for key in keys {
        let found = crashed.get(key);
        // A seat whose kind changes may be absent between its removal and
        // its addition (CrashAtomicity's TypeChange).
        let between = found.is_none()
            && old.contains_key(key)
            && new.contains_key(key)
            && kind_of(old.get(key)) != kind_of(new.get(key));
        assert!(
            found == old.get(key) || found == new.get(key) || between,
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

    if kind == EstateConverge::SwitchBack {
        switch_back(&world, &label);
    }
    if kind == EstateConverge::DoubleCheckout {
        double_checkout(&world, &label);
        return;
    }
    if kind == EstateConverge::FreshIndexLock {
        fresh_index_lock(&world, &label);
    }
    if kind == EstateConverge::PackedNew {
        packed_new(&world, &label);
    }

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

/// The `LinkedLanding` row: a main and its linked worktree land for the
/// first time; the child crashes inside the linked worktree's landing at
/// `point` (after `git worktree add`). The rerun finishes the linked
/// worktree forward from its landing intent (#216's residual: before the
/// fix every rerun refused `GIT_DESTINATION_OCCUPIED`), and it equals a
/// fresh restore of the same capture.
fn linked_landing(point: Point, nth: u64, kind: EstateConverge) {
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::with(&point.name().replace('.', "-"), true);
    world.capture("plan", "corpus");
    world.capture("plan-fresh", "corpus-fresh");
    let fresh = world.apply("plan-fresh", "corpus-fresh", "applied-fresh");
    assert!(
        fresh.len() == 2 && fresh.iter().all(|row| row.0 == "workspace-restored"),
        "{label}: {fresh:?}"
    );

    crash(&world, &format!("{}:{nth}", point.name()));

    let feat = world.base.join("dst-feat");
    assert!(
        feat.join(".git").is_file(),
        "{label}: the worktree was added"
    );
    let ledgers = world.dst().join(".git/bulkload/v1/neo");
    let intents = || -> Vec<String> {
        fs::read_dir(&ledgers)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("landing-"))
            .collect()
    };
    assert_eq!(intents().len(), 1, "{label}: the landing intent names it");
    assert_eq!(world.unaccounted(), 1, "{label}: journal-missing");

    if kind == EstateConverge::LinkedOperator {
        fs::write(feat.join("mine.txt"), b"the operator's\n").unwrap();
        git(&feat, &["add", "mine.txt"]);
        // A prefix of f1's blob ("seat 1, c0\n").
        fs::write(feat.join("f1"), b"seat").unwrap();
        let rows = world.apply("plan", "corpus", "applied");
        assert_eq!(rows.len(), 2, "{label}: {rows:?}");
        assert_eq!(
            rows[1],
            ("refused", Some("GIT_DESTINATION_MODIFIED".to_owned())),
            "{label}"
        );
        assert!(
            !git(&feat, &["ls-files", "--stage", "mine.txt"]).is_empty(),
            "{label}: the operator's staged file is kept"
        );
        assert_eq!(fs::read(feat.join("f1")).unwrap(), b"seat", "{label}");
        assert_eq!(intents().len(), 1, "{label}: the landing intent stays");
        return;
    }

    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(rows.len(), 2, "{label}: {rows:?}");
    assert_eq!(
        rows[0],
        ("previous-workspace-restoration-not-revalidated", None),
        "{label}"
    );
    assert_eq!(rows[1].0, "workspace-restored", "{label}: {rows:?}");
    assert!(
        rows[1]
            .1
            .as_deref()
            .is_some_and(|reason| reason.contains("landing-recovered=1")),
        "{label}: {rows:?}"
    );
    assert!(
        intents().is_empty(),
        "{label}: the landing intent is removed"
    );
    assert!(world
        .dst()
        .join(".git/worktrees/dst-feat/bulkload/v1/neo/workspace.landed")
        .is_file());
    assert_eq!(
        snapshot(&feat),
        snapshot(&world.base.join("fresh-feat")),
        "{label}: the recovered landing equals a fresh restore"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(
        rows,
        vec![
            ("previous-workspace-restoration-not-revalidated", None),
            ("previous-workspace-restoration-not-revalidated", None),
        ],
        "{label}"
    );
}

/// The `SwitchBack` row's middle: after a crash past `set_head`, the
/// operator puts HEAD back on the branch it left; the rerun refuses and
/// leaves it there (the intent records HEAD done), and the operator puts it
/// forward again.
fn switch_back(world: &World, label: &str) {
    git(&world.dst(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(
        rows,
        vec![("refused", Some("GIT_CONVERGE_INTERRUPTED".to_owned()))],
        "{label}: the operator's HEAD is never moved again"
    );
    assert_eq!(
        git(&world.dst(), &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main",
        "{label}"
    );
    git(&world.dst(), &["symbolic-ref", "HEAD", "refs/heads/side"]);
}

/// The `DoubleCheckout` row's end: the crash left `side` at its new value
/// and HEAD on `main`; the operator adds a worktree on `side`, as Git
/// allows. The rerun re-plans HEAD (detached at the captured head, `side`
/// recorded) and never puts a second worktree on `side`.
fn double_checkout(world: &World, label: &str) {
    let operator = world.base.join("operator-wt");
    git(
        &world.dst(),
        &[
            "worktree",
            "add",
            "--quiet",
            operator.to_str().unwrap(),
            "side",
        ],
    );
    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(reason.contains("head-replanned=1"), "{label}: {reason}");
    assert!(
        reason.contains("head-detached-ref-conflict=1"),
        "{label}: {reason}"
    );
    let listed = git(&world.dst(), &["worktree", "list", "--porcelain"]);
    assert_eq!(
        listed
            .lines()
            .filter(|line| *line == "branch refs/heads/side")
            .count(),
        1,
        "{label}: one worktree on side: {listed}"
    );
    let symbolic = git_command(&world.dst())
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    assert!(!symbolic.status.success(), "{label}: HEAD stays detached");
    assert_eq!(
        git(&world.dst(), &["rev-parse", "HEAD"]),
        git(&world.src(), &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(
        git(&operator, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/side",
        "{label}"
    );
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
    assert_eq!(world.unaccounted(), 0, "{label}");
}

/// The `PackedNew` row's middle: what a killed delete child leaves beside
/// the pending prune; the rerun keeps the lock (not provably bulkload's)
/// and refuses typed; the operator removes it, as Git instructs.
fn packed_new(world: &World, label: &str) {
    let git_dir = world.dst().join(".git");
    let (lock, new) = (
        git_dir.join("packed-refs.lock"),
        git_dir.join("packed-refs.new"),
    );
    fs::write(&lock, b"").unwrap();
    fs::write(&new, b"# pack-refs with: peeled fully-peeled sorted \n").unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
    for path in [&lock, &new] {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(past))
            .unwrap();
    }
    let rows = world.apply("plan", "corpus", "applied");
    assert_eq!(
        rows,
        vec![("refused", Some("GIT_CONVERGE_INTERRUPTED".to_owned()))],
        "{label}"
    );
    assert!(lock.exists(), "{label}: an empty lock is never removed");
    fs::remove_file(&lock).unwrap();
}

/// The `FreshIndexLock` row's middle: the lock Git takes on the converge's
/// scratch index, as a `read-tree` killed while holding it leaves it.
fn fresh_index_lock(world: &World, label: &str) {
    let git_dir = world.dst().join(".git");
    let fresh = fs::read_dir(&git_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("bulkload-index-") && name.ends_with("-fresh"))
        .unwrap_or_else(|| panic!("{label}: no scratch index at the crash"));
    fs::write(git_dir.join(format!("{fresh}.lock")), b"DIRC").unwrap();
}
