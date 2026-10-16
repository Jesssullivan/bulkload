//! Estate converge (2026-10-08; `docs/agent-notes/2026-10-08-estate-converge-design.md`):
//! native ref restore (#215) and re-apply convergence, driven through the
//! estate verbs the way `sting-apply-2.sh` drives the CLI.
//!
//! - P80 NATIVE-REFS: a first landing restores every native ref, the stash
//!   (order and messages) and each worktree's HEAD as at the source; no
//!   `carry/*` branch.
//! - P81 CONVERGE-EQUIV: converging a landing to a changed capture equals a
//!   fresh restore of that capture (refs deleted at the source and kept by
//!   R7 excepted, and reported).
//! - P82 CONVERGE-NO-CLOBBER: a destination changed since landing refuses
//!   `GIT_DESTINATION_MODIFIED` and is left as the operator left it.
//! - P83 CONVERGE-IDEMPOTENT: a second apply is a no-op.
//! - P85 STASH-ORDER: restore and converge reproduce the stash order and
//!   messages.
//!
//! Fixture repositories are created and destroyed here; their Git runs with
//! no global configuration and no hooks (R-N98: only the product's own tests
//! disable hooks, in fixture repositories they own).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use bulkload_agent::estate::{self, Item, Receipt};

const SOURCE: &str = "neo";

fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-10-08T00:00:00Z")
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
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

// Seats must be older than one timestamp tick to be reused (R-N76), and a
// recapture must be observed after the landing it converges.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

/// One estate: a source main repository with a linked worktree on a branch
/// and a detached one, planned as three items into one destination.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// What one apply reported, per item: (source, outcome, reason).
type Rows = Vec<(PathBuf, &'static str, Option<String>)>;

impl World {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-converge-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let world = Self { _dir: dir, root };
        let src = world.src();
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(src.join("a.txt"), b"alpha\n").unwrap();
        fs::create_dir_all(src.join("dir")).unwrap();
        fs::write(src.join("dir/b.txt"), b"beta\n").unwrap();
        fs::write(src.join("exec.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(src.join("exec.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
        fs::write(src.join("kind.txt"), b"a file, later a link\n").unwrap();
        fs::create_dir_all(src.join("sub/deep")).unwrap();
        fs::write(src.join("sub/deep/c.txt"), b"gamma\n").unwrap();
        git(&src, &["add", "."]);
        git(&src, &["commit", "--quiet", "-m", "c1"]);
        git(&src, &["tag", "v1"]);
        git(&src, &["tag", "-a", "v2", "-m", "annotated"]);
        git(&src, &["branch", "other"]);
        git(&src, &["branch", "feat"]);
        git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&src, &["update-ref", "refs/remotes/origin/old", "HEAD"]);
        git(
            &src,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        git(&src, &["notes", "add", "-m", "a note", "HEAD"]);
        fs::write(src.join("a.txt"), b"alpha stashed\n").unwrap();
        git(&src, &["stash", "push", "--quiet", "-m", "first"]);
        fs::write(src.join("dir/b.txt"), b"beta stashed\n").unwrap();
        git(&src, &["stash", "push", "--quiet", "-m", "second"]);
        fs::write(src.join("notes.txt"), b"untracked\n").unwrap();
        fs::write(src.join("staged.txt"), b"staged only\n").unwrap();
        git(&src, &["add", "staged.txt"]);
        git(
            &src,
            &[
                "worktree",
                "add",
                "--quiet",
                world.src_feat().to_str().unwrap(),
                "feat",
            ],
        );
        git(
            &src,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                world.src_det().to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(world.src_feat().join("feat.txt"), b"feature work\n").unwrap();
        fs::create_dir_all(world.root.join("dst")).unwrap();
        world
    }

    fn src(&self) -> PathBuf {
        self.root.join("src")
    }
    fn src_feat(&self) -> PathBuf {
        self.root.join("src-feat")
    }
    fn src_det(&self) -> PathBuf {
        self.root.join("src-det")
    }
    fn dst(&self, set: &str) -> PathBuf {
        self.root.join(set).join("main")
    }
    fn dst_feat(&self, set: &str) -> PathBuf {
        self.root.join(set).join("feat")
    }
    fn dst_det(&self, set: &str) -> PathBuf {
        self.root.join(set).join("det")
    }

    /// A plan of the main only, or of the main and both linked worktrees,
    /// landing under `set`.
    fn plan(&self, set: &str, linked: bool) -> PathBuf {
        let plan = self.root.join(format!("plan-{set}-{linked}"));
        if plan.exists() {
            return plan;
        }
        fs::create_dir_all(self.root.join(set)).unwrap();
        let mut items = vec![Item {
            source: self.src(),
            repository: self.dst(set),
            workspace: Some(self.dst(set)),
        }];
        if linked {
            // Linked first in plan order: the apply orders mains first.
            items.insert(
                0,
                Item {
                    source: self.src_feat(),
                    repository: self.dst(set),
                    workspace: Some(self.dst_feat(set)),
                },
            );
            items.push(Item {
                source: self.src_det(),
                repository: self.dst(set),
                workspace: Some(self.dst_det(set)),
            });
        }
        estate::add_batch(&plan, &items).unwrap();
        plan
    }

    fn capture(&self, plan: &Path, corpus: &str) -> Rows {
        let rows = Mutex::new(Vec::new());
        let record = |row: &Receipt| {
            rows.lock()
                .unwrap()
                .push((row.source.clone(), row.outcome, row.reason.clone()));
            Ok(())
        };
        estate::capture(
            plan,
            &self.root.join(format!("{corpus}-cstate")),
            &self.root.join(corpus),
            1,
            &record,
        )
        .unwrap();
        rows.into_inner().unwrap()
    }

    fn apply(&self, plan: &Path, corpus: &str, state: &str) -> (bool, Rows) {
        let rows = Mutex::new(Vec::new());
        let record = |row: &Receipt| {
            rows.lock()
                .unwrap()
                .push((row.source.clone(), row.outcome, row.reason.clone()));
            Ok(())
        };
        let result = estate::apply(
            plan,
            &self.root.join(corpus),
            &self.root.join(state),
            SOURCE,
            2,
            &record,
        );
        (result.is_ok(), rows.into_inner().unwrap())
    }

    fn unaccounted(&self, plan: &Path, corpus: &str, state: &str) -> u64 {
        let ledger = estate::ledger(
            plan,
            &self.root.join(corpus),
            SOURCE,
            &[self.root.join(state)],
        )
        .unwrap();
        bulkload_agent::closure::Report::from_ledger(&ledger).unaccounted
    }
}

/// Native refs: name to (oid, symref target), bulkload's namespaces left out.
fn refs(repo: &Path) -> BTreeMap<String, (String, String)> {
    git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname) %(symref)",
        ],
    )
    .lines()
    .filter_map(|line| {
        let mut fields = line.splitn(3, ' ');
        let (value, name) = (fields.next()?, fields.next()?);
        let target = fields.next().unwrap_or_default();
        (!name.starts_with("refs/carry"))
            .then(|| (name.to_owned(), (value.to_owned(), target.to_owned())))
    })
    .collect()
}

/// The refs a native restore reproduces: no stash, no remote HEAD symref.
fn restorable(repo: &Path) -> BTreeMap<String, String> {
    refs(repo)
        .into_iter()
        .filter(|(name, (_, target))| name != "refs/stash" && target.is_empty())
        .map(|(name, (value, _))| (name, value))
        .collect()
}

fn stash(repo: &Path) -> Vec<String> {
    let present = git_command(repo)
        .args(["rev-parse", "--verify", "--quiet", "refs/stash"])
        .output()
        .unwrap();
    if !present.status.success() {
        return Vec::new();
    }
    git(repo, &["reflog", "show", "--format=%H %gs", "refs/stash"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// `git stash list` as an operator reads it: `stash@{n}: <reflog message>`.
fn stash_list(repo: &Path) -> String {
    git(repo, &["stash", "list"])
}

/// Store an edit of `file` as a stash entry whose reflog message is
/// `message`, not its commit's subject (`git stash store -m`), and leave
/// the worktree and the index as they were. Returns the stash commit.
fn store_stash(repo: &Path, file: &str, content: &[u8], message: &str) -> String {
    let before = fs::read(repo.join(file)).unwrap();
    fs::write(repo.join(file), content).unwrap();
    let commit = git(repo, &["stash", "create", "a subject of its own"])
        .trim()
        .to_owned();
    fs::write(repo.join(file), before).unwrap();
    git(repo, &["stash", "store", "-m", message, &commit]);
    commit
}

fn head(worktree: &Path) -> (String, String) {
    let symbolic = git_command(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    (
        String::from_utf8(symbolic.stdout)
            .unwrap()
            .trim()
            .to_owned(),
        git(worktree, &["rev-parse", "HEAD"]).trim().to_owned(),
    )
}

fn index(worktree: &Path) -> String {
    git(worktree, &["ls-files", "--stage"])
}

/// Every seat under `root` but `.git`: kind, mode and bytes or link target.
fn files(root: &Path) -> BTreeMap<PathBuf, (char, u32, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (char, u32, Vec<u8>)>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(
                    relative,
                    ('l', 0, target.into_os_string().into_encoded_bytes()),
                );
            } else if meta.is_dir() {
                out.insert(relative, ('d', meta.mode() & 0o777, Vec::new()));
                walk(root, &path, out);
            } else {
                out.insert(
                    relative,
                    ('f', meta.mode() & 0o777, fs::read(&path).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Every seat of a checkout, its index listing and its HEAD.
type View = (
    BTreeMap<PathBuf, (char, u32, Vec<u8>)>,
    String,
    (String, String),
);

/// Everything a checkout shows an operator.
fn view(worktree: &Path) -> View {
    (files(worktree), index(worktree), head(worktree))
}

fn outcome_of<'a>(rows: &'a Rows, source: &Path) -> &'a (PathBuf, &'static str, Option<String>) {
    rows.iter()
        .find(|row| row.0 == source)
        .unwrap_or_else(|| panic!("no receipt for {}", source.display()))
}

#[test]
fn p80_a_first_landing_restores_native_refs_stash_and_heads() {
    let world = World::new();
    let plan = world.plan("dst", true);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert!(
        rows.iter().all(|row| row.1 == "workspace-restored"),
        "{rows:?}"
    );
    let dst = world.dst("dst");
    assert_eq!(restorable(&dst), restorable(&world.src()));
    assert!(
        !refs(&dst)
            .keys()
            .any(|name| name.starts_with("refs/heads/carry")),
        "no carry/* branch"
    );
    // P85: the stash, oldest first in the reflog as at the source, with its
    // messages.
    assert_eq!(stash(&dst), stash(&world.src()));
    assert_eq!(stash(&dst).len(), 2);
    assert_eq!(view(&dst), view(&world.src()));
    assert_eq!(view(&world.dst_feat("dst")), view(&world.src_feat()));
    assert_eq!(view(&world.dst_det("dst")), view(&world.src_det()));
    assert_eq!(head(&world.dst_feat("dst")).0, "refs/heads/feat");
    assert_eq!(head(&world.dst_det("dst")).0, "");
    // The ledgers sit next to what they describe.
    assert!(dst.join(".git/bulkload/v1/neo/refs.landed").is_file());
    assert!(dst.join(".git/bulkload/v1/neo/workspace.landed").is_file());
    assert!(dst
        .join(".git/worktrees/feat/bulkload/v1/neo/workspace.landed")
        .is_file());
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

// P85 under OI-1003-Q169 (stash-order-v2): each stash entry's reflog
// message is part of its identity, so `git stash list` on the destination
// is the source's exactly, messages and order, after the first landing and
// after converging pushes, a pop, a drop below the tip and an entry stored
// under a message of its own; a message-only change at the source (the
// same commits in the same order) is carried too. Before Q169 the messages
// were the commits' subjects and a message-only change moved no key.
#[test]
fn p85_the_stash_list_is_the_sources_exactly_messages_and_order() {
    let world = World::new();
    let src = world.src();
    store_stash(
        &src,
        "a.txt",
        b"alpha, stored by hand\n",
        "a message that is not the subject",
    );
    assert_eq!(stash_list(&src).lines().count(), 3);
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let dst = world.dst("dst");
    assert_eq!(stash_list(&dst), stash_list(&src));
    assert_eq!(stash(&dst), stash(&src));

    // A day of stashing: two pushes, a pop, a drop below the tip, and an
    // entry stored under its own message.
    settle();
    fs::write(src.join("a.txt"), b"alpha, third\n").unwrap();
    git(&src, &["stash", "push", "--quiet", "-m", "third"]);
    fs::write(src.join("dir/b.txt"), b"beta, fourth\n").unwrap();
    git(&src, &["stash", "push", "--quiet", "-m", "fourth"]);
    git(&src, &["stash", "pop", "--quiet"]);
    git(&src, &["stash", "drop", "--quiet", "stash@{2}"]);
    store_stash(
        &src,
        "exec.sh",
        b"#!/bin/sh\necho fifth\n",
        "fifth,   stored\tby hand  ",
    );
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!(stash_list(&dst), stash_list(&src));
    assert_eq!(stash(&dst), stash(&src));
    assert_eq!(stash_list(&src).lines().count(), 4);

    // A message-only change: the tip dropped and stored again under a new
    // message. The commits and their order are unchanged.
    settle();
    let commits = |repo: &Path| -> Vec<String> {
        stash(repo)
            .iter()
            .map(|line| line.split(' ').next().unwrap().to_owned())
            .collect()
    };
    let before = commits(&src);
    let tip = git(&src, &["rev-parse", "refs/stash"]).trim().to_owned();
    git(&src, &["stash", "drop", "--quiet"]);
    git(&src, &["stash", "store", "-m", "fifth, renamed", &tip]);
    assert_eq!(commits(&src), before);
    assert_ne!(stash_list(&dst), stash_list(&src));
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap_or_default();
    assert!(reason.contains("stash-rewritten"), "{rows:?}");
    assert_eq!(stash_list(&dst), stash_list(&src));
    assert_eq!(stash(&dst), stash(&src));
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

// OI-1003-Q169 review finding 5: a source reflog message Git would fold
// (written by a libgit2 tool, which folds no tabs or runs of spaces, or a
// hand-edited `logs/refs/stash`) is stored folded at the destination,
// since Git folds every message it writes. The carried entry is folded
// before it becomes the op's target, so the op completes, the receipt says
// so (`stash-message-folded`), and a later push at the source still lands.
// Before the fix the store never read back equal to the unfolded target,
// the row named a reflog the destination never held, and every later pass
// kept the destination's stash (T6): the push never landed.
#[test]
fn p85_a_source_message_git_would_fold_lands_folded_and_later_pushes_land() {
    let world = World::new();
    let src = world.src();
    let log = src.join(".git/logs/refs/stash");
    let text = fs::read_to_string(&log).unwrap();
    let edited = text.replace("\tOn main: second\n", "\tOn main: libgit2  style\ttab\n");
    assert_ne!(edited, text);
    fs::write(&log, edited).unwrap();
    assert!(stash_list(&src).contains("libgit2  style\ttab"));
    let folded = |list: String| list.replace("libgit2  style\ttab", "libgit2 style tab");
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let dst = world.dst("dst");
    let reason = outcome_of(&rows, &src).2.clone().unwrap_or_default();
    assert!(reason.contains("stash-message-folded=1"), "{rows:?}");
    assert!(
        !reason.contains("stash-modified-at-destination"),
        "{rows:?}"
    );
    assert_eq!(stash_list(&dst), folded(stash_list(&src)));

    settle();
    fs::write(src.join("a.txt"), b"alpha, third\n").unwrap();
    git(&src, &["stash", "push", "--quiet", "-m", "third"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap_or_default();
    assert!(
        !reason.contains("stash-modified-at-destination"),
        "{rows:?}"
    );
    assert_eq!(stash_list(&dst), folded(stash_list(&src)));
    assert_eq!(stash_list(&dst).lines().count(), 3);
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

// The source moves the way a working day moves it.
fn work_a_day(world: &World) {
    let src = world.src();
    fs::write(src.join("a.txt"), b"alpha, second day\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    git(&src, &["branch", "topic"]);
    git(&src, &["branch", "-D", "other"]);
    git(&src, &["tag", "v3"]);
    git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&src, &["update-ref", "-d", "refs/remotes/origin/old"]);
    fs::write(src.join("exec.sh"), b"#!/bin/sh\necho stash\n").unwrap();
    git(&src, &["stash", "push", "--quiet", "-m", "third"]);
    git(&src, &["stash", "drop", "--quiet", "stash@{2}"]);
    fs::write(src.join("notes.txt"), b"untracked, edited\n").unwrap();
    fs::remove_file(src.join("dir/b.txt")).unwrap();
    fs::set_permissions(src.join("exec.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::remove_file(src.join("link")).unwrap();
    std::os::unix::fs::symlink("dir", src.join("link")).unwrap();
    fs::remove_file(src.join("kind.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", src.join("kind.txt")).unwrap();
    fs::remove_dir_all(src.join("sub/deep")).unwrap();
    fs::write(src.join("sub/deep"), b"was a directory\n").unwrap();
    fs::create_dir_all(src.join("newdir/inner")).unwrap();
    fs::write(src.join("newdir/inner/x"), b"new\n").unwrap();
    fs::write(src.join("staged.txt"), b"staged, changed\n").unwrap();
    git(&src, &["add", "staged.txt"]);
    // A commit on the linked worktree's branch: the main converges first and
    // must leave `feat` to its own worktree's item (finding 1).
    let feat = world.src_feat();
    fs::write(feat.join("feat.txt"), b"feature, day two\n").unwrap();
    git(&feat, &["add", "feat.txt"]);
    git(&feat, &["commit", "--quiet", "-m", "feat work"]);
    // The detached worktree switches to a branch.
    git(&world.src_det(), &["switch", "--quiet", "topic"]);
}

#[test]
fn p81_p83_a_landing_converges_to_a_new_capture_and_then_stands_still() {
    let world = World::new();
    let plan = world.plan("dst", true);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    settle();
    work_a_day(&world);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    for row in &rows {
        assert_eq!(row.1, "workspace-restored", "{rows:?}");
        assert!(
            row.2
                .as_deref()
                .is_some_and(|reason| reason.starts_with("converged")),
            "{rows:?}"
        );
    }
    let main = outcome_of(&rows, &world.src()).2.clone().unwrap();
    assert!(main.contains("ref-checked-out-elsewhere"), "{main}");
    assert!(main.contains("ref-deleted-at-source=1"), "{main}");
    assert!(main.contains("ref-pruned=1"), "{main}");
    assert!(main.contains("stash-rewritten"), "{main}");
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);

    // A fresh restore of the same sources, now, into an empty destination.
    let fresh = world.plan("fresh", true);
    world.capture(&fresh, "corpus-fresh");
    let (ok, rows) = world.apply(&fresh, "corpus-fresh", "applied-fresh");
    assert!(ok, "{rows:?}");
    let (dst, again) = (world.dst("dst"), world.dst("fresh"));
    for (converged, restored) in [
        (dst.clone(), again.clone()),
        (world.dst_feat("dst"), world.dst_feat("fresh")),
        (world.dst_det("dst"), world.dst_det("fresh")),
    ] {
        assert_eq!(view(&converged), view(&restored), "{}", converged.display());
    }
    assert_eq!(view(&dst), view(&world.src()));
    assert_eq!(stash(&dst), stash(&again));
    assert_eq!(stash(&dst), stash(&world.src()));
    // Every ref equal, but the local branch the source deleted: R7 keeps it
    // (and reports it); its value is at provenance either way.
    let mut converged = restorable(&dst);
    assert!(converged.remove("refs/heads/other").is_some());
    assert_eq!(converged, restorable(&again));
    assert!(!converged.contains_key("refs/remotes/origin/old"));
    assert_eq!(head(&world.dst_det("dst")).0, "refs/heads/topic");

    // P83: an unchanged source applies as a no-op.
    let before: Vec<_> = [&dst, &world.dst_feat("dst"), &world.dst_det("dst")]
        .iter()
        .map(|path| {
            (
                view(path),
                fs::metadata(path.join("a.txt"))
                    .map(|meta| meta.mtime())
                    .ok(),
            )
        })
        .collect();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert!(
        rows.iter()
            .all(|row| row.1 == "previous-workspace-restoration-not-revalidated"),
        "{rows:?}"
    );
    let after: Vec<_> = [&dst, &world.dst_feat("dst"), &world.dst_det("dst")]
        .iter()
        .map(|path| {
            (
                view(path),
                fs::metadata(path.join("a.txt"))
                    .map(|meta| meta.mtime())
                    .ok(),
            )
        })
        .collect();
    assert_eq!(before, after);
}

/// A destination change an operator could make.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    Edit,
    AddWhereSourceAdds,
    Remove,
    Chmod,
    Stage,
    Detach,
    MoveBranch,
    /// A file of the destination's own inside a directory the source turns
    /// into a file (review 2 findings 2 and 4).
    OwnFileWhereSourceTurnsDirectoryIntoFile,
    /// The same, the directory turned into a symlink.
    OwnFileWhereSourceTurnsDirectoryIntoSymlink,
}

#[test]
fn p82_a_locally_modified_destination_refuses_and_is_left_as_it_was() {
    for mutation in [
        Mutation::Edit,
        Mutation::AddWhereSourceAdds,
        Mutation::Remove,
        Mutation::Chmod,
        Mutation::Stage,
        Mutation::Detach,
        Mutation::MoveBranch,
        Mutation::OwnFileWhereSourceTurnsDirectoryIntoFile,
        Mutation::OwnFileWhereSourceTurnsDirectoryIntoSymlink,
    ] {
        let world = World::new();
        let plan = world.plan("dst", false);
        world.capture(&plan, "corpus");
        let (ok, rows) = world.apply(&plan, "corpus", "applied");
        assert!(ok, "{rows:?}");
        let dst = world.dst("dst");
        settle();
        fs::write(world.src().join("a.txt"), b"alpha, moved on\n").unwrap();
        fs::write(world.src().join("new.txt"), b"the source adds this\n").unwrap();
        git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
        fs::remove_dir_all(world.src().join("sub/deep")).unwrap();
        if matches!(
            mutation,
            Mutation::OwnFileWhereSourceTurnsDirectoryIntoSymlink
        ) {
            std::os::unix::fs::symlink("../dir", world.src().join("sub/deep")).unwrap();
        } else {
            fs::write(world.src().join("sub/deep"), b"was a directory\n").unwrap();
        }
        settle();
        world.capture(&plan, "corpus");
        match mutation {
            Mutation::Edit => fs::write(dst.join("dir/b.txt"), b"operator edit\n").unwrap(),
            Mutation::AddWhereSourceAdds => {
                fs::write(dst.join("new.txt"), b"operator's own\n").unwrap();
            }
            Mutation::Remove => fs::remove_file(dst.join("dir/b.txt")).unwrap(),
            Mutation::Chmod => {
                fs::set_permissions(dst.join("exec.sh"), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            Mutation::Stage => {
                fs::write(dst.join("staged.txt"), b"operator staged\n").unwrap();
                git(&dst, &["add", "staged.txt"]);
                fs::write(dst.join("staged.txt"), b"staged only\n").unwrap();
            }
            Mutation::Detach => {
                git(&dst, &["checkout", "--quiet", "--detach", "HEAD"]);
            }
            Mutation::MoveBranch => {
                git(
                    &dst,
                    &["commit", "--quiet", "--allow-empty", "-m", "operator"],
                );
            }
            Mutation::OwnFileWhereSourceTurnsDirectoryIntoFile
            | Mutation::OwnFileWhereSourceTurnsDirectoryIntoSymlink => {
                fs::write(dst.join("sub/deep/mine.txt"), b"the destination's own\n").unwrap();
            }
        }
        let before = (view(&dst), restorable(&dst), stash(&dst));
        let (ok, rows) = world.apply(&plan, "corpus", "applied");
        assert!(!ok, "{mutation:?}: {rows:?}");
        let row = outcome_of(&rows, &world.src());
        assert_eq!(row.1, "refused", "{mutation:?}");
        assert_eq!(
            row.2.as_deref(),
            Some("GIT_DESTINATION_MODIFIED"),
            "{mutation:?}"
        );
        assert_eq!(
            (view(&dst), restorable(&dst), stash(&dst)),
            before,
            "{mutation:?}: the destination must be left as the operator left it"
        );
        assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
        // The refusal is typed and pending review, never unaccounted.
        assert_eq!(
            world.unaccounted(&plan, "corpus", "applied"),
            0,
            "{mutation:?}"
        );
    }
}

#[test]
fn touch_identical_rewrite_and_git_status_are_not_modifications() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let dst = world.dst("dst");
    settle();
    fs::write(world.src().join("a.txt"), b"alpha, moved on\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    // An identical rewrite (new inode, same bytes), a touch, git status, and
    // an untracked file of the destination's own (finding 18).
    let bytes = fs::read(dst.join("dir/b.txt")).unwrap();
    fs::remove_file(dst.join("dir/b.txt")).unwrap();
    fs::write(dst.join("dir/b.txt"), bytes).unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dst.join("notes.txt"))
        .unwrap();
    file.set_modified(std::time::SystemTime::now()).unwrap();
    drop(file);
    git(&dst, &["status", "--porcelain"]);
    fs::write(dst.join(".destination-own"), b"an editor's file\n").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!(fs::read(dst.join("a.txt")).unwrap(), b"alpha, moved on\n");
    assert_eq!(
        fs::read(dst.join(".destination-own")).unwrap(),
        b"an editor's file\n"
    );
    let mut converged = files(&dst);
    converged.remove(Path::new(".destination-own"));
    assert_eq!(converged, files(&world.src()));
    assert_eq!(index(&dst), index(&world.src()));
}

#[test]
fn an_older_corpus_over_a_newer_landing_refuses_stale() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "older");
    settle();
    fs::write(world.src().join("a.txt"), b"alpha, newer\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "newer");
    let (ok, rows) = world.apply(&plan, "newer", "applied-newer");
    assert!(ok, "{rows:?}");
    let before = view(&world.dst("dst"));
    let (ok, rows) = world.apply(&plan, "older", "applied-older");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &world.src()).2.as_deref(),
        Some("GIT_CONVERGE_STALE_CAPTURE")
    );
    assert_eq!(view(&world.dst("dst")), before);
}

#[test]
fn a_removed_landing_is_not_laid_down_again() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    fs::remove_dir_all(world.dst("dst")).unwrap();
    settle();
    fs::write(world.src().join("a.txt"), b"alpha, moved on\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &world.src()).2.as_deref(),
        Some("GIT_DESTINATION_MODIFIED")
    );
    assert!(!world.dst("dst").exists());
}

#[test]
fn a_destination_not_landed_by_bulkload_stays_occupied() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    // An operator's own clone already at the destination: no ledger.
    git(
        &world.root,
        &[
            "clone",
            "--quiet",
            world.src().to_str().unwrap(),
            world.dst("dst").to_str().unwrap(),
        ],
    );
    let before = (view(&world.dst("dst")), refs(&world.dst("dst")));
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &world.src()).2.as_deref(),
        Some("GIT_DESTINATION_OCCUPIED")
    );
    assert_eq!((view(&world.dst("dst")), refs(&world.dst("dst"))), before);
    assert!(!world.dst("dst").join(".git/bulkload").exists());
}

// Review 2 finding 1: the source switches its main checkout to a branch it
// just created, then back to a branch that moved meanwhile. Each HEAD
// branch moves only with HEAD, so its compare-and-swap holds.
#[test]
fn a_main_switching_to_a_new_or_moved_branch_converges() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    for (switch, bytes, branch) in [
        (
            &["switch", "--quiet", "-c", "feature"][..],
            &b"alpha, on feature\n"[..],
            "refs/heads/feature",
        ),
        (
            &["switch", "--quiet", "other"][..],
            &b"alpha, on other\n"[..],
            "refs/heads/other",
        ),
    ] {
        settle();
        if branch == "refs/heads/other" {
            // `other` moves at the source while the destination is not on it.
            git(&src, &["branch", "--quiet", "-f", "other", "feature"]);
        }
        git(&src, switch);
        fs::write(src.join("a.txt"), bytes).unwrap();
        git(&src, &["commit", "--quiet", "-am", branch]);
        settle();
        world.capture(&plan, "corpus");
        let (ok, rows) = world.apply(&plan, "corpus", "applied");
        assert!(ok, "{branch}: {rows:?}");
        assert_eq!(view(&dst), view(&src), "{branch}");
        assert_eq!(head(&dst).0, branch);
        assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
        assert!(!dst.join(".git/bulkload/v1/neo/refs.intent").exists());
        assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0, "{branch}");
    }
}

// Review 2 finding 1, linked: the main's item refuses (an operator edit),
// so its ref step never runs, and the linked worktree switches to a branch
// whose value changed. The linked item moves that branch with its HEAD.
#[test]
fn a_linked_worktree_switching_to_a_moved_branch_converges() {
    let world = World::new();
    let plan = world.plan("dst", true);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    settle();
    let src = world.src();
    fs::write(src.join("a.txt"), b"alpha, second day\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    git(&src, &["branch", "--quiet", "-f", "other", "HEAD"]);
    git(&world.src_feat(), &["switch", "--quiet", "other"]);
    settle();
    world.capture(&plan, "corpus");
    fs::write(world.dst("dst").join("dir/b.txt"), b"operator edit\n").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok, "{rows:?}");
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_DESTINATION_MODIFIED")
    );
    let feat = outcome_of(&rows, &world.src_feat());
    assert_eq!(feat.1, "workspace-restored", "{rows:?}");
    assert_eq!(view(&world.dst_feat("dst")), view(&world.src_feat()));
    assert_eq!(head(&world.dst_feat("dst")).0, "refs/heads/other");
    assert!(!world
        .dst("dst")
        .join(".git/worktrees/feat/bulkload/v1/neo/workspace.intent")
        .exists());
}

// Review 2 finding 3: a branch renamed into a sub-path of its old name, a
// remote-tracking ref the same way, and the HEAD branch the same way. Git
// cannot hold `foo` and `foo/bar` together: the kept name stays, the new
// one stays at provenance (`ref-name-conflict`), the pruned remote-tracking
// ref is deleted before its successor is created, and HEAD detaches with
// its branch recorded. Nothing is left pending.
#[test]
fn a_ref_renamed_into_a_sub_path_converges_without_blocking() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    settle();
    git(&src, &["branch", "-D", "other"]);
    git(&src, &["branch", "other/bar"]);
    git(&src, &["update-ref", "-d", "refs/remotes/origin/old"]);
    git(&src, &["update-ref", "refs/remotes/origin/old/1.0", "HEAD"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("ref-name-conflict=1"), "{reason}");
    assert!(reason.contains("ref-pruned=1"), "{reason}");
    let native = restorable(&dst);
    assert!(native.contains_key("refs/heads/other"));
    assert!(!native.contains_key("refs/heads/other/bar"));
    assert!(!native.contains_key("refs/remotes/origin/old"));
    assert_eq!(
        native.get("refs/remotes/origin/old/1.0"),
        restorable(&src).get("refs/remotes/origin/old/1.0")
    );
    assert!(!dst.join(".git/bulkload/v1/neo/refs.intent").exists());
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);

    // The HEAD branch renamed into a sub-path of itself.
    settle();
    git(&src, &["branch", "-m", "main", "main/next"]);
    fs::write(src.join("a.txt"), b"alpha, on main/next\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "next"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("head-detached-ref-conflict=1"), "{reason}");
    assert_eq!(files(&dst), files(&src));
    assert_eq!(index(&dst), index(&src));
    assert_eq!(head(&dst), (String::new(), head(&src).1));
    assert!(!restorable(&dst).contains_key("refs/heads/main/next"));
    assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
    assert!(!dst.join(".git/bulkload/v1/neo/refs.intent").exists());
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);

    // P83: and then it stands still.
    let before = (view(&dst), restorable(&dst));
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!((view(&dst), restorable(&dst)), before);
}

// Review 2 finding 7: a ref intent one item left in the shared common
// directory is settled by the next item without proving or restaging an
// item whose journal for its current capture exists, so an operator's edit
// after a finished landing stays allowed and its outcome stands.
#[test]
fn a_sibling_ref_intent_does_not_re_prove_a_finished_item() {
    let world = World::new();
    let plan = world.plan("dst", true);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    // Edits after a finished landing: the main's item runs first, and
    // would settle the intent by re-proving its checkout.
    fs::write(dst.join("dir/b.txt"), b"operator edit\n").unwrap();
    fs::write(world.dst_feat("dst").join("feat.txt"), b"operator edit\n").unwrap();
    // A refs-only item of the same repository, in its own plan, is cut
    // short by a concurrent writer's lock on the ref it creates.
    settle();
    git(&src, &["tag", "v9"]);
    let refs_plan = world.root.join("plan-refs");
    estate::add_batch(
        &refs_plan,
        &[Item {
            source: src.clone(),
            repository: dst.clone(),
            workspace: None,
        }],
    )
    .unwrap();
    world.capture(&refs_plan, "corpus-refs");
    fs::create_dir_all(dst.join(".git/refs/tags")).unwrap();
    let lock = dst.join(".git/refs/tags/v9.lock");
    fs::write(&lock, b"").unwrap();
    let (ok, rows) = world.apply(&refs_plan, "corpus-refs", "applied-refs");
    assert!(!ok, "{rows:?}");
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_CONVERGE_INTERRUPTED")
    );
    let intent = dst.join(".git/bulkload/v1/neo/refs.intent");
    assert!(intent.exists());
    fs::remove_file(&lock).unwrap();
    // The first plan again, its captures unchanged: every item replays its
    // journal, the shared intent settles forward, and the edit stays.
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert!(
        rows.iter()
            .all(|row| row.1 == "previous-workspace-restoration-not-revalidated"),
        "{rows:?}"
    );
    assert!(!intent.exists());
    assert_eq!(
        restorable(&dst).get("refs/tags/v9"),
        restorable(&src).get("refs/tags/v9")
    );
    for edited in [
        dst.join("dir/b.txt"),
        world.dst_feat("dst").join("feat.txt"),
    ] {
        assert_eq!(fs::read(edited).unwrap(), b"operator edit\n");
    }
    let main = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(main.contains("ref-intent-settled=1"), "{main}");
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

// ---- the convergence lane's fixes (2026-10-09) -------------------------------
//
// Each test names the EstateConverge.tla mutation that is the code before
// its fix; the model's MC_ec_neg_<mutation> row fails, and the fixed
// behaviour's MC_ec_ row passes.

/// Hold the main checkout's `index.lock` as an operator's Git command
/// would, so a converge stops at its index compare-and-swap with its
/// workspace intent pending and every seat written.
fn hold_index(dst: &Path) -> PathBuf {
    let lock = dst.join(".git/index.lock");
    fs::write(&lock, b"an operator's git add\n").unwrap();
    lock
}

/// Review 3 finding 4 (mutation `unborn_head_untyped`): the operator deletes
/// the checked-out branch, so HEAD is unborn. The item refuses
/// `GIT_DESTINATION_MODIFIED` and is left as it was; before the fix
/// `rev-parse --verify HEAD` failed and it refused `GIT_CHILD_FAILED`.
#[test]
fn an_unborn_head_refuses_modified() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let dst = world.dst("dst");
    settle();
    fs::write(world.src().join("a.txt"), b"alpha, moved on\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    git(&dst, &["update-ref", "-d", "refs/heads/main"]);
    let before = (files(&dst), restorable(&dst));
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &world.src()).2.as_deref(),
        Some("GIT_DESTINATION_MODIFIED"),
        "{rows:?}"
    );
    assert_eq!((files(&dst), restorable(&dst)), before);
}

/// Review 3 finding 9 (mutation `probe_left`): probe files a crashed pass
/// left in the worktree root are removed by the next apply; before the fix
/// they stayed for good.
#[test]
fn probe_files_a_crashed_pass_left_are_removed() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let dst = world.dst("dst");
    let probes = [
        dst.join(".bulkload-converge-00ff00ff00ff00ff-probe-a"),
        dst.join(".bulkload-converge-00ff00ff00ff00ff-probe-b"),
    ];
    for probe in &probes {
        fs::write(probe, b"").unwrap();
    }
    settle();
    fs::write(world.src().join("a.txt"), b"alpha, moved on\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert!(probes.iter().all(|probe| !probe.exists()));
    assert_eq!(files(&dst), files(&world.src()));
}

/// The earlier report's finding 1 (mutation `settle_remove_dir`): a file
/// the source turns into a directory, and a converge stopped after its
/// seats (an operator's Git held the index). The settle's removal of the
/// file finds the new directory there and is done; before the fix it
/// refused `GIT_CONVERGE_INTERRUPTED` on every rerun.
#[test]
fn a_file_turned_directory_settles_after_an_interrupted_converge() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    settle();
    fs::remove_file(src.join("kind.txt")).unwrap();
    fs::create_dir(src.join("kind.txt")).unwrap();
    fs::write(src.join("kind.txt/inner"), b"a directory now\n").unwrap();
    settle();
    world.capture(&plan, "corpus");
    let lock = hold_index(&dst);
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_CONVERGE_INTERRUPTED")
    );
    assert!(
        dst.join("kind.txt/inner").is_file(),
        "the seats were written"
    );
    fs::remove_file(lock).unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!(view(&dst), view(&src));
    assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
}

/// Review 3 finding 5 (mutation `seat_parent_untyped`): after the intent,
/// the operator replaces a landed directory that holds a pending seat with
/// a file. The settle refuses `GIT_CONVERGE_INTERRUPTED` and keeps the
/// operator's file; before the fix `Seat::at`'s error passed on
/// (`PATH_ESCAPES_ROOT`).
#[test]
fn a_seat_parent_replaced_after_the_intent_refuses_interrupted() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    settle();
    fs::write(src.join("dir/b.txt"), b"beta, moved on\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    let lock = hold_index(&dst);
    let (ok, _) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    fs::remove_file(lock).unwrap();
    fs::remove_dir_all(dst.join("dir")).unwrap();
    fs::write(dst.join("dir"), b"the operator's file\n").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_CONVERGE_INTERRUPTED"),
        "{rows:?}"
    );
    assert_eq!(fs::read(dst.join("dir")).unwrap(), b"the operator's file\n");
    assert!(dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
}

/// The earlier report's finding 5 (review 2 finding 1's stated limit;
/// mutation `old_head_branch_skipped`): a commit on the checkout's branch,
/// then a switch to another. The step skips the old branch while HEAD is
/// on it; a step after `set_head` brings it to the capture. Before the fix
/// it stayed behind while the source held still.
#[test]
fn the_branch_head_left_converges_with_it() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    settle();
    fs::write(src.join("a.txt"), b"alpha, on main\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    git(&src, &["switch", "--quiet", "other"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!(head(&dst).0, "refs/heads/other");
    assert_eq!(
        restorable(&dst).get("refs/heads/main"),
        restorable(&src).get("refs/heads/main")
    );
    assert_eq!(view(&dst), view(&src));
}

/// Review 3's finding 19 (mutation `settle_replays_branch`): the source
/// renames `other` to `other/bar` and switches to it; the operator deletes
/// `other`, so the converge plans HEAD onto a new `other/bar`; the converge
/// stops before `set_head`; the operator puts `other` back. The settle
/// re-plans HEAD (detached at the captured head, `other/bar` recorded)
/// instead of replaying a create Git refuses beside `other`, which refused
/// `GIT_CONVERGE_INTERRUPTED` on every rerun before the fix.
#[test]
fn a_settle_re_plans_a_head_branch_a_put_back_ref_blocks() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    let other = restorable(&dst).get("refs/heads/other").unwrap().clone();
    settle();
    git(&src, &["branch", "-m", "other", "other/bar"]);
    git(&src, &["switch", "--quiet", "other/bar"]);
    settle();
    world.capture(&plan, "corpus");
    git(&dst, &["branch", "-D", "--quiet", "other"]);
    let lock = hold_index(&dst);
    let (ok, _) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    fs::remove_file(lock).unwrap();
    git(&dst, &["update-ref", "refs/heads/other", &other]);
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("head-replanned=1"), "{reason}");
    assert!(reason.contains("head-detached-ref-conflict=1"), "{reason}");
    assert_eq!(head(&dst), (String::new(), head(&src).1));
    assert_eq!(restorable(&dst).get("refs/heads/other"), Some(&other));
    assert!(!restorable(&dst).contains_key("refs/heads/other/bar"));
    assert_eq!(files(&dst), files(&src));
    assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
}

/// The earlier report's finding 2 (mutation `step_moves_intent_branch`):
/// the main's intent moves HEAD onto `other` and `other` with it, and stops
/// (an operator's Git holds the main's index) twice, while the source
/// moves `other` again. The linked worktrees' ref steps leave `other` to
/// the main's pending intent; before the fix they moved it to the newer
/// capture's value, and the main's settle replayed a compare-and-swap that
/// could never hold again, refusing on every rerun.
#[test]
fn a_pending_intents_head_branch_is_left_to_its_item() {
    let world = World::new();
    let plan = world.plan("dst", true);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    let landed = restorable(&dst).get("refs/heads/other").unwrap().clone();
    settle();
    git(&src, &["switch", "--quiet", "other"]);
    fs::write(src.join("a.txt"), b"alpha, on other\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    let lock = hold_index(&dst);
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_CONVERGE_INTERRUPTED")
    );
    assert_eq!(restorable(&dst).get("refs/heads/other"), Some(&landed));
    settle();
    fs::write(src.join("a.txt"), b"alpha, on other again\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c3"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok, "{rows:?}");
    assert_eq!(restorable(&dst).get("refs/heads/other"), Some(&landed));
    fs::remove_file(lock).unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert_eq!(head(&dst).0, "refs/heads/other");
    assert_eq!(view(&dst), view(&src));
    assert_eq!(
        restorable(&dst).get("refs/heads/other"),
        restorable(&src).get("refs/heads/other")
    );
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

/// The step for the branch HEAD left runs after `set_head`; when another
/// writer holds that branch, the item still lands (HEAD moved, ledger
/// written), the branch's ref intent stays for the next apply, which
/// settles it. A refusal there would leave HEAD moved under a pending
/// workspace intent (EstateConverge.tla, `LeftBranchDeferred`).
#[test]
fn the_branch_head_left_waits_for_a_writer_without_refusing() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    let landed = restorable(&dst).get("refs/heads/main").unwrap().clone();
    settle();
    fs::write(src.join("a.txt"), b"alpha, on main\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    git(&src, &["switch", "--quiet", "other"]);
    settle();
    world.capture(&plan, "corpus");
    let lock = dst.join(".git/refs/heads/main.lock");
    fs::write(&lock, b"").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("ref-left-branch-deferred=1"), "{reason}");
    assert_eq!(head(&dst).0, "refs/heads/other");
    assert_eq!(restorable(&dst).get("refs/heads/main"), Some(&landed));
    assert!(dst.join(".git/bulkload/v1/neo/refs.intent").exists());
    assert!(!dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
    fs::remove_file(&lock).unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    assert!(!dst.join(".git/bulkload/v1/neo/refs.intent").exists());
    assert_eq!(
        restorable(&dst).get("refs/heads/main"),
        restorable(&src).get("refs/heads/main")
    );
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

/// Review 4 (mutation `intent_head_lock_kept`): a Git child killed inside
/// `set_head`'s HEAD transaction leaves `HEAD.lock` holding Git's own
/// content (the intent's new HEAD), and a ref intent of the repository is
/// pending too (here: a step stopped by another writer's lock). Every ref
/// transaction holds every worktree's HEAD, so before the fix the ref
/// settle, which runs first, met that `HEAD.lock` on every rerun and never
/// reached the workspace settle that removes it. Now the leftovers of every
/// pending workspace intent go first, and the rerun converges.
#[test]
fn a_killed_set_heads_lock_beside_a_pending_ref_intent_settles() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    settle();
    git(&src, &["switch", "--quiet", "other"]);
    fs::write(src.join("a.txt"), b"alpha, on other\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    git(&src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    settle();
    world.capture(&plan, "corpus");
    // Another writer holds origin/main: the converge's ref step stops with
    // its ref intent pending, after the workspace intent.
    let foreign = dst.join(".git/refs/remotes/origin/main.lock");
    fs::write(&foreign, b"another writer\n").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(!ok);
    assert_eq!(
        outcome_of(&rows, &src).2.as_deref(),
        Some("GIT_CONVERGE_INTERRUPTED"),
        "{rows:?}"
    );
    fs::remove_file(&foreign).unwrap();
    assert!(dst.join(".git/bulkload/v1/neo/refs.intent").exists());
    assert!(dst.join(".git/bulkload/v1/neo/workspace.intent").exists());
    // What a killed `set_head` HEAD transaction leaves.
    fs::write(dst.join(".git/HEAD.lock"), b"ref: refs/heads/other\n").unwrap();
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("git-lock-removed=1"), "{reason}");
    assert!(!dst.join(".git/HEAD.lock").exists());
    assert_eq!(head(&dst).0, "refs/heads/other");
    assert_eq!(view(&dst), view(&src));
    assert_eq!(world.unaccounted(&plan, "corpus", "applied"), 0);
}

/// Review 4 (mutation `symref_names_unplanned`): the operator makes a
/// symbolic ref `refs/heads/alias`, and the source creates `alias/x`. Git
/// can never create it beside the symbolic ref, so the converge drops the
/// create (`ref-name-conflict`) and lands everything else; before the fix
/// every pass refused `GIT_CONVERGE_INTERRUPTED` and the checkout never
/// converged again.
#[test]
fn a_create_beside_an_operators_symbolic_ref_is_dropped() {
    let world = World::new();
    let plan = world.plan("dst", false);
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let (src, dst) = (world.src(), world.dst("dst"));
    git(
        &dst,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    settle();
    git(&src, &["branch", "alias/x"]);
    fs::write(src.join("a.txt"), b"alpha, moved on\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "c2"]);
    settle();
    world.capture(&plan, "corpus");
    let (ok, rows) = world.apply(&plan, "corpus", "applied");
    assert!(ok, "{rows:?}");
    let reason = outcome_of(&rows, &src).2.clone().unwrap();
    assert!(reason.contains("ref-name-conflict=1"), "{reason}");
    assert!(!restorable(&dst).contains_key("refs/heads/alias/x"));
    assert_eq!(
        git(&dst, &["symbolic-ref", "refs/heads/alias"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(files(&dst), files(&src));
    assert!(!dst.join(".git/bulkload/v1/neo/refs.intent").exists());
}
