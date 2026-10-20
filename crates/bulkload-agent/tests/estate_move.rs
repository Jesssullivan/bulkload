//! SPEC rule 2 (reset 2026-10-20) on `estate-move-source` and
//! `estate-move`, through the binary, on this host (`-`). The operator,
//! 2026-10-20: "this pattern can never happen again" (every item's private
//! state held a capture bundle and a repository copy of its live
//! repository, beside a corpus). Neither machine ever gets a staging copy,
//! and bulkload's private state holds ledgers and resume marks only:
//!
//! - the source half inventories and plans, then refuses its capture
//!   `STATE_PAYLOAD_LEFT` before it writes: no bundle, repository copy or
//!   corpus on the source, and its state line measures zero payload within
//!   the resume-mark bound;
//! - the destination half refuses its pull of the corpus the same way,
//!   before any transport, and touches no live path;
//! - a state holding a retired staging format (a corpus, a capture attempt
//!   directory with its bundle and repository copy, a landing area) is
//!   refused `STATE_FORMAT_RETIRED` at startup by either half, each one
//!   named, and nothing is written;
//! - any payload a pass leaves in its state is named and refused
//!   `STATE_PAYLOAD_LEFT`.
//!
//! The P-MOVE-1..3 landing proofs this file held (M2 item 7) wait for the
//! streamed capture, which brings them back over live-store transfers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use bulkload_agent::state_payload as payload;

const LABEL: &str = "neo-20261020";

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

fn repo(path: &Path, file: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "--quiet", "-m", "c0"]);
}

struct World {
    base: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("bulkload-move-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/git")).unwrap();
        fs::create_dir_all(base.join("dst/git")).unwrap();
        Self {
            base: fs::canonicalize(&base).unwrap(),
        }
    }

    fn src(&self) -> PathBuf {
        self.base.join("src/git")
    }

    fn dst(&self) -> PathBuf {
        self.base.join("dst/git")
    }

    fn source_state(&self) -> PathBuf {
        self.base.join("move-src")
    }

    fn state(&self) -> PathBuf {
        self.base.join("move-dst")
    }

    /// Two repositories with work in progress (a dirty tracked file, an
    /// untracked and an ignored one, a stash) and a linked worktree.
    fn estate(&self) {
        let src = self.src();
        let land = src.join("land");
        repo(&land, "a.txt");
        fs::write(land.join(".gitignore"), "ignored.bin\n").unwrap();
        git(&land, &["add", ".gitignore"]);
        git(&land, &["commit", "--quiet", "-m", "c1"]);
        fs::write(land.join("a.txt"), "a, edited\n").unwrap();
        git(&land, &["stash", "push", "--quiet", "-m", "parked"]);
        fs::write(land.join("a.txt"), "a, dirty\n").unwrap();
        fs::write(land.join("new.txt"), "untracked\n").unwrap();
        fs::write(land.join("ignored.bin"), vec![7u8; 4096]).unwrap();
        git(
            &land,
            &["worktree", "add", "--quiet", "-b", "wt", "../land-wt"],
        );
        repo(&src.join("other"), "o.txt");
    }

    fn source(&self) -> Ran {
        let map = format!("{}={}", self.src().display(), self.dst().display());
        agent(&os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &self.source_state(),
            &self.src(),
        ]))
    }

    fn moved(&self) -> Ran {
        agent(&os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"-",
            &self.source_state(),
            &self.state(),
        ]))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

// The `VERB phase=state` line's `key=value` words.
fn state_line(stdout: &str, verb: &str) -> BTreeMap<String, String> {
    let prefix = format!("{verb} phase=state ");
    let line = stdout
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line:\n{stdout}"));
    line.split(' ')
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

// Every path under `root`, relative, sorted (directories too).
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read {
            let path = entry.unwrap().path();
            found.push(path.strip_prefix(root).unwrap().to_path_buf());
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            }
        }
    }
    found.sort();
    found
}

// No staging format anywhere under `state`, and nothing the guard counts
// as payload; its ledgers within the resume-mark bound for `items`.
fn holds_no_payload(state: &Path, items: u64) {
    for rel in tree(state) {
        let name = rel.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            !name.ends_with(".bundle") && name != "repository.git" && name != "corpus",
            "{} holds {}",
            state.display(),
            rel.display()
        );
    }
    assert!(payload::retired(state).unwrap().is_empty());
    let measured = payload::measure(state).unwrap();
    assert_eq!(measured.payload_files, 0, "{measured:?}");
    assert!(
        measured.ledger_bytes <= payload::resume_mark_bound(items),
        "{measured:?}"
    );
}

#[test]
fn the_source_half_plans_and_refuses_to_stage_its_capture() {
    let world = World::new("source");
    world.estate();
    let ran = world.source();
    assert!(!ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: STATE_PAYLOAD_LEFT"),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stdout
            .contains("estate-move-source phase=inventory outcome=ok items=3 new=3 planned=3"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains("estate-move-source phase=capture outcome=refused:STATE_PAYLOAD_LEFT"),
        "{}",
        ran.stdout
    );
    let words = state_line(&ran.stdout, "estate-move-source");
    assert_eq!(words["outcome"], "ok", "{}", ran.stdout);
    assert_eq!(words["state_payload_bytes"], "0", "{}", ran.stdout);
    let state = world.source_state();
    assert!(state.join("carry/plan.postcard").is_file());
    // The pass did not finish: the destination half refuses its marks.
    assert!(!state.join("carry/move-source.done").exists());
    holds_no_payload(&state, 3);
    // The plan is a ledger: a second pass appends nothing and refuses the
    // same way, its state still free of payload.
    let again = world.source();
    assert!(!again.ok);
    assert!(
        again.stdout.contains("items=3 new=0 planned=3"),
        "{}",
        again.stdout
    );
    holds_no_payload(&state, 3);
}

#[test]
fn the_destination_half_refuses_to_pull_a_corpus_and_touches_no_live_path() {
    let world = World::new("destination");
    world.estate();
    let _ = world.source();
    fs::create_dir_all(world.dst().join("other")).unwrap();
    fs::write(world.dst().join("other/mine.txt"), b"destination's\n").unwrap();
    let live = tree(&world.dst());
    for extra in [&[][..], &["--plan-only"][..]] {
        let mut args = os(&[&"estate-move", &"--label", &LABEL, &"--source", &"neo"]);
        args.extend(extra.iter().map(OsString::from));
        args.extend(os(&[&"-", &world.source_state(), &world.state()]));
        let ran = agent(&args);
        assert!(!ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr.contains("refused: STATE_PAYLOAD_LEFT"),
            "{}",
            ran.stderr
        );
        assert!(
            ran.stdout
                .contains("estate-move phase=pull outcome=refused:STATE_PAYLOAD_LEFT"),
            "{}",
            ran.stdout
        );
        assert!(
            !ran.stdout.contains("estate-move phase=plan"),
            "{}",
            ran.stdout
        );
        let words = state_line(&ran.stdout, "estate-move");
        assert_eq!(words["outcome"], "ok", "{}", ran.stdout);
        assert_eq!(words["state_payload_bytes"], "0", "{}", ran.stdout);
        assert_eq!(tree(&world.dst()), live, "a live path changed");
        holds_no_payload(&world.state(), 3);
        holds_no_payload(&world.source_state(), 3);
    }
}

#[test]
fn a_retired_staging_format_is_named_and_refused_at_startup() {
    let world = World::new("retired");
    world.estate();
    let source_state = world.source_state();
    for (rel, bytes) in [
        ("carry/corpus/item-head.bundle", &b"bundle"[..]),
        ("carry/corpus/item.capture", b"record"),
        ("private/item-key-0/capture.bundle", b"bundle"),
        (
            "private/item-key-0/repository.git/HEAD",
            b"ref: refs/heads/main\n",
        ),
    ] {
        let path = source_state.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let before = tree(&source_state);
    let ran = world.source();
    assert!(!ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: STATE_FORMAT_RETIRED"),
        "{}",
        ran.stderr
    );
    for rel in ["carry/corpus", "private/item-key-0"] {
        let line = format!(
            "estate-move-source state=retired path={:?}",
            source_state.join(rel)
        );
        assert!(ran.stdout.contains(&line), "want {line}:\n{}", ran.stdout);
    }
    assert!(!ran.stdout.contains("phase=inventory"), "{}", ran.stdout);
    // Nothing written, reused or grown: not even the lock or the plan.
    assert_eq!(tree(&source_state), before);

    // The destination half names both states' (its own and, on this host,
    // the source half's) and refuses before anything else.
    let state = world.state();
    let landing = state.join("landing/.claude/projects/p/s.jsonl");
    fs::create_dir_all(landing.parent().unwrap()).unwrap();
    fs::write(&landing, b"{}\n").unwrap();
    let moved = world.moved();
    assert!(!moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stderr.contains("refused: STATE_FORMAT_RETIRED"),
        "{}",
        moved.stderr
    );
    for path in [
        state.join("landing"),
        source_state.join("carry/corpus"),
        source_state.join("private/item-key-0"),
    ] {
        let line = format!("estate-move state=retired path={path:?}");
        assert!(
            moved.stdout.contains(&line),
            "want {line}:\n{}",
            moved.stdout
        );
    }
    assert!(!state.join("move.ledger").exists());
    assert!(!state.join("move.lock").exists());
}

#[test]
fn payload_a_pass_leaves_in_its_state_is_named_and_refused() {
    let world = World::new("left");
    // No repository under the root: the pass plans nothing and completes,
    // so only the stray file can fail it.
    let plain = world.src().join("plain");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("f.txt"), b"f\n").unwrap();
    let state = world.source_state();
    let stray = state.join("carry/seat-copy.bin");
    fs::create_dir_all(stray.parent().unwrap()).unwrap();
    // The state's directories as the verb makes them: private (0700).
    for dir in [&state, &state.join("carry")] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(&stray, vec![1u8; 1000]).unwrap();
    let ran = world.source();
    assert!(!ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: STATE_PAYLOAD_LEFT"),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stdout
            .contains("estate-move-source phase=capture outcome=ok captured=0 refused=0"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains(&format!("estate-move-source state=payload path={stray:?}")),
        "{}",
        ran.stdout
    );
    let words = state_line(&ran.stdout, "estate-move-source");
    assert_eq!(words["outcome"], "refused:STATE_PAYLOAD_LEFT");
    assert_eq!(words["state_payload_bytes"], "1000");
    assert_eq!(words["state_payload_files"], "1");
    // Removed by literal path, the next pass completes.
    fs::remove_file(&stray).unwrap();
    let clean = world.source();
    assert!(clean.ok, "{}\n{}", clean.stdout, clean.stderr);
    assert_eq!(
        state_line(&clean.stdout, "estate-move-source")["state_payload_bytes"],
        "0"
    );
    holds_no_payload(&state, 0);
}
