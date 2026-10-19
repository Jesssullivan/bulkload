//! `estate-move-source` through the binary, on its state tree and its
//! roots:
//!
//! - #276 (a): the pass makes `MOVE_STATE/serve` and `MOVE_STATE/serve/carry`
//!   (0700) beside `carry/` and `private/`, so the destination half's first
//!   cross-host `pull` and its carry phase find the directories their
//!   `serve` states are named in;
//! - #276, both sites, over a transport: `estate-move` to a host that is not
//!   `-` runs every `pull` through `ssh`, here a stand-in first on `PATH`
//!   that runs the remote command on this host. Over a fresh
//!   `SOURCE_MOVE_STATE` the corpus pull (`serve/corpus`) and the carry phase
//!   (`serve/carry/ITEM`) both end `ok`; with `serve/` removed (a source half
//!   from before the fix) `serve` makes every missing parent itself (#276 b);
//! - #277: a `SOURCE_ROOT` gone by the time the pass reaches it is a typed
//!   `source-vanished` note (`SOURCE_ROOT_MISSING`) and the pass continues
//!   with the rest; a pass with no usable root at all refuses, typed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn agent(args: &[OsString]) -> Ran {
    agent_with_path(args, None)
}

/// [`agent`], with `first` ahead of the inherited `PATH`.
fn agent_with_path(args: &[OsString], first: Option<&Path>) -> Ran {
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

struct World {
    base: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-move-source-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("live/repo")).unwrap();
        let base = fs::canonicalize(&base).unwrap();
        let repo = base.join("live/repo");
        git(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(repo.join("f"), "f\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "--quiet", "-m", "c0"]);
        Self { base }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_pass_makes_the_serve_directory_beside_carry_and_private() {
    let world = World::new("serve");
    let state = world.path("state");
    let ran = agent(&os(&[&"estate-move-source", &state, &world.path("live")]));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    for name in ["carry", "private", "serve", "serve/carry", "serve/notes"] {
        let dir = state.join(name);
        assert!(dir.is_dir(), "{name} missing:\n{}", ran.stdout);
        assert_eq!(mode(&dir), 0o700, "{name}");
    }
    // Nothing of the pass's own lands in `serve/`: it is the destination
    // half's to fill, one `pull` state per name (and `carry/` per item,
    // `notes/` per source note, 2026-10-19).
    assert_eq!(fs::read_dir(state.join("serve")).unwrap().count(), 2);
    assert_eq!(fs::read_dir(state.join("serve/carry")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(state.join("serve/notes")).unwrap().count(), 0);
}

#[test]
fn a_vanished_root_is_a_note_and_the_pass_continues() {
    let world = World::new("vanished");
    let state = world.path("state");
    let gone = world.path("gone");
    let ran = agent(&os(&[
        &"estate-move-source",
        &state,
        &gone,
        &world.path("live"),
    ]));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    let note = format!(
        "estate-move-source note=source-vanished path={gone:?} count=1 code=SOURCE_ROOT_MISSING"
    );
    assert!(
        ran.stdout.contains(&note),
        "want {note:?} in\n{}",
        ran.stdout
    );
    let inventory = ran
        .stdout
        .lines()
        .find(|line| line.starts_with("estate-move-source phase=inventory "))
        .unwrap_or_else(|| panic!("no inventory line:\n{}", ran.stdout));
    assert!(inventory.contains(" items=1 "), "{inventory}");
    assert!(inventory.contains(" source_vanished=1 "), "{inventory}");
    assert!(
        ran.stdout
            .contains("estate-move-source phase=capture outcome=ok captured=1"),
        "{}",
        ran.stdout
    );
    assert!(state.join("carry/plan.postcard").is_file());
}

#[test]
fn a_pass_with_no_usable_root_refuses_typed() {
    let world = World::new("none");
    let state = world.path("state");
    let ran = agent(&os(&[
        &"estate-move-source",
        &state,
        &world.path("gone"),
        &world.path("also-gone"),
    ]));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: SOURCE_ROOT_MISSING"),
        "{}",
        ran.stderr
    );
    assert!(
        !ran.stdout.contains("phase=inventory outcome=ok"),
        "{}",
        ran.stdout
    );
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

/// `live` (lands) and `broken` (no index: its capture is refused, so the
/// carry phase carries it as files), sourced, then moved to `fakehost`
/// over the `ssh` stand-in. `before_move` runs on the source state between
/// the two halves.
fn carry_over_ssh(name: &str, before_move: impl FnOnce(&Path)) {
    let world = World::new(name);
    let src = world.path("src");
    let dst = world.path("dst");
    fs::create_dir_all(src.join("broken")).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::rename(world.path("live/repo"), src.join("land")).unwrap();
    let broken = src.join("broken");
    git(&broken, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(broken.join("b.txt"), "b\n").unwrap();
    git(&broken, &["add", "."]);
    git(&broken, &["commit", "--quiet", "-m", "c0"]);
    fs::remove_file(broken.join(".git/index")).unwrap();
    let source_state = world.path("move-src");
    let state = world.path("move-dst");
    let map = format!("{}={}", src.display(), dst.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    before_move(&source_state);
    let bin = world.path("fake-bin");
    fake_ssh(&bin);
    let moved = agent_with_path(
        &os(&[
            &"estate-move",
            &"--label",
            &"neo-20261019",
            &"--source",
            &"neo",
            &"--remote-executable",
            &env!("CARGO_BIN_EXE_bulkload-agent"),
            &"fakehost",
            &source_state,
            &state,
        ]),
        Some(&bin),
    );
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.contains("estate-move phase=pull outcome=ok"),
        "{both}"
    );
    assert!(
        moved.stdout.contains("estate-move phase=carry outcome=ok"),
        "{both}"
    );
    assert!(!both.contains("TRANSPORT_LOST"), "{both}");
    assert_eq!(
        fs::read(dst.join("broken/b.txt")).unwrap(),
        b"b\n",
        "{both}"
    );
    // Each `serve` state is where the destination half named it, private.
    for level in ["serve", "serve/carry", "serve/corpus"] {
        assert_eq!(mode(&source_state.join(level)), 0o700, "{level}\n{both}");
    }
    let items: Vec<_> = fs::read_dir(source_state.join("serve/carry"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(items.len(), 1, "{items:?}\n{both}");
    assert_eq!(mode(&items[0]), 0o700);
}

// #276, both sites, over a fresh SOURCE_MOVE_STATE.
#[test]
fn the_carry_phase_over_ssh_finds_its_serve_states() {
    carry_over_ssh("carry-fresh", |_| {});
}

// #276 (b): a source half from before the fix made no `serve/`: `serve`
// makes `serve/` and `serve/carry/` itself, 0700, for both sites.
#[test]
fn the_carry_phase_over_ssh_without_a_serve_directory() {
    carry_over_ssh("carry-no-serve", |source_state| {
        fs::remove_dir_all(source_state.join("serve")).unwrap();
    });
}

// #283 review 5: a first pass whose usable roots hold no repository
// completes with an empty plan (never `IO (errno 2)` reading a plan that
// was never written), and its files are counted as not-git.
#[test]
fn a_first_pass_with_no_repository_completes_empty() {
    let world = World::new("norepo");
    let plain = world.path("plain");
    fs::create_dir_all(plain.join("d")).unwrap();
    fs::write(plain.join("d/f.txt"), "f\n").unwrap();
    let state = world.path("state");
    let ran = agent(&os(&[&"estate-move-source", &state, &plain]));
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout
            .contains("estate-move-source phase=inventory outcome=ok items=0 new=0 planned=0"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains(" not_git_files=1 "), "{}", ran.stdout);
    assert!(
        ran.stdout
            .contains("estate-move-source phase=capture outcome=ok captured=0 refused=0"),
        "{}",
        ran.stdout
    );
    assert!(state.join("carry/plan.postcard").is_file());
    assert!(state.join("carry/move-source.done").is_file());
}
