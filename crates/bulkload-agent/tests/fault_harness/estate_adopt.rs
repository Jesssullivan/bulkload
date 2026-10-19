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
//! `ExcludeUnion` (M2 round 2, item 2): a source linked worktree whose
//! common exclude is a superset of the destination repository's, applied
//! with `--exclude-union` (this row only: the others run without it),
//! crashed after the append's intent is recorded (the exclude untouched,
//! its temporary written) or after the rename (the record still an
//! intent): the worktree is absent and the item `journal-missing`; the
//! rerun settles the intent by identity (`not-written`, its temporary
//! removed, then a fresh append; or `appended`, with nothing left to
//! append), counts the unreported append `exclude-appended=1` either way,
//! lands the worktree, and a second rerun is a journal no-op.
//!
//! `Beside` (M2 round 2 item c): the `Workspace` shape with one seat the
//! destination changed, so `--adopt` refuses `GIT_DESTINATION_DIVERGED`,
//! applied with `--adopt --beside` (this row only), crashed after the
//! beside worktree is added and before its ledger (`linked_added`: the
//! rerun finishes it forward from its landing intent), or after the refs
//! are created and before the STATE record and the journal (`beside_refs`):
//! the destination checkout is untouched either way, the item
//! `journal-missing`; the rerun lands (or finds landed) the beside checkout,
//! every `refs/<suffix>/` ref holds the capture's value and none moved,
//! the record is written and the journal says `workspace-beside`; a second
//! rerun is a journal no-op.
//!
//! `BesideConverge` (gap C12, operator ruling A-115): the `Beside` shape
//! landed beside by a first pass, then the source moved and was captured
//! again, and the second pass's converge of the beside checkout crashed
//! after its intent, a seat, the index or HEAD. The destination checkout is
//! untouched; the rerun settles the converge forward with no native ref of
//! the destination repository stepped: the beside checkout holds the new
//! capture, still detached, every namespace ref is at the new capture's
//! value, and a second rerun is a journal no-op.
//!
//! `BesideRekey` (review of #282, F6): the in-place hand plans' shape.
//! Another plan's item landed the source standalone at the beside path;
//! the source moved and was captured again; this item's pass re-keyed that
//! landing and its converge, in the beside checkout's own repository,
//! crashed after its intent, a seat or HEAD. The destination checkout and
//! its native refs are untouched; the rerun settles the converge forward:
//! the beside checkout holds the new capture and stays standalone, every
//! namespace ref is at the new capture's value, and a second rerun is a
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
/// Set only in an `ExcludeUnion` child: it applies with `--exclude-union`.
const CHILD_UNION_ENV: &str = "BULKLOAD_W7_ADOPT_CHILD_UNION";
/// Set only in a `Beside` child: it applies with `--beside SUFFIX`.
const CHILD_BESIDE_ENV: &str = "BULKLOAD_W7_ADOPT_CHILD_BESIDE";
/// The `Beside` rows' suffix.
const SUFFIX: &str = ".neo-20261016";

/// What is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateAdopt {
    /// A standalone checkout adopted in place.
    Workspace,
    /// A bare first landing (A11c).
    Bare,
    /// A first landing under absent parents (M2 finding 1).
    Parents,
    /// A first linked landing under `--exclude-union` (M2 round 2, item 2).
    ExcludeUnion,
    /// A diverged checkout landed beside under `--beside` (M2 round 2,
    /// item c).
    Beside,
    /// `Beside` crashed at `linked_added`, then the source moved and was
    /// captured again (review of #266, B1).
    BesideStale,
    /// `Beside` landed, the source moved, and the beside converge crashed
    /// (gap C12, A-115).
    BesideConverge,
    /// Another plan's item landed standalone at the beside path (the
    /// in-place hand plans), the source moved, and its re-key's converge
    /// crashed (review of #282, F6).
    BesideRekey,
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
    /// `--exclude-union` on every apply: the `ExcludeUnion` row only, so the
    /// other rows keep their crash coverage without the flag.
    union: bool,
    /// `--beside` on every apply: the `Beside` row only.
    beside: bool,
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
            union: kind == EstateAdopt::ExcludeUnion,
            beside: matches!(
                kind,
                EstateAdopt::Beside
                    | EstateAdopt::BesideStale
                    | EstateAdopt::BesideConverge
                    | EstateAdopt::BesideRekey
            ),
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
        let (item, dest) = if kind == EstateAdopt::ExcludeUnion {
            (world.exclude_union_item(&src), None)
        } else if kind == EstateAdopt::Parents {
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
            if matches!(
                kind,
                EstateAdopt::Beside
                    | EstateAdopt::BesideStale
                    | EstateAdopt::BesideConverge
                    | EstateAdopt::BesideRekey
            ) {
                fs::write(dest.join("d/b.txt"), b"beta, edited at the destination\n").unwrap();
                // Review B6: the source's HEAD is a branch the destination
                // lacks, so an attach would create it there.
                git(&src, &["switch", "--quiet", "-c", "beside-only"]);
            }
        }
        estate::add_batch(&world.base.join("plan"), &[item]).unwrap();
        world
    }

    /// The `ExcludeUnion` item: a linked worktree of `src` whose common
    /// exclude is a superset of the destination's, a `cp -a` of `src`
    /// without its worktree administration.
    fn exclude_union_item(&self, src: &Path) -> Item {
        let tree = self.base.join("src-wt");
        git(
            src,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                tree.to_str().unwrap(),
            ],
        );
        fs::write(tree.join("wt.txt"), b"wt\n").unwrap();
        fs::create_dir_all(src.join(".git/info")).unwrap();
        fs::write(src.join(".git/info/exclude"), b"*.log\nbuild/\n").unwrap();
        let out = Command::new("cp")
            .arg("-a")
            .arg(src)
            .arg(self.dst())
            .output()
            .unwrap();
        assert!(out.status.success());
        fs::remove_dir_all(self.dst().join(".git/worktrees")).unwrap();
        fs::write(self.dst().join(".git/info/exclude"), b"*.log\n").unwrap();
        Item {
            source: tree,
            repository: self.dst(),
            workspace: Some(self.base.join("dst-wt")),
        }
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
                exclude_union: self.union,
                beside: self.beside.then(|| SUFFIX.to_owned()),
                side_roots: Vec::new(),
                refs_in: false,
                advance_ancestor: false,
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
        .env(CHILD_UNION_ENV, if world.union { "1" } else { "" })
        .env(CHILD_BESIDE_ENV, if world.beside { "1" } else { "" })
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
            exclude_union: std::env::var_os(CHILD_UNION_ENV).is_some_and(|value| value == "1"),
            beside: std::env::var_os(CHILD_BESIDE_ENV)
                .is_some_and(|value| value == "1")
                .then(|| SUFFIX.to_owned()),
            side_roots: Vec::new(),
            refs_in: false,
            advance_ancestor: false,
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
    if kind == EstateAdopt::ExcludeUnion {
        exclude_union(&world, &label, point, nth);
        return;
    }
    let besides: Option<fn(&World, &str, Point, u64)> = match kind {
        EstateAdopt::Beside => Some(beside),
        EstateAdopt::BesideStale => Some(beside_stale),
        EstateAdopt::BesideConverge => Some(beside_converge),
        EstateAdopt::BesideRekey => Some(beside_rekey),
        _ => None,
    };
    if let Some(row) = besides {
        row(&world, &label, point, nth);
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

/// The `ExcludeUnion` row: see the module docs.
fn exclude_union(world: &World, label: &str, point: Point, nth: u64) {
    use bulkload_agent::git_carry::exclude_union::{read_record, AppendState};
    let exclude = world.dst().join(".git/info/exclude");
    let record = || {
        let found: Vec<PathBuf> = fs::read_dir(world.base.join("applied"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "exclude-union"))
            .collect();
        assert_eq!(found.len(), 1, "{label}: one record");
        read_record(&found[0]).unwrap().unwrap()
    };
    let written = point == Point::EstateApplyExcludeAppended;
    crash(world, &format!("{}:{nth}", point.name()));
    assert_eq!(
        fs::read(&exclude).unwrap(),
        if written {
            b"*.log\nbuild/\n".as_slice()
        } else {
            b"*.log\n"
        },
        "{label}"
    );
    assert_eq!(
        record()
            .appends
            .iter()
            .map(|append| append.state)
            .collect::<Vec<_>>(),
        [AppendState::Intent],
        "{label}: recorded ahead of the write, settled after it"
    );
    assert!(
        fs::symlink_metadata(world.base.join("dst-wt")).is_err(),
        "{label}: nothing landed"
    );
    assert_eq!(world.unaccounted(), 1, "{label}: journal-missing");
    let rows = world.apply(false);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_some_and(|reason| reason.contains("exclude-appended=1")),
        "{label}: the append reaches a receipt either way: {rows:?}"
    );
    let leftovers: Vec<_> = fs::read_dir(world.dst().join(".git/info"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["exclude"], "{label}: a temporary was left");
    assert_eq!(fs::read(&exclude).unwrap(), b"*.log\nbuild/\n", "{label}");
    let states: Vec<AppendState> = record().appends.iter().map(|append| append.state).collect();
    if written {
        assert_eq!(states, [AppendState::Appended], "{label}");
    } else {
        assert_eq!(
            states,
            [AppendState::NotWritten, AppendState::Appended],
            "{label}"
        );
    }
    assert_eq!(
        fs::read(world.base.join("dst-wt/wt.txt")).unwrap(),
        b"wt\n",
        "{label}"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(false);
    assert_eq!(
        rows[0].0, "previous-workspace-restoration-not-revalidated",
        "{label}"
    );
}

/// The beside checkout of the `Beside` rows.
fn beside_path(world: &World) -> PathBuf {
    world.base.join(format!("dst{SUFFIX}"))
}

/// Every ref under `refs/neo-20261016/` and `refs/neo-20261016-stash/`.
fn beside_refs(repo: &Path) -> std::collections::BTreeMap<String, String> {
    refs(repo)
        .into_iter()
        .filter(|(name, _)| name.starts_with("refs/neo-"))
        .collect()
}

/// The refs the `Beside` rows' capture wants under the namespace.
fn wanted_refs(world: &World) -> std::collections::BTreeMap<String, String> {
    let src = world.base.join("src");
    let mut wanted: std::collections::BTreeMap<String, String> = refs(&src)
        .into_iter()
        .filter(|(name, _)| name != "refs/stash")
        .map(|(name, value)| {
            (
                format!("refs/neo-20261016/{}", name.strip_prefix("refs/").unwrap()),
                value,
            )
        })
        .collect();
    if let Ok(tip) = git_command(&src)
        .args(["rev-parse", "--verify", "-q", "refs/stash"])
        .output()
    {
        let tip = String::from_utf8(tip.stdout).unwrap().trim().to_owned();
        if !tip.is_empty() {
            wanted.insert("refs/neo-20261016/stash".to_owned(), tip);
            let list = git(&src, &["stash", "list", "--format=%H"]);
            for (index, oid) in list.lines().enumerate() {
                wanted.insert(format!("refs/neo-20261016-stash/{index}"), oid.to_owned());
            }
        }
    }
    wanted
}

/// The `Beside` rows' STATE record, if written.
fn beside_record(world: &World) -> Option<bulkload_agent::git_carry::beside::BesideRecord> {
    let found: Vec<PathBuf> = fs::read_dir(world.base.join("applied"))
        .ok()?
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "beside"))
        .collect();
    assert!(found.len() <= 1, "{found:?}");
    found.first().map(|path| {
        bulkload_agent::git_carry::beside::read_record(path)
            .unwrap()
            .unwrap()
    })
}

/// The `Beside` row: see the module docs.
fn beside(world: &World, label: &str, point: Point, nth: u64) {
    let dst = world.dst();
    let before = checkout(&dst);
    let native: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    let wanted = wanted_refs(world);
    crash(world, &format!("{}:{nth}", point.name()));
    assert_eq!(checkout(&dst), before, "{label}: the destination moved");
    let after_refs = point == Point::EstateApplyBesideRefs;
    // Review B5/B6: at `beside_refs` every wanted ref is in and the record
    // names them all pending; at `linked_added` neither.
    if after_refs {
        assert_eq!(beside_refs(&dst), wanted, "{label}: the refs at the crash");
        let record = beside_record(world).unwrap();
        let pending: std::collections::BTreeMap<String, String> =
            record.pending.into_iter().collect();
        assert_eq!(pending, wanted, "{label}: the write-ahead record");
        assert!(record.created.is_empty(), "{label}");
    } else {
        assert!(beside_refs(&dst).is_empty(), "{label}");
        assert!(
            beside_record(world).is_none(),
            "{label}: no record before the refs"
        );
    }
    assert_eq!(world.unaccounted(), 1, "{label}: journal-missing");
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    if after_refs {
        assert!(reason.contains("beside-present=1"), "{label}: {reason}");
    } else {
        // Finished forward from the landing intent, never converged.
        assert!(
            reason.contains("landing-recovered") && !reason.contains("converged"),
            "{label}: {reason}"
        );
    }
    assert_eq!(
        checkout(&dst),
        before,
        "{label}: the rerun moved the destination"
    );
    let now: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    assert_eq!(now, native, "{label}: a native ref moved or was created");
    assert_eq!(beside_refs(&dst), wanted, "{label}");
    let record = beside_record(world).unwrap();
    assert!(
        record.pending.is_empty() && record.kept.is_empty(),
        "{label}"
    );
    let created: std::collections::BTreeMap<String, String> = record.created.into_iter().collect();
    assert_eq!(created, wanted, "{label}: every ref recorded as created");
    let beside = beside_path(world);
    assert_eq!(
        fs::read(beside.join("d/b.txt")).unwrap(),
        b"beta\n",
        "{label}: the beside holds the capture"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&world.base.join("src"), &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD",
        "{label}: detached"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-beside-not-revalidated",
        "{label}"
    );
}

/// The `BesideStale` row (review B1; gap C12, A-115): a beside landing
/// crashed after its worktree was added, then the source moved and was
/// captured again. The rerun finishes the crashed landing forward (the
/// older capture) and converges it to the new capture in the same pass:
/// still detached at the (new) captured head, the destination checkout and
/// its native refs untouched, every namespace ref at the new capture's
/// value and recorded created, the item `applied-beside`; a second rerun
/// is a journal no-op.
fn beside_stale(world: &World, label: &str, point: Point, nth: u64) {
    let dst = world.dst();
    let before = checkout(&dst);
    let native: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    let src = world.base.join("src");
    crash(world, &format!("{}:{nth}", point.name()));
    fs::write(src.join("a.txt"), b"alpha, moved after the crash\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "moved"]);
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    world.capture();
    let wanted = wanted_refs(world);
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(
        reason.contains("landing-recovered")
            && reason.contains("converged")
            && reason.contains("beside-converged=1"),
        "{label}: {reason}"
    );
    assert_eq!(checkout(&dst), before, "{label}");
    let now: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    assert_eq!(now, native, "{label}: a native ref moved or was created");
    assert_eq!(beside_refs(&dst), wanted, "{label}");
    let record = beside_record(world).unwrap();
    assert!(
        record.pending.is_empty() && record.kept.is_empty(),
        "{label}"
    );
    let created: std::collections::BTreeMap<String, String> = record.created.into_iter().collect();
    assert_eq!(created, wanted, "{label}");
    let beside = beside_path(world);
    assert_eq!(
        fs::read(beside.join("a.txt")).unwrap(),
        b"alpha, moved after the crash\n",
        "{label}: the beside holds the new capture"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD",
        "{label}: detached"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-beside-not-revalidated",
        "{label}"
    );
}

/// The `BesideConverge` rows: see the module docs.
fn beside_converge(world: &World, label: &str, point: Point, nth: u64) {
    let dst = world.dst();
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "workspace-beside",
        "{label}: first pass {rows:?}"
    );
    let before = checkout(&dst);
    let native: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    let src = world.base.join("src");
    fs::write(src.join("a.txt"), b"alpha, moved\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "moved"]);
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    world.capture();
    let wanted = wanted_refs(world);
    crash(world, &format!("{}:{nth}", point.name()));
    assert_eq!(checkout(&dst), before, "{label}: the destination moved");
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(reason.contains("converged"), "{label}: {reason}");
    assert_eq!(
        checkout(&dst),
        before,
        "{label}: the rerun moved the destination"
    );
    let now: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    assert_eq!(now, native, "{label}: a native ref moved or was created");
    assert_eq!(beside_refs(&dst), wanted, "{label}");
    let record = beside_record(world).unwrap();
    assert!(
        record.pending.is_empty() && record.kept.is_empty(),
        "{label}"
    );
    let beside = beside_path(world);
    assert_eq!(
        fs::read(beside.join("a.txt")).unwrap(),
        b"alpha, moved\n",
        "{label}: the beside holds the new capture"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD",
        "{label}: detached"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-beside-not-revalidated",
        "{label}"
    );
}

/// The `BesideRekey` rows: see the module docs.
fn beside_rekey(world: &World, label: &str, point: Point, nth: u64) {
    let dst = world.dst();
    let src = world.base.join("src");
    let beside = beside_path(world);
    let hand = world.base.join("plan-hand");
    estate::add_batch(
        &hand,
        &[Item {
            source: src.clone(),
            repository: beside.clone(),
            workspace: Some(beside.clone()),
        }],
    )
    .unwrap();
    estate::capture(
        &hand,
        &world.base.join("corpus-state-hand"),
        &world.base.join("corpus-hand"),
        1,
        &|_| Ok(()),
    )
    .unwrap();
    let landed = Mutex::new(Vec::new());
    let _ = estate::apply_with(
        &hand,
        &world.base.join("corpus-hand"),
        &world.base.join("applied-hand"),
        "neo",
        1,
        &ApplyOptions {
            adopt: false,
            activate_origin: Vec::new(),
            exclude_union: false,
            beside: None,
            side_roots: Vec::new(),
            refs_in: false,
            advance_ancestor: false,
        },
        &|row| {
            landed.lock().unwrap().push(row.outcome);
            Ok(())
        },
    );
    assert_eq!(
        landed.into_inner().unwrap(),
        ["workspace-restored"],
        "{label}: the hand plan's standalone landing"
    );
    let before = checkout(&dst);
    let native: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    fs::write(src.join("a.txt"), b"alpha, moved\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "moved"]);
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    world.capture();
    let wanted = wanted_refs(world);
    crash(world, &format!("{}:{nth}", point.name()));
    assert_eq!(checkout(&dst), before, "{label}: the destination moved");
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(
        reason.contains("converged") && reason.contains("beside-standalone=1"),
        "{label}: {reason}"
    );
    assert_eq!(
        checkout(&dst),
        before,
        "{label}: the rerun moved the destination"
    );
    let now: std::collections::BTreeMap<String, String> = refs(&dst)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect();
    assert_eq!(now, native, "{label}: a native ref moved or was created");
    assert_eq!(beside_refs(&dst), wanted, "{label}");
    assert!(
        fs::symlink_metadata(beside.join(".git")).unwrap().is_dir(),
        "{label}: still standalone"
    );
    assert_eq!(
        fs::read(beside.join("a.txt")).unwrap(),
        b"alpha, moved\n",
        "{label}: the beside holds the new capture"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(world.unaccounted(), 0, "{label}");
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-beside-not-revalidated",
        "{label}"
    );
}
