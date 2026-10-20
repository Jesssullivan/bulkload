//! A clean source lands no tree beside (operator ruling 2026-10-16
//! 21:24Z: "refs in always; a tree beside ONLY where neo has uncommitted
//! work"; lane note `docs/agent-notes/2026-10-19-clean-source-no-beside.md`):
//! `estate-apply --adopt --beside SUFFIX --refs-in`, and
//! `estate-beside-retire` for the beside trees earlier passes landed.
//!
//! The fixture is the 2026-10-19 sting shape, scaled down: a `cp -a` copy
//! of neo's checkout at sting's live path, sting's own tracked edits and
//! untracked files in it, and neo with no tracked change, a branch and a
//! stash sting lacks, and untracked and ignored files of its own (absent,
//! equal or different at sting).
//!
//! - C1 (today's common case): a dirty live checkout and a clean source at
//!   the same HEAD: no beside tree; the refs in (the new branch and the
//!   stash entry under the namespace); neo's untracked and ignored files
//!   placed, adopted or landed as `<file>.neo-20261016`; sting's own state
//!   untouched; closure `applied` with every file counted; a rerun a
//!   journal no-op; a later capture reconciles again, adopting what the
//!   first pass placed.
//! - C2: a clean source whose HEAD differs from the live one (the live
//!   checkout committed on its own; or neo is ahead) still lands no tree.
//! - C3: a source with a tracked change (unstaged, or staged) lands
//!   beside as before.
//! - C4: a clean source detached at a commit no ref brings in lands
//!   beside (its HEAD is not obtainable by ref).
//! - C5: a file-level beside path held by something else refuses
//!   `GIT_BESIDE_FOREIGN` naming it, after the rest landed; no tree.
//! - C6: without `--refs-in` a clean source still lands beside (the refs
//!   would not go in).
//! - R1 to R4 (`estate-beside-retire`): an earlier pass's linked beside
//!   tree is reported retirable with its proof, writing nothing; with
//!   `--delete` its files are reconciled into the live checkout, it is
//!   ledgered and removed with its administration; an empty leftover
//!   directory is retirable and removed; a beside tree with a tracked
//!   change, an unreachable HEAD, or a standalone copy whose commits the
//!   live repository lacks, refuses typed with nothing written; a
//!   standalone copy whose commits the live repository holds is retired,
//!   its branch created under the namespace and its configuration
//!   ledgered.
//!
//! Fixture repositories are created and destroyed here; their Git runs
//! with no global configuration and no hooks (R-N98).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    // Receipt paths print escaped and quoted, as the receipts do.
    clippy::unnecessary_debug_formatting
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

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Neo at `c1`, copied to sting; then sting's own tracked edit and
    /// untracked file, and neo's branch, stash, untracked and ignored
    /// files. Neo holds no tracked change. Nothing captured yet.
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-clean-")
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
        fs::write(neo.join(".gitignore"), b"*.log\nbuild/\n").unwrap();
        git(&neo, &["add", "."]);
        git(&neo, &["commit", "--quiet", "-m", "c0"]);
        git(&neo, &["tag", "v0"]);
        fs::write(neo.join("src/a.txt"), b"a1\n").unwrap();
        git(&neo, &["commit", "--quiet", "-am", "c1"]);
        fs::write(neo.join("same.txt"), b"the same on both\n").unwrap();
        fs::create_dir_all(neo.join("build")).unwrap();
        let sting = world.sting();
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&neo)
            .arg(&sting)
            .status()
            .unwrap();
        assert!(copied.success());
        // Sting's own work: a tracked edit, an untracked file, its own
        // versions of two files neo also has.
        fs::write(sting.join("src/a.txt"), b"sting's own edit\n").unwrap();
        fs::write(sting.join("sting-own.txt"), b"sting's untracked\n").unwrap();
        fs::write(sting.join("differs.txt"), b"sting's differs\n").unwrap();
        fs::write(sting.join("build/out.bin"), b"sting build output\n").unwrap();
        // Neo: a branch and a stash sting lacks, then untracked and ignored
        // files; no tracked change.
        git(&neo, &["branch", "neo-only"]);
        fs::write(neo.join("README"), b"v1, stashed on neo\n").unwrap();
        git(&neo, &["stash", "push", "--quiet", "-m", "neo-wip"]);
        fs::create_dir_all(neo.join("notes")).unwrap();
        fs::write(neo.join("notes/todo.md"), b"neo's untracked todo\n").unwrap();
        fs::write(neo.join("differs.txt"), b"neo's differs\n").unwrap();
        fs::write(neo.join("neo.log"), b"neo's ignored log\n").unwrap();
        fs::write(neo.join("build/out.bin"), b"neo build output\n").unwrap();
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

    fn closure(&self) -> Ran {
        agent(&os(&[
            &"closure-report",
            &self.plan(),
            &self.corpus(),
            &SOURCE,
            &self.state(),
        ]))
    }
}

/// `estate-beside-retire EXTRA... SUFFIX SOURCE STATE BESIDE...`.
fn retire(extra: &[&str], state: &Path, besides: &[&Path]) -> Ran {
    let mut list: Vec<OsString> = vec!["estate-beside-retire".into()];
    list.extend(extra.iter().map(OsString::from));
    list.extend(os(&[&SUFFIX, &SOURCE, &state]));
    list.extend(besides.iter().map(|path| path.as_os_str().to_owned()));
    agent(&list)
}

/// Every ref of `repo` by name, provenance (`refs/carry`) left out.
fn named_refs(repo: &Path) -> BTreeMap<String, String> {
    git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|line| !line.starts_with("refs/carry"))
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

/// A file's bytes, inode and mtime (an untouched file keeps all three).
type Identity = (Vec<u8>, u64, i64);

fn identity(path: &Path) -> Identity {
    let meta = fs::symlink_metadata(path).unwrap();
    (fs::read(path).unwrap(), meta.ino(), meta.mtime_nsec())
}

/// Sting's own state, which no reconcile may touch.
struct Own {
    head: String,
    status: String,
    files: Vec<(PathBuf, Identity)>,
}

impl Own {
    fn of(world: &World) -> Self {
        let sting = world.sting();
        Self {
            head: git(&sting, &["rev-parse", "HEAD"]),
            status: git(
                &sting,
                &["status", "--porcelain=v1", "--untracked-files=no"],
            ),
            files: [
                "src/a.txt",
                "sting-own.txt",
                "differs.txt",
                "build/out.bin",
                "same.txt",
                "README",
                ".gitignore",
            ]
            .iter()
            .map(|file| {
                let path = sting.join(file);
                let held = identity(&path);
                (path, held)
            })
            .collect(),
        }
    }

    fn stays(&self, world: &World) {
        let now = Self::of(world);
        assert_eq!(now.head, self.head, "sting's HEAD");
        assert_eq!(now.status, self.status, "sting's tracked changes");
        assert_eq!(now.files, self.files, "sting's files");
    }
}

/// Neo's untracked and ignored files at sting, as C1's first pass leaves
/// them.
fn holds_neo_files(world: &World) {
    let sting = world.sting();
    assert_eq!(
        fs::read(sting.join("notes/todo.md")).unwrap(),
        b"neo's untracked todo\n"
    );
    assert_eq!(
        fs::read(sting.join("neo.log")).unwrap(),
        b"neo's ignored log\n"
    );
    assert_eq!(
        fs::read(sting.join(format!("differs.txt{SUFFIX}"))).unwrap(),
        b"neo's differs\n"
    );
    assert_eq!(
        fs::read(sting.join(format!("build/out.bin{SUFFIX}"))).unwrap(),
        b"neo build output\n"
    );
}

/// C1: the 2026-10-19 common case. No tree beside; the refs and the stash
/// in; each untracked or ignored file placed, adopted or landed beside as
/// a file; sting untouched; closure applied, every file counted.
#[test]
fn c1_a_clean_same_head_source_into_a_dirty_live_checkout_lands_no_tree() {
    let world = World::new();
    assert_eq!(
        git(&world.neo(), &["rev-parse", "HEAD"]),
        git(&world.sting(), &["rev-parse", "HEAD"])
    );
    world.capture();
    let own = Own::of(&world);
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-refs-in", "{reason}");
    for held in [
        "beside-tree=none",
        "files-tracked=3",
        "files-placed=2",
        "files-adopted=1",
        "files-beside=2",
        "dirs-placed=1",
        "dirs-adopted=1",
        "files-occupied=0",
        "refs-in-created=2",
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    assert!(!world.beside().exists(), "no tree beside");
    let listed = git(&world.sting(), &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains(SUFFIX), "{listed}");
    own.stays(&world);
    holds_neo_files(&world);
    let refs = named_refs(&world.sting());
    assert_eq!(
        refs.get("refs/neo-20261016/heads/neo-only"),
        Some(
            &git(&world.neo(), &["rev-parse", "neo-only"])
                .trim()
                .to_owned()
        )
    );
    assert_eq!(
        refs.get("refs/neo-20261016-stash/0"),
        Some(
            &git(&world.neo(), &["rev-parse", "refs/stash"])
                .trim()
                .to_owned()
        )
    );
    assert!(
        !refs.contains_key("refs/neo-20261016/heads/main"),
        "{refs:?}"
    );
    let closure = world.closure();
    for held in [
        "\"applied\":1",
        "\"unaccounted\":0",
        "\"outcome\":\"workspace-refs-in\",\"disposition\":\"applied\"",
        "\"files\":{\"tracked\":3,\"placed\":2,\"adopted\":1,\"beside\":2,\"beside_present\":0,\"dirs_placed\":1,\"dirs_adopted\":1,\"dirs_beside\":0,",
        "\"refs_in\":{\"ns\":\"neo-20261016\",\"created\":2,\"kept\":0}",
    ] {
        assert!(closure.stdout.contains(held), "{held}: {}", closure.stdout);
    }
    // A rerun is a journal no-op.
    let ran = world.apply(&FLAGS);
    assert_eq!(
        receipt(&ran.stdout).0,
        "previous-workspace-refs-in-not-revalidated"
    );
    // A later capture: neo adds a file; the pass places it and adopts what
    // the first one placed, writing nothing else.
    fs::write(world.neo().join("notes/later.md"), b"later\n").unwrap();
    world.capture();
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "workspace-refs-in", "{reason}");
    for held in [
        "files-placed=1",
        "files-adopted=3",
        "files-beside=0",
        "files-beside-present=2",
        "dirs-placed=0",
        "dirs-adopted=2",
    ] {
        assert!(word(&reason, held), "{held}: {reason}");
    }
    assert!(!world.beside().exists());
    own.stays(&world);
    holds_neo_files(&world);
    assert_eq!(
        fs::read(world.sting().join("notes/later.md")).unwrap(),
        b"later\n"
    );
    assert!(world.closure().stdout.contains("\"applied\":1"));
}

/// C2: a clean source whose HEAD differs from the live one: sting
/// committed on its own (neo's HEAD its parent), or neo is a commit ahead
/// (its HEAD comes in by its branch, under the namespace).
#[test]
fn c2_a_clean_source_whose_head_differs_still_lands_no_tree() {
    for case in ["sting-ahead", "neo-ahead"] {
        let world = World::new();
        if case == "sting-ahead" {
            git(&world.sting(), &["commit", "--quiet", "-am", "sting's own"]);
        } else {
            fs::write(world.neo().join("README"), b"v2 on neo\n").unwrap();
            git(&world.neo(), &["commit", "--quiet", "-am", "c2"]);
        }
        world.capture();
        let ran = world.apply(&FLAGS);
        assert!(ran.ok, "{case}: {}\n{}", ran.stdout, ran.stderr);
        let (outcome, reason) = receipt(&ran.stdout);
        assert_eq!(outcome, "workspace-refs-in", "{case}: {reason}");
        assert!(!world.beside().exists(), "{case}");
        holds_neo_files(&world);
        let refs = named_refs(&world.sting());
        if case == "neo-ahead" {
            assert_eq!(
                refs.get("refs/neo-20261016/heads/main"),
                Some(&git(&world.neo(), &["rev-parse", "HEAD"]).trim().to_owned()),
                "{refs:?}"
            );
            // The live checkout stays where it was.
            assert_ne!(
                git(&world.sting(), &["rev-parse", "HEAD"]),
                git(&world.neo(), &["rev-parse", "HEAD"])
            );
        }
        assert!(world.closure().stdout.contains("\"applied\":1"), "{case}");
    }
}

/// C3: neo's uncommitted tracked work is what a tree beside is for: an
/// unstaged or a staged change lands beside as before.
#[test]
fn c3_a_source_with_tracked_changes_lands_beside() {
    for case in ["unstaged", "staged"] {
        let world = World::new();
        fs::write(world.neo().join("README"), b"v1, edited on neo\n").unwrap();
        if case == "staged" {
            git(&world.neo(), &["add", "README"]);
        }
        world.capture();
        let ran = world.apply(&FLAGS);
        assert!(ran.ok, "{case}: {}\n{}", ran.stdout, ran.stderr);
        assert_eq!(receipt(&ran.stdout).0, "workspace-beside", "{case}");
        assert_eq!(
            fs::read(world.beside().join("README")).unwrap(),
            b"v1, edited on neo\n",
            "{case}"
        );
        assert!(!world.sting().join("notes/todo.md").exists(), "{case}");
    }
}

/// C4: a clean source detached at a commit no branch or tag holds: its
/// HEAD is not obtainable by ref, so it lands beside.
#[test]
fn c4_a_detached_head_no_ref_brings_in_lands_beside() {
    let world = World::new();
    git(&world.neo(), &["checkout", "--quiet", "--detach"]);
    git(
        &world.neo(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "detached on neo",
        ],
    );
    world.capture();
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    assert_eq!(
        git(&world.beside(), &["rev-parse", "HEAD"]),
        git(&world.neo(), &["rev-parse", "HEAD"])
    );
}

/// C5: a file-level beside path that holds something else refuses
/// `GIT_BESIDE_FOREIGN` naming it, after every other file landed; no tree
/// beside; the refs went in; closure holds it pending review.
#[test]
fn c5_an_occupied_file_beside_refuses_named_and_the_rest_lands() {
    let world = World::new();
    let taken = world.sting().join(format!("differs.txt{SUFFIX}"));
    fs::write(&taken, b"someone else's\n").unwrap();
    world.capture();
    let ran = world.apply(&FLAGS);
    let (outcome, reason) = receipt(&ran.stdout);
    assert_eq!(outcome, "refused", "{reason}");
    assert!(
        reason.contains("GIT_BESIDE_FOREIGN") && reason.contains(&taken.display().to_string()),
        "{reason}"
    );
    assert_eq!(fs::read(&taken).unwrap(), b"someone else's\n");
    assert!(!world.beside().exists());
    assert_eq!(
        fs::read(world.sting().join("notes/todo.md")).unwrap(),
        b"neo's untracked todo\n"
    );
    assert!(named_refs(&world.sting()).contains_key("refs/neo-20261016/heads/neo-only"));
    let closure = world.closure();
    assert!(
        closure.stdout.contains("\"refused_pending_review\":1")
            && closure.stdout.contains("\"unaccounted\":0")
            && closure.stdout.contains("GIT_BESIDE_FOREIGN"),
        "{}",
        closure.stdout
    );
}

/// C6: without `--refs-in` the refs would not go in, so a clean source
/// lands beside as before.
#[test]
fn c6_without_refs_in_a_clean_source_lands_beside() {
    let world = World::new();
    world.capture();
    let ran = world.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(receipt(&ran.stdout).0, "workspace-beside");
    assert!(world.beside().join(".git").is_file());
}

/// Every entry under `root` with its bytes or link target, `.git` included.
fn tree(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>, i64)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let bytes = if meta.file_type().is_symlink() {
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_file() {
                fs::read(&path).unwrap()
            } else {
                pending.push(path.clone());
                Vec::new()
            };
            out.insert(path, (meta.mode(), bytes, meta.mtime_nsec()));
        }
    }
    out
}

fn line_of<'a>(stdout: &'a str, beside: &Path) -> &'a str {
    let needle = format!("beside={beside:?}");
    stdout
        .lines()
        .find(|line| line.contains(&needle))
        .unwrap_or_else(|| panic!("no line for {needle} in {stdout}"))
}

/// R1: a linked beside tree an earlier pass landed for a clean source (as
/// on sting before the rule) is reported retirable with its proof, and
/// nothing is written; `--delete` reconciles its files into the live
/// checkout, ledgers it and removes it with its administration. An empty
/// leftover directory beside another checkout is retirable and removed.
#[test]
fn r1_a_redundant_beside_tree_is_reported_then_retired() {
    let world = World::new();
    world.capture();
    let ran = world.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(world.beside().join(".git").is_file());
    // The refs a beside landing creates under the namespace bring its HEAD
    // in; sting's live tree has none of neo's untracked files yet.
    let empty_live = world.path("sting/other");
    fs::create_dir_all(&empty_live).unwrap();
    let empty = world.path(&format!("sting/other{SUFFIX}"));
    fs::create_dir(&empty).unwrap();
    let state = world.path("retire-state");
    let frozen = (
        tree(&world.sting()),
        tree(&world.beside()),
        named_refs(&world.sting()),
    );
    let report = retire(
        &["--scan", world.path("sting").to_str().unwrap()],
        &state,
        &[],
    );
    assert!(report.ok, "{}\n{}", report.stdout, report.stderr);
    let line = line_of(&report.stdout, &world.beside());
    for held in [
        "verdict=retirable",
        "shape=linked",
        "tracked=3",
        "to-place=2",
        "to-beside=2",
        "present=1",
    ] {
        assert!(line.split_whitespace().any(|w| w == held), "{held}: {line}");
    }
    assert!(line_of(&report.stdout, &empty).contains("verdict=retirable shape=empty"));
    assert!(
        report
            .stdout
            .contains("estate-beside-retire summary mode=report candidates=2 retirable=2"),
        "{}",
        report.stdout
    );
    assert_eq!(
        (
            tree(&world.sting()),
            tree(&world.beside()),
            named_refs(&world.sting())
        ),
        frozen,
        "report mode wrote"
    );
    assert!(!state.exists(), "report mode made STATE");

    let own = Own::of(&world);
    let done = retire(
        &["--delete"],
        &state,
        &[world.beside().as_path(), empty.as_path()],
    );
    assert!(done.ok, "{}\n{}", done.stdout, done.stderr);
    assert!(
        line_of(&done.stdout, &world.beside()).contains("outcome=retired"),
        "{}",
        done.stdout
    );
    assert!(line_of(&done.stdout, &empty).contains("outcome=retired"));
    assert!(!world.beside().exists());
    assert!(!empty.exists());
    assert!(empty_live.exists());
    let listed = git(&world.sting(), &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains(SUFFIX), "{listed}");
    own.stays(&world);
    holds_neo_files(&world);
    let ledgers: Vec<PathBuf> = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "retired"))
        .collect();
    assert_eq!(ledgers.len(), 2, "{ledgers:?}");
    assert!(
        !fs::read_dir(world.path("sting")).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bulkload-retire-"))
    );
    // Nothing left to retire.
    let again = retire(
        &["--scan", world.path("sting").to_str().unwrap()],
        &state,
        &[],
    );
    assert!(again.stdout.contains("candidates=0"), "{}", again.stdout);
}

/// R2: a beside tree that is not a subset refuses typed, nothing written:
/// a tracked change in it, or a commit made in it no live ref holds.
#[test]
fn r2_a_beside_tree_with_its_own_work_refuses_typed() {
    for case in ["tracked", "head"] {
        let world = World::new();
        world.capture();
        let ran = world.apply(&["--adopt", "--beside", SUFFIX]);
        assert!(ran.ok, "{case}: {}\n{}", ran.stdout, ran.stderr);
        let beside = world.beside();
        let reason = if case == "tracked" {
            fs::write(beside.join("README"), b"edited in the beside tree\n").unwrap();
            "reason=tracked-changes"
        } else {
            git(
                &beside,
                &["commit", "--quiet", "--allow-empty", "-m", "beside's own"],
            );
            "reason=unreachable:head="
        };
        let state = world.path("retire-state");
        let frozen = tree(&beside);
        let done = retire(&["--delete"], &state, &[beside.as_path()]);
        assert!(done.ok, "{case}: {}\n{}", done.stdout, done.stderr);
        let line = line_of(&done.stdout, &beside);
        assert!(
            line.contains("verdict=refused") && line.contains(reason),
            "{case}: {line}"
        );
        assert_eq!(tree(&beside), frozen, "{case}: the beside tree moved");
        assert!(!world.sting().join("notes/todo.md").exists(), "{case}");
    }
}

/// R3: a standalone copy of neo's checkout (as the in-place lane's hand
/// plans landed) whose HEAD the live repository lacks refuses
/// `unreachable:head`; once a clean pass brought neo's commits in by ref,
/// it is retired, its own branch created under the namespace where the
/// live repository lacks it by name, and its configuration ledgered.
#[test]
fn r3_a_standalone_copy_is_retired_only_once_its_commits_are_in() {
    let world = World::new();
    fs::write(world.neo().join("README"), b"v2 on neo\n").unwrap();
    git(&world.neo(), &["commit", "--quiet", "-am", "c2"]);
    let copied = Command::new("cp")
        .arg("-a")
        .arg(world.neo())
        .arg(world.beside())
        .status()
        .unwrap();
    assert!(copied.success());
    git(&world.beside(), &["branch", "copy-only"]);
    let state = world.path("retire-state");
    let report = retire(&[], &state, &[world.beside().as_path()]);
    assert!(report.ok, "{}\n{}", report.stdout, report.stderr);
    let line = line_of(&report.stdout, &world.beside()).to_owned();
    assert!(
        line.contains("verdict=refused") && line.contains("reason=unreachable:head="),
        "{line}"
    );
    world.capture();
    let ran = world.apply(&FLAGS);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(receipt(&ran.stdout).0, "workspace-refs-in");
    let report = retire(&[], &state, &[world.beside().as_path()]);
    let line = line_of(&report.stdout, &world.beside()).to_owned();
    for held in [
        "verdict=retirable",
        "shape=standalone",
        "to-place=0",
        "to-beside=0",
        "refs-to-import=1",
    ] {
        assert!(line.split_whitespace().any(|w| w == held), "{held}: {line}");
    }
    let done = retire(&["--delete"], &state, &[world.beside().as_path()]);
    assert!(done.ok, "{}\n{}", done.stdout, done.stderr);
    let line = line_of(&done.stdout, &world.beside());
    assert!(line.contains("outcome=retired"), "{line}");
    assert!(!world.beside().exists());
    let refs = named_refs(&world.sting());
    let head = git(&world.neo(), &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(
        refs.get("refs/neo-20261016/heads/copy-only"),
        Some(&head),
        "{refs:?}"
    );
    assert_eq!(refs.get("refs/neo-20261016/heads/main"), Some(&head));
}
