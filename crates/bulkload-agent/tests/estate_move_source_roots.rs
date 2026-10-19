//! `estate-move-source` through the binary, on its state tree and its
//! roots:
//!
//! - #276 (a): the pass makes `MOVE_STATE/serve` (0700) beside `carry/` and
//!   `private/`, so the destination half's first cross-host `pull` finds the
//!   directory its `serve` state is named in;
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
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("--min-free-percent=0")
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
    for name in ["carry", "private", "serve"] {
        let dir = state.join(name);
        assert!(dir.is_dir(), "{name} missing:\n{}", ran.stdout);
        assert_eq!(mode(&dir), 0o700, "{name}");
    }
    // Nothing of the pass's own lands in `serve/`: it is the destination
    // half's to fill, one `pull` state per name.
    assert_eq!(fs::read_dir(state.join("serve")).unwrap().count(), 0);
}

#[test]
fn a_vanished_root_is_a_note_and_the_pass_continues() {
    let world = World::new("vanished");
    let state = world.path("state");
    let gone = world.path("gone");
    let ran = agent(&os(&[&"estate-move-source", &state, &gone, &world.path("live")]));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    let note = format!(
        "estate-move-source note=source-vanished path={gone:?} count=1 code=SOURCE_ROOT_MISSING"
    );
    assert!(ran.stdout.contains(&note), "want {note:?} in\n{}", ran.stdout);
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
    assert!(!ran.stdout.contains("phase=inventory outcome=ok"), "{}", ran.stdout);
}
