//! `worktree-fold` and `worktree-reattach` through the binary (TIN-7491;
//! operator 2026-10-27, Linear TIN-3692 `ae17f1bd` and `36925e2d`).
//!
//! A kept clone with four linked worktrees (a branch new to the canonical
//! repository, one equal there, one that moved there, and a detached HEAD)
//! folds into the canonical repository: refs under the ruled namespace,
//! objects only an index reaches, each worktree moved with HEAD, branch,
//! index and `status -z` unchanged, its kept admin directory parked. A
//! worktree written in the last 30 minutes, or with a process inside, is
//! held. A move that does not verify is undone, and the branch the same pass
//! created is deleted. The fold's own floor is 10% unless the flag says
//! otherwise, and the receipts say which. Fixture repositories run with
//! hooks off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const RULING: &str = "TIN-7491 test";
const NAMESPACE: &str = "refs/kept-test-20261027";

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-worktree-fold-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Self(fs::canonicalize(base).unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn text(&self, name: &str) -> String {
        self.path(name).to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fixture Git child: no hooks, no global or system configuration, no
/// optional locks (so a read never refreshes an index).
fn g(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "user.name=Bulkload test"])
        .args(["-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    fs::write(dir.join(file), body).unwrap();
    g(dir, &["add", file]);
    g(dir, &["commit", "-q", "-m", file]);
    g(dir, &["rev-parse", "HEAD"])
}

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn agent(globals: &[&str], args: &[&str]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(globals)
        .args(args)
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Set every file and directory under `root` to two hours ago, so the
/// fold's 30-minute quiet check passes.
fn age(root: &Path) {
    let then = SystemTime::now()
        .checked_sub(Duration::from_secs(2 * 3600))
        .unwrap();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        }
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(then))
            .unwrap();
    }
}

/// `git status --porcelain=v1 -z -uall` of a worktree.
fn status(dir: &Path) -> String {
    g(dir, &["status", "--porcelain=v1", "-z", "-uall"])
}

struct Estate {
    scratch: Scratch,
    canonical: PathBuf,
    kept: PathBuf,
    state: PathBuf,
}

impl Estate {
    /// A canonical repository and a kept clone with four worktrees:
    /// `new` (branch `feat` the canonical repository lacks, with a commit,
    /// a staged blob and an untracked file), `same` (branch `same`, equal
    /// in both), `moved` (branch `moved`, ahead in the canonical
    /// repository) and `loose` (detached).
    fn new(name: &str) -> Self {
        let scratch = Scratch::new(name);
        let (canonical, kept) = (scratch.path("canonical"), scratch.path("kept"));
        fs::create_dir(&kept).unwrap();
        g(&kept, &["init", "-q"]);
        let base = commit(&kept, "a.txt", "one\n");
        g(&kept, &["branch", "same"]);
        g(&kept, &["branch", "moved"]);
        g(
            &scratch.0,
            &[
                "clone",
                "-q",
                "--no-local",
                kept.to_str().unwrap(),
                "canonical",
            ],
        );
        g(&canonical, &["branch", "same", "origin/same"]);
        g(&canonical, &["branch", "moved", "origin/moved"]);
        g(&canonical, &["remote", "remove", "origin"]);
        let wts = scratch.path("wts");
        fs::create_dir(&wts).unwrap();
        let at = |leaf: &str| wts.join(leaf).to_str().unwrap().to_owned();
        g(
            &kept,
            &["worktree", "add", "-q", "-b", "feat", &at("new"), &base],
        );
        commit(&wts.join("new"), "b.txt", "feat\n");
        fs::write(wts.join("new/staged.txt"), "only in an index\n").unwrap();
        g(&wts.join("new"), &["add", "staged.txt"]);
        fs::write(wts.join("new/scratch.txt"), "untracked\n").unwrap();
        fs::write(wts.join("new/a.txt"), "one, edited\n").unwrap();
        g(&kept, &["worktree", "add", "-q", &at("same"), "same"]);
        g(&kept, &["worktree", "add", "-q", &at("moved"), "moved"]);
        g(
            &kept,
            &["worktree", "add", "-q", "--detach", &at("loose"), &base],
        );
        // The canonical repository's `moved` advances past the kept one.
        g(&canonical, &["checkout", "-q", "moved"]);
        commit(&canonical, "c.txt", "canonical\n");
        g(&canonical, &["checkout", "-q", "main"]);
        let state = scratch.path("state");
        Self {
            scratch,
            canonical,
            kept,
            state,
        }
    }

    fn worktree(&self, leaf: &str) -> PathBuf {
        self.scratch.path("wts").join(leaf)
    }

    fn fold(&self, globals: &[&str], step: &str, rest: &[&str]) -> Ran {
        let mut args = vec![
            "worktree-fold",
            step,
            self.canonical.to_str().unwrap(),
            self.kept.to_str().unwrap(),
            self.state.to_str().unwrap(),
        ];
        args.extend_from_slice(rest);
        if step != "plan" {
            args.extend_from_slice(&["--ruling", RULING]);
        }
        agent(globals, &args)
    }

    fn receipts(&self) -> String {
        fs::read_to_string(self.state.join("worktree-fold.receipts")).unwrap()
    }

    fn dotgit(&self, leaf: &str) -> String {
        fs::read_to_string(self.worktree(leaf).join(".git"))
            .unwrap()
            .trim_end()
            .to_owned()
    }
}

#[test]
fn a_kept_clone_folds_into_the_canonical_repository() {
    let estate = Estate::new("fold");
    let before: Vec<(String, String, String)> = ["new", "same", "moved", "loose"]
        .iter()
        .map(|leaf| {
            let dir = estate.worktree(leaf);
            (
                g(&dir, &["rev-parse", "HEAD"]),
                status(&dir),
                (*leaf).to_owned(),
            )
        })
        .collect();
    let feat = g(&estate.worktree("new"), &["rev-parse", "HEAD"]);
    age(&estate.scratch.0);
    let free = ["--min-free-percent=0"];

    let ran = estate.fold(&free, "plan", &["kc2-test"]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    for word in [
        "worktrees=4",
        "create=1",
        "attach=1",
        "detach=1",
        "detached=1",
        "min_free_percent=0 from=flag",
    ] {
        assert!(ran.stdout.contains(word), "{word}: {}", ran.stdout);
    }

    let ran = estate.fold(&free, "refs", &[NAMESPACE]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains("missing=0 extra=0"), "{}", ran.stdout);
    assert_eq!(
        g(
            &estate.canonical,
            &["rev-parse", &format!("{NAMESPACE}/heads/feat")]
        ),
        feat
    );
    // The batch is packed.
    let packed = fs::read_to_string(estate.canonical.join(".git/packed-refs")).unwrap();
    assert!(
        packed.contains(&format!("{NAMESPACE}/heads/feat")),
        "{packed}"
    );

    let staged = g(&estate.worktree("new"), &["rev-parse", ":staged.txt"]);
    let ran = estate.fold(&free, "objects", &[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains("still_missing=0"), "{}", ran.stdout);
    g(&estate.canonical, &["cat-file", "-e", &staged]);

    let ran = estate.fold(&free, "attach", &[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    for word in [
        "moved-create=1",
        "moved-attach=1",
        "moved-detach=1",
        "moved-detached=1",
    ] {
        assert!(ran.stdout.contains(word), "{word}: {}", ran.stdout);
    }
    let admins = estate.canonical.join(".git/worktrees");
    for (head, before_status, leaf) in &before {
        let dir = estate.worktree(leaf);
        let dotgit = estate.dotgit(leaf);
        let pointer = PathBuf::from(dotgit.strip_prefix("gitdir: ").unwrap());
        assert_eq!(pointer.parent().unwrap(), admins, "{leaf}: {dotgit}");
        assert_eq!(&g(&dir, &["rev-parse", "HEAD"]), head, "{leaf}");
        assert_eq!(&status(&dir), before_status, "{leaf}");
    }
    assert_eq!(
        g(&estate.worktree("new"), &["symbolic-ref", "HEAD"]),
        "refs/heads/feat"
    );
    assert_eq!(
        g(&estate.worktree("same"), &["symbolic-ref", "HEAD"]),
        "refs/heads/same"
    );
    assert_eq!(g(&estate.canonical, &["rev-parse", "feat"]), feat);
    // The kept clone lists only its own checkout; its admin directories are
    // parked outside `.git/worktrees`, never deleted.
    let kept_list = g(&estate.kept, &["worktree", "list", "--porcelain"]);
    assert_eq!(kept_list.matches("worktree ").count(), 1, "{kept_list}");
    let parked = estate.kept.join(".git/worktrees-parked-kc2-test");
    assert_eq!(fs::read_dir(&parked).unwrap().count(), 4 + 4);

    let ran = estate.fold(&free, "verify", &[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout
            .contains("verified=4 mismatched=0 not_moved=0 fsck_rc=0"),
        "{}",
        ran.stdout
    );

    // A rerun moves nothing.
    let ran = estate.fold(&free, "attach", &[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains("already-moved=4"), "{}", ran.stdout);
    let receipts = estate.receipts();
    assert!(receipts.contains(RULING), "{receipts}");
    assert!(!receipts.contains("\theld\t"), "{receipts}");
}

#[test]
fn a_recent_write_or_a_process_inside_holds_the_worktree() {
    let estate = Estate::new("live");
    let free = ["--min-free-percent=0"];
    // Nothing aged: every worktree was written just now.
    let ran = estate.fold(&[], "plan", &["kc2-live"]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("+recent-write=1").count(),
        4,
        "{}",
        ran.stdout
    );
    // The fold's own floor, from no flag.
    assert!(
        estate
            .receipts()
            .contains("min_free_percent=10 from=default"),
        "{}",
        estate.receipts()
    );
    let ran = estate.fold(&free, "refs", &[NAMESPACE]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    let ran = estate.fold(&free, "objects", &[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    let before = estate.dotgit("same");
    let ran = estate.fold(&free, "attach", &["same"]);
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("ITEMS_REFUSED"), "{}", ran.stderr);
    assert_eq!(estate.dotgit("same"), before);

    // Quiet files, but a process with its working directory inside.
    age(&estate.scratch.0);
    let mut sleeper = Command::new("sleep")
        .arg("30")
        .current_dir(estate.worktree("same"))
        .spawn()
        .unwrap();
    let ran = estate.fold(&free, "attach", &["same"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(estate.dotgit("same"), before);
    let receipts = estate.receipts();
    assert!(receipts.contains("recent-write"), "{receipts}");
    assert!(
        receipts.contains("live-process") || receipts.contains("live-unknown"),
        "{receipts}"
    );

    // Without the flag the fold's 10% floor applies; at 100% nothing fits.
    age(&estate.scratch.0);
    let ran = estate.fold(&["--min-free-percent=100"], "attach", &["same"]);
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        estate.receipts().contains("\tfloor\t"),
        "{}",
        estate.receipts()
    );
    assert_eq!(estate.dotgit("same"), before);
}

#[test]
fn a_move_that_does_not_verify_is_undone_and_its_new_branch_deleted() {
    let estate = Estate::new("undo");
    // The canonical repository ignores what the kept clone shows as
    // untracked, so `new`'s status cannot match after the move.
    fs::write(estate.canonical.join(".git/info/exclude"), "scratch.txt\n").unwrap();
    age(&estate.scratch.0);
    let free = ["--min-free-percent=0"];
    for (step, rest) in [
        ("plan", vec!["kc2-undo"]),
        ("refs", vec![NAMESPACE]),
        ("objects", vec![]),
    ] {
        let ran = estate.fold(&free, step, &rest);
        assert!(ran.ok, "{step}: {}{}", ran.stdout, ran.stderr);
    }
    let before = estate.dotgit("new");
    let ran = estate.fold(&free, "attach", &["new"]);
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.is_empty() || !ran.stdout.contains("moved-"),
        "{}",
        ran.stdout
    );
    assert_eq!(estate.dotgit("new"), before);
    assert!(estate.kept.join(".git/worktrees/new/HEAD").exists());
    let branches = g(&estate.canonical, &["branch", "--list", "feat"]);
    assert!(branches.is_empty(), "{branches}");
    let receipts = estate.receipts();
    assert!(receipts.contains("verify-failed"), "{receipts}");
    assert!(receipts.contains("created branch deleted"), "{receipts}");
    assert!(estate
        .canonical
        .join(".git/worktrees-parked-kc2-undo")
        .read_dir()
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("rollback-")));
    // The kept worktree still works where it was.
    assert!(status(&estate.worktree("new")).contains("scratch.txt"));
}

#[test]
fn reattach_gives_an_orphaned_worktree_a_new_admin_directory() {
    let scratch = Scratch::new("reattach");
    let canonical = scratch.path("canonical");
    fs::create_dir(&canonical).unwrap();
    g(&canonical, &["init", "-q"]);
    commit(&canonical, "a.txt", "one\n");
    g(&canonical, &["branch", "side"]);
    let worktree = scratch.path("side");
    g(
        &canonical,
        &["worktree", "add", "-q", worktree.to_str().unwrap(), "side"],
    );
    // Its admin directory goes missing (parked elsewhere by hand).
    fs::rename(
        canonical.join(".git/worktrees/side"),
        scratch.path("lost-admin"),
    )
    .unwrap();
    age(&scratch.0);
    let ran = agent(
        &["--min-free-percent=0"],
        &[
            "worktree-reattach",
            canonical.to_str().unwrap(),
            worktree.to_str().unwrap(),
            "side-reattached",
            "ref: refs/heads/side",
            &scratch.text("state"),
            "--ruling",
            RULING,
        ],
    );
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains("status_entries=0 listed=1 prunable=0"),
        "{}",
        ran.stdout
    );
    assert_eq!(g(&worktree, &["symbolic-ref", "HEAD"]), "refs/heads/side");
    assert_eq!(status(&worktree), "");
    // Never twice: the name is taken now.
    let ran = agent(
        &["--min-free-percent=0"],
        &[
            "worktree-reattach",
            canonical.to_str().unwrap(),
            worktree.to_str().unwrap(),
            "side-reattached",
            "ref: refs/heads/side",
            &scratch.text("state"),
            "--ruling",
            RULING,
        ],
    );
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
}

#[test]
fn every_step_but_plan_needs_a_ruling_and_the_namespace_is_ruled() {
    let estate = Estate::new("args");
    let canonical = estate.canonical.to_str().unwrap();
    let kept = estate.kept.to_str().unwrap();
    let state = estate.state.to_str().unwrap();
    let ran = agent(&[], &["worktree-fold", "objects", canonical, kept, state]);
    assert!(!ran.ok);
    assert!(
        ran.stderr.contains("REQUIRED_FIELD_MISSING"),
        "{}",
        ran.stderr
    );
    let ran = estate.fold(&["--min-free-percent=0"], "refs", &["refs/heads"]);
    assert!(!ran.ok);
    assert!(
        ran.stderr.contains("FIELD_DOMAIN_VIOLATION"),
        "{}",
        ran.stderr
    );
    let ran = estate.fold(&[], "plan", &["Bad_Tag"]);
    assert!(!ran.ok);
}
