//! Estate advance (operator ruling 2026-10-19, interview, "Yes, advance
//! those"; lane note `docs/agent-notes/2026-10-19-adopt-ancestor.md`):
//! `estate-apply --adopt --beside SUFFIX --refs-in --advance-ancestor`.
//!
//! The fixture is the 2026-10-19 sting shape, scaled down. The source
//! ("neo") is two commits ahead of a `cp -a` copy of itself (the live
//! "sting" checkout, clean, its HEAD an ancestor), with neo's own dirty,
//! staged, untracked and ignored files and a stash. The live checkout has
//! a stash, a branch and a registered linked worktree of its own, and
//! ignored files neo lacks.
//!
//! - A1 (GloriousFlywheel): a first pass without the flag lands the
//!   capture beside (a linked worktree); the next pass with it advances the
//!   live checkout to neo's HEAD and working-tree changes (its branch moved
//!   by compare-and-swap, still attached), leaves sting's stash, branches,
//!   worktrees and ignored files as they were, puts neo's stash entry in
//!   under the namespace, and retires the beside landing once proved equal;
//!   closure `applied`, a rerun a journal no-op, and a later capture
//!   converges the live branch in place.
//! - A2: the same with a standalone beside checkout adopted by proof (#282).
//! - A3 (glorious.build): a live checkout that committed its own refuses
//!   `GIT_ADVANCE_NOT_ANCESTOR`, lands beside, closure pending review until
//!   closure-dispose; an unchanged rerun proves nothing again.
//! - A4 to A6: a dirty live checkout (`GIT_ADVANCE_NOT_CLEAN`), its branch
//!   checked out elsewhere (`GIT_ADVANCE_BRANCH_BUSY`), a detached capture
//!   (`GIT_ADVANCE_BRANCH_MISMATCH`): each lands beside, the live
//!   checkout untouched.
//! - A12 (review of #289, finding 1): a Git lock held at the live checkout
//!   (`index.lock`, `HEAD.lock`) refuses `GIT_ADVANCE_NOT_CLEAN`, nothing
//!   moved, the lock as it was.
//! - S1 and S2 (operator ruling 2026-10-19, "Switch when clean"): the
//!   capture on another branch than the clean live checkout's switches it
//!   (the capture's branch created at the captured HEAD, the live branch
//!   left where it was, the beside landing retired); a dirty checkout, or
//!   a process inside it, refuses `GIT_SWITCH_*` and lands beside.
//! - R1 and R2 (findings 2 and 5): a process inside the beside landing
//!   keeps it (`in-use`, tried again once it is gone); a standalone beside
//!   checkout with a configuration entry the live repository lacks is
//!   kept (`configuration`), never carried into the live configuration.
//! - A7: ignored files present: kept; a captured ignored file of another
//!   content at the same path lands as a file-level beside `<path>SUFFIX`
//!   and the beside tree is still retired (the coordinator's decision).
//! - A11: a beside landing kept after the advance is a duplicate: closure
//!   holds it `refused-pending-review` with `GIT_ADVANCE_BESIDE_KEPT`.
//! - A8 to A10 (the coordinator's refinement): sting's own untracked files
//!   apart from the capture stay and the checkout advances; an untracked
//!   path the capture tracks, or an untracked directory the capture also
//!   holds untracked (`.serena`), refuses `GIT_ADVANCE_UNTRACKED_COLLIDES`
//!   naming it, and lands beside.
//!
//! Fixture repositories are created and destroyed here; their Git runs with
//! no global configuration and no hooks (R-N98).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "neo";
const SUFFIX: &str = ".neo-20261016";
const FLAGS: [&str; 4] = ["--adopt", "--beside", SUFFIX, "--refs-in"];

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
            "-c",
            "init.defaultBranch=main",
            "-C",
        ])
        .arg(repo);
    command
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = git_command(repo).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn agent(args: &[OsString]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn os(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

/// The one receipt line's (outcome, reason).
fn receipt(stdout: &str) -> (String, String) {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("item=") && line.contains(" outcome="))
        .unwrap_or_else(|| panic!("no receipt in {stdout}"));
    let between = |from: &str, to: &str| {
        let start = line.find(from).unwrap() + from.len();
        let end = line[start..].find(to).map_or(line.len(), |end| start + end);
        line[start..end].to_owned()
    };
    (
        between(" outcome=", " reason="),
        between(" reason=", " source_bytes_read="),
    )
}

/// Whether the receipt reason holds the word `word` exactly.
fn word(reason: &str, word: &str) -> bool {
    reason
        .strip_prefix("Some(")
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(reason)
        .trim_matches('"')
        .split_whitespace()
        .any(|held| held == word)
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn neo(&self) -> PathBuf {
        self.root.join("neo/repo")
    }

    fn sting(&self) -> PathBuf {
        self.root.join("sting/repo")
    }

    fn beside(&self) -> PathBuf {
        self.root.join(format!("sting/repo{SUFFIX}"))
    }

    fn feat(&self) -> PathBuf {
        self.root.join("sting/repo.worktrees/feat")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Neo two commits ahead of sting's `cp -a` copy (made at the first),
    /// both with their own state; nothing captured yet.
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-advance-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let world = Self { _dir: dir, root };
        let neo = world.neo();
        fs::create_dir_all(&neo).unwrap();
        fs::create_dir_all(world.path("sting")).unwrap();
        git(&neo, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(neo.join("README"), b"v0\n").unwrap();
        fs::create_dir_all(neo.join("src")).unwrap();
        fs::write(neo.join("src/a.txt"), b"a0\n").unwrap();
        fs::create_dir_all(neo.join("docs/agent-notes")).unwrap();
        fs::write(neo.join("docs/agent-notes/n1.md"), b"note\n").unwrap();
        fs::write(neo.join(".gitignore"), b"*.log\nbuild/\n.env\n").unwrap();
        git(&neo, &["add", "."]);
        git(&neo, &["commit", "--quiet", "-m", "c0"]);
        git(&neo, &["tag", "v0"]);
        git(&neo, &["branch", "topic"]);
        fs::write(neo.join("src/a.txt"), b"a1\n").unwrap();
        git(&neo, &["commit", "--quiet", "-am", "c1"]);
        // Sting: a copy at c1, clean, then its own branch, stash, worktree
        // and ignored files.
        let sting = world.sting();
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&neo)
            .arg(&sting)
            .status()
            .unwrap();
        assert!(copied.success());
        git(&sting, &["branch", "sting-only", "v0"]);
        fs::write(sting.join("src/a.txt"), b"sting wip\n").unwrap();
        git(&sting, &["stash", "push", "--quiet", "-m", "sting-wip"]);
        fs::create_dir_all(world.path("sting/repo.worktrees")).unwrap();
        git(
            &sting,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feat",
                world.feat().to_str().unwrap(),
            ],
        );
        fs::create_dir_all(sting.join("build")).unwrap();
        fs::write(sting.join("build/out.bin"), b"sting build output\n").unwrap();
        fs::write(sting.join(".env"), b"STING_SECRET=1\n").unwrap();
        // Neo: two commits more, a stash, then its working-tree changes.
        fs::create_dir_all(neo.join("src")).unwrap();
        fs::write(neo.join("src/b.txt"), b"b2\n").unwrap();
        fs::write(neo.join("README"), b"v2\n").unwrap();
        git(&neo, &["add", "."]);
        git(&neo, &["commit", "--quiet", "-m", "c2"]);
        fs::write(neo.join("src/a.txt"), b"a3\n").unwrap();
        git(&neo, &["commit", "--quiet", "-am", "c3"]);
        fs::write(neo.join("src/a.txt"), b"neo wip\n").unwrap();
        git(&neo, &["stash", "push", "--quiet", "-m", "neo-wip"]);
        fs::write(neo.join("README"), b"v2, edited on neo\n").unwrap();
        fs::write(neo.join("src/b.txt"), b"b2, staged on neo\n").unwrap();
        git(&neo, &["add", "src/b.txt"]);
        fs::create_dir_all(neo.join("notes")).unwrap();
        fs::write(neo.join("notes/todo.md"), b"neo's untracked todo\n").unwrap();
        fs::write(neo.join("neo.log"), b"neo's ignored log\n").unwrap();
        world
    }

    fn plan(&self) -> PathBuf {
        self.path("carry/plan.postcard")
    }

    fn corpus(&self) -> PathBuf {
        self.path("carry/corpus")
    }

    fn state(&self) -> PathBuf {
        self.path("apply-state")
    }

    /// Plan the one item (neo's checkout into sting's live one) and capture.
    fn capture(&self) {
        if !self.plan().exists() {
            fs::create_dir_all(self.path("carry")).unwrap();
            let added = agent(&os(&[
                &"estate-add-batch",
                &self.plan(),
                &self.neo(),
                &self.sting(),
                &self.sting(),
            ]));
            assert!(added.ok, "{}", added.stderr);
            fs::DirBuilder::new()
                .mode(0o700)
                .create(self.state())
                .unwrap();
        }
        settle();
        let ran = agent(&os(&[
            &"estate-capture",
            &self.plan(),
            &self.path("carry/private"),
            &self.corpus(),
            &"1",
        ]));
        assert!(ran.ok, "capture: {}\n{}", ran.stdout, ran.stderr);
    }

    fn apply(&self, extra: &[&str]) -> Ran {
        let mut list: Vec<OsString> = vec!["estate-apply".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(os(&[
            &self.plan(),
            &self.corpus(),
            &self.state(),
            &SOURCE,
            &"1",
        ]));
        agent(&list)
    }

    fn advance(&self) -> Ran {
        let mut flags = FLAGS.to_vec();
        flags.push("--advance-ancestor");
        self.apply(&flags)
    }

    fn diff(&self, extra: &[&str]) -> Ran {
        let mut list: Vec<OsString> = vec!["estate-diff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(os(&[&self.plan(), &self.corpus(), &SOURCE, &self.state()]));
        let ran = agent(&list);
        assert!(ran.ok, "diff: {}\n{}", ran.stdout, ran.stderr);
        ran
    }

    fn closure(&self, dispositions: Option<&Path>) -> Ran {
        let mut list: Vec<OsString> = vec!["closure-report".into()];
        if let Some(ledger) = dispositions {
            list.push("--dispositions".into());
            list.push(ledger.into());
        }
        list.extend(os(&[&self.plan(), &self.corpus(), &SOURCE, &self.state()]));
        agent(&list)
    }
}

/// Every ref of `repo` but bulkload's provenance and the beside namespaces.
fn own_refs(repo: &Path) -> BTreeMap<String, String> {
    git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|line| !line.starts_with("refs/carry") && !line.starts_with("refs/neo-"))
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

/// A file's bytes, inode and mtime (an untouched file keeps all three).
type Identity = (Vec<u8>, u64, i64);

/// A file's bytes, inode and mtime (an untouched file keeps all three).
fn identity(path: &Path) -> Identity {
    let meta = fs::symlink_metadata(path).unwrap();
    (fs::read(path).unwrap(), meta.ino(), meta.mtime_nsec())
}

/// Neo's state, as the live checkout must hold it after an advance.
fn holds_neo(world: &World) {
    let (neo, sting) = (world.neo(), world.sting());
    assert_eq!(
        git(&sting, &["rev-parse", "HEAD"]),
        git(&neo, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&sting, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    for file in [
        "README",
        "src/a.txt",
        "src/b.txt",
        "notes/todo.md",
        "neo.log",
    ] {
        assert_eq!(
            fs::read(sting.join(file)).unwrap(),
            fs::read(neo.join(file)).unwrap(),
            "{file}"
        );
    }
    assert_eq!(
        git(&sting, &["diff", "--cached", "--name-only"]),
        git(&neo, &["diff", "--cached", "--name-only"])
    );
    assert_eq!(
        git(&sting, &["diff", "--name-only"]),
        git(&neo, &["diff", "--name-only"])
    );
}

/// Sting's own state the advance must leave: its stash, branches and
/// worktrees, its ignored files, byte for byte and inode for inode.
struct Own {
    stash: String,
    refs: BTreeMap<String, String>,
    feat: (String, Vec<u8>),
    ignored: Vec<(PathBuf, Identity)>,
}

impl Own {
    fn of(world: &World) -> Self {
        let sting = world.sting();
        let mut refs = own_refs(&sting);
        refs.remove("refs/heads/main");
        // A switch's: created at neo's HEAD (S1).
        refs.remove("refs/heads/feature");
        Self {
            stash: git(&sting, &["stash", "list", "--format=%H %gs"]),
            refs,
            feat: (
                git(&world.feat(), &["rev-parse", "HEAD"]),
                fs::read(world.feat().join("src/a.txt")).unwrap(),
            ),
            ignored: [sting.join("build/out.bin"), sting.join(".env")]
                .into_iter()
                .map(|path| {
                    let held = identity(&path);
                    (path, held)
                })
                .collect(),
        }
    }

    fn stays(&self, world: &World) {
        let now = Self::of(world);
        assert_eq!(now.stash, self.stash, "sting's stash");
        assert_eq!(now.refs, self.refs, "sting's branches and tags");
        assert_eq!(now.feat, self.feat, "sting's worktree");
        assert_eq!(now.ignored, self.ignored, "sting's ignored files");
        let listed = git(&world.sting(), &["worktree", "list", "--porcelain"]);
        assert!(
            listed.contains(&format!("worktree {}", world.feat().display())),
            "{listed}"
        );
    }
}

/// A1, the `GloriousFlywheel` shape: beside first, then advanced in place,
/// the beside landing retired; later passes converge in place.
#[test]
fn a1_a_clean_ancestor_is_advanced_and_its_beside_landing_retired() {
    let world = World::new();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    assert!(world.beside().join(".git").is_file(), "a linked landing");
    let own = Own::of(&world);
    let before = git(&world.sting(), &["rev-parse", "HEAD"]);
    // The plan's preview says what the apply will do.
    let diff = world.diff(&["--beside", SUFFIX, "--advance-ancestor"]);
    assert!(
        diff.stdout.contains("\"adopt\":\"advance\""),
        "{}",
        diff.stdout
    );
    assert!(
        world
            .diff(&["--beside", SUFFIX])
            .stdout
            .contains("\"adopt\":\"diverged\""),
        "without the flag"
    );

    // The same capture, with the flag: the beside journal does not end it.
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(!word(&reason, "converged"), "{reason}");
    for held in [
        "advanced=1",
        "advance-branch-moved=1",
        "advance-beside=retired",
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    holds_neo(&world);
    own.stays(&world);
    // The advance moved main from sting's tip (an ancestor) forward.
    assert_eq!(
        git(
            &world.sting(),
            &["merge-base", "--is-ancestor", before.trim(), "HEAD"]
        ),
        ""
    );
    // Neo's stash entry and branches went in under the namespace.
    let refs = git(&world.sting(), &["for-each-ref", "--format=%(refname)"]);
    assert!(refs.contains("refs/neo-20261016-stash/0"), "{refs}");
    assert!(refs.contains("refs/neo-20261016/heads/topic"), "{refs}");
    // The beside landing is gone, its administration with it.
    assert!(!world.beside().exists());
    let listed = git(&world.sting(), &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains(SUFFIX), "{listed}");
    assert!(
        !fs::read_dir(world.path("sting")).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bulkload-retire-")),
        "no retirement leftover"
    );
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains("\"applied\":1")
            && closure.stdout.contains(
                "\"advance\":{\"outcome\":\"advanced\",\"detail\":\"\",\"beside\":\"retired\""
            ),
        "{}",
        closure.stdout
    );
    // A rerun is a journal no-op.
    let ran = world.advance();
    assert_eq!(
        receipt(&ran.stdout).0,
        "previous-workspace-restoration-not-revalidated"
    );

    // Neo moves on: the next pass converges the live checkout in place, its
    // branch moved as the advance moved it, never detached.
    fs::write(world.neo().join("src/c.txt"), b"c4\n").unwrap();
    git(&world.neo(), &["add", "src/c.txt"]);
    git(&world.neo(), &["commit", "--quiet", "-m", "c4"]);
    world.capture();
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "converged"), "{reason}");
    assert_eq!(
        git(&world.sting(), &["rev-parse", "HEAD"]),
        git(&world.neo(), &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&world.sting(), &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(fs::read(world.sting().join("src/c.txt")).unwrap(), b"c4\n");
    own.stays(&world);
}

/// A2: a standalone beside checkout (a hand copy adopted by proof, #282)
/// is retired the same way: a guarded removal of the directory.
#[test]
fn a2_an_adopted_standalone_beside_is_retired_after_the_advance() {
    let world = World::new();
    world.capture();
    let copied = Command::new("cp")
        .arg("-a")
        .arg(world.neo())
        .arg(world.beside())
        .status()
        .unwrap();
    assert!(copied.success());
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-beside");
    assert!(word(&reason, "beside-adopted=1"), "{reason}");
    assert!(world.beside().join(".git").is_dir());
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "advance-beside=retired"), "{reason}");
    holds_neo(&world);
    assert!(!world.beside().exists());
}

/// A3, the glorious.build shape: a live checkout that committed its own
/// is no ancestor of the capture: refused typed, landed beside, the live
/// checkout untouched; closure holds it pending review until
/// closure-dispose; an unchanged rerun keeps the beside journal.
#[test]
fn a3_a_diverged_live_checkout_is_refused_typed_and_lands_beside() {
    let world = World::new();
    fs::write(world.sting().join("src/a.txt"), b"sting's own commit\n").unwrap();
    git(&world.sting(), &["commit", "--quiet", "-am", "sting's own"]);
    world.capture();
    let head = git(&world.sting(), &["rev-parse", "HEAD"]);
    let a = identity(&world.sting().join("src/a.txt"));
    let diff = world.diff(&["--beside", SUFFIX, "--advance-ancestor"]);
    assert!(
        diff.stdout.contains("\"adopt\":\"diverged\"")
            && diff
                .stdout
                .contains("advance-refused=GIT_ADVANCE_NOT_ANCESTOR"),
        "{}",
        diff.stdout
    );
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-beside", "{reason}");
    assert!(
        word(&reason, "advance-refused=GIT_ADVANCE_NOT_ANCESTOR"),
        "{reason}"
    );
    assert_eq!(git(&world.sting(), &["rev-parse", "HEAD"]), head);
    assert_eq!(identity(&world.sting().join("src/a.txt")), a);
    assert_eq!(
        git(&world.beside(), &["rev-parse", "HEAD"]),
        git(&world.neo(), &["rev-parse", "HEAD"])
    );
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains(
            "\"disposition\":\"refused-pending-review\",\"refusal\":\"GIT_ADVANCE_NOT_ANCESTOR\""
        ) && closure
            .stdout
            .contains("\"advance\":{\"outcome\":\"GIT_ADVANCE_NOT_ANCESTOR\""),
        "{}",
        closure.stdout
    );
    // Dispositionable: an operator review accepts the beside landing.
    let item = {
        let start = closure.stdout.find("\"item\":\"").unwrap() + 8;
        closure.stdout[start..start + 64].to_owned()
    };
    let reviews = world.path("reviews");
    let disposed = agent(&os(&[
        &"closure-dispose",
        &reviews,
        &world.plan(),
        &world.corpus(),
        &SOURCE,
        &item,
        &"GIT_ADVANCE_NOT_ANCESTOR",
        &"accept",
        &"jess",
        &"2026-10-19",
        &world.state(),
    ]));
    assert!(disposed.ok, "{}\n{}", disposed.stdout, disposed.stderr);
    // An unchanged rerun keeps the beside journal and the refusal's
    // instance, so the review still disposes it.
    let ran = world.advance();
    assert_eq!(
        receipt(&ran.stdout).0,
        "previous-workspace-beside-not-revalidated"
    );
    let closure = world.closure(Some(&reviews));
    assert!(
        closure.ok
            && closure
                .stdout
                .contains("\"disposition\":\"refused\",\"refusal\":\"GIT_ADVANCE_NOT_ANCESTOR\""),
        "{}\n{}",
        closure.stdout,
        closure.stderr
    );
    assert_eq!(git(&world.sting(), &["rev-parse", "HEAD"]), head);
}

/// A4 to A6: a dirty live checkout, its branch checked out by another
/// worktree, or a capture on another branch: each refused typed with
/// nothing written at the live checkout, and landed beside.
#[test]
fn a4_a6_each_precondition_refuses_typed_and_lands_beside() {
    for (shape, code) in [
        ("unstaged", "GIT_ADVANCE_NOT_CLEAN"),
        ("staged", "GIT_ADVANCE_NOT_CLEAN"),
        ("busy", "GIT_ADVANCE_BRANCH_BUSY"),
        ("detached", "GIT_ADVANCE_BRANCH_MISMATCH"),
    ] {
        let world = World::new();
        let sting = world.sting();
        match shape {
            "unstaged" => fs::write(sting.join("README"), b"sting's edit\n").unwrap(),
            "staged" => {
                fs::write(sting.join("README"), b"sting's staged edit\n").unwrap();
                git(&sting, &["add", "README"]);
            }
            "busy" => {
                git(
                    &sting,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "--force",
                        world.path("sting/also-main").to_str().unwrap(),
                        "main",
                    ],
                );
            }
            _ => {
                git(&world.neo(), &["switch", "--quiet", "--detach"]);
            }
        }
        world.capture();
        let head = git(&sting, &["rev-parse", "HEAD"]);
        let status = git(&sting, &["status", "--porcelain"]);
        let readme = identity(&sting.join("README"));
        let ran = world.advance();
        assert!(ran.ok, "{shape}: {}\n{}", ran.stdout, ran.stderr);
        let (outcome, reason) = receipt(&ran.stdout);
        assert_eq!(outcome, "workspace-beside", "{shape}: {reason}");
        assert!(
            word(&reason, &format!("advance-refused={code}")),
            "{shape}: {reason}"
        );
        assert_eq!(git(&sting, &["rev-parse", "HEAD"]), head, "{shape}");
        assert_eq!(git(&sting, &["status", "--porcelain"]), status, "{shape}");
        assert_eq!(identity(&sting.join("README")), readme, "{shape}");
        assert!(world.beside().join(".git").is_file(), "{shape}");
        let closure = world.closure(None);
        assert!(
            closure.stdout.contains(&format!(
                "\"disposition\":\"refused-pending-review\",\"refusal\":\"{code}\""
            )),
            "{shape}: {}",
            closure.stdout
        );
    }
}

/// A7 (the coordinator's decision on #289): ignored files present.
/// Sting's own are never touched. Where the capture holds its own ignored
/// file of another content at the path, sting's stays and the captured one
/// lands as a file-level beside, `<path>SUFFIX`, create-only (counted).
/// A captured ignored file under sting's ignored directory lands at its
/// own path, sting's files there untouched. The beside tree, everything
/// else proved equal, is then retired.
#[test]
fn a7_ignored_files_are_kept_and_a_conflict_lands_as_a_file_level_beside() {
    let world = World::new();
    fs::write(world.sting().join("neo.log"), b"sting's own log\n").unwrap();
    fs::create_dir_all(world.neo().join("build")).unwrap();
    fs::write(world.neo().join("build/neo.bin"), b"neo's build output\n").unwrap();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    let own = Own::of(&world);
    let log = identity(&world.sting().join("neo.log"));
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    for held in [
        "advanced=1",
        "advance-kept=1",
        "advance-file-besides=1",
        "advance-beside=retired",
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    assert_eq!(identity(&world.sting().join("neo.log")), log);
    assert_eq!(
        fs::read(world.sting().join(format!("neo.log{SUFFIX}"))).unwrap(),
        b"neo's ignored log\n"
    );
    assert_eq!(
        fs::read(world.sting().join("build/neo.bin")).unwrap(),
        b"neo's build output\n"
    );
    own.stays(&world);
    assert!(!world.beside().exists());
    assert_eq!(
        git(&world.sting(), &["rev-parse", "HEAD"]),
        git(&world.neo(), &["rev-parse", "HEAD"])
    );
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains("\"disposition\":\"applied\"")
            && closure.stdout.contains("\"file_besides\":1"),
        "{}",
        closure.stdout
    );
}

/// A11 (the coordinator's decision on #289): a beside landing kept after
/// the advance (here an edit in it since its landing) is a duplicate:
/// closure holds the item `refused-pending-review` with
/// `GIT_ADVANCE_BESIDE_KEPT` and the reason, and the beside stays whole.
#[test]
fn a11_a_beside_kept_after_the_advance_is_pending_review() {
    let world = World::new();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    fs::write(world.beside().join("README"), b"edited in the beside\n").unwrap();
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    for held in [
        "advanced=1",
        "advance-beside=kept",
        "advance-beside-reason=differs:seat=README",
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    holds_neo(&world);
    assert_eq!(
        fs::read(world.beside().join("README")).unwrap(),
        b"edited in the beside\n"
    );
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains(
            "\"disposition\":\"refused-pending-review\",\"refusal\":\"GIT_ADVANCE_BESIDE_KEPT\""
        ) && closure.stdout.contains(
            "\"refusal_detail\":\"GIT_ADVANCE_BESIDE_KEPT reason=\\\"differs:seat=README\\\"\""
        ),
        "{}",
        closure.stdout
    );
    // A rerun is a journal no-op, and the row stays pending review.
    let ran = world.advance();
    assert_eq!(
        receipt(&ran.stdout).0,
        "previous-workspace-restoration-not-revalidated"
    );
    assert!(world
        .closure(None)
        .stdout
        .contains("\"refusal\":\"GIT_ADVANCE_BESIDE_KEPT\""),);
}

/// A8 (the coordinator's refinement, the `MassageIthaca` shape): sting's
/// own untracked directory, apart from every captured path, stays byte for
/// byte; the checkout advances and the beside landing is still retired.
#[test]
fn a8_untracked_files_apart_from_the_capture_stay_and_the_checkout_advances() {
    let world = World::new();
    let review = world
        .sting()
        .join("docs/agent-notes/2026-08-28-convergence-review");
    fs::create_dir_all(&review).unwrap();
    fs::write(review.join("x.md"), b"sting-only review\n").unwrap();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    let held = identity(&review.join("x.md"));
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "advance-beside=retired"), "{reason}");
    holds_neo(&world);
    assert_eq!(identity(&review.join("x.md")), held);
    assert!(!world.beside().exists());
}

/// A9 and A10 (the coordinator's refinement): an untracked path the
/// capture tracks (the medical-massage-specialists-infra shape), or an
/// untracked directory the capture also holds untracked (the `.serena`
/// shape), collides: refused `GIT_ADVANCE_UNTRACKED_COLLIDES` naming it,
/// landed beside, the untracked files untouched.
#[test]
fn a9_a10_an_untracked_path_that_collides_with_the_capture_is_refused() {
    // Git lists a directory that holds nothing tracked as itself: sting's
    // untracked `tofu/` (all of it untracked there) is the entry that
    // collides with the capture's tracked modules under it.
    let modules = "tofu/stacks/application/modules";
    for (shape, collides) in [("tracked", "tofu"), ("serena", ".serena")] {
        let world = World::new();
        let (neo, sting) = (world.neo(), world.sting());
        let mine = if shape == "tracked" {
            fs::create_dir_all(neo.join(modules)).unwrap();
            fs::write(neo.join(modules).join("main.tf"), b"module {}\n").unwrap();
            git(&neo, &["add", modules]);
            git(&neo, &["commit", "--quiet", "-m", "c4: the modules"]);
            fs::create_dir_all(sting.join(modules)).unwrap();
            sting.join(modules).join("local.tf")
        } else {
            fs::create_dir_all(neo.join(".serena")).unwrap();
            fs::write(neo.join(".serena/neo.yml"), b"neo\n").unwrap();
            fs::create_dir_all(sting.join(".serena")).unwrap();
            sting.join(".serena/sting.yml")
        };
        fs::write(&mine, b"sting's own\n").unwrap();
        world.capture();
        let head = git(&sting, &["rev-parse", "HEAD"]);
        let untouched = identity(&mine);
        let ran = world.advance();
        assert!(ran.ok, "{shape}: {}\n{}", ran.stdout, ran.stderr);
        let (outcome, reason) = receipt(&ran.stdout);
        assert_eq!(outcome, "workspace-beside", "{shape}: {reason}");
        assert!(
            word(&reason, "advance-refused=GIT_ADVANCE_UNTRACKED_COLLIDES"),
            "{shape}: {reason}"
        );
        assert_eq!(git(&sting, &["rev-parse", "HEAD"]), head, "{shape}");
        assert_eq!(identity(&mine), untouched, "{shape}");
        let closure = world.closure(None);
        assert!(
            closure.stdout.contains(
                "\"disposition\":\"refused-pending-review\",\"refusal\":\"GIT_ADVANCE_UNTRACKED_COLLIDES\""
            ) && closure.stdout.contains(&format!(
                "\"refusal_detail\":\"GIT_ADVANCE_UNTRACKED_COLLIDES paths=\\\"{collides}\\\"\""
            )),
            "{shape}: {}",
            closure.stdout
        );
    }
}

/// `--advance-ancestor` needs `--adopt` and `--beside`.
#[test]
fn the_flag_needs_adopt_and_beside() {
    let world = World::new();
    world.capture();
    for flags in [
        &["--advance-ancestor"][..],
        &["--adopt", "--advance-ancestor"][..],
        &["--beside", SUFFIX, "--advance-ancestor"][..],
    ] {
        let ran = world.apply(flags);
        assert!(
            !ran.ok && ran.stderr.contains("REQUIRED_FIELD_MISSING"),
            "{flags:?}: {}",
            ran.stderr
        );
    }
}

/// A12 (review of #289, finding 1): a Git lock another command holds at
/// the live checkout is an operation in progress: refused typed, landed
/// beside, nothing moved, the lock as it was.
#[test]
fn a12_a_held_git_lock_refuses_the_advance_with_nothing_moved() {
    for lock in ["index.lock", "HEAD.lock"] {
        let world = World::new();
        let sting = world.sting();
        world.capture();
        fs::write(sting.join(".git").join(lock), b"another command's lock").unwrap();
        let head = git(&sting, &["rev-parse", "HEAD"]);
        let ran = world.advance();
        assert!(ran.ok, "{lock}: {}\n{}", ran.stdout, ran.stderr);
        let (outcome, reason) = receipt(&ran.stdout);
        assert_eq!(outcome, "workspace-beside", "{lock}: {reason}");
        assert!(
            word(&reason, "advance-refused=GIT_ADVANCE_NOT_CLEAN"),
            "{lock}: {reason}"
        );
        assert_eq!(git(&sting, &["rev-parse", "HEAD"]), head, "{lock}");
        assert_eq!(
            fs::read(sting.join(".git").join(lock)).unwrap(),
            b"another command's lock",
            "{lock}"
        );
    }
}

/// S1 (operator ruling 2026-10-19, "Switch when clean"; the
/// `MassageIthaca` shape): neo works on `feature`, sting's clean checkout
/// is on `main`. The advance switches it: `feature` created at neo's HEAD
/// with neo's working-tree changes, sting's `main` exactly where it was
/// (named in the receipt), its stash, branches, worktrees and ignored
/// files untouched, the beside landing retired.
#[test]
fn s1_a_clean_checkout_on_another_branch_is_switched() {
    let world = World::new();
    git(&world.neo(), &["switch", "--quiet", "-c", "feature"]);
    world.capture();
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    let own = Own::of(&world);
    let sting = world.sting();
    let main = git(&sting, &["rev-parse", "main"]);
    let diff = world.diff(&["--beside", SUFFIX, "--advance-ancestor"]);
    assert!(
        diff.stdout.contains("\"adopt\":\"advance-switch\""),
        "{}",
        diff.stdout
    );
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    for held in [
        "advanced=1",
        "advance-switched=1",
        "advance-beside=retired",
        &format!("advance-switched-from=refs/heads/main@{}", main.trim()),
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    assert_eq!(
        git(&sting, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature"
    );
    assert_eq!(
        git(&sting, &["rev-parse", "feature"]),
        git(&world.neo(), &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&sting, &["rev-parse", "main"]), main, "sting's main");
    for file in [
        "README",
        "src/a.txt",
        "src/b.txt",
        "notes/todo.md",
        "neo.log",
    ] {
        assert_eq!(
            fs::read(sting.join(file)).unwrap(),
            fs::read(world.neo().join(file)).unwrap(),
            "{file}"
        );
    }
    own.stays(&world);
    assert!(!world.beside().exists());
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains("\"applied\":1") && closure.stdout.contains("\"switched\":true"),
        "{}",
        closure.stdout
    );
    let ran = world.advance();
    assert_eq!(
        receipt(&ran.stdout).0,
        "previous-workspace-restoration-not-revalidated"
    );
}

/// S2: the switch's conditions refused typed, each landing beside with
/// nothing written at the live checkout: a dirty checkout
/// (`GIT_SWITCH_NOT_CLEAN`), a process whose working directory is inside
/// it (`GIT_SWITCH_IN_USE`, the ruling's condition c), the capture's
/// branch held by sting at a commit of its own
/// (`GIT_SWITCH_BRANCH_DIVERGED`). Closure holds each pending review.
#[test]
fn s2_each_switch_condition_refuses_typed_and_lands_beside() {
    for (shape, code) in [
        ("dirty", "GIT_SWITCH_NOT_CLEAN"),
        ("in-use", "GIT_SWITCH_IN_USE"),
        ("diverged", "GIT_SWITCH_BRANCH_DIVERGED"),
    ] {
        let world = World::new();
        let sting = world.sting();
        git(&world.neo(), &["switch", "--quiet", "-c", "feature"]);
        let mut child = None;
        match shape {
            "dirty" => fs::write(sting.join("README"), b"sting's edit\n").unwrap(),
            "in-use" => {
                child = Some(
                    Command::new("sleep")
                        .arg("120")
                        .current_dir(sting.join("src"))
                        .spawn()
                        .unwrap(),
                );
            }
            _ => {
                // A commit of sting's own (its stash's), no ancestor of
                // neo's HEAD.
                git(&sting, &["branch", "feature", "refs/stash"]);
            }
        }
        world.capture();
        let head = git(&sting, &["rev-parse", "HEAD"]);
        let status = git(&sting, &["status", "--porcelain"]);
        let ran = world.advance();
        if let Some(mut child) = child {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert!(ran.ok, "{shape}: {}\n{}", ran.stdout, ran.stderr);
        let (outcome, reason) = receipt(&ran.stdout);
        assert_eq!(outcome, "workspace-beside", "{shape}: {reason}");
        assert!(
            word(&reason, &format!("advance-refused={code}")),
            "{shape}: {reason}"
        );
        assert_eq!(git(&sting, &["rev-parse", "HEAD"]), head, "{shape}");
        assert_eq!(
            git(&sting, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/main",
            "{shape}"
        );
        assert_eq!(git(&sting, &["status", "--porcelain"]), status, "{shape}");
        let closure = world.closure(None);
        assert!(
            closure.stdout.contains(&format!(
                "\"disposition\":\"refused-pending-review\",\"refusal\":\"{code}\""
            )),
            "{shape}: {}",
            closure.stdout
        );
    }
}

/// R1 (review of #289, finding 2): a process whose working directory is
/// inside the beside landing keeps it (`in-use`: a duplicate pending
/// review), never removed under it; once the process is gone, a rerun
/// tries again and retires it.
#[test]
fn r1_a_beside_landing_in_use_is_kept_then_retired_once_free() {
    let world = World::new();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    let mut child = Command::new("sleep")
        .arg("120")
        .current_dir(world.beside().join("src"))
        .spawn()
        .unwrap();
    let ran = world.advance();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "advance-beside=pending"), "{reason}");
    assert!(word(&reason, "advance-beside-reason=in-use"), "{reason}");
    holds_neo(&world);
    assert!(world.beside().join(".git").is_file(), "kept whole");
    assert!(world
        .closure(None)
        .stdout
        .contains("\"refusal\":\"GIT_ADVANCE_BESIDE_KEPT\""));
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "advance-beside=retired"), "{reason}");
    assert!(!world.beside().exists());
    let closure = world.closure(None);
    assert!(
        closure.stdout.contains("\"applied\":1"),
        "{}",
        closure.stdout
    );
}

/// R2 (review of #289, finding 5): a standalone beside checkout whose
/// configuration holds an entry the live repository lacks (a branch
/// upstream, the softconnect shape) is kept (`configuration`), and the live
/// configuration is never touched.
#[test]
fn r2_a_standalone_beside_with_its_own_configuration_is_kept() {
    let world = World::new();
    world.capture();
    let copied = Command::new("cp")
        .arg("-a")
        .arg(world.neo())
        .arg(world.beside())
        .status()
        .unwrap();
    assert!(copied.success());
    git(&world.beside(), &["config", "branch.main.remote", "origin"]);
    git(
        &world.beside(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    let ran = world.apply(&FLAGS);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside", "{}", ran.stderr);
    let config = fs::read(world.sting().join(".git/config")).unwrap();
    let ran = world.advance();
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-restored", "{reason}");
    assert!(word(&reason, "advance-beside=kept"), "{reason}");
    assert!(word(&reason, "advance-beside-reason=configuration"), "{reason}");
    holds_neo(&world);
    assert!(world.beside().join(".git").is_dir());
    assert_eq!(
        fs::read(world.sting().join(".git/config")).unwrap(),
        config,
        "the live configuration"
    );
}
