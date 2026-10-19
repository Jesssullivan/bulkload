//! P-MOVE-1..3 (M2 item 7, design `2026-10-16-estate-move-design.md`
//! section 3; operator ruling A-20261018-110): `estate-move-source` then
//! `estate-move` through the binary, on this host (`-`), over the in-place
//! lane's shapes:
//!
//! - `land` and its linked worktree `land-wt`, absent at the destination:
//!   landed at their live paths;
//! - `same`: an equal clone at the destination (adopted in place) whose
//!   `topic` branch the destination moved: refs-in creates
//!   `refs/<label>/heads/topic` at the source's value, and the destination's
//!   `topic` and checkout stay as they were;
//! - `diverged`: a destination checkout with its own commit: the capture
//!   lands beside it at `<path>.<label>`, the live checkout untouched;
//! - `broken` and `broken2`: captures refused (no index): carried as files,
//!   at the live path (absent) and beside it (occupied);
//! - `occupied`: a non-Git tree at the live path (a standalone item's):
//!   refused typed (`GIT_DESTINATION_NONREPO_DIFFERS`), untouched;
//! - `bare.git`: a bare repository landed refs-only.
//!
//! Then: closure accounts for every item; a rerun changes no ref and keeps
//! every carry where it went (P-MOVE-2); another label refuses
//! `RECEIPT_BINDING_INVALID`; `--plan-only` writes nothing at a live path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "neo-20261018";

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

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn repo(path: &Path, file: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "--quiet", "-m", "c0"]);
}

fn refs(repo: &Path) -> String {
    git(repo, &["for-each-ref", "--format=%(objectname) %(refname)"])
}

// The `estate-move closure` line's `key=value` words.
fn closure(stdout: &str) -> std::collections::BTreeMap<String, String> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("estate-move closure "))
        .unwrap_or_else(|| panic!("no closure line:\n{stdout}"));
    line.split(' ')
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn one_pass_lands_adopts_besides_carries_and_accounts_for_every_item() {
    let base = std::env::temp_dir().join(format!("bulkload-move-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src = base.join("src/git");
    let dst = base.join("dst/git");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // land + land-wt: absent at the destination.
    repo(&src.join("land"), "a.txt");
    git(
        &src.join("land"),
        &["worktree", "add", "--quiet", "-b", "wt", "../land-wt"],
    );
    // same: an equal clone at the destination, whose topic moved there.
    repo(&src.join("same"), "s.txt");
    git(&src.join("same"), &["branch", "topic"]);
    git(
        &dst,
        &[
            "clone",
            "--quiet",
            "--template=",
            src.join("same").to_str().unwrap(),
            "same",
        ],
    );
    let same = dst.join("same");
    git(&same, &["branch", "topic", "origin/topic"]);
    git(
        &same,
        &["commit", "--quiet", "--allow-empty", "-m", "dest topic"],
    );
    git(&same, &["branch", "-f", "topic", "HEAD"]);
    git(&same, &["reset", "--quiet", "--hard", "HEAD~1"]);
    git(&same, &["remote", "remove", "origin"]);
    // The source's topic moves on too, so the two differ.
    git(&src.join("same"), &["checkout", "--quiet", "topic"]);
    git(
        &src.join("same"),
        &["commit", "--quiet", "--allow-empty", "-m", "src topic"],
    );
    git(&src.join("same"), &["checkout", "--quiet", "main"]);
    let src_topic = git(&src.join("same"), &["rev-parse", "topic"]);
    let dst_topic = git(&same, &["rev-parse", "topic"]);
    assert_ne!(src_topic, dst_topic);
    // diverged: the destination committed on its own.
    repo(&src.join("diverged"), "d.txt");
    git(
        &dst,
        &[
            "clone",
            "--quiet",
            "--template=",
            src.join("diverged").to_str().unwrap(),
            "diverged",
        ],
    );
    git(&dst.join("diverged"), &["remote", "remove", "origin"]);
    git(
        &dst.join("diverged"),
        &["commit", "--quiet", "--allow-empty", "-m", "dest only"],
    );
    let diverged_head = git(&dst.join("diverged"), &["rev-parse", "HEAD"]);
    // broken, broken2: no index, so the capture refuses them.
    for name in ["broken", "broken2"] {
        repo(&src.join(name), "b.txt");
        fs::remove_file(src.join(name).join(".git/index")).unwrap();
    }
    fs::create_dir_all(dst.join("broken2")).unwrap();
    fs::write(dst.join("broken2/mine.txt"), b"destination's\n").unwrap();
    // occupied: a non-Git tree at the live path.
    repo(&src.join("occupied"), "o.txt");
    fs::create_dir_all(dst.join("occupied")).unwrap();
    fs::write(dst.join("occupied/keep.txt"), b"keep\n").unwrap();
    // bare.git: refs-only.
    git(
        base,
        &[
            "init",
            "--quiet",
            "--template=",
            "--bare",
            src.join("bare.git").to_str().unwrap(),
        ],
    );
    git(
        &src.join("land"),
        &[
            "push",
            "--quiet",
            src.join("bare.git").to_str().unwrap(),
            "main",
        ],
    );
    // A plain file in a root is no repository: noted, not moved.
    fs::write(src.join("README.txt"), b"not in any repository\n").unwrap();
    let same_refs = refs(&same);
    let diverged_refs = refs(&dst.join("diverged"));

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src.display(), dst.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    assert!(
        sourced
            .stdout
            .contains("estate-move-source phase=inventory outcome=ok items=8 new=8"),
        "{}",
        sourced.stdout
    );
    assert!(
        sourced
            .stdout
            .contains("phase=capture outcome=items-refused")
            && sourced.stdout.contains("captured=6 refused=2"),
        "{}",
        sourced.stdout
    );
    assert!(source_state.join("carry/move-source.done").is_file());

    let move_args = |extra: &[&str], label: &str| {
        let mut args = os(&[&"estate-move", &"--label", &label, &"--source", &"neo"]);
        args.extend(extra.iter().map(OsString::from));
        args.extend(os(&[&"-", &source_state, &state]));
        args
    };

    // --plan-only: the plan and the preview, nothing at a live path.
    let planned = agent(&move_args(&["--plan-only"], LABEL));
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    assert!(
        planned.stdout.contains(
            "estate-move plan items=8 land=3 adopt=2 refs-only=0 carry-live=1 carry-beside=1 refuse=1"
        ),
        "{}",
        planned.stdout
    );
    assert!(planned.stdout.contains("estate-move diff classes="));
    assert!(!dst.join("land").exists());
    assert!(!dst.join("broken").exists());

    // One pass.
    let moved = agent(&move_args(&[], LABEL));
    // Pending review: `occupied` is refused, so the gate holds.
    assert!(!moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stderr.contains("CLOSURE_UNACCOUNTED"),
        "{}",
        moved.stderr
    );
    let first = closure(&moved.stdout);
    assert_eq!(first["unaccounted"], "0", "{}", moved.stdout);
    assert_eq!(first["applied_beside"], "1", "{}", moved.stdout);
    assert_eq!(first["carried"], "2", "{}", moved.stdout);
    assert_eq!(first["refused_pending_review"], "1", "{}", moved.stdout);
    assert_eq!(first["not_moved"], "1", "{}", moved.stdout);
    assert!(
        moved.stdout.contains("estate-move gate=fail"),
        "{}",
        moved.stdout
    );

    // Landed.
    assert_eq!(
        git(&dst.join("land"), &["rev-parse", "HEAD"]),
        git(&src.join("land"), &["rev-parse", "HEAD"])
    );
    assert!(dst.join("land-wt/a.txt").is_file());
    assert!(dst.join("bare.git/HEAD").is_file());
    // Adopted, refs in: the destination's topic and checkout as they were.
    assert_eq!(git(&same, &["rev-parse", "topic"]), dst_topic);
    assert_eq!(
        git(&same, &["rev-parse", &format!("refs/{LABEL}/heads/topic")]),
        src_topic
    );
    for line in same_refs.lines() {
        assert!(refs(&same).contains(line), "native ref moved: {line}");
    }
    // Beside: the live checkout untouched.
    assert_eq!(
        git(&dst.join("diverged"), &["rev-parse", "HEAD"]),
        diverged_head
    );
    for line in diverged_refs.lines() {
        assert!(
            refs(&dst.join("diverged")).contains(line),
            "native ref moved: {line}"
        );
    }
    assert!(dst.join(format!("diverged.{LABEL}/d.txt")).is_file());
    // Carried as files, live and beside; the destination's file kept.
    assert!(dst.join("broken/b.txt").is_file());
    assert!(dst.join(format!("broken2.{LABEL}/b.txt")).is_file());
    assert_eq!(
        fs::read(dst.join("broken2/mine.txt")).unwrap(),
        b"destination's\n"
    );
    // Refused: untouched.
    assert!(!dst.join("occupied/.git").exists());
    assert_eq!(fs::read(dst.join("occupied/keep.txt")).unwrap(), b"keep\n");

    // P-MOVE-2: a rerun keeps every carry where it went and moves no ref.
    let refs_before = (
        refs(&same),
        refs(&dst.join("diverged")),
        refs(&dst.join("land")),
    );
    let again = agent(&move_args(&[], LABEL));
    assert!(!again.ok);
    let second = closure(&again.stdout);
    assert_eq!(second, first, "{}", again.stdout);
    assert_eq!(
        (
            refs(&same),
            refs(&dst.join("diverged")),
            refs(&dst.join("land"))
        ),
        refs_before
    );
    let kept = again
        .stdout
        .lines()
        .filter(|line| line.starts_with("estate-move item=") && line.contains(" kept=true"))
        .count();
    assert_eq!(kept, 2, "{}", again.stdout);
    assert!(!dst.join(format!("broken.{LABEL}")).exists());

    // A converge pass after the source moved `topic` again: refs-in keeps
    // the ref it made (never moves it), counts it kept, and the item stays
    // applied; nothing native moves.
    git(&src.join("same"), &["checkout", "--quiet", "topic"]);
    git(
        &src.join("same"),
        &["commit", "--quiet", "--allow-empty", "-m", "src topic 2"],
    );
    git(&src.join("same"), &["checkout", "--quiet", "main"]);
    let resourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src,
    ]));
    assert!(resourced.ok, "{}\n{}", resourced.stdout, resourced.stderr);
    let converged = agent(&move_args(&[], LABEL));
    assert!(
        converged.stdout.contains("refs-in-kept=1"),
        "{}",
        converged.stdout
    );
    // When the new pass recaptured `diverged` too (whether it does depends
    // on the source census's timing), its beside landing converges to the
    // new capture (gap C12, A-115: a beside landing used to refuse every
    // later pass `GIT_DESTINATION_OCCUPIED`); otherwise its journal is a
    // no-op. Either way the live checkout and its refs are as they were,
    // the beside checkout still detached at the captured head, and
    // everything accounts as before.
    let diverged_row = converged
        .stdout
        .lines()
        .find(|line| {
            line.starts_with("item=")
                && line.contains(" outcome=")
                && line.contains("/src/git/diverged\"")
        })
        .unwrap_or_default();
    assert!(
        (diverged_row.contains(" outcome=workspace-beside ")
            && diverged_row.contains("beside-converged=1"))
            || diverged_row.contains(" outcome=previous-workspace-beside-not-revalidated "),
        "{}",
        converged.stdout
    );
    let mut want = first;
    let mut got = closure(&converged.stdout);
    assert!(
        got.remove("refusals")
            .is_some_and(|codes| codes.contains("GIT_DESTINATION_NONREPO_DIFFERS:1")),
        "{}",
        converged.stdout
    );
    want.remove("refusals");
    assert_eq!(got, want, "{}", converged.stdout);
    assert_eq!(
        git(&dst.join("diverged"), &["rev-parse", "HEAD"]),
        diverged_head
    );
    for line in diverged_refs.lines() {
        assert!(
            refs(&dst.join("diverged")).contains(line),
            "native ref moved: {line}"
        );
    }
    let beside = dst.join(format!("diverged.{LABEL}"));
    assert!(beside.join("d.txt").is_file());
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&src.join("diverged"), &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
    assert_eq!(
        git(&same, &["rev-parse", &format!("refs/{LABEL}/heads/topic")]),
        src_topic
    );
    assert_eq!(git(&same, &["rev-parse", "topic"]), dst_topic);

    // Another label for the same state refuses before anything runs; a
    // label naming one of Git's namespaces is refused outright.
    let rebound = agent(&move_args(&[], "neo-20261019"));
    assert!(!rebound.ok);
    assert!(
        rebound.stderr.contains("RECEIPT_BINDING_INVALID"),
        "{}",
        rebound.stderr
    );
    let reserved = agent(&move_args(&[], "heads"));
    assert!(
        reserved.stderr.contains("FIELD_DOMAIN_VIOLATION"),
        "{}",
        reserved.stderr
    );
}
