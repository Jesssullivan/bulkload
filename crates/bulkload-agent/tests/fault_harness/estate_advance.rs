//! Estate advance crash rows (operator ruling 2026-10-19, "Yes, advance
//! those"; `estate-apply --advance-ancestor`): the `estate.advance.*`
//! fault points.
//!
//! The world: a source checkout two commits ahead of a `cp -a` copy of
//! itself (the live checkout: clean, its HEAD an ancestor), whose commits
//! change a seat, add one and remove one, with a stash and a dirty edit,
//! an untracked and an ignored file of its own. The live checkout holds a
//! stash, a branch and a registered linked worktree of its own, and an
//! ignored file the source lacks. A child (this test binary, re-run)
//! applies with `--adopt --beside SUFFIX --refs-in --advance-ancestor` and
//! one point armed, and ends itself there with `_exit`; nothing here sends
//! it a signal (R-N11).
//!
//! - `Advance`: crashed after the advance's intent, after the branch's
//!   compare-and-swap, after a seat (the first, and one mid-materialize),
//!   after the index (the checkout done, no ledger) and after the ledger.
//! - `Retire`: a first pass without the flag landed the capture beside (a
//!   linked worktree); the advance pass crashed after the retirement's
//!   STATE record, after the rename to its private name, or with its
//!   administration removed and the renamed tree not yet deleted.
//! - `RetireStandalone`: the same with a standalone beside checkout a pass
//!   adopted by proof (#282).
//! - `RetireNoFlag`: `Retire`'s shape crashed mid-advance or after the
//!   rename, its rerun without the flag: the advance and the retirement a
//!   flagged pass began are finished all the same.
//! - `AdvanceEdited`: crashed after the branch's compare-and-swap, then a
//!   seat no operation touches (`.gitignore`) edited at the live checkout:
//!   the rerun finishes the advance from the proof's prints, then proves
//!   the ledger it wrote and refuses `GIT_DESTINATION_MODIFIED`, the edit
//!   kept (never recorded as bulkload's output).
//! - `AdvanceRolledBack` (review of #289, finding 1): crashed after the
//!   branch's compare-and-swap or mid-materialize, then a seat the advance
//!   replaces (`a.txt`) edited at the live checkout: the rerun meets the
//!   edit and rolls everything back (the seats to the live HEAD's bytes,
//!   the branch by compare-and-swap, bulkload's `index.lock` released),
//!   the edit kept, `GIT_ADVANCE_ROLLED_BACK`, the capture landed beside;
//!   never HEAD advanced over the old index, never wedged.
//! - `RollbackCrash`: the same rollback crashed after its intent or
//!   mid-seat; the next rerun finishes it.
//! - `Switch` (operator ruling 2026-10-19, "Switch when clean"): the
//!   source on another branch (`feature`) than the clean live checkout
//!   (`main`); crashed after the intent, between the branch's creation and
//!   HEAD's move, after both, mid-materialize, after the index and after
//!   the ledger; the rerun finishes the switch, the live `main` where it
//!   was.
//! - `RetireLiveEdited` (finding 3): the retirement crashed after its
//!   rename, then the live checkout edited: the rerun finishes the
//!   retirement before the item's workspace step, which then refuses
//!   `GIT_DESTINATION_MODIFIED`; nothing is left hidden.
//! - `RetireCarry` (the verification of #289, r6): a standalone beside
//!   whose configuration holds a key the live repository lacks with two
//!   values, crashed after the first went in: the rerun adds the second
//!   (all of a key's values or none), then retires the beside.
//! - `RetireWritten` (finding 2): the retirement crashed after its rename
//!   or marked deleting, then a file written into the renamed tree: the
//!   rerun never deletes it; the tree goes back to its path, kept (a
//!   duplicate pending review), the file with it.
//!
//! The parent asserts after the crash that neither tree is lost: every
//! live seat holds the live HEAD's bytes or the capture's, the live
//! checkout's own stash, branches, worktree and ignored file are as they
//! were, and the beside landing's bytes are whole at its path or at its
//! private retirement name. The rerun finishes: the live checkout holds the
//! capture on its branch, the beside landing is retired (nothing left at
//! either name), no intent or STATE retirement record stays, closure counts
//! nothing unaccounted, and a second rerun is a journal no-op.
//!
//! `_exit` models a process crash only (see the harness header).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use bulkload_agent::estate::{self, ApplyOptions, Item};
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the scratch base it applies in.
const CHILD_ENV: &str = "BULKLOAD_W7_ADVANCE_CHILD_BASE";
/// The beside suffix.
const SUFFIX: &str = ".neo-20261016";

/// What is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateAdvance {
    /// The advance of the live checkout.
    Advance,
    /// The retirement of a linked beside landing after the advance.
    Retire,
    /// The retirement of a standalone beside checkout adopted by proof.
    RetireStandalone,
    /// `Retire`'s shape, its rerun without `--advance-ancestor` (review
    /// of this lane, finding 1): the rerun still finishes the advance and
    /// the retirement a flagged pass began.
    RetireNoFlag,
    /// `Advance`, a seat no operation touches edited at the live checkout
    /// after the crash (review finding 2): the settle records the proof's
    /// print, so its proof refuses `GIT_DESTINATION_MODIFIED` and keeps the
    /// edit, never taking it for bulkload's output.
    AdvanceEdited,
    /// `Advance`, a seat it replaces edited after the crash: rolled back.
    AdvanceRolledBack,
    /// `AdvanceRolledBack`'s rollback itself crashed, then finished.
    RollbackCrash,
    /// The live checkout on `main`, the source on `feature`: switched.
    Switch,
    /// `Retire` crashed after its rename, the live checkout then edited.
    RetireLiveEdited,
    /// `Retire` crashed, a file then written into the renamed tree.
    RetireWritten,
    /// `RetireStandalone` with a two-valued key to carry, crashed mid-key.
    RetireCarry,
}

impl Scenario for EstateAdvance {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth, self);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn cp_a(from: &Path, to: &Path) {
    let out = Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(out.status.success());
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
    fn new(tag: &str, switch: bool) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-advance-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let world = Self {
            base: fs::canonicalize(&base).unwrap(),
        };
        let (src, dst) = (world.src(), world.dst());
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(src.join("a.txt"), b"alpha\n").unwrap();
        fs::create_dir(src.join("d")).unwrap();
        fs::write(src.join("d/b.txt"), b"beta\n").unwrap();
        fs::write(src.join("c.txt"), b"gamma\n").unwrap();
        fs::write(src.join(".gitignore"), b"*.log\n").unwrap();
        git(&src, &["add", "."]);
        git(&src, &["commit", "--quiet", "-m", "c0"]);
        git(&src, &["branch", "side"]);
        cp_a(&src, &dst);
        git(&dst, &["branch", "sting-only"]);
        fs::write(dst.join("a.txt"), b"alpha, sting's wip\n").unwrap();
        git(&dst, &["stash", "push", "--quiet", "-m", "sting-wip"]);
        git(
            &dst,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feat",
                world.feat().to_str().unwrap(),
            ],
        );
        fs::write(dst.join("sting.log"), b"sting's own ignored log\n").unwrap();
        if switch {
            git(&src, &["switch", "--quiet", "-c", "feature"]);
        }
        fs::write(src.join("a.txt"), b"alpha, c1\n").unwrap();
        fs::write(src.join("e.txt"), b"epsilon\n").unwrap();
        git(&src, &["add", "a.txt", "e.txt"]);
        git(&src, &["rm", "--quiet", "c.txt"]);
        git(&src, &["commit", "--quiet", "-m", "c1"]);
        fs::write(src.join("d/b.txt"), b"beta, c2\n").unwrap();
        git(&src, &["commit", "--quiet", "-am", "c2"]);
        fs::write(src.join("d/b.txt"), b"beta, stashed\n").unwrap();
        git(&src, &["stash", "push", "--quiet", "-m", "neo-wip"]);
        fs::write(src.join("a.txt"), b"alpha, dirty on the source\n").unwrap();
        fs::write(src.join("f.txt"), b"phi, untracked\n").unwrap();
        fs::write(src.join("neo.log"), b"the source's ignored log\n").unwrap();
        estate::add_batch(
            &world.base.join("plan"),
            &[Item {
                source: src,
                repository: dst.clone(),
                workspace: Some(dst),
            }],
        )
        .unwrap();
        world
    }

    fn src(&self) -> PathBuf {
        self.base.join("src")
    }

    fn dst(&self) -> PathBuf {
        self.base.join("dst")
    }

    fn feat(&self) -> PathBuf {
        self.base.join("dst-feat")
    }

    fn beside(&self) -> PathBuf {
        self.base.join(format!("dst{SUFFIX}"))
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

    fn apply(&self, advance: bool) -> Vec<(&'static str, Option<String>)> {
        let rows = Mutex::new(Vec::new());
        let _ = estate::apply_with(
            &self.base.join("plan"),
            &self.base.join("corpus"),
            &self.base.join("applied"),
            "neo",
            1,
            &options(advance),
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

    /// The private names a retirement renames to, in the beside's directory.
    fn retiring(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.base)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .as_bytes()
                    .starts_with(b".bulkload-retire-")
            })
            .collect()
    }
}

fn options(advance: bool) -> ApplyOptions {
    ApplyOptions {
        adopt: true,
        activate_origin: Vec::new(),
        exclude_union: false,
        beside: Some(SUFFIX.to_owned()),
        side_roots: Vec::new(),
        refs_in: true,
        advance_ancestor: advance,
        place_derived: false,
    }
}

/// Every entry under `root` but its top `.git`: file bytes, or a link's
/// target.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if at == root && path.file_name().unwrap() == ".git" {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.is_dir() {
                pending.push(path);
            } else if meta.file_type().is_symlink() {
                found.insert(
                    relative,
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                );
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    found
}

/// The live checkout's own state: its stash, branches and tags but `main`
/// (and `feature`, a switch's), its worktree's HEAD and its ignored file,
/// which nothing may touch.
fn own(world: &World) -> (String, Vec<String>, String, Vec<u8>) {
    let dst = world.dst();
    let refs: Vec<String> = git(&dst, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|line| {
            !line.starts_with("refs/carry")
                && !line.starts_with("refs/neo-")
                && !line.starts_with("refs/heads/main ")
                && !line.starts_with("refs/heads/feature ")
        })
        .map(str::to_owned)
        .collect();
    (
        git(&dst, &["stash", "list", "--format=%H %gs"]),
        refs,
        git(&world.feat(), &["rev-parse", "HEAD"]),
        fs::read(dst.join("sting.log")).unwrap(),
    )
}

/// Run this test binary as a child that applies with the flag, armed with
/// `fault`: it must end itself at that point.
fn crash(world: &World, fault: &str) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "estate_advance::advance_child_entry",
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
            panic!("{fault}: advance child still running ({timeout}); left running, not signalled")
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

/// The child half of every advance scenario; a no-op in an ordinary run.
#[test]
fn advance_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("advance child watchdog: not done within {CHILD_WATCHDOG:?}");
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
        &options(true),
        &|_| Ok(()),
    );
    panic!("the armed point was not reached: apply returned {outcome:?}");
}

#[allow(clippy::too_many_lines)]
fn scenario(point: Point, nth: u64, kind: EstateAdvance) {
    use EstateAdvance as K;
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::new(&point.name().replace('.', "-"), kind == K::Switch);
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    world.capture();
    // Off Linux nothing proves no process is inside the live checkout
    // (operator ruling 2026-10-19: "on Darwin refuse unless proven"): the
    // flagged apply refuses `_IN_USE`, reaches no advance point, and lands
    // the capture beside.
    if cfg!(not(target_os = "linux")) {
        let rows = world.apply(true);
        assert_eq!(rows.len(), 1, "{label}");
        assert!(
            rows[0]
                .1
                .as_deref()
                .is_some_and(|reason| reason.contains("_IN_USE")),
            "{label}: {rows:?}"
        );
        return;
    }
    if matches!(kind, K::RetireStandalone | K::RetireCarry) {
        cp_a(&world.src(), &world.beside());
    }
    if kind == K::RetireCarry {
        let beside = world.beside();
        git(&beside, &["config", "branch.main.remote", "origin"]);
        git(
            &beside,
            &["config", "--add", "branch.main.merge", "refs/heads/main"],
        );
        git(
            &beside,
            &["config", "--add", "branch.main.merge", "refs/heads/side"],
        );
    }
    let besides = !matches!(
        kind,
        K::Advance | K::AdvanceEdited | K::AdvanceRolledBack | K::RollbackCrash | K::Switch
    );
    if besides {
        let rows = world.apply(false);
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    }
    let dst = world.dst();
    let live = files(&dst);
    let mut captured = files(&world.src());
    let sting_log = PathBuf::from("sting.log");
    captured.insert(sting_log.clone(), live[&sting_log].clone());
    let mine = own(&world);
    let beside = besides.then(|| files(&world.beside()));
    let main = git(&dst, &["rev-parse", "main"]);

    // A rollback's crash comes on the rerun of an advance crashed after its
    // branch moved, a seat it replaces edited in between.
    let first = if kind == K::RollbackCrash {
        "estate.advance.after_branch:1".to_owned()
    } else {
        format!("{}:{nth}", point.name())
    };
    crash(&world, &first);

    // Neither tree lost: every live seat is the live HEAD's or the
    // capture's, the live checkout's own state is as it was, and the
    // beside landing is whole at its path or at its private name.
    for (path, bytes) in files(&dst) {
        assert!(
            live.get(&path) == Some(&bytes) || captured.get(&path) == Some(&bytes),
            "{label}: {} is neither the live HEAD's nor the capture's",
            path.display()
        );
    }
    assert_eq!(own(&world), mine, "{label}: the live checkout's own state");
    if let Some(beside) = &beside {
        let at = if world.beside().exists() {
            assert!(world.retiring().is_empty(), "{label}");
            world.beside()
        } else {
            let renamed = world.retiring();
            assert_eq!(renamed.len(), 1, "{label}: the beside landing is lost");
            renamed[0].clone()
        };
        assert_eq!(&files(&at), beside, "{label}: the beside landing's bytes");
    }

    match kind {
        K::AdvanceEdited => return edited(&world, &label),
        K::AdvanceRolledBack | K::RollbackCrash => {
            return rolled_back(&world, &label, (&live, &main), (kind, point, nth));
        }
        K::RetireLiveEdited => return retire_live_edited(&world, &label),
        K::RetireWritten => return retire_written(&world, &label),
        _ => {}
    }
    let rows = world.apply(kind != K::RetireNoFlag);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    assert_eq!(files(&dst), captured, "{label}: the live checkout");
    assert_eq!(
        git(&dst, &["rev-parse", "HEAD"]),
        git(&world.src(), &["rev-parse", "HEAD"]),
        "{label}"
    );
    assert_eq!(
        git(&dst, &["symbolic-ref", "HEAD"]).trim(),
        if kind == K::Switch {
            "refs/heads/feature"
        } else {
            "refs/heads/main"
        },
        "{label}"
    );
    if kind == K::Switch {
        // The live branch stays exactly where it was.
        assert_eq!(git(&dst, &["rev-parse", "main"]), main, "{label}: main");
        let reason = rows[0].1.clone().unwrap_or_default();
        assert!(
            reason
                .split_whitespace()
                .any(|word| word.starts_with("advance-switched-from=refs/heads/main@")),
            "{label}: {reason}"
        );
    }
    assert!(!dst.join(".git/index.lock").exists(), "{label}: the lock");
    assert_eq!(
        git(&dst, &["diff", "--cached", "--name-only"]),
        git(&world.src(), &["diff", "--cached", "--name-only"]),
        "{label}: the index"
    );
    assert_eq!(own(&world), mine, "{label}: the live checkout's own state");
    assert!(
        !dst.join(".git/bulkload/v1/neo/advance.intent").exists(),
        "{label}"
    );
    if besides {
        let reason = rows[0].1.clone().unwrap_or_default();
        assert!(
            reason
                .split_whitespace()
                .any(|word| word == "advance-beside=retired"),
            "{label}: {reason}"
        );
        assert!(!world.beside().exists(), "{label}");
        assert!(world.retiring().is_empty(), "{label}");
        let listed = git(&dst, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains(SUFFIX), "{label}: {listed}");
    }
    let records: Vec<PathBuf> = fs::read_dir(world.base.join("applied"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "retire"))
        .collect();
    assert!(records.is_empty(), "{label}: {records:?}");
    assert_eq!(world.unaccounted(), 0, "{label}");
    if kind == K::RetireCarry {
        assert_eq!(
            git(&dst, &["config", "--get-all", "branch.main.merge"]),
            "refs/heads/main\nrefs/heads/side\n",
            "{label}: both values, once each"
        );
        assert_eq!(
            git(&dst, &["config", "branch.main.remote"]),
            "origin\n",
            "{label}"
        );
    }
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-restoration-not-revalidated",
        "{label}: a second rerun is a no-op"
    );
}

/// The `AdvanceRolledBack` and `RollbackCrash` rows after the crash (module
/// docs): `live` is the live checkout's tree before, `main` its branch.
fn rolled_back(
    world: &World,
    label: &str,
    (live, main): (&BTreeMap<PathBuf, Vec<u8>>, &str),
    (kind, point, nth): (EstateAdvance, Point, u64),
) {
    let dst = world.dst();
    let edit = b"alpha, the operator's edit after the crash\n";
    fs::write(dst.join("a.txt"), edit).unwrap();
    if kind == EstateAdvance::RollbackCrash {
        crash(world, &format!("{}:{nth}", point.name()));
    }
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-beside", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(
        reason
            .split_whitespace()
            .any(|word| word == "advance-refused=GIT_ADVANCE_ROLLED_BACK"),
        "{label}: {reason}"
    );
    // Everything back but the edit: the branch, every seat, the index.
    assert_eq!(git(&dst, &["rev-parse", "main"]), main, "{label}: main");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), main, "{label}: HEAD");
    assert_eq!(
        git(&dst, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main",
        "{label}"
    );
    let mut expected = live.clone();
    expected.insert(PathBuf::from("a.txt"), edit.to_vec());
    assert_eq!(files(&dst), expected, "{label}: the live checkout");
    assert_eq!(
        git(&dst, &["status", "--porcelain", "--untracked-files=no"]),
        " M a.txt\n",
        "{label}: the index is the live HEAD's"
    );
    assert!(!dst.join(".git/index.lock").exists(), "{label}: the lock");
    assert!(
        !dst.join(".git/bulkload/v1/neo/advance.intent").exists(),
        "{label}"
    );
    assert!(
        world.beside().join(".git").is_file(),
        "{label}: landed beside"
    );
    let ledger = estate::ledger(
        &world.base.join("plan"),
        &world.base.join("corpus"),
        "neo",
        &[world.base.join("applied")],
    )
    .unwrap();
    let report = bulkload_agent::closure::Report::from_ledger(&ledger);
    assert_eq!(report.unaccounted, 0, "{label}");
    assert_eq!(
        report.refusals.get("GIT_ADVANCE_ROLLED_BACK"),
        Some(&1),
        "{label}: {:?}",
        report.refusals
    );
    // Never wedged: the next rerun refuses afresh (the edit is not clean)
    // and its beside journal stands.
    let rows = world.apply(true);
    assert_eq!(
        rows[0].0, "previous-workspace-beside-not-revalidated",
        "{label}: {rows:?}"
    );
}

/// The `RetireLiveEdited` row after its crash (module docs).
fn retire_live_edited(world: &World, label: &str) {
    let dst = world.dst();
    let edit = b"alpha, edited after the retirement's crash\n";
    fs::write(dst.join("a.txt"), edit).unwrap();
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_some_and(|reason| reason.contains("GIT_DESTINATION_MODIFIED")),
        "{label}: {rows:?}"
    );
    // The retirement finished first: nothing hidden, nothing pending.
    assert!(!world.beside().exists(), "{label}");
    assert!(world.retiring().is_empty(), "{label}");
    let records: Vec<PathBuf> = fs::read_dir(world.base.join("applied"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "retire"))
        .collect();
    assert!(records.is_empty(), "{label}: {records:?}");
    assert_eq!(
        fs::read(dst.join("a.txt")).unwrap(),
        edit,
        "{label}: the edit"
    );
}

/// The `RetireWritten` row after its crash (module docs).
fn retire_written(world: &World, label: &str) {
    let renamed = world.retiring();
    assert_eq!(renamed.len(), 1, "{label}: renamed");
    fs::write(renamed[0].join("written-since.txt"), b"a session's file\n").unwrap();
    let rows = world.apply(true);
    assert_eq!(rows.len(), 1, "{label}");
    assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
    let reason = rows[0].1.clone().unwrap_or_default();
    assert!(
        reason
            .split_whitespace()
            .any(|word| word == "advance-beside=kept"),
        "{label}: {reason}"
    );
    // Never deleted: back at its path, the file with it, kept.
    assert!(world.retiring().is_empty(), "{label}");
    assert_eq!(
        fs::read(world.beside().join("written-since.txt")).unwrap(),
        b"a session's file\n",
        "{label}"
    );
    let ledger = estate::ledger(
        &world.base.join("plan"),
        &world.base.join("corpus"),
        "neo",
        &[world.base.join("applied")],
    )
    .unwrap();
    let report = bulkload_agent::closure::Report::from_ledger(&ledger);
    assert_eq!(report.unaccounted, 0, "{label}");
    assert_eq!(
        report.refusals.get("GIT_ADVANCE_BESIDE_KEPT"),
        Some(&1),
        "{label}: {:?}",
        report.refusals
    );
}

/// The `AdvanceEdited` row after its crash (module docs).
fn edited(world: &World, label: &str) {
    let dst = world.dst();
    let edit = b"*.log\nedited at the destination\n";
    fs::write(dst.join(".gitignore"), edit).unwrap();
    // The rerun settles the advance forward (the branch, the seats, the
    // index, the ledger from the proof's prints), then proves the ledger
    // it wrote: the edit is a difference, refused and kept.
    for pass in ["settle", "again"] {
        let rows = world.apply(true);
        assert_eq!(rows.len(), 1, "{label} {pass}");
        assert!(
            rows[0]
                .1
                .as_deref()
                .is_some_and(|reason| reason.contains("GIT_DESTINATION_MODIFIED")),
            "{label} {pass}: {rows:?}"
        );
        assert_eq!(
            fs::read(dst.join(".gitignore")).unwrap(),
            edit,
            "{label} {pass}: the edit is never taken for bulkload's output"
        );
        assert_eq!(
            git(&dst, &["rev-parse", "HEAD"]),
            git(&world.src(), &["rev-parse", "HEAD"]),
            "{label} {pass}"
        );
        assert!(
            !dst.join(".git/bulkload/v1/neo/advance.intent").exists(),
            "{label} {pass}"
        );
    }
    assert_eq!(
        fs::read(dst.join("a.txt")).unwrap(),
        fs::read(world.src().join("a.txt")).unwrap(),
        "{label}: the advance is finished"
    );
}
