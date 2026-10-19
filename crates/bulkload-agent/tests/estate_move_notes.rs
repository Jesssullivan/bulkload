//! `estate-move`'s notes phase (2026-10-19; operator: "NOTHING is EVER
//! excluded"): what `estate-move-source` notes and does not plan as an item
//! is moved by `estate-move`, not reported `not_moved`. Through the binary,
//! on this host (`-`) and over an `ssh` stand-in (`pull` and `serve`):
//!
//! - loose files at the top of the root (`clockdiag-receipt.txt`, equal at
//!   the destination: adopted; `notes.md`, differing there: landed beside
//!   as `notes.md.LABEL`, the live file untouched) and a `*.worktrees`
//!   directory's log, each one `--file` session;
//! - a directory tree holding no repository (`archive`), one tree session
//!   into the live `archive` the destination already has: its own file
//!   kept, a differing file landed beside, a symlink inside carried as a
//!   link, an empty directory carried;
//! - a symlink note (`lab-link`), carried as the link itself (its target
//!   bytes), never followed;
//! - a prunable worktree whose directory lost its `.git`, carried as files
//!   (custody), never registered again;
//! - an unreadable directory and a nested repository held as custody only:
//!   typed notes pending review (`SOURCE_ROOT_UNREADABLE`,
//!   `GIT_NEST_CUSTODY_ONLY`), the only `not_moved` left, until
//!   `closure-dispose --source-notes` reviews them and the gate passes;
//! - an unchanged rerun of both halves reads 0 source bytes for the notes
//!   (R25) and closes as before.

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
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "neo-20261019";

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn both(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn agent(args: &[OsString], first: Option<&Path>) -> Ran {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
    command
        .arg("--min-free-percent=0")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    if let Some(first) = first {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut path = OsString::from(first);
        path.push(":");
        path.push(inherited);
        command.env("PATH", path);
    }
    let out = command.output().unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn os(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

fn git(repo: &Path, args: &[&str]) {
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
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo(path: &Path, file: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "--quiet", "-m", "c0"]);
}

// A line's `key=value` words.
fn words(line: &str) -> BTreeMap<String, String> {
    line.split(' ')
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

// The first line starting with `prefix`, as words.
fn line(stdout: &str, prefix: &str) -> BTreeMap<String, String> {
    words(
        stdout
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix:?} line:\n{stdout}")),
    )
}

// Each `estate-move note=` line's identity by its `what`.
fn note_ids(stdout: &str, what: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| line.starts_with("estate-move note="))
        .map(words)
        .filter(|words| words["what"] == what)
        .map(|words| words["note"].clone())
        .collect()
}

struct World {
    base: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("bulkload-move-notes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Self {
            base: fs::canonicalize(&base).unwrap(),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let locked = self.base.join("src/git/locked");
        if locked.exists() {
            let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The source estate: one repository with a linked worktree in a
/// `*.worktrees` directory and a nested repository, and every note shape.
/// Returns (src, dst).
fn estate(world: &World, unreadable: bool) -> (PathBuf, PathBuf) {
    let src = world.base.join("src/git");
    let dst = world.base.join("dst/git");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    let lab = src.join("lab");
    repo(&lab, "l.txt");
    git(
        &lab,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt1",
            "../lab.worktrees/wt1",
        ],
    );
    fs::write(src.join("lab.worktrees/run.log"), b"run log\n").unwrap();
    // A nested repository the capture holds as custody only.
    repo(&lab.join("vendor/inner"), "i.txt");
    // A prunable worktree: its directory is there, its `.git` is gone.
    git(
        &lab,
        &["worktree", "add", "--quiet", "-b", "kept", "../kept-wt"],
    );
    fs::remove_file(src.join("kept-wt/.git")).unwrap();
    fs::write(src.join("kept-wt/notes.txt"), b"kept notes\n").unwrap();
    // Loose files at the top of the root.
    fs::write(src.join("clockdiag-receipt.txt"), b"receipt\n").unwrap();
    fs::write(src.join("notes.md"), b"source notes\n").unwrap();
    // A tree holding no repository.
    fs::create_dir_all(src.join("archive/sub")).unwrap();
    fs::create_dir_all(src.join("archive/empty")).unwrap();
    fs::write(src.join("archive/a.txt"), b"source a\n").unwrap();
    let binary: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    fs::write(src.join("archive/sub/b.bin"), &binary).unwrap();
    std::os::unix::fs::symlink("../a.txt", src.join("archive/sub/link-in")).unwrap();
    // A symlink beside the repositories.
    std::os::unix::fs::symlink("lab", src.join("lab-link")).unwrap();
    if unreadable {
        fs::create_dir(src.join("locked")).unwrap();
        fs::write(src.join("locked/secret.txt"), b"x\n").unwrap();
        fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    }
    // The destination already holds an equal receipt, its own notes.md,
    // and its own archive with a differing a.txt and a file of its own.
    fs::write(dst.join("clockdiag-receipt.txt"), b"receipt\n").unwrap();
    fs::write(dst.join("notes.md"), b"destination notes\n").unwrap();
    fs::create_dir_all(dst.join("archive")).unwrap();
    fs::write(dst.join("archive/a.txt"), b"destination a\n").unwrap();
    fs::write(dst.join("archive/mine.txt"), b"destination's own\n").unwrap();
    (src, dst)
}

fn source_pass(world: &World, src: &Path, dst: &Path) -> Ran {
    let map = format!("{}={}", src.display(), dst.display());
    let sourced = agent(
        &os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &world.base.join("move-src"),
            &src,
        ]),
        None,
    );
    assert!(sourced.ok, "{}", sourced.both());
    sourced
}

fn move_pass(world: &World, host: &str, extra: &[OsString], ssh: Option<&Path>) -> Ran {
    let mut args = os(&[&"estate-move", &"--label", &LABEL, &"--source", &"neo"]);
    args.extend(extra.iter().cloned());
    if host != "-" {
        args.extend(os(&[
            &"--remote-executable",
            &env!("CARGO_BIN_EXE_bulkload-agent"),
        ]));
    }
    args.extend(os(&[
        &host,
        &world.base.join("move-src"),
        &world.base.join("move-dst"),
    ]));
    agent(&args, ssh)
}

/// The carried notes, as the destination holds them after a pass.
fn assert_landed(src: &Path, dst: &Path, both: &str) {
    // Equal: adopted. Differing: beside, the live file untouched.
    assert_eq!(
        fs::read(dst.join("clockdiag-receipt.txt")).unwrap(),
        b"receipt\n"
    );
    assert_eq!(
        fs::read(dst.join("notes.md")).unwrap(),
        b"destination notes\n",
        "{both}"
    );
    assert_eq!(
        fs::read(dst.join(format!("notes.md.{LABEL}"))).unwrap(),
        b"source notes\n",
        "{both}"
    );
    assert_eq!(
        fs::read(dst.join("lab.worktrees/run.log")).unwrap(),
        b"run log\n",
        "{both}"
    );
    // The tree, unioned into the live archive.
    assert_eq!(
        fs::read(dst.join("archive/a.txt")).unwrap(),
        b"destination a\n"
    );
    assert_eq!(
        fs::read(dst.join(format!("archive/a.txt.{LABEL}"))).unwrap(),
        b"source a\n",
        "{both}"
    );
    assert_eq!(
        fs::read(dst.join("archive/mine.txt")).unwrap(),
        b"destination's own\n"
    );
    assert_eq!(
        fs::read(dst.join("archive/sub/b.bin")).unwrap(),
        fs::read(src.join("archive/sub/b.bin")).unwrap()
    );
    assert_eq!(
        fs::read_link(dst.join("archive/sub/link-in")).unwrap(),
        Path::new("../a.txt")
    );
    assert!(dst.join("archive/empty").is_dir(), "{both}");
    // The symlink note: the link itself, its target bytes, never followed.
    let link = dst.join("lab-link");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{both}"
    );
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("lab"));
    // The prunable worktree: its files, as files; never registered again.
    assert_eq!(
        fs::read(dst.join("kept-wt/notes.txt")).unwrap(),
        b"kept notes\n",
        "{both}"
    );
    assert!(!dst.join("kept-wt/.git").exists());
}

#[test]
fn notes_are_carried_held_typed_and_a_rerun_reads_nothing() {
    let world = World::new("local");
    let (src, dst) = estate(&world, true);
    let receipt_inode = std::os::unix::fs::MetadataExt::ino(
        &fs::symlink_metadata(dst.join("clockdiag-receipt.txt")).unwrap(),
    );
    let sourced = source_pass(&world, &src, &dst);
    let inventory = line(&sourced.stdout, "estate-move-source phase=inventory");
    assert_eq!(inventory["items"], "2", "{}", sourced.stdout);
    assert_eq!(inventory["symlinks"], "1", "{}", sourced.stdout);
    assert_eq!(inventory["not_git_trees"], "1", "{}", sourced.stdout);
    assert_eq!(inventory["unreadable"], "1", "{}", sourced.stdout);
    // The prunable worktree's note counts its tree's entries (its checkout
    // and the file added there).
    assert_eq!(inventory["prunable"], "2", "{}", sourced.stdout);
    let notes_file =
        fs::read_to_string(world.base.join("move-src/carry/move-source.notes")).unwrap();
    // Each note names its live path, mapped as an item's is.
    assert!(
        notes_file.contains(&format!(
            "not-git-file 1 none {}\t{}",
            src.join("notes.md").display(),
            dst.join("notes.md").display()
        )),
        "{notes_file}"
    );
    assert!(notes_file.contains("nested 1 none "), "{notes_file}");

    // --plan-only: the notes' plan, nothing carried.
    let planned = move_pass(&world, "-", &os(&[&"--plan-only"]), None);
    assert!(planned.ok, "{}", planned.both());
    let plan = line(&planned.stdout, "estate-move notes-plan");
    assert_eq!(plan["carry-file"], "3", "{}", planned.stdout);
    assert_eq!(plan["carry-link"], "1", "{}", planned.stdout);
    assert_eq!(plan["carry-tree"], "2", "{}", planned.stdout);
    assert_eq!(plan["held"], "2", "{}", planned.stdout);
    assert!(!dst.join("lab-link").exists());

    // One pass: every carriable note carried; the two held notes are all
    // that is not moved, and they hold the gate pending review.
    let moved = move_pass(&world, "-", &[], None);
    let both = moved.both();
    assert!(!moved.ok, "{both}");
    assert_landed(&src, &dst, &both);
    // The equal receipt was adopted by its content proof, not rewritten.
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(
            &fs::symlink_metadata(dst.join("clockdiag-receipt.txt")).unwrap()
        ),
        receipt_inode,
        "{both}"
    );
    let first = line(&moved.stdout, "estate-move closure ");
    assert_eq!(first["notes"], "8", "{both}");
    assert_eq!(first["notes_carried"], "6", "{both}");
    assert_eq!(first["not_moved"], "2", "{both}");
    assert_eq!(first["not_moved_dispositioned"], "0", "{both}");
    assert_eq!(first["unaccounted"], "0", "{both}");
    assert_eq!(first["refused_pending_review"], "2", "{both}");
    assert!(
        first["refusals"].contains("GIT_NEST_CUSTODY_ONLY:1")
            && first["refusals"].contains("SOURCE_ROOT_UNREADABLE:1"),
        "{both}"
    );
    assert!(
        moved.stdout.contains("estate-move phase=notes outcome=ok"),
        "{both}"
    );
    assert!(moved.stdout.contains("estate-move gate=fail"), "{both}");
    let summary = line(&moved.stdout, "estate-move notes notes=");
    assert_eq!(summary["carried"], "6", "{both}");
    assert_ne!(summary["source_bytes_read"], "0", "{both}");
    let report = fs::read_to_string(world.base.join("move-dst/closure.json")).unwrap();
    assert!(report.contains("\"kind\":\"source-note\""), "{report}");

    // The operator reviews the two held notes by their identities.
    let state = world.base.join("move-dst");
    let ledger = world.base.join("dispositions");
    for (what, code) in [
        ("unreadable", "SOURCE_ROOT_UNREADABLE"),
        ("nested", "GIT_NEST_CUSTODY_ONLY"),
    ] {
        let ids = note_ids(&moved.stdout, what);
        assert_eq!(ids.len(), 1, "{what}: {both}");
        let reviewed = agent(
            &os(&[
                &"closure-dispose",
                &"--source-notes",
                &state.join("notes.rows"),
                &ledger,
                &state.join("carry/plan.postcard"),
                &state.join("carry/corpus"),
                &"neo",
                &ids[0],
                &code,
                &"accept",
                &"operator",
                &"2026-10-19",
                &state.join("private"),
            ]),
            None,
        );
        assert!(reviewed.ok, "{what}: {}", reviewed.both());
    }

    // An unchanged rerun of both halves: 0 source bytes for the notes, the
    // destination as it was, and the gate passes with the reviews.
    source_pass(&world, &src, &dst);
    let again = move_pass(&world, "-", &os(&[&"--dispositions", &ledger]), None);
    let both = again.both();
    assert_landed(&src, &dst, &both);
    let summary = line(&again.stdout, "estate-move notes notes=");
    assert_eq!(summary["source_bytes_read"], "0", "{both}");
    assert_eq!(summary["carried"], "6", "{both}");
    let second = line(&again.stdout, "estate-move closure ");
    assert_eq!(second["not_moved"], "2", "{both}");
    assert_eq!(second["not_moved_dispositioned"], "2", "{both}");
    assert_eq!(second["refused_pending_review"], "0", "{both}");
    assert_eq!(second["unaccounted"], "0", "{both}");
    assert_eq!(second["transfer_rows"], first["transfer_rows"], "{both}");
    assert!(again.stdout.contains("estate-move gate=pass"), "{both}");
    assert!(again.ok, "{both}");
    // Nothing landed beside twice.
    assert!(!dst.join(format!("notes.md.{LABEL}.{LABEL}")).exists());
    assert!(!dst.join(format!("archive.{LABEL}")).exists());
}

/// An `ssh` stand-in: drops ssh's own options and the host, then runs the
/// remote command here, its stdin and stdout the session's.
fn fake_ssh(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let ssh = dir.join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nshift 2\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
}

// The same estate moved from `fakehost` over the `ssh` stand-in: every note
// goes through `pull` and `serve` (`serve --file` for a file and the link),
// and a rerun reads nothing.
#[test]
fn notes_over_pull_are_carried_and_a_rerun_reads_nothing() {
    let world = World::new("pull");
    let (src, dst) = estate(&world, false);
    source_pass(&world, &src, &dst);
    let bin = world.base.join("fake-bin");
    fake_ssh(&bin);
    let moved = move_pass(&world, "fakehost", &[], Some(&bin));
    let both = moved.both();
    assert_landed(&src, &dst, &both);
    assert!(!both.contains("TRANSPORT_LOST"), "{both}");
    let first = line(&moved.stdout, "estate-move closure ");
    assert_eq!(first["notes_carried"], "6", "{both}");
    // Only the nested repository is left, held typed.
    assert_eq!(first["not_moved"], "1", "{both}");
    assert_eq!(first["unaccounted"], "0", "{both}");
    // Each note's serve state is where the destination half named it.
    assert_eq!(
        fs::read_dir(world.base.join("move-src/serve/notes"))
            .unwrap()
            .count(),
        6,
        "{both}"
    );
    source_pass(&world, &src, &dst);
    let again = move_pass(&world, "fakehost", &[], Some(&bin));
    let both = again.both();
    assert_landed(&src, &dst, &both);
    let summary = line(&again.stdout, "estate-move notes notes=");
    assert_eq!(summary["source_bytes_read"], "0", "{both}");
    assert_eq!(line(&again.stdout, "estate-move closure "), first, "{both}");
}
