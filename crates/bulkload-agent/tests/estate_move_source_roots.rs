//! `estate-move-source` through the binary, on its state tree and its
//! roots:
//!
//! - #276 (a): the pass makes `MOVE_STATE/serve` and `MOVE_STATE/serve/carry`
//!   (0700) beside `carry/`, so the destination half's first cross-host
//!   `pull` and its carry phase find the directories their `serve` states
//!   are named in;
//! - SPEC rule 2 (reset 2026-10-20), over a transport: `estate-move` to a
//!   host that is not `-` refuses its corpus pull `STATE_PAYLOAD_LEFT`
//!   before it starts any transport (an `ssh` stand-in first on `PATH`
//!   records that it never ran). The #276 serve-state proofs over `ssh`
//!   wait for the streamed capture;
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
    for name in ["carry", "serve", "serve/carry"] {
        let dir = state.join(name);
        assert!(dir.is_dir(), "{name} missing:\n{}", ran.stdout);
        assert_eq!(mode(&dir), 0o700, "{name}");
    }
    // Nothing of the pass's own lands in `serve/`: it is the destination
    // half's to fill, one `pull` state per name (and `carry/` per item).
    assert_eq!(fs::read_dir(state.join("serve")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(state.join("serve/carry")).unwrap().count(), 0);
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
/// remote command here through a shell, as ssh hands it to the remote
/// one, its stdin and stdout the session's.
fn fake_ssh(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let ssh = dir.join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nshift 2\nexec sh -c \"$*\"\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
}

// SPEC rule 2 (reset 2026-10-20), over a transport: the pull lands only
// the source half's plan, marks and item snapshots. A corpus a source half
// from before the reset left is never pulled (excluded, its bytes never
// read), and the item lands over the same transport.
#[test]
fn a_remote_pass_pulls_only_ledgers_and_lands_over_ssh() {
    let world = World::new("remote");
    let src = world.path("src");
    fs::create_dir_all(&src).unwrap();
    fs::rename(world.path("live/repo"), src.join("land")).unwrap();
    let source_state = world.path("move-src");
    let state = world.path("move-dst");
    let dst = world.path("dst");
    fs::create_dir_all(&dst).unwrap();
    let map = format!("{}={}", src.display(), dst.display());
    let _ = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src,
    ]));
    let old = source_state.join("carry/corpus/old-head.bundle");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, vec![9u8; 2048]).unwrap();
    fake_ssh(&world.path("fake-bin"));
    let moved = agent_with_path(
        &os(&[
            &"estate-move",
            &"--label",
            &"neo-20261020",
            &"--source",
            &"neo",
            &"--remote-executable",
            &env!("CARGO_BIN_EXE_bulkload-agent"),
            &"fakehost",
            &source_state,
            &state,
        ]),
        Some(&world.path("fake-bin")),
    );
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.contains("estate-move phase=pull outcome=ok"),
        "{both}"
    );
    // The streamed fetch relayed over ssh: the item lands from the live
    // source repository, its objects fetched through the remote
    // `upload-pack`.
    assert!(
        moved.stdout.contains("estate-move phase=apply outcome=ok"),
        "{both}"
    );
    let head = |repo: &Path| {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(head(&dst.join("land")), head(&src.join("land")), "{both}");
    assert!(
        moved
            .stdout
            .contains("estate-move phase=state outcome=ok state_payload_bytes=0"),
        "{both}"
    );
    assert!(state.join("carry/plan.postcard").is_file(), "{both}");
    assert!(!state.join("carry/corpus").exists(), "{both}");
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
