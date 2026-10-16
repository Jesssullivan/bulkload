//! The GFTB inventory's shapes, scaled down (#226; design
//! `docs/agent-notes/2026-10-15-estate-adopt-design.md`, section 9): a
//! source estate ("neo") and the live destination tree ("sting") that a
//! hand swap left, from `inventory.tsv`'s classes:
//!
//! - `infra`: a main checkout with tracked, untracked and ignored seats, a
//!   symlink, an executable, a stash, a `node_modules` (a rebuildable root)
//!   and a nested linked worktree under `.worktrees/` (`nest`); a sibling
//!   linked worktree (`infra.worktrees/feat`); a worktree only the source
//!   has (`infra.worktrees/only-src`, neo-only); a branch `diverge` the
//!   destination moved to its own commit; and a worktree only the
//!   destination has (`infra.worktrees/dest-only`, sting-only), whose
//!   branch the destination committed on. The destination is a `cp -a` of
//!   the source's family with `git worktree repair` (the 10-08 swap).
//! - `lamesh`: identical checkout, three branches the destination lacks
//!   (neo-ahead), an scp-style origin the destination keeps.
//! - `sshrepo`: neo-only, with an scp-style origin under the
//!   `--activate-origin` prefix.
//! - `preservation.git`: a bare repository whose objects live in a mirror
//!   through `alternates`, refs-only, absent at the destination.
//!
//! Fixture repositories are created and destroyed here; their Git runs with
//! no global configuration and no hooks (R-N98).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-10-15T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-15T00:00:00Z")
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

pub fn git(repo: &Path, args: &[&str]) -> String {
    let out = git_command(repo).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `cp -a`, as the 10-08 swap copied the landing.
pub fn copy_tree(from: &Path, to: &Path) {
    let out = Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Seats must be older than one timestamp tick to be reused (R-N76), and a
/// recapture must be observed after the landing it converges.
pub fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

pub struct Gftb {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Gftb {
    pub fn neo(&self, rel: &str) -> PathBuf {
        self.root.join("neo").join(rel)
    }

    pub fn sting(&self, rel: &str) -> PathBuf {
        self.root.join("sting").join(rel)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The source estate and the live destination tree.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-adopt-gftb-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let world = Self { _dir: dir, root };
        fs::create_dir_all(world.neo("infra.worktrees")).unwrap();
        fs::create_dir_all(world.sting("")).unwrap();
        world.source_infra();
        world.source_lamesh();
        world.source_sshrepo();
        world.source_preservation();
        world.destination();
        world
    }

    fn source_infra(&self) {
        let infra = self.neo("infra");
        fs::create_dir(&infra).unwrap();
        git(&infra, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(infra.join(".gitignore"), b".worktrees/\nbuild.log\n").unwrap();
        fs::write(infra.join("a.txt"), b"alpha\n").unwrap();
        fs::create_dir_all(infra.join("dir/deep")).unwrap();
        fs::write(infra.join("dir/deep/b.txt"), b"beta\n").unwrap();
        fs::write(infra.join("run.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(infra.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", infra.join("link")).unwrap();
        git(&infra, &["add", "."]);
        git(&infra, &["commit", "--quiet", "-m", "c1"]);
        git(&infra, &["tag", "v1"]);
        for branch in ["nest", "feat", "src-only", "diverge"] {
            git(&infra, &["branch", branch]);
        }
        // The source's own commit on `diverge` (the destination makes
        // another): diverged, not ahead.
        let tree = git(&infra, &["rev-parse", "HEAD:"]);
        let commit = git(
            &infra,
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                "HEAD",
                "-m",
                "neo's diverge",
            ],
        );
        git(&infra, &["update-ref", "refs/heads/diverge", commit.trim()]);
        git(&infra, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fs::write(infra.join("a.txt"), b"alpha, stashed\n").unwrap();
        git(&infra, &["stash", "push", "--quiet", "-m", "wip"]);
        // Dirty work the reconcile kept: a staged edit, an untracked file,
        // an ignored file and a rebuildable root.
        fs::write(infra.join("dir/deep/b.txt"), b"beta, staged\n").unwrap();
        git(&infra, &["add", "dir/deep/b.txt"]);
        fs::write(infra.join("notes.txt"), b"untracked\n").unwrap();
        fs::write(infra.join("build.log"), b"ignored\n").unwrap();
        fs::create_dir_all(infra.join("node_modules/pkg")).unwrap();
        fs::write(
            infra.join("node_modules/pkg/index.js"),
            b"module.exports = 1;\n",
        )
        .unwrap();
        fs::write(infra.join("node_modules/pkg/big.bin"), vec![7u8; 4096]).unwrap();
        // The nested worktree under .worktrees/ and the two siblings.
        let nest = infra.join(".worktrees/nest");
        git(
            &infra,
            &["worktree", "add", "--quiet", nest.to_str().unwrap(), "nest"],
        );
        fs::write(nest.join("nest.txt"), b"nested work\n").unwrap();
        for (name, branch) in [("feat", "feat"), ("only-src", "src-only")] {
            let path = self.neo("infra.worktrees").join(name);
            git(
                &infra,
                &["worktree", "add", "--quiet", path.to_str().unwrap(), branch],
            );
            fs::write(path.join(format!("{name}.txt")), format!("{name} work\n")).unwrap();
        }
    }

    fn source_lamesh(&self) {
        let lamesh = self.neo("lamesh");
        fs::create_dir(&lamesh).unwrap();
        git(&lamesh, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(lamesh.join("README"), b"LA-Mesh\n").unwrap();
        git(&lamesh, &["add", "."]);
        git(&lamesh, &["commit", "--quiet", "-m", "m1"]);
        for branch in ["chore/one", "chore/two", "chore/three"] {
            git(&lamesh, &["branch", branch]);
        }
        git(&lamesh, &["remote", "add", "origin", "yoga:git/LA-Mesh"]);
    }

    fn source_sshrepo(&self) {
        let ssh = self.neo("sshrepo");
        fs::create_dir(&ssh).unwrap();
        git(&ssh, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(ssh.join("x"), b"x\n").unwrap();
        git(&ssh, &["add", "."]);
        git(&ssh, &["commit", "--quiet", "-m", "s1"]);
        git(&ssh, &["remote", "add", "origin", "yoga:git/sshrepo"]);
        git(&ssh, &["config", "branch.main.remote", "origin"]);
        git(&ssh, &["config", "branch.main.merge", "refs/heads/main"]);
    }

    // A project, its self-contained mirror, and the preservation bare that
    // borrows the mirror's objects through `alternates` (neo-capture.sh).
    fn source_preservation(&self) {
        let upstream = self.neo("upstream");
        fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(upstream.join("site"), b"site\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "--quiet", "-m", "u1"]);
        git(&upstream, &["branch", "archive/carrier"]);
        let mirror = self.neo("mirror.git");
        git(
            &self.neo(""),
            &[
                "clone",
                "--quiet",
                "--mirror",
                "--no-local",
                upstream.to_str().unwrap(),
                mirror.to_str().unwrap(),
            ],
        );
        let preservation = self.neo("preservation.git");
        git(
            &self.neo(""),
            &[
                "clone",
                "--quiet",
                "--bare",
                "--shared",
                mirror.to_str().unwrap(),
                preservation.to_str().unwrap(),
            ],
        );
        assert!(preservation.join("objects/info/alternates").is_file());
        git(
            &preservation,
            &[
                "update-ref",
                "refs/remotes/origin/archive/root",
                "refs/heads/main",
            ],
        );
        git(
            &preservation,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/example/site.git",
            ],
        );
    }

    // The live tree the reconcile left: a `cp -a` of the source's families,
    // repaired; the source-only worktree removed; a destination-only one
    // with its own commit; `diverge` moved to it; lamesh missing three
    // branches; the destination's own node_modules file.
    fn destination(&self) {
        copy_tree(&self.neo("infra"), &self.sting("infra"));
        fs::create_dir(self.sting("infra.worktrees")).unwrap();
        copy_tree(
            &self.neo("infra.worktrees/feat"),
            &self.sting("infra.worktrees/feat"),
        );
        copy_tree(
            &self.neo("infra.worktrees/only-src"),
            &self.sting("infra.worktrees/only-src"),
        );
        let infra = self.sting("infra");
        git(
            &infra,
            &[
                "worktree",
                "repair",
                self.sting("infra/.worktrees/nest").to_str().unwrap(),
                self.sting("infra.worktrees/feat").to_str().unwrap(),
                self.sting("infra.worktrees/only-src").to_str().unwrap(),
            ],
        );
        git(
            &infra,
            &[
                "worktree",
                "remove",
                "--force",
                self.sting("infra.worktrees/only-src").to_str().unwrap(),
            ],
        );
        let dest_only = self.sting("infra.worktrees/dest-only");
        git(
            &infra,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "dest-work",
                dest_only.to_str().unwrap(),
                "v1",
            ],
        );
        fs::write(dest_only.join("sting.txt"), b"sting's own commit\n").unwrap();
        git(&dest_only, &["add", "."]);
        git(&dest_only, &["commit", "--quiet", "-m", "sting work"]);
        fs::write(dest_only.join("dirty.txt"), b"sting's uncommitted work\n").unwrap();
        git(
            &infra,
            &["update-ref", "refs/heads/diverge", "refs/heads/dest-work"],
        );
        fs::write(
            infra.join("node_modules/pkg/sting-build.js"),
            b"built on sting\n",
        )
        .unwrap();
        copy_tree(&self.neo("lamesh"), &self.sting("lamesh"));
        for branch in ["chore/one", "chore/two", "chore/three"] {
            git(&self.sting("lamesh"), &["branch", "-D", branch]);
        }
    }

    /// The plan, as the GFTB lane's gen-items.sh writes it, with the live
    /// paths as destinations.
    pub fn plan_rows(&self) -> Vec<(PathBuf, PathBuf, Option<PathBuf>)> {
        let infra = self.sting("infra");
        vec![
            (self.neo("infra"), infra.clone(), Some(infra.clone())),
            (
                self.neo("infra/.worktrees/nest"),
                infra.clone(),
                Some(self.sting("infra/.worktrees/nest")),
            ),
            (
                self.neo("infra.worktrees/feat"),
                infra.clone(),
                Some(self.sting("infra.worktrees/feat")),
            ),
            (
                self.neo("infra.worktrees/only-src"),
                infra,
                Some(self.sting("infra.worktrees/only-src")),
            ),
            (
                self.neo("lamesh"),
                self.sting("lamesh"),
                Some(self.sting("lamesh")),
            ),
            (
                self.neo("sshrepo"),
                self.sting("sshrepo"),
                Some(self.sting("sshrepo")),
            ),
            (
                self.neo("preservation.git"),
                self.sting("preservation.git"),
                None,
            ),
        ]
    }
}

/// One entry's `lstat` identity: path, inode, mtime (s, ns), ctime (s, ns),
/// size, mode.
pub type Lstat = (PathBuf, u64, i64, i64, i64, i64, u64, u32);

/// Every entry under `root` but `.git` (a directory or a gitfile), with its
/// full `lstat` identity: what an adopt must leave exactly as it was
/// (P34-style).
pub fn freeze(root: &Path) -> Vec<Lstat> {
    use std::os::unix::fs::MetadataExt as _;
    fn walk(root: &Path, at: &Path, out: &mut Vec<Lstat>) {
        let mut entries: Vec<_> = fs::read_dir(at)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            out.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                meta.ino(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
                meta.size(),
                meta.mode(),
            ));
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// The same identity for every entry under `root`, `.git` included: what a
/// refused adopt or `estate-diff` must leave exactly as it was.
pub fn freeze_all(root: &Path) -> Vec<(PathBuf, u64, i64, i64, i64, u64, u32)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        let mut entries: Vec<_> = fs::read_dir(&at)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = fs::symlink_metadata(&path).unwrap();
            out.push((
                path.clone(),
                meta.ino(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime_nsec(),
                meta.size(),
                meta.mode(),
            ));
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    out.sort();
    out
}

/// What `git` shows of a checkout: HEAD (symbolic and oid), the index
/// listing and the porcelain status.
pub fn view(worktree: &Path) -> (String, String, String, String) {
    let symbolic = git_command(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&symbolic.stdout).trim().to_owned(),
        git(worktree, &["rev-parse", "HEAD"]).trim().to_owned(),
        git(worktree, &["ls-files", "--stage"]),
        git(
            worktree,
            &["status", "--porcelain=v1", "-uall", "--ignored"],
        ),
    )
}

/// Native refs, bulkload's namespaces left out.
pub fn refs(repo: &Path) -> Vec<(String, String)> {
    git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter(|line| !line.starts_with("refs/carry"))
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}
