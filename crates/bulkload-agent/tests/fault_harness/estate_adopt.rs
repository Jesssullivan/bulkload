//! Estate adopt crash rows (#226; design
//! `docs/agent-notes/2026-10-15-estate-adopt-design.md`, section 7): the
//! `estate.adopt.*` fault points.
//!
//! `Workspace`: a source checkout with a stash, a tag and branches, and a
//! `cp -a` copy of it at the destination lacking one branch and holding
//! one of its own (the GFTB swap's shape). A child (this test binary,
//! re-run) applies with `--adopt` and one point armed, and ends itself
//! there with `_exit`; nothing here sends it a signal (R-N11). The parent
//! asserts:
//!
//! - **nothing but bulkload's administration moved**: every seat keeps
//!   every `lstat` field, HEAD and the index listing are unchanged, and
//!   every native ref holds its old value or the capture's (a create);
//! - **accounted**: closure counts the item unaccounted
//!   (`journal-missing`) until a rerun;
//! - **typed without the flag**: before the workspace ledger, a rerun
//!   without `--adopt` refuses `GIT_DESTINATION_OCCUPIED` and writes
//!   nothing;
//! - **finishes**: the rerun with `--adopt` (or, after the ledger, any
//!   rerun) applies; the destination-only branch stays; every source ref is
//!   native; closure 0 unaccounted;
//! - **idempotent**: a second rerun is a journal no-op.
//!
//! `Bare`: a refs-only capture of a bare repository into an absent path
//! (A11c), crashed with the repository complete in its private stage: the
//! destination does not exist, and the rerun lands it.
//!
//! `Parents` (M2 finding 1): a standalone checkout planned under two absent
//! parents, crashed after they are made and synced and before STATE
//! records them: both are 0755 directories, the destination and the record
//! are absent, the item is `journal-missing`; the rerun lands the checkout
//! (nothing left to make, so nothing recorded), and a second rerun is a
//! journal no-op.
//!
//! `_exit` models a process crash only (see the harness header).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use bulkload_agent::estate::{self, ApplyOptions, Item};
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the scratch base it applies in.
const CHILD_ENV: &str = "BULKLOAD_W7_ADOPT_CHILD_BASE";

/// What is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateAdopt {
    /// A standalone checkout adopted in place.
    Workspace,
    /// A bare first landing (A11c).
    Bare,
    /// A first landing under absent parents (M2 finding 1).
    Parents,
}

impl Scenario for EstateAdopt {
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

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

impl World {
    fn new(tag: &str, kind: EstateAdopt) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-adopt-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let world = Self {
            base: fs::canonicalize(&base).unwrap(),
        };
        let src = world.base.join("src");
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(src.join("a.txt"), b"alpha\n").unwrap();
        fs::create_dir(src.join("d")).unwrap();
        fs::write(src.join("d/b.txt"), b"beta\n").unwrap();
        git(&src, &["add", "."]);
        git(&src, &["commit", "--quiet", "-m", "c0"]);
        git(&src, &["tag", "v0"]);
        git(&src, &["branch", "side"]);
        git(&src, &["branch", "neo-only"]);
        git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let (item, dest) = if kind == EstateAdopt::Parents {
            (
                Item {
                    source: src.clone(),
                    repository: world.parents_dst(),
                    workspace: Some(world.parents_dst()),
                },
                None,
            )
        } else if kind == EstateAdopt::Bare {
            let mirror = world.base.join("src.git");
            git(
                &world.base,
                &[
                    "clone",
                    "--quiet",
                    "--bare",
                    src.to_str().unwrap(),
                    mirror.to_str().unwrap(),
                ],
            );
            (
                Item {
                    source: mirror,
                    repository: world.dst(),
                    workspace: None,
                },
                None,
            )
        } else {
            fs::write(src.join("a.txt"), b"alpha, stashed\n").unwrap();
            git(&src, &["stash", "push", "--quiet", "-m", "wip"]);
            fs::write(src.join("untracked.txt"), b"kept\n").unwrap();
            (
                Item {
                    source: src.clone(),
                    repository: world.dst(),
                    workspace: Some(world.dst()),
                },
                Some(world.dst()),
            )
        };
        if let Some(dest) = dest {
            let out = Command::new("cp")
                .arg("-a")
                .arg(&src)
                .arg(&dest)
                .output()
                .unwrap();
            assert!(out.status.success());
            git(&dest, &["branch", "-D", "neo-only"]);
            git(&dest, &["branch", "sting-only", "v0"]);
        }
        estate::add_batch(&world.base.join("plan"), &[item]).unwrap();
        world
    }

    fn dst(&self) -> PathBuf {
        self.base.join("dst")
    }

    fn parents_dst(&self) -> PathBuf {
        self.base.join("made/deep/dst")
    }

    fn capture(&self) {
        estate::capture(
            &self.base.join("plan"),
            &self.base.join("corpus-state"),
            &self.base.join("corpus"),
            1,
            &|_| Ok(()),
        )
        .unwrap();
    }

    fn apply(&self, adopt: bool) -> Vec<(&'static str, Option<String>)> {
        let rows = Mutex::new(Vec::new());
        let _ = estate::apply_with(
            &self.base.join("plan"),
            &self.base.join("corpus"),
            &self.base.join("applied"),
            "neo",
            1,
            &ApplyOptions {
                adopt,
                activate_origin: Vec::new(),
            },
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
}

/// Every seat but `.git` with its full `lstat` identity, HEAD and the
/// index listing.
fn checkout(worktree: &Path) -> (Vec<String>, String, String) {
    use std::os::unix::fs::MetadataExt as _;
    let mut seats = Vec::new();
    let mut pending = vec![worktree.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            seats.push(format!(
                "{} {} {} {} {} {}",
                path.display(),
                meta.ino(),
                meta.mtime_nsec(),
                meta.ctime_nsec(),
                meta.size(),
                meta.mode()
            ));
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    seats.sort();
    (
        seats,
        git(worktree, &["rev-parse", "--symbolic-full-name", "HEAD"])
            + &git(worktree, &["rev-parse", "HEAD"]),
        git(worktree, &["ls-files", "--stage"]),
    )
}

fn refs(repo: &Path) -> std::collections::BTreeMap<String, String> {
    git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|line| !line.starts_with("refs/carry"))
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

/// Run this test binary as a child that applies with `--adopt`, armed with
/// `fault`: it must end itself at that point.
fn crash(world: &World, fault: &str) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "estate_adopt::adopt_child_entry",
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
            panic!("{fault}: adopt child still running ({timeout}); left running, not signalled")
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

/// The child half of every adopt scenario; a no-op in an ordinary run.
#[test]
fn adopt_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("adopt child watchdog: not done within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
    let outcome = estate::apply_with(
        &base.join("plan"),
        &base.join("corpus"),
        &base.join("applied"),
        "neo",
        1,
        &ApplyOptions {
            adopt: true,
            activate_origin: Vec::new(),
        },
        &|_| Ok(()),
    );
    panic!("the armed point was not reached: apply returned {outcome:?}");
}

fn scenario(point: Point, nth: u64, kind: EstateAdopt) {
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::new(&point.name().replace('.', "-"), kind);
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    world.capture();
    if kind == EstateAdopt::Parents {
        parents(&world, &label, point, nth);
        return;
    }
    if kind == EstateAdopt::Bare {
        crash(&world, &format!("{}:{nth}", point.name()));
        assert!(
            fs::symlink_metadata(world.dst()).is_err(),
            "{label}: a bare landing is published whole or not at all"
        );
        assert_eq!(world.unaccounted(), 1, "{label}: journal-missing");
        let rows = world.apply(false);
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].0, "refs-imported", "{label}: {rows:?}");
        let source = world.base.join("src.git");
        assert_eq!(refs(&world.dst()), refs(&source), "{label}");
        assert_eq!(world.unaccounted(), 0, "{label}");
        let rows = world.apply(false);
        assert_eq!(
            rows[0].0, "previous-ref-custody-not-workspace-parity",
            "{label}"
        );
        return;
    }
    let dst = world.dst();
    let before = checkout(&dst);
    let old = refs(&dst);
    let source = refs(&world.base.join("src"));

    crash(&world, &format!("{}:{nth}", point.name()));

    // Nothing but bulkload's administration moved: no seat, HEAD or index,
    // and each ref old or created at the capture's value.
    assert_eq!(
        checkout(&dst),
        before,
        "{label}: a seat, HEAD or the index moved"
    );
    for (name, value) in refs(&dst) {
        assert!(
            old.get(&name) == Some(&value) || source.get(&name) == Some(&value),
            "{label}: {name} is neither old nor the capture's"
        );
    }
    assert_eq!(
        world.unaccounted(),
        1,
        "{label}: journal-missing before the rerun"
    );

    // Before the workspace ledger, a rerun without --adopt is typed and
    // writes nothing.
    let ledgered = dst.join(".git/bulkload/v1/neo/workspace.landed").exists();
    if !ledgered {
        let frozen = (checkout(&dst), refs(&dst));
        let rows = world.apply(false);
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("GIT_DESTINATION_OCCUPIED"),
            "{label}: {rows:?}"
        );
        assert_eq!((checkout(&dst), refs(&dst)), frozen, "{label}");
    }
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    assert_eq!(checkout(&dst), before, "{label}: the rerun moved a seat");
    let now = refs(&dst);
    assert_eq!(
        now.get("refs/heads/sting-only"),
        old.get("refs/heads/sting-only")
    );
    for (name, value) in &source {
        assert_eq!(now.get(name), Some(value), "{label}: {name}");
    }
    assert!(
        !dst.join(".git/bulkload/v1/neo/refs.intent").exists(),
        "{label}"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-restoration-not-revalidated",
        "{label}: a second rerun is a no-op"
    );
}

/// The `Parents` row: see the module docs.
fn parents(world: &World, label: &str, point: Point, nth: u64) {
    use std::os::unix::fs::PermissionsExt as _;
    crash(world, &format!("{}:{nth}", point.name()));
    for level in ["made", "made/deep"] {
        let meta = fs::symlink_metadata(world.base.join(level)).unwrap();
        assert!(meta.is_dir(), "{label}: {level}");
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755, "{label}");
    }
    assert!(
        fs::symlink_metadata(world.parents_dst()).is_err(),
        "{label}: nothing landed before the record"
    );
    let records = |world: &World| {
        fs::read_dir(world.base.join("applied")).map_or(0, |dir| {
            dir.filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "parents")
            })
            .count()
        })
    };
    assert_eq!(records(world), 0, "{label}: no record before the sync");
    assert_eq!(world.unaccounted(), 1, "{label}: journal-missing");
    let rows = world.apply(false);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_none_or(|reason| !reason.contains("parent-created")),
        "{label}: the rerun made nothing: {rows:?}"
    );
    assert_eq!(
        git(&world.parents_dst(), &["rev-parse", "HEAD"]),
        git(&world.base.join("src"), &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(records(world), 0, "{label}");
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(false);
    assert_eq!(
        rows[0].0, "previous-workspace-restoration-not-revalidated",
        "{label}"
    );
}
