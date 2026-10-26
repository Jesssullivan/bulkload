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
//! The `--sessions` passes over a Git lane below land the lane through the
//! streamed capture and consolidate the homes (#297).

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
use std::os::unix::fs::MetadataExt as _;
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

// The `estate-move closure` line's `key=value` words.
fn closure(stdout: &str) -> BTreeMap<String, String> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("estate-move closure "))
        .unwrap_or_else(|| panic!("no closure line:\n{stdout}"));
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

// A read of `repo` that never refreshes its index (`status` otherwise
// rewrites it), so a later pass sees the landed seats as it left them.
fn out(repo: &Path, args: &[&str]) -> String {
    let ran = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&ran.stderr)
    );
    String::from_utf8_lossy(&ran.stdout).into_owned()
}

// The receipt line of `item` on the destination half's stdout (receipts
// print paths escaped and quoted).
#[allow(clippy::unnecessary_debug_formatting)]
fn item_line<'a>(stdout: &'a str, path: &Path) -> &'a str {
    let live = format!("live={path:?}");
    stdout
        .lines()
        .find(|line| {
            line.starts_with("estate-move item=")
                && line.ends_with(&live)
                && line.contains(" outcome=")
        })
        .unwrap_or_else(|| panic!("no receipt for {live}:\n{stdout}"))
}

/// B1 (SPEC rule 2 reset, coordinator decision (a)): the streamed capture
/// lands every item absent at the destination at its live path. The source
/// half records only ledgers; the objects are fetched by id from the live
/// source repository into the live store; the seats (dirty, untracked,
/// ignored) land through the engine; the linked worktree's pointers are
/// mapped. The landed repositories hold exactly the source's refs, stash
/// and status, and only their own objects: no custody refs. Neither state
/// holds payload. A rerun fetches nothing and reads no seat.
#[test]
fn a_streamed_pass_lands_every_absent_item_with_no_payload_anywhere() {
    let world = World::new("streamed");
    world.estate();
    let untouched = admin_digest(&world.src().join("land/.git"));
    let sourced = world.source();
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    assert!(
        sourced.stdout.contains("items=3 new=3 planned=3"),
        "{}",
        sourced.stdout
    );
    assert_eq!(
        sourced.stdout.matches("outcome=captured").count(),
        3,
        "{}",
        sourced.stdout
    );
    holds_no_payload(&world.source_state(), 3);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    // The source repository is as it was, byte for byte: no ref, lock,
    // reflog, index or configuration write by either half.
    assert_eq!(
        admin_digest(&world.src().join("land/.git")),
        untouched,
        "{both}"
    );
    assert!(
        moved.stdout.contains("estate-move phase=pull outcome=ok"),
        "{both}"
    );
    assert!(
        moved.stdout.contains("estate-move phase=apply outcome=ok"),
        "{both}"
    );
    let (src, dst) = (world.src(), world.dst());
    for name in ["land", "land-wt", "other"] {
        assert!(
            item_line(&moved.stdout, &dst.join(name)).contains("outcome=workspace-restored"),
            "{both}"
        );
    }
    // History, refs and stash exactly as on the source; no custody refs.
    for name in ["land", "other"] {
        assert_eq!(
            out(
                &dst.join(name),
                &["for-each-ref", "--format=%(objectname) %(refname)"]
            ),
            out(
                &src.join(name),
                &["for-each-ref", "--format=%(objectname) %(refname)"]
            ),
            "{name}: {both}"
        );
        out(
            &dst.join(name),
            &["fsck", "--connectivity-only", "--no-dangling"],
        );
    }
    assert_eq!(
        out(&dst.join("land"), &["stash", "list"]),
        out(&src.join("land"), &["stash", "list"])
    );
    // The working state: dirty, untracked and ignored seats, byte for byte,
    // and the same status (the index carried).
    for rel in ["a.txt", "new.txt", "ignored.bin", ".gitignore"] {
        assert_eq!(
            fs::read(dst.join("land").join(rel)).unwrap(),
            fs::read(src.join("land").join(rel)).unwrap(),
            "{rel}"
        );
    }
    // The same status, but the hand-off notes the move wrote (R0.5).
    let landed_status: String = out(&dst.join("land"), &["status", "--porcelain", "--ignored"])
        .lines()
        .filter(|line| *line != "?? CLAUDE.local.md" && *line != "!! CLAUDE.local.md")
        .fold(String::new(), |mut all, line| {
            all.push_str(line);
            all.push('\n');
            all
        });
    assert_eq!(
        landed_status,
        out(&src.join("land"), &["status", "--porcelain", "--ignored"])
    );
    assert!(
        moved
            .stdout
            .contains("estate-move phase=hand-off outcome=ok"),
        "{both}"
    );
    // The hand-off is CLAUDE.local.md, excluded; no tracked file is
    // written and Codex gets none (2026-10-21).
    assert!(dst.join("land/CLAUDE.local.md").is_file(), "{both}");
    assert!(!dst.join("land/AGENTS.md").exists(), "{both}");
    assert!(!dst.join("land/CLAUDE.md").exists(), "{both}");
    // The linked worktree is linked, at its live path.
    assert_eq!(
        out(&dst.join("land-wt"), &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "wt"
    );
    let listed = out(&dst.join("land"), &["worktree", "list", "--porcelain"]);
    assert!(
        listed.contains(&format!("worktree {}", dst.join("land-wt").display())),
        "{listed}"
    );
    assert!(!listed.contains("prunable"), "{listed}");
    // Neither state holds payload; the closure line says so.
    assert_eq!(
        state_line(&moved.stdout, "estate-move")["state_payload_bytes"],
        "0",
        "{both}"
    );
    assert_eq!(closure(&moved.stdout)["unaccounted"], "0", "{both}");
    holds_no_payload(&world.state(), 3);
    holds_no_payload(&world.source_state(), 3);
    // SPEC rule 8: a `git status` on the destination refreshes the landed
    // index's stat data; that is no change, and the rerun converges.
    git(&dst.join("land"), &["status", "--porcelain"]);
    // A rerun of both halves over an unchanged source fetches nothing and
    // changes no ref.
    let refs = out(&dst.join("land"), &["for-each-ref"]);
    assert!(world.source().ok);
    let again = world.moved();
    let both = format!("{}\n{}", again.stdout, again.stderr);
    for name in ["land", "other"] {
        let line = item_line(&again.stdout, &dst.join(name));
        assert!(line.contains("object_batches=0 "), "{line}\n{both}");
        assert!(
            line.contains("outcome=workspace-restored"),
            "{line}\n{both}"
        );
        assert!(line.contains(" index=same "), "{line}\n{both}");
    }
    assert_eq!(out(&dst.join("land"), &["for-each-ref"]), refs);
    holds_no_payload(&world.state(), 3);
}

/// B2: a live path holding a related repository of the destination's own
/// is consolidated, never replaced and never landed beside. Refs union by
/// name: a branch only the source has is created; a checked-out branch the
/// destination holds behind the source's, its tree clean, is advanced with
/// its checkout ("switch when clean"); a branch both sides moved keeps the
/// destination's tip and takes the source's as `refs/remotes/neo/NAME`,
/// listed. The source's stash joins the destination's. The worktree's new
/// seats are created and the destination's own files kept; a seat both
/// sides hold differently is held typed, nothing written to it.
#[test]
fn a_related_live_repository_is_consolidated_and_refs_union_by_name() {
    let world = World::new("union");
    let (src, dst) = (world.src(), world.dst());
    let source = src.join("same");
    repo(&source, "s.txt");
    git(&source, &["branch", "topic"]);
    git(
        &dst,
        &[
            "clone",
            "--quiet",
            "--template=",
            source.to_str().unwrap(),
            "same",
        ],
    );
    let live = dst.join("same");
    git(&live, &["remote", "remove", "origin"]);
    git(&live, &["branch", "topic", "refs/heads/main"]);
    // The destination moves topic on its own, and has a file of its own.
    git(&live, &["checkout", "--quiet", "topic"]);
    git(
        &live,
        &["commit", "--quiet", "--allow-empty", "-m", "dest topic"],
    );
    git(&live, &["checkout", "--quiet", "main"]);
    fs::write(live.join("dest-only.txt"), b"sting's\n").unwrap();
    let dest_topic = out(&live, &["rev-parse", "topic"]);
    // The source moves main ahead, topic its own way, adds a branch, a
    // stash and an untracked file.
    fs::write(source.join("s.txt"), "s, second\n").unwrap();
    git(&source, &["commit", "--quiet", "-am", "src main"]);
    git(&source, &["checkout", "--quiet", "topic"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "src topic"],
    );
    git(&source, &["checkout", "--quiet", "main"]);
    git(&source, &["branch", "fresh"]);
    fs::write(source.join("s.txt"), "s, stashed\n").unwrap();
    git(
        &source,
        &["stash", "push", "--quiet", "-m", "parked on neo"],
    );
    fs::write(source.join("new.txt"), b"untracked on neo\n").unwrap();
    let src_main = out(&source, &["rev-parse", "main"]);
    let src_topic = out(&source, &["rev-parse", "topic"]);
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let receipt = item_line(&moved.stdout, &live);
    assert!(receipt.contains("union=consolidated"), "{receipt}\n{both}");
    assert!(
        receipt.contains("outcome=workspace-restored"),
        "{receipt}\n{both}"
    );
    assert!(receipt.contains("refs_created=1 "), "{receipt}\n{both}");
    assert!(receipt.contains("refs_switched=1 "), "{receipt}\n{both}");
    assert!(receipt.contains("refs_tracked=1 "), "{receipt}\n{both}");
    assert!(receipt.contains("stash_added=1 "), "{receipt}\n{both}");
    assert!(
        moved
            .stdout
            .contains("ref=refs/heads/topic union=tracked at=refs/remotes/neo/topic"),
        "{both}"
    );
    assert_eq!(out(&live, &["rev-parse", "main"]), src_main, "switched");
    assert_eq!(fs::read(live.join("s.txt")).unwrap(), b"s, second\n");
    assert_eq!(out(&live, &["rev-parse", "topic"]), dest_topic, "kept");
    assert_eq!(
        out(&live, &["rev-parse", "refs/remotes/neo/topic"]),
        src_topic
    );
    assert_eq!(
        out(&live, &["rev-parse", "fresh"]),
        out(&source, &["rev-parse", "fresh"])
    );
    assert!(
        out(&live, &["stash", "list"]).contains("parked on neo"),
        "{both}"
    );
    assert_eq!(
        fs::read(live.join("new.txt")).unwrap(),
        b"untracked on neo\n"
    );
    assert_eq!(fs::read(live.join("dest-only.txt")).unwrap(), b"sting's\n");
    out(&live, &["fsck", "--connectivity-only", "--no-dangling"]);
    holds_no_payload(&world.state(), 1);
    // A seat both sides hold differently is held typed, nothing written.
    fs::write(source.join("new.txt"), b"neo changed it\n").unwrap();
    fs::write(live.join("new.txt"), b"sting changed it\n").unwrap();
    assert!(world.source().ok);
    let again = world.moved();
    let both = format!("{}\n{}", again.stdout, again.stderr);
    let receipt = item_line(&again.stdout, &live);
    assert!(receipt.contains("outcome=held"), "{receipt}\n{both}");
    assert!(receipt.contains("seats_held=true"), "{receipt}\n{both}");
    assert_eq!(
        fs::read(live.join("new.txt")).unwrap(),
        b"sting changed it\n"
    );
}

// A source repository `name` with a five-line file, and the destination's
// clone of it at the mapped live path (no remote): a related pair.
fn related_pair(world: &World, name: &str) -> (PathBuf, PathBuf) {
    let (source, live) = (world.src().join(name), world.dst().join(name));
    fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "c0"]);
    git(
        &world.dst(),
        &[
            "clone",
            "--quiet",
            "--template=",
            source.to_str().unwrap(),
            name,
        ],
    );
    git(&live, &["remote", "remove", "origin"]);
    (source, live)
}

/// Part 1, confirmed: a checked-out branch the destination holds behind the
/// source's but whose tree is dirty keeps the destination's tip; the
/// source's is reachable at `refs/remotes/neo/NAME`, and listed.
#[test]
fn a_dirty_checkout_behind_keeps_its_tip_and_tracks_the_sources() {
    let world = World::new("dirtybehind");
    let (source, live) = related_pair(&world, "p");
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "ahead"],
    );
    let ahead = out(&source, &["rev-parse", "main"]);
    let mine = out(&live, &["rev-parse", "main"]);
    fs::write(
        live.join("f.txt"),
        "one\ntwo\nthree\nfour\nfive, sting's edit\n",
    )
    .unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved
            .stdout
            .contains("ref=refs/heads/main union=tracked at=refs/remotes/neo/main"),
        "{both}"
    );
    assert_eq!(out(&live, &["rev-parse", "main"]), mine, "{both}");
    assert_eq!(out(&live, &["rev-parse", "refs/remotes/neo/main"]), ahead);
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"one\ntwo\nthree\nfour\nfive, sting's edit\n"
    );
}

/// Part 2, the three-way union: a file both sides changed merges against
/// its version at the merge base of the two HEADs, Git's `merge-file`
/// semantics, when no hunk conflicts; a conflicting hunk holds the file
/// typed, nothing written.
#[test]
fn a_file_both_sides_changed_merges_three_way_or_is_held() {
    let world = World::new("merge3");
    let (source, live) = related_pair(&world, "p");
    fs::write(
        source.join("f.txt"),
        "one\ntwo\nthree\nfour\nFIVE from neo\n",
    )
    .unwrap();
    fs::write(
        live.join("f.txt"),
        "ONE from sting\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let receipt = item_line(&moved.stdout, &live);
    assert!(receipt.contains("seats_held=false"), "{receipt}\n{both}");
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"ONE from sting\ntwo\nthree\nfour\nFIVE from neo\n",
        "{both}"
    );
    // Both sides now change the same line: held, the destination's kept.
    fs::write(source.join("f.txt"), "one\ntwo\nTHREE neo\nfour\nfive\n").unwrap();
    fs::write(live.join("f.txt"), "one\ntwo\nTHREE sting\nfour\nfive\n").unwrap();
    assert!(world.source().ok);
    let again = world.moved();
    let both = format!("{}\n{}", again.stdout, again.stderr);
    let receipt = item_line(&again.stdout, &live);
    assert!(receipt.contains("seats_held=true"), "{receipt}\n{both}");
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"one\ntwo\nTHREE sting\nfour\nfive\n"
    );
    // The closure names the held seat on the item's own row, with why
    // (2026-10-22): a held lane's files are read from the closure alone.
    let report = fs::read_to_string(world.state().join("closure.json")).unwrap();
    let held = report
        .split("{\"item\":")
        .find(|row| row.contains("\"held_seats\":["))
        .unwrap_or_else(|| panic!("no held_seats in {report}"));
    assert!(
        held.contains("\"path\":\"f.txt\",\"refusal\":\"HOMES_CONSOLIDATE_UNSUPPORTED\""),
        "{held}"
    );
    assert!(!held.contains("\"reason\":null"), "{held}");
}

/// Part 2, a different branch on each side (the `GloriousFlywheel` case):
/// sting on main and clean, neo on a feature branch with dirty and
/// untracked files. After the ref union sting switches to neo's branch
/// ("switch when clean") and neo's files land. A dirty sting is held typed
/// with both branches listed, nothing switched or landed.
#[test]
fn a_different_branch_switches_when_clean_and_is_held_when_dirty() {
    let world = World::new("branches");
    let (source, live) = related_pair(&world, "p");
    git(&source, &["checkout", "--quiet", "-b", "feature"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "feature work"],
    );
    fs::write(
        source.join("f.txt"),
        "one\ntwo\nthree\nfour\nfive, dirty on neo\n",
    )
    .unwrap();
    fs::write(source.join("wip.txt"), b"untracked on neo\n").unwrap();
    // A second, dirty destination of the same source, on its own path.
    let (_, held) = related_pair(&world, "q");
    fs::write(held.join("f.txt"), "sting's own edit\n").unwrap();
    let q_source = world.src().join("q");
    git(&q_source, &["checkout", "--quiet", "-b", "feature"]);
    git(
        &q_source,
        &["commit", "--quiet", "--allow-empty", "-m", "feature work"],
    );
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.contains("switched=refs/heads/feature"),
        "{both}"
    );
    assert_eq!(
        out(&live, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "feature"
    );
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"one\ntwo\nthree\nfour\nfive, dirty on neo\n",
        "{both}"
    );
    assert_eq!(
        fs::read(live.join("wip.txt")).unwrap(),
        b"untracked on neo\n"
    );
    assert!(
        moved
            .stdout
            .contains("branches=held destination=refs/heads/main source=refs/heads/feature"),
        "{both}"
    );
    let receipt = item_line(&moved.stdout, &held);
    assert!(receipt.contains("outcome=held"), "{receipt}\n{both}");
    assert_eq!(
        out(&held, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert_eq!(fs::read(held.join("f.txt")).unwrap(), b"sting's own edit\n");
}

/// Part 2, nested repositories and submodules are their own items: the
/// parent excludes their paths and its `.git/modules`, each lands with its
/// own fetch, a submodule's administration at the parent's `.git/modules`
/// and its worktree linked to it.
#[test]
fn nested_repositories_and_submodules_land_as_their_own_items() {
    let world = World::new("nested");
    let src = world.src();
    let library = world.base.join("library");
    repo(&library, "lib.txt");
    let parent = src.join("parent");
    repo(&parent, "p.txt");
    let ran = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "protocol.file.allow=always",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-C",
        ])
        .arg(&parent)
        .args(["submodule", "add", "--quiet"])
        .arg(&library)
        .arg("sub")
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    git(&parent, &["commit", "--quiet", "-m", "add sub"]);
    // An independent repository nested in the worktree (untracked by it).
    repo(&parent.join("vendor/tool"), "tool.txt");
    fs::create_dir_all(parent.join(".git/info")).unwrap();
    fs::write(parent.join(".git/info/exclude"), "vendor/\n").unwrap();
    let sourced = world.source();
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    assert!(sourced.stdout.contains(" items=3 "), "{}", sourced.stdout);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let dst = world.dst();
    for rel in ["parent", "parent/sub", "parent/vendor/tool"] {
        assert!(
            item_line(&moved.stdout, &dst.join(rel)).contains("outcome=workspace-restored"),
            "{rel}\n{both}"
        );
    }
    let live = dst.join("parent");
    assert_eq!(
        out(&live.join("sub"), &["rev-parse", "HEAD"]),
        out(&parent.join("sub"), &["rev-parse", "HEAD"])
    );
    assert_eq!(fs::read(live.join("sub/lib.txt")).unwrap(), b"lib.txt\n");
    assert_eq!(
        out(&live.join("vendor/tool"), &["rev-parse", "HEAD"]),
        out(&parent.join("vendor/tool"), &["rev-parse", "HEAD"])
    );
    assert!(
        out(&live, &["submodule", "status"]).contains(" sub"),
        "{both}"
    );
    out(
        &live.join("sub"),
        &["fsck", "--connectivity-only", "--no-dangling"],
    );
    holds_no_payload(&world.state(), 3);
}

/// A shallow source carries its boundary without copying objects into state.
#[test]
fn a_shallow_source_carries_its_frontier() {
    let world = World::new("shallow");
    world.estate();
    let src = world.src();
    let from = format!("file://{}", src.join("other").display());
    let ran = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["clone", "--quiet", "--depth=1", &from])
        .arg(src.join("shallow"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let source = src.join("shallow");
    let frontier = fs::read(source.join(".git/shallow")).unwrap();
    let before = admin_digest(&source.join(".git"));
    let sourced = world.source();
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(moved.ok, "{both}");
    assert_eq!(closure(&moved.stdout)["applied"], "4", "{both}");
    assert_eq!(closure(&moved.stdout)["unaccounted"], "0", "{both}");
    assert_eq!(
        fs::read(world.dst().join("shallow/.git/shallow")).unwrap(),
        frontier
    );
    assert_eq!(admin_digest(&source.join(".git")), before);
    assert_eq!(
        payload::measure(&world.source_state())
            .unwrap()
            .payload_bytes,
        0
    );
    assert_eq!(payload::measure(&world.state()).unwrap().payload_bytes, 0);
}

// Every file under `admin` (a repository's administration), by path and
// bytes: what a pass must never change on the source.
fn admin_digest(admin: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    tree(admin)
        .into_iter()
        .filter(|rel| fs::symlink_metadata(admin.join(rel)).unwrap().is_file())
        .map(|rel| {
            let bytes = fs::read(admin.join(&rel)).unwrap();
            (rel, bytes)
        })
        .collect()
}

/// B1 holds, typed, a live path that holds another version: nothing is
/// written there until the consolidate path (B2) decides it.
#[test]
fn a_live_path_holding_another_version_is_held_and_untouched() {
    let world = World::new("held");
    world.estate();
    let mine = world.dst().join("other");
    repo(&mine, "mine.txt");
    let before = tree(&mine);
    let head = out(&mine, &["rev-parse", "HEAD"]);
    assert!(world.source().ok);
    // A fetch scratch a crashed pass left is swept at startup.
    let left = world.state().join("scratch/bulkload-fetch-1-0");
    fs::create_dir_all(&left).unwrap();
    fs::write(left.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    for dir in [world.state(), world.state().join("scratch")] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let moved = world.moved();
    assert!(
        moved.stdout.contains("estate-move swept="),
        "{}",
        moved.stdout
    );
    assert!(!left.exists());
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let line = item_line(&moved.stdout, &mine);
    assert!(
        line.contains("outcome=held code=GIT_DESTINATION_OCCUPIED"),
        "{line}\n{both}"
    );
    assert_eq!(tree(&mine), before);
    assert_eq!(out(&mine, &["rev-parse", "HEAD"]), head);
    holds_no_payload(&world.state(), 3);
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

// ---- plan of record 2026-10-19, Step C gaps C9, C10, C11 -------------------

const SID: &str = "5e5e5e5e-1111-4222-8333-444444444444";
const DEST_SID: &str = "5e5e5e5e-2222-4222-8333-444444444444";
const THREAD: &str = "019a0000-0000-7000-8000-000000000001";
const DEST_THREAD: &str = "019a0000-0000-7000-8000-000000000002";
const ROWLESS_THREAD: &str = "019a0000-0000-7000-8000-000000000003";

/// Claude Code's project key (`reseat::key::project_key`, short paths).
fn key(path: &Path) -> String {
    path.to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A minimal transcript in Claude Code 2.1.299's shape.
fn transcript(sid: &str, cwd: &Path, label: &str) -> String {
    let cwd = cwd.to_str().unwrap();
    let common = format!(
        "\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"{cwd}\",\"sessionId\":\"{sid}\",\"version\":\"2.1.299\",\"gitBranch\":\"main\",\"entrypoint\":\"cli\""
    );
    let ts = "2026-10-19T12:00:00.000Z";
    format!(
        "{{\"parentUuid\":null,\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"prompt {label}\"}},\"uuid\":\"u-{label}\",\"timestamp\":\"{ts}\",{common}}}\n\
         {{\"parentUuid\":\"u-{label}\",\"type\":\"assistant\",\"message\":{{\"id\":\"msg_{label}\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"reply {label}\"}}],\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}},\"requestId\":\"req_{label}\",\"uuid\":\"a-{label}\",\"timestamp\":\"{ts}\",{common}}}\n"
    )
}

const THREADS: &str = "CREATE TABLE threads (
    id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL, source TEXT NOT NULL, cwd TEXT NOT NULL,
    title TEXT NOT NULL, preview TEXT NOT NULL DEFAULT '', project_id TEXT,
    archived INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL,
    child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);
    CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL, position INTEGER NOT NULL,
    name TEXT NOT NULL, PRIMARY KEY(thread_id, position));";

/// A Codex index with one thread row.
fn codex_index(path: &Path, id: &str, rollout: &str, cwd: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
    conn.execute_batch(THREADS).unwrap();
    conn.execute(
        "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title, preview, project_id)
         VALUES (?1, ?2, 1, 1, 'cli', ?3, 'title', 'hello', 'project')",
        rusqlite::params![id, rollout, cwd.to_str().unwrap()],
    )
    .unwrap();
    conn.close().map_err(|(_, error)| error).unwrap();
}

/// A Codex index in WAL mode, as a live Codex keeps it: the row union
/// reads it from one held read snapshot.
fn codex_index_wal(path: &Path, id: &str, rollout: &str, cwd: &Path) {
    codex_index(path, id, rollout, cwd);
    let conn = rusqlite::Connection::open(path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.close().map_err(|(_, error)| error).unwrap();
}

fn thread_cwds(path: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let mut statement = conn
        .prepare("SELECT id, cwd FROM threads ORDER BY id")
        .unwrap();
    let rows: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows
}

/// The homes pass under SPEC rule 2: `estate-move --sessions` over a source
/// half that planned no Git item runs, landing `.claude` and `.codex` at
/// the LIVE destination home (no landing area), with only the plan and
/// marks pulled into `MOVE_STATE` and no payload in either state.
#[test]
fn a_sessions_only_pass_lands_the_live_home_with_no_payload_in_state() {
    let world = World::new("homes");
    let base = world.base.clone();
    let (src_home, dst_home) = (base.join("src/home"), base.join("dst/home"));
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(src_git.join("notes")).unwrap();
    fs::write(src_git.join("notes/todo.txt"), b"not a repository\n").unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    let lane = src_git.join("lab");
    let projects = src_home.join(".claude/projects").join(key(&lane));
    fs::create_dir_all(&projects).unwrap();
    let neo = transcript(SID, &lane, "NEO");
    fs::write(projects.join(format!("{SID}.jsonl")), &neo).unwrap();
    fs::write(
        src_home.join(".claude/history.jsonl"),
        b"{\"display\":\"p\"}\n",
    )
    .unwrap();
    let rollout_rel = format!("sessions/2026/10/20/rollout-2026-10-20T10-00-00-{THREAD}.jsonl");
    let rollout = src_home.join(".codex").join(&rollout_rel);
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    fs::write(&rollout, b"{\"type\":\"session_meta\"}\n").unwrap();
    codex_index(
        &src_home.join(".codex/state_5.sqlite"),
        THREAD,
        rollout.to_str().unwrap(),
        &lane,
    );
    fs::create_dir_all(dst_home.join(".claude")).unwrap();
    fs::write(dst_home.join(".claude/settings.json"), b"{}\n").unwrap();
    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src_git,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let moved = agent(&os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"-",
        &source_state,
        &state,
    ]));
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.contains("estate-move phase=pull outcome=ok"),
        "{both}"
    );
    // The Codex store is held for its row union (#297), typed; everything
    // else lands.
    assert!(
        moved
            .stdout
            .contains("estate-move phase=sessions-pull outcome=ok")
            || moved
                .stdout
                .contains("estate-move phase=sessions-pull outcome=refused:ITEMS_REFUSED"),
        "{both}"
    );
    assert_eq!(
        fs::read_to_string(
            dst_home
                .join(".claude/projects")
                .join(key(&lane))
                .join(format!("{SID}.jsonl"))
        )
        .unwrap(),
        neo,
        "the transcript landed live, byte for byte"
    );
    assert_eq!(
        fs::read(dst_home.join(".codex").join(&rollout_rel)).unwrap(),
        b"{\"type\":\"session_meta\"}\n"
    );
    assert_eq!(
        fs::read(dst_home.join(".claude/settings.json")).unwrap(),
        b"{}\n"
    );
    assert!(!state.join("landing").exists(), "no landing area");
    let words = state_line(&moved.stdout, "estate-move");
    assert_eq!(words["state_payload_bytes"], "0", "{both}");
    assert_eq!(closure(&moved.stdout)["state_payload_bytes"], "0", "{both}");
    holds_no_payload(&state, 0);
    holds_no_payload(&source_state, 0);
}

/// `estate-move --sessions` lands the source's `.claude` and `.codex` at the
/// LIVE destination home with `--beside .LABEL` (C9: no landing stage):
/// nothing there is overwritten, a differing settings file and the live
/// Codex index's snapshot land beside, new transcripts and rollouts land in
/// `estate-move --sessions` consolidates the source's `.claude` and
/// `.codex` into the LIVE destination home (operator ruling 2026-10-20;
/// C9: no landing stage, no beside).
///
/// - Differing config files are merged in place.
/// - New transcripts and rollouts land in place.
/// - The Codex store is held for its row union, which holds the gate.
/// - The re-seat runs although one path was held (C10). It re-keys the
///   source's session under the lane's destination key, and the
///   destination's own session is in place.
/// - The closure counts the session transfers and the re-seat (C11).
/// - An unchanged rerun rewrites nothing.
#[test]
fn sessions_are_consolidated_into_the_live_home_and_the_gate_reads_them() {
    let base = std::env::temp_dir().join(format!("bulkload-move-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));

    // The source's .claude: a session of the lane, settings, history, and a
    // plugin file whose beside path the destination already holds.
    let src_claude = src_home.join(".claude");
    let src_projects = src_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&src_projects).unwrap();
    let neo_transcript = transcript(SID, &src_lane, "NEO");
    fs::write(src_projects.join(format!("{SID}.jsonl")), &neo_transcript).unwrap();
    let src_settings = b"{\"cleanupPeriodDays\": 3650, \"theme\": \"neo\"}\n";
    fs::write(src_claude.join("settings.json"), src_settings).unwrap();
    fs::write(
        src_claude.join("history.jsonl"),
        b"{\"display\":\"neo prompt\"}\n",
    )
    .unwrap();
    fs::create_dir_all(src_claude.join("plugins")).unwrap();
    fs::write(src_claude.join("plugins/config.json"), b"{\"neo\":true}\n").unwrap();
    // The source's .codex: an index naming one rollout of the lane.
    let src_codex = src_home.join(".codex");
    let rollout_rel = format!("sessions/2026/10/19/rollout-2026-10-19T10-00-00-{THREAD}.jsonl");
    let rollout = src_codex.join(&rollout_rel);
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    let rollout_bytes = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{THREAD}\",\"cwd\":\"{}\"}}}}\n",
        src_lane.display()
    );
    fs::write(&rollout, &rollout_bytes).unwrap();
    codex_index_wal(
        &src_codex.join("state_5.sqlite"),
        THREAD,
        rollout.to_str().unwrap(),
        &src_lane,
    );

    // The destination's live home: its own settings, its own session of the
    // same lane, a foreign file at the plugin's beside path, and a live
    // Codex index with a sidecar.
    let dst_claude = dst_home.join(".claude");
    let dst_projects = dst_claude.join("projects").join(key(&dst_lane));
    fs::create_dir_all(&dst_projects).unwrap();
    let dest_transcript = transcript(DEST_SID, &dst_lane, "STING");
    fs::write(
        dst_projects.join(format!("{DEST_SID}.jsonl")),
        &dest_transcript,
    )
    .unwrap();
    let dst_settings = b"{\"cleanupPeriodDays\": 3650}\n";
    fs::write(dst_claude.join("settings.json"), dst_settings).unwrap();
    fs::create_dir_all(dst_claude.join("plugins")).unwrap();
    fs::write(
        dst_claude.join("plugins/config.json"),
        b"{\"sting\":true}\n",
    )
    .unwrap();
    let foreign = dst_claude.join(format!("plugins/config.json.{LABEL}"));
    fs::write(&foreign, b"someone else's\n").unwrap();
    let dst_codex = dst_home.join(".codex");
    fs::create_dir_all(&dst_codex).unwrap();
    // The destination's own rollouts: one its index names, one it does not
    // (rowless). Both are the destination's, in place, never re-seated.
    let dest_meta = |id: &str| {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{}\"}}}}\n",
            dst_lane.display()
        )
    };
    let dest_rollout = dst_codex.join(format!(
        "sessions/2026/10/18/rollout-2026-10-18T10-00-00-{DEST_THREAD}.jsonl"
    ));
    let rowless_rollout = dst_codex.join(format!(
        "sessions/2026/10/18/rollout-2026-10-18T11-00-00-{ROWLESS_THREAD}.jsonl"
    ));
    fs::create_dir_all(dest_rollout.parent().unwrap()).unwrap();
    fs::write(&dest_rollout, dest_meta(DEST_THREAD)).unwrap();
    fs::write(&rowless_rollout, dest_meta(ROWLESS_THREAD)).unwrap();
    codex_index(
        &dst_codex.join("state_5.sqlite"),
        DEST_THREAD,
        dest_rollout.to_str().unwrap(),
        &dst_lane,
    );
    fs::write(dst_codex.join("state_5.sqlite-wal"), b"").unwrap();
    let live_index = fs::read(dst_codex.join("state_5.sqlite")).unwrap();

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let source_pass = || {
        let sourced = agent(&os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &source_state,
            &src_git,
        ]));
        assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    };
    source_pass();
    let move_pass = || {
        agent(&os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--sessions",
            &src_home,
            &"--dest-home",
            &dst_home,
            &"--map",
            &map,
            &"-",
            &source_state,
            &state,
        ]))
    };

    // Pass 1 (operator ruling 2026-10-20): the homes are consolidated.
    // The Codex store, in WAL mode as a live Codex keeps it, is merged row
    // by row into the live index (the row union, one serve session).
    let moved = move_pass();
    assert!(moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved
            .stdout
            .contains("estate-move phase=sessions-pull outcome=ok"),
        "{}",
        moved.stdout
    );
    assert!(
        moved.stdout.contains("estate-move phase=reseat outcome=ok"),
        "{}",
        moved.stdout
    );
    // The differing config files are merged in place, key by key, and
    // nothing lands beside them.
    assert_eq!(
        fs::read_to_string(dst_claude.join("settings.json")).unwrap(),
        "{\"cleanupPeriodDays\": 3650, \"theme\": \"neo\"}\n"
    );
    assert_eq!(
        fs::read_to_string(dst_claude.join("plugins/config.json")).unwrap(),
        "{\"sting\":true,\"neo\":true}\n"
    );
    assert!(!dst_claude.join(format!("settings.json.{LABEL}")).exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"someone else's\n");
    assert_eq!(
        fs::read(dst_claude.join("history.jsonl")).unwrap(),
        b"{\"display\":\"neo prompt\"}\n",
        "absent at the destination: landed in place"
    );
    // #280 finding 9: the source's session landed once, under the lane's
    // destination key, byte for byte; nothing under the source's key.
    assert!(
        !dst_claude.join("projects").join(key(&src_lane)).exists(),
        "{}",
        moved.stdout
    );
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{SID}.jsonl"))).unwrap(),
        neo_transcript,
        "landed under the lane's destination key"
    );
    assert!(
        moved.stdout.contains(&format!(
            "estate-move key-map from={} to={}",
            key(&src_lane),
            key(&dst_lane)
        )),
        "{}",
        moved.stdout
    );
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{DEST_SID}.jsonl"))).unwrap(),
        dest_transcript,
        "the destination's own session is untouched"
    );
    assert!(
        moved.stdout.contains("\"outcome\":\"in-place\""),
        "the destination's own session is in place:\n{}",
        moved.stdout
    );
    assert!(!state.join("landing").exists(), "no intermediary stage");
    // SPEC rule 2: the pass leaves no payload byte in MOVE_STATE, and no
    // temporary beside a live path.
    for (name, bytes) in tree_files(&state) {
        for payload in [b"reply NEO".as_slice(), b"neo prompt", b"{\"neo\":true}"] {
            assert!(
                !bytes.windows(payload.len()).any(|window| window == payload),
                "{name} holds payload bytes"
            );
        }
    }
    for root in [&dst_claude, &dst_home.join(".codex")] {
        for (name, _) in tree_files(root) {
            // bulkload's private names: a staged temporary, a union's.
            let leaf = name.rsplit('/').next().unwrap_or_default();
            assert!(
                !leaf.starts_with(".bulkload") && !leaf.contains(".bulkload-consolidate."),
                "{name} left beside a live path"
            );
        }
    }
    // Codex: the live index is never written as a file and nothing lands
    // beside it; its rows took the source's thread by the row union. The
    // source's rollout lands in place.
    assert!(dst_codex.join("state_5.sqlite-wal").exists());
    assert!(!dst_codex.join(format!("state_5.sqlite.{LABEL}")).exists());
    assert!(
        moved
            .stderr
            .contains("consolidated state_5.sqlite action=consolidate"),
        "{}",
        moved.stderr
    );
    assert_eq!(
        fs::read_to_string(dst_codex.join(&rollout_rel)).unwrap(),
        rollout_bytes
    );
    let cwds = thread_cwds(&dst_codex.join("state_5.sqlite"));
    assert!(
        cwds.iter().any(|(id, _)| id == THREAD),
        "the source's thread is a row of the live index: {cwds:?}"
    );
    assert!(
        cwds.contains(&(
            DEST_THREAD.to_owned(),
            dst_lane.to_str().unwrap().to_owned()
        )),
        "{cwds:?}\n{}",
        moved.stdout
    );
    assert_ne!(
        fs::read(dst_codex.join("state_5.sqlite")).unwrap(),
        live_index
    );
    assert_eq!(
        fs::read_to_string(&rowless_rollout).unwrap(),
        dest_meta(ROWLESS_THREAD)
    );
    // The closure: the session transfers joined, the merged configs and the
    // merged store accounted: the gate passes.
    let first = closure(&moved.stdout);
    assert_eq!(first["sessions"], "claude,codex", "{}", moved.stdout);
    assert_eq!(first["applied_beside"], "0", "{}", moved.stdout);
    assert_eq!(first["refused_pending_review"], "0", "{}", moved.stdout);
    assert_eq!(first["unaccounted"], "0", "{}", moved.stdout);
    assert!(
        moved.stdout.contains("estate-move gate=pass"),
        "{}",
        moved.stdout
    );
    assert_eq!(first["key_maps_held"], "0", "{}", moved.stdout);
    let closure_json = fs::read_to_string(state.join("closure.json")).unwrap();
    assert!(
        closure_json.contains("\"transfer_outcome\":\"consolidated\""),
        "{closure_json}"
    );
    assert!(
        closure_json.contains(&format!(
            "\"transfer_outcome\":\"mapped\",\"mapped\":\"projects/{}\"",
            key(&dst_lane)
        )),
        "the closure names the source key's destination key: {closure_json}"
    );
    assert!(
        closure_json.contains("\"consolidation\":\"consolidate\""),
        "{closure_json}"
    );
    assert!(
        !closure_json.contains("\"HOMES_CONSOLIDATE_UNSUPPORTED\""),
        "{closure_json}"
    );

    // Pass 2: unchanged. The merged configs are reused, nothing is
    // rewritten, and the unchanged store is covered with nothing read.
    let settings_inode = fs::metadata(dst_claude.join("settings.json"))
        .unwrap()
        .ino();
    source_pass();
    let again = move_pass();
    assert_eq!(
        fs::metadata(dst_claude.join("settings.json"))
            .unwrap()
            .ino(),
        settings_inode
    );
    assert!(
        again
            .stderr
            .contains("consolidated state_5.sqlite action=covered"),
        "{}",
        again.stderr
    );
    let second = closure(&again.stdout);
    assert_eq!(second["refused_pending_review"], "0", "{}", again.stdout);
    assert_eq!(second["unaccounted"], "0", "{}", again.stdout);
    // SPEC rule 3: no `<path>.LABEL` copy anywhere in the homes; the one
    // there is the destination's own, untouched.
    assert_eq!(
        label_copies(&dst_home),
        [format!(".claude/plugins/config.json.{LABEL}")]
    );
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{DEST_SID}.jsonl"))).unwrap(),
        dest_transcript
    );
}

/// A destination with no Codex index (review of #280, findings 6 and 7, as
/// the operator ruling of 2026-10-20 reshapes them). The source's store is
/// the row union's (#278): never landed as a file, nor beside, and held
/// typed until that union lands. The symlinked homes are resolved once
/// (N1), the rollout lands in place, and a destination session with no
/// `cwd` under a destination key is in place and holds nothing.
#[test]
fn sessions_into_a_destination_with_no_codex_index_hold_the_store() {
    let base = std::env::temp_dir().join(format!("bulkload-move-fresh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));

    // The source: one Claude session, one Codex thread of the lane.
    let src_projects = src_home.join(".claude/projects").join(key(&src_lane));
    fs::create_dir_all(&src_projects).unwrap();
    fs::write(
        src_projects.join(format!("{SID}.jsonl")),
        transcript(SID, &src_lane, "NEO"),
    )
    .unwrap();
    fs::write(
        src_home.join(".claude/settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    let src_codex = src_home.join(".codex");
    let rollout_rel = format!("sessions/2026/10/19/rollout-2026-10-19T10-00-00-{THREAD}.jsonl");
    let rollout = src_codex.join(&rollout_rel);
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    fs::write(
        &rollout,
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{THREAD}\",\"cwd\":\"{}\"}}}}\n",
            src_lane.display()
        ),
    )
    .unwrap();
    codex_index(
        &src_codex.join("state_5.sqlite"),
        THREAD,
        rollout.to_str().unwrap(),
        &src_lane,
    );
    {
        let conn = rusqlite::Connection::open(src_codex.join("state_5.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE backfill_state (id INTEGER PRIMARY KEY, status TEXT NOT NULL);
             INSERT INTO backfill_state VALUES (1, 'complete');",
        )
        .unwrap();
    }

    // The destination: a live .claude with its own session that records no
    // cwd, and an empty .codex with no index. Both are symlinks to real
    // state directories, as sting's are (second review of #280, N1).
    let state_dir = base.join("dst/state");
    fs::create_dir_all(state_dir.join("claude")).unwrap();
    fs::create_dir_all(state_dir.join("codex")).unwrap();
    std::os::unix::fs::symlink(state_dir.join("claude"), dst_home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(state_dir.join("codex"), dst_home.join(".codex")).unwrap();
    let dst_claude = dst_home.join(".claude");
    let own_projects = dst_claude.join("projects").join(key(&dst_lane));
    fs::create_dir_all(&own_projects).unwrap();
    fs::write(
        dst_claude.join("settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    let no_cwd = format!(
        "{{\"type\":\"summary\",\"summary\":\"no cwd here\",\"sessionId\":\"{DEST_SID}\"}}\n"
    );
    fs::write(own_projects.join(format!("{DEST_SID}.jsonl")), &no_cwd).unwrap();
    let dst_codex = dst_home.join(".codex");

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src_git,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    // The plan resolves the symlinked homes as the real pass does (never
    // creating or writing them) and lists the Claude tree under the key map
    // the real pass uses: the source's session at the lane's destination
    // key.
    let before = tree_files(&state_dir);
    let planned = agent(&os(&[
        &"estate-move",
        &"--plan-only",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"-",
        &source_state,
        &state,
    ]));
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    assert!(
        planned
            .stdout
            .contains("estate-move sessions-home name=claude"),
        "{}",
        planned.stdout
    );
    // The Claude home's plan is refused nothing (the Codex store's typed
    // hold is its own line).
    assert!(
        !planned.stdout.contains("homes-plan name=.claude refused="),
        "{}",
        planned.stdout
    );
    assert!(
        planned.stdout.lines().any(|line| line.starts_with(&format!(
            "planned projects/{}/{SID}.jsonl class=append-log action=create",
            key(&dst_lane)
        ))),
        "{}",
        planned.stdout
    );
    assert!(
        !planned
            .stdout
            .contains(&format!("planned projects/{}/", key(&src_lane))),
        "nothing planned under the source key: {}",
        planned.stdout
    );
    assert_eq!(tree_files(&state_dir), before, "the plan wrote nothing");
    let moved = agent(&os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"-",
        &source_state,
        &state,
    ]));
    // The source's Codex store is the row union's (#278), never landed as
    // a file, not even where the destination has none: held typed, so the
    // pull ends ITEMS_REFUSED and the gate holds until that union lands.
    assert!(!moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved
            .stdout
            .contains("estate-move phase=sessions-pull outcome=refused:ITEMS_REFUSED"),
        "{}",
        moved.stdout
    );
    assert!(
        moved
            .stderr
            .contains("held state_5.sqlite: HOMES_CONSOLIDATE_UNSUPPORTED"),
        "{}",
        moved.stderr
    );
    assert!(
        moved.stdout.contains("estate-move gate=fail"),
        "{}",
        moved.stdout
    );
    // N1: each symlinked home was resolved once and named; the links stay.
    for name in ["claude", "codex"] {
        assert!(
            moved
                .stdout
                .contains(&format!("estate-move sessions-home name={name} ")),
            "{}",
            moved.stdout
        );
    }
    for link in [".claude", ".codex"] {
        assert!(fs::symlink_metadata(dst_home.join(link))
            .unwrap()
            .file_type()
            .is_symlink());
    }
    assert!(!dst_codex.join("state_5.sqlite").exists());
    assert!(!dst_codex.join(format!("state_5.sqlite.{LABEL}")).exists());
    assert!(
        state_dir.join("codex").join(&rollout_rel).is_file(),
        "landed under the real path the link names"
    );
    // The destination's own cwd-less session: in place, nothing held.
    assert_eq!(
        fs::read_to_string(own_projects.join(format!("{DEST_SID}.jsonl"))).unwrap(),
        no_cwd
    );
    // With no index at the destination and the store held, the re-seat has
    // no index to register threads in until the row union lands: typed.
    assert!(
        moved
            .stdout
            .contains("estate-move phase=reseat outcome=refused:RESEAT_INDEX_ABSENT"),
        "{}",
        moved.stdout
    );
    let totals = closure(&moved.stdout);
    assert_eq!(totals["refused_pending_review"], "1", "{}", moved.stdout);
    assert_eq!(totals["unaccounted"], "0", "{}", moved.stdout);
}

/// Second review of #280, N2, N4 and N5, on sting's shapes, under the
/// operator ruling of 2026-10-20 (consolidated, never beside; A-106 a
/// union). The destination's own transcripts and rollouts that are
/// symlinks into an archive are never read or written through.
/// - When neo has the same session at the lane's key, the destination's
///   fork stays what the picker reads there (its archive link regularized
///   in place, the archive untouched). Neo's copy is held at the pull and
///   written at no key (SPEC rule 3, one session one file) until the
///   append-log union merges it; the plan lists that hold first.
/// - Neo's copy whose live path is such a link is held at the pull, never
///   landed beside.
/// - A link neo has no copy of is the destination's own and stays.
/// - The destination's own `.jsonl.gz` and oddly named rollouts stay.
/// - A source session with no lane is `RESEAT_LANE_UNMAPPED`, typed, and
///   holds the gate.
#[test]
fn archive_links_follow_a106_and_destination_links_stay() {
    const SID_FORK: &str = "5e5e5e5e-3333-4222-8333-444444444444";
    const SID_NEO_KEY: &str = "5e5e5e5e-4444-4222-8333-444444444444";
    const SID_DEST: &str = "5e5e5e5e-5555-4222-8333-444444444444";
    const SID_NO_LANE: &str = "5e5e5e5e-6666-4222-8333-444444444444";
    const SID_OUTSIDE: &str = "5e5e5e5e-7777-4222-8333-444444444444";
    const T_FORK: &str = "019a0000-0000-7000-8000-000000000009";
    let base = std::env::temp_dir().join(format!("bulkload-move-archive-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));
    let archive = base.join("archive");
    fs::create_dir_all(&archive).unwrap();

    // neo: two sessions of the lane and one whose cwd has no lane.
    let src_projects = src_home.join(".claude/projects").join(key(&src_lane));
    fs::create_dir_all(&src_projects).unwrap();
    let neo = |sid: &str| transcript(sid, &src_lane, &format!("NEO-{sid}"));
    for sid in [SID_FORK, SID_NEO_KEY] {
        fs::write(src_projects.join(format!("{sid}.jsonl")), neo(sid)).unwrap();
    }
    let elsewhere = src_home.join("elsewhere");
    let no_lane_projects = src_home.join(".claude/projects").join(key(&elsewhere));
    fs::create_dir_all(&no_lane_projects).unwrap();
    fs::write(
        no_lane_projects.join(format!("{SID_NO_LANE}.jsonl")),
        transcript(SID_NO_LANE, &elsewhere, "NOLANE"),
    )
    .unwrap();
    // A neo session whose cwd is outside neo's home (neo's
    // /private/var/folders or /Volumes): its key looks like no source
    // key, but this pull listed it, so it is neo's, never taken for the
    // destination's own (independent review of #280, N4).
    let outside = base.join("outside");
    let outside_projects = src_home.join(".claude/projects").join(key(&outside));
    fs::create_dir_all(&outside_projects).unwrap();
    fs::write(
        outside_projects.join(format!("{SID_OUTSIDE}.jsonl")),
        transcript(SID_OUTSIDE, &outside, "OUTSIDE"),
    )
    .unwrap();
    fs::write(
        src_home.join(".claude/settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    // neo's Codex: one thread of the lane.
    let src_codex = src_home.join(".codex");
    let fork_rel = format!("sessions/2026/10/19/rollout-2026-10-19T10-00-00-{T_FORK}.jsonl");
    let neo_rollout = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{T_FORK}\",\"cwd\":\"{}\"}}}}\n{{\"neo\":true}}\n",
        src_lane.display()
    );
    fs::create_dir_all(src_codex.join(&fork_rel).parent().unwrap()).unwrap();
    fs::write(src_codex.join(&fork_rel), &neo_rollout).unwrap();
    codex_index(
        &src_codex.join("state_5.sqlite"),
        T_FORK,
        src_codex.join(&fork_rel).to_str().unwrap(),
        &src_lane,
    );

    // sting: archive links at the lane's key (a fork of a neo session, and
    // one of its own) and under the neo key; a live index; its own .gz and
    // an oddly named file among the rollouts; a rollout link into the
    // archive for neo's thread.
    let dst_claude = dst_home.join(".claude");
    let dst_projects = dst_claude.join("projects").join(key(&dst_lane));
    let neo_key_dir = dst_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&dst_projects).unwrap();
    fs::create_dir_all(&neo_key_dir).unwrap();
    fs::write(
        dst_claude.join("settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    let fork_bytes = transcript(SID_FORK, &dst_lane, "FORK");
    fs::write(archive.join("fork.jsonl"), &fork_bytes).unwrap();
    std::os::unix::fs::symlink(
        archive.join("fork.jsonl"),
        dst_projects.join(format!("{SID_FORK}.jsonl")),
    )
    .unwrap();
    fs::write(
        archive.join("old-neo-key.jsonl"),
        transcript(SID_NEO_KEY, &src_lane, "OLD"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        archive.join("old-neo-key.jsonl"),
        neo_key_dir.join(format!("{SID_NEO_KEY}.jsonl")),
    )
    .unwrap();
    fs::write(
        archive.join("own.jsonl"),
        transcript(SID_DEST, &dst_lane, "OWN"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        archive.join("own.jsonl"),
        dst_projects.join(format!("{SID_DEST}.jsonl")),
    )
    .unwrap();
    let dst_codex = dst_home.join(".codex");
    fs::create_dir_all(dst_codex.join("sessions/2026/10/19")).unwrap();
    let archived_rollout: &[u8] =
        b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"fork\"}}\n{\"sting\":true}\n";
    fs::write(archive.join("rollout-fork.jsonl"), archived_rollout).unwrap();
    std::os::unix::fs::symlink(
        archive.join("rollout-fork.jsonl"),
        dst_codex.join(&fork_rel),
    )
    .unwrap();
    codex_index(
        &dst_codex.join("state_5.sqlite"),
        T_FORK,
        dst_codex.join(&fork_rel).to_str().unwrap(),
        &dst_lane,
    );
    fs::write(dst_codex.join("state_5.sqlite-wal"), b"").unwrap();
    let gz = dst_codex.join(
        "sessions/2026/10/19/rollout-2026-10-18T09-00-00-019a0000-0000-7000-8000-00000000000a.jsonl.gz",
    );
    fs::write(&gz, b"\x1f\x8b old").unwrap();
    let odd = dst_codex.join("sessions/2026/10/19/notes.txt");
    fs::write(&odd, b"an odd name\n").unwrap();
    // homes-20261021: neo rollouts no index names, whose live paths on sting
    // are links into the archive. One holds neo's bytes exactly (covered,
    // nothing written; the re-seat never reads it, where O_NOFOLLOW was
    // ELOOP, a bare IO that ended the re-seat); one holds other bytes (held).
    let same_rel = "sessions/2026/10/19/rollout-2026-10-19T11-00-00-019a0000-0000-7000-8000-00000000000b.jsonl.zst";
    let diff_rel = "sessions/2026/10/19/rollout-2026-10-19T12-00-00-019a0000-0000-7000-8000-00000000000c.jsonl.zst";
    fs::write(src_codex.join(same_rel), b"zst frames, the same on both\n").unwrap();
    fs::write(src_codex.join(diff_rel), b"zst frames, neo's\n").unwrap();
    fs::write(archive.join("same.zst"), b"zst frames, the same on both\n").unwrap();
    fs::write(archive.join("diff.zst"), b"zst frames, sting's\n").unwrap();
    std::os::unix::fs::symlink(archive.join("same.zst"), dst_codex.join(same_rel)).unwrap();
    std::os::unix::fs::symlink(archive.join("diff.zst"), dst_codex.join(diff_rel)).unwrap();

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src_git,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    // The plan names the fork's hold before anything lands.
    let planned = agent(&os(&[
        &"estate-move",
        &"--plan-only",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"--replace-archived-dest",
        &archive,
        &"-",
        &source_state,
        &state,
    ]));
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    // Planned under the key map: the fork at the lane's key is an archive
    // link, unioned and regularized; the neo-key copy of another session is
    // folded (moved) into the lane's key.
    assert!(
        planned.stdout.lines().any(|line| line.starts_with(&format!(
            "planned projects/{}/{SID_FORK}.jsonl class=append-log action=consolidate ",
            key(&dst_lane)
        ))),
        "{}",
        planned.stdout
    );
    assert!(
        planned.stdout.lines().any(|line| line.starts_with(&format!(
            "planned projects/{}/{SID_NEO_KEY}.jsonl class=append-log action=fold-move ",
            key(&src_lane)
        ))),
        "{}",
        planned.stdout
    );
    let moved = agent(&os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"--replace-archived-dest",
        &archive,
        // The by-ID prefixes (`/private/tmp`, `/tmp`) would take a session
        // under a `/tmp` test directory for a by-ID one (the merge gate runs
        // with nix's `TMPDIR=/tmp/nix-shell.*`): off, so the session with no
        // lane is unmapped wherever the test runs.
        &"--reseat-arg",
        &"--no-default-by-id",
        &"-",
        &source_state,
        &state,
    ]));
    assert!(
        !moved.stdout.contains("refused:IO"),
        "{}\n{}",
        moved.stdout,
        moved.stderr
    );
    assert!(
        moved.stdout.contains("estate-move phase=reseat outcome=ok"),
        "{}\n{}",
        moved.stdout,
        moved.stderr
    );
    // A-106 is a union at the pull (operator ruling 2026-10-20; A-055): a
    // transcript linked into the archive root is unioned with the archived
    // file and regularized at the live name; the archive copy stays.
    assert!(
        moved.stdout.contains("estate-move a106=union-at-pull"),
        "{}",
        moved.stdout
    );
    let at_live = dst_projects.join(format!("{SID_FORK}.jsonl"));
    assert!(
        fs::symlink_metadata(&at_live).unwrap().is_file(),
        "regularized: {}\n{}",
        moved.stdout,
        moved.stderr
    );
    let unioned = fs::read_to_string(&at_live).unwrap();
    assert!(
        unioned.starts_with(&neo(SID_FORK)),
        "neo's bytes first: {unioned}"
    );
    for line in fork_bytes.lines() {
        assert!(
            unioned.contains(line),
            "the archived fork's line kept: {line}"
        );
    }
    assert_eq!(
        fs::read_to_string(archive.join("fork.jsonl")).unwrap(),
        fork_bytes,
        "the archive copy stays"
    );
    let copies: Vec<String> = tree_files(&dst_claude)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.contains(SID_FORK))
        .collect();
    assert_eq!(
        copies,
        [format!("projects/{}/{SID_FORK}.jsonl", key(&dst_lane))],
        "one file per session"
    );
    assert!(
        !moved.stdout.contains("RESEAT_DEST_DIVERGED"),
        "{}",
        moved.stdout
    );
    // Under the neo key: the same session the map lands at the lane's key.
    // The neo-key link was moved there (never copied), then unioned with
    // neo's copy and regularized; nothing stays under the neo key.
    assert!(
        fs::symlink_metadata(neo_key_dir.join(format!("{SID_NEO_KEY}.jsonl"))).is_err(),
        "{}",
        moved.stderr
    );
    let moved_here = dst_projects.join(format!("{SID_NEO_KEY}.jsonl"));
    assert!(fs::symlink_metadata(&moved_here).unwrap().is_file());
    let folded = fs::read_to_string(&moved_here).unwrap();
    assert!(folded.starts_with(&neo(SID_NEO_KEY)), "{folded}");
    for line in transcript(SID_NEO_KEY, &src_lane, "OLD").lines() {
        assert!(folded.contains(line), "the old copy's line kept: {line}");
    }
    assert_eq!(
        fs::read_to_string(archive.join("old-neo-key.jsonl")).unwrap(),
        transcript(SID_NEO_KEY, &src_lane, "OLD")
    );
    assert!(
        moved.stderr.contains("key-fold ") && moved.stderr.contains("action=move"),
        "{}",
        moved.stderr
    );
    // The destination's own session keeps its own bytes.
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{SID_DEST}.jsonl"))).unwrap(),
        transcript(SID_DEST, &dst_lane, "OWN")
    );
    // Codex: the archive link at neo's rollout path is unioned and
    // regularized the same way; the archived rollout is untouched.
    let live_rollout = dst_codex.join(&fork_rel);
    assert!(
        fs::symlink_metadata(&live_rollout).unwrap().is_file(),
        "{}",
        moved.stdout
    );
    let rollout = fs::read(&live_rollout).unwrap();
    assert!(rollout.starts_with(neo_rollout.as_bytes()));
    assert!(rollout.ends_with(b"{\"sting\":true}\n"));
    assert_eq!(
        fs::read(archive.join("rollout-fork.jsonl")).unwrap(),
        archived_rollout
    );
    assert_eq!(fs::read(&gz).unwrap(), b"\x1f\x8b old");
    assert_eq!(fs::read(&odd).unwrap(), b"an odd name\n");
    // The archive links to rollouts no index names: both stay links, the
    // archive untouched; the equal one is covered, the other held, judged
    // by its events (its one line is not in the linked file).
    for (rel, archived) in [
        (same_rel, &b"zst frames, the same on both\n"[..]),
        (diff_rel, &b"zst frames, sting's\n"[..]),
    ] {
        assert!(
            fs::symlink_metadata(dst_codex.join(rel))
                .unwrap()
                .file_type()
                .is_symlink(),
            "{rel}: {}",
            moved.stderr
        );
        assert_eq!(fs::read(dst_codex.join(rel)).unwrap(), archived);
    }
    assert!(
        !moved
            .stderr
            .contains(&format!("held {same_rel}: HOMES_CONSOLIDATE_UNSUPPORTED")),
        "{}",
        moved.stderr
    );
    assert!(
        moved.stderr.contains(&format!(
            "held {diff_rel}: HOMES_CONSOLIDATE_UNSUPPORTED reason=\"source holds 1 of its 1 events the linked file lacks\""
        )),
        "{}",
        moved.stderr
    );
    // SPEC rule 3: nothing lands beside a link either.
    assert!(
        label_copies(&dst_home).is_empty(),
        "{:?}",
        label_copies(&dst_home)
    );
    // N4: the two neo sessions with no lane are typed and reviewable, the
    // one outside neo's home included (by provenance), and hold the gate.
    let totals = closure(&moved.stdout);
    assert_eq!(
        totals["reseat_held"], "claude/RESEAT_LANE_UNMAPPED:2",
        "{}",
        moved.stdout
    );
    assert!(
        moved.stdout.contains("estate-move gate=fail"),
        "{}",
        moved.stdout
    );
}

/// #280 finding 9 ("no duplicates"): the source's project keys are mapped
/// at the pull to their lanes' destination keys (the re-seat's lanes), so a
/// session lands once, where Claude Code lists it here: no source-key
/// directory, the re-seat finds it in place, and the closure names each
/// source key with its destination key. A key two lane sources share
/// (`a.b` and `a-b`) stays unmapped and typed, and its session is re-keyed
/// by the re-seat as before. A transcript that grows on the source is
/// extended in place under the mapped key; an unchanged pass reads nothing;
/// the report-only proof lists the source-key directory left behind.
#[test]
fn sessions_land_once_under_the_mapped_key_and_a_shared_key_stays_typed() {
    const SID_AB: &str = "5e5e5e5e-7777-4222-8333-444444444444";
    const SID_RESUMED: &str = "5e5e5e5e-8888-4222-8333-444444444444";
    const SID_EMPTY: &str = "5e5e5e5e-9999-4222-8333-444444444444";
    let base = std::env::temp_dir().join(format!("bulkload-move-keymap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    for name in ["lab", "a.b", "a-b"] {
        repo(&src_git.join(name), "f.txt");
    }
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));
    let (src_ab, dst_ab) = (src_git.join("a.b"), dst_git.join("a.b"));
    assert_eq!(key(&src_ab), key(&src_git.join("a-b")), "a lossy key");

    let settings = b"{\"cleanupPeriodDays\": 3650}\n";
    let src_claude = src_home.join(".claude");
    let src_projects = src_claude.join("projects");
    let neo = transcript(SID, &src_lane, "NEO");
    let lab = src_projects.join(key(&src_lane));
    fs::create_dir_all(lab.join(format!("{SID}/subagents"))).unwrap();
    fs::write(lab.join(format!("{SID}.jsonl")), &neo).unwrap();
    fs::write(
        lab.join(format!("{SID}/subagents/agent-1.jsonl")),
        b"{\"agent\":1}\n",
    )
    .unwrap();
    let ab = transcript(SID_AB, &src_ab, "AB");
    fs::create_dir_all(src_projects.join(key(&src_ab))).unwrap();
    fs::write(
        src_projects
            .join(key(&src_ab))
            .join(format!("{SID_AB}.jsonl")),
        &ab,
    )
    .unwrap();
    fs::write(src_claude.join("settings.json"), settings).unwrap();
    fs::create_dir_all(src_home.join(".codex")).unwrap();
    let dst_claude = dst_home.join(".claude");
    fs::create_dir_all(dst_claude.join("projects")).unwrap();
    fs::write(dst_claude.join("settings.json"), settings).unwrap();
    fs::create_dir_all(dst_home.join(".codex")).unwrap();
    // A session an earlier pass re-seated here and the destination resumed,
    // neo's left as it was (the destination holds its bytes and more:
    // dest-ahead); and one the destination holds empty (the pull lands it
    // beside; the re-seat, judging the bytes, extends the live file).
    let dst_lane_projects = dst_claude.join("projects").join(key(&dst_lane));
    fs::create_dir_all(&dst_lane_projects).unwrap();
    let old = transcript(SID_RESUMED, &src_lane, "OLD");
    let mut here = old.clone();
    here.push_str(&transcript(SID_RESUMED, &dst_lane, "STING-MORE"));
    fs::write(
        dst_lane_projects.join(format!("{SID_RESUMED}.jsonl")),
        &here,
    )
    .unwrap();
    fs::write(lab.join(format!("{SID_RESUMED}.jsonl")), old).unwrap();
    // Extending a live file needs the proof that no process is at it, which
    // only Linux gives (`procs::inside`): elsewhere this session is held
    // `live-in-use-unknown`, so it is left out there (the re-seat's own
    // tests hold it off Linux).
    let linux = cfg!(target_os = "linux");
    let neo_empty = transcript(SID_EMPTY, &src_lane, "NEO-EMPTY");
    if linux {
        fs::write(dst_lane_projects.join(format!("{SID_EMPTY}.jsonl")), b"").unwrap();
        fs::write(lab.join(format!("{SID_EMPTY}.jsonl")), &neo_empty).unwrap();
    }
    let resumed = fs::read(dst_lane_projects.join(format!("{SID_RESUMED}.jsonl"))).unwrap();
    // Both sides quiet past the racy window, as between real passes.
    let settle = || {
        bulkload_agent::transfer::settle_racy_window(&src_claude).unwrap();
        bulkload_agent::transfer::settle_racy_window(&dst_claude).unwrap();
    };

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let source_pass = || {
        let sourced = agent(&os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &source_state,
            &src_git,
        ]));
        assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    };
    let move_args = |extra: &[&dyn AsRef<std::ffi::OsStr>]| {
        let mut args = os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--sessions",
            &src_home,
            &"--dest-home",
            &dst_home,
            &"--map",
            &map,
        ]);
        args.extend(os(extra));
        args.extend(os(&[&"-", &source_state, &state]));
        agent(&args)
    };
    // The claude transfer's stats line: the one session pulled with maps.
    let claude_line = |stdout: &str| -> String {
        stdout
            .lines()
            .find(|line| line.starts_with("completed=") && !line.ends_with(" mapped=0"))
            .unwrap_or_else(|| panic!("no mapped transfer line:\n{stdout}"))
            .to_owned()
    };

    settle();
    source_pass();
    let moved = move_args(&[]);
    assert!(moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    // The resumed session: nothing landed, beside or in place.
    assert_eq!(
        fs::read(dst_lane_projects.join(format!("{SID_RESUMED}.jsonl"))).unwrap(),
        resumed
    );
    assert!(!dst_lane_projects
        .join(format!("{SID_RESUMED}.jsonl.{LABEL}"))
        .exists());
    // The empty live file: consolidated in place with neo's bytes by the
    // pull (ruling 2026-10-20), never landed beside; in place for the
    // re-seat.
    let empty_at = dst_lane_projects.join(format!("{SID_EMPTY}.jsonl"));
    if linux {
        assert_eq!(fs::read_to_string(&empty_at).unwrap(), neo_empty);
        assert!(!dst_lane_projects
            .join(format!("{SID_EMPTY}.jsonl.{LABEL}"))
            .exists());
        assert!(
            moved.stdout.lines().any(|line| line.contains(SID_EMPTY)
                && line.contains("\"kind\":\"session\"")
                && line.contains("\"outcome\":\"in-place\"")),
            "{}",
            moved.stdout
        );
    }
    let at_lane = dst_claude
        .join("projects")
        .join(key(&dst_lane))
        .join(format!("{SID}.jsonl"));
    assert_eq!(fs::read_to_string(&at_lane).unwrap(), neo);
    assert_eq!(
        fs::read(
            dst_claude
                .join("projects")
                .join(key(&dst_lane))
                .join(format!("{SID}/subagents/agent-1.jsonl"))
        )
        .unwrap(),
        b"{\"agent\":1}\n"
    );
    assert!(
        !dst_claude.join("projects").join(key(&src_lane)).exists(),
        "no source-key directory: {}",
        moved.stdout
    );
    assert!(
        moved
            .stdout
            .lines()
            .any(|line| line.contains("\"kind\":\"session\"")
                && line.contains(SID)
                && line.contains("\"outcome\":\"in-place\"")),
        "the re-seat finds it in place:\n{}",
        moved.stdout
    );
    // The shared key: held, typed, and its session re-keyed as before.
    assert!(
        moved.stdout.contains(&format!(
            "estate-move key-map-held key={} reason=ambiguous-source-key",
            key(&src_ab)
        )),
        "{}",
        moved.stdout
    );
    assert_eq!(
        fs::read_to_string(
            dst_claude
                .join("projects")
                .join(key(&src_ab))
                .join(format!("{SID_AB}.jsonl"))
        )
        .unwrap(),
        ab
    );
    assert_eq!(
        fs::read_to_string(
            dst_claude
                .join("projects")
                .join(key(&dst_ab))
                .join(format!("{SID_AB}.jsonl"))
        )
        .unwrap(),
        ab
    );
    let first = closure(&moved.stdout);
    // Nothing lands beside: the empty transcript is consolidated in place.
    assert_eq!(first["applied_beside"], "0", "{}", moved.stdout);
    // No stage dupes (ruling 2026-10-20): the pulls' state holds no payload.
    assert_eq!(first["pull_payload_bytes"], "0", "{}", moved.stdout);
    assert!(first["move_state_bytes"].parse::<u64>().unwrap() > 0);
    assert_eq!(first["key_maps"], "2", "{}", moved.stdout);
    assert_eq!(first["key_maps_held"], "1", "{}", moved.stdout);
    assert_eq!(first["unaccounted"], "0", "{}", moved.stdout);
    assert_eq!(first["reseat_unaccounted"], "0", "{}", moved.stdout);
    assert!(
        moved.stdout.contains("estate-move gate=pass"),
        "{}",
        moved.stdout
    );
    let closure_json = fs::read_to_string(state.join("closure.json")).unwrap();
    assert!(
        closure_json.contains(&format!(
            "\"path\":\"projects/{}\",\"transfer_outcome\":\"mapped\",\"mapped\":\"projects/{}\"",
            key(&src_lane),
            key(&dst_lane)
        )),
        "{closure_json}"
    );
    assert!(
        closure_json.contains(&format!(
            "\"path\":\"projects/{}/{SID_RESUMED}.jsonl\",\"transfer_outcome\":\"consolidated\",\"consolidation\":\"covered\"",
            key(&dst_lane)
        )),
        "{closure_json}"
    );
    let inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&at_lane).unwrap());

    // The session grows on the source: extended in place, under the key
    // (both sides quiet past the racy window, as between real passes).
    let mut grown = neo;
    grown.push_str(&transcript(SID, &src_lane, "NEO2"));
    fs::write(lab.join(format!("{SID}.jsonl")), &grown).unwrap();
    settle();
    source_pass();
    let again = move_args(&[]);
    assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
    assert_eq!(
        fs::read_to_string(&at_lane).unwrap(),
        grown,
        "{}\n{}",
        again.stdout,
        again.stderr
    );
    // SPEC: the missing tail is appended in place, the same inode, so a
    // session writing through an open handle never loses a turn.
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(&at_lane).unwrap()),
        inode,
        "extended in place"
    );
    assert!(!at_lane.with_extension(format!("jsonl.{LABEL}")).exists());
    assert!(!dst_claude.join("projects").join(key(&src_lane)).exists());
    // The extended empty file is the seat's now: adopted, never landed
    // beside again.
    if linux {
        assert_eq!(fs::read_to_string(&empty_at).unwrap(), neo_empty);
        assert!(!dst_lane_projects
            .join(format!("{SID_EMPTY}.jsonl.{LABEL}"))
            .exists());
    }

    // Unchanged: the claude transfer reads nothing.
    settle();
    source_pass();
    let third = move_args(&[]);
    assert!(third.ok, "{}\n{}", third.stdout, third.stderr);
    let line = claude_line(&third.stdout);
    assert!(line.contains(" source_bytes_read=0 "), "{line}");
    assert!(line.ends_with(" mapped=1"), "{line}");

    // The report-only proof: the shared key's directory, its file already
    // under the lane's key, byte for byte; nothing pulled or written.
    let before = fs::read_dir(&state).unwrap().count();
    let proof = move_args(&[&"--key-map-proof"]);
    assert!(proof.ok, "{}\n{}", proof.stdout, proof.stderr);
    assert!(
        proof
            .stdout
            .contains("estate-move key-map-proof dirs=1 files=1 statuses=identical:1"),
        "{}",
        proof.stdout
    );
    assert!(
        proof.stdout.contains("\"basis\":\"cwd\""),
        "{}",
        proof.stdout
    );
    assert!(
        !proof.stdout.contains("estate-move phase="),
        "{}",
        proof.stdout
    );
    assert_eq!(fs::read_dir(&state).unwrap().count(), before);
}

/// Operator ruling 2026-10-20 ("neither WIN. they must be consolidated"): a
/// session both sides continued, under the lane's key the pull maps to,
/// lands as one file holding both branches (neo's bytes, then sting's
/// records neo lacks), never a beside copy; the re-seat finds it in place,
/// the closure names the consolidation, and the gate passes.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "the process check proves nothing off Linux: every consolidation is unsettled"
)]
fn a_session_both_sides_continued_is_consolidated_into_one_file() {
    const SID_BOTH: &str = "5e5e5e5e-aaaa-4222-8333-444444444444";
    let base = std::env::temp_dir().join(format!("bulkload-move-both-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "f.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));
    let settings = b"{\"cleanupPeriodDays\": 3650}\n";
    let src_claude = src_home.join(".claude");
    let lab = src_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&lab).unwrap();
    let old = transcript(SID_BOTH, &src_lane, "OLD");
    let mut neo = old.clone();
    neo.push_str(&transcript(SID_BOTH, &src_lane, "NEO-MORE"));
    fs::write(lab.join(format!("{SID_BOTH}.jsonl")), &neo).unwrap();
    fs::write(src_claude.join("settings.json"), settings).unwrap();
    fs::create_dir_all(src_home.join(".codex")).unwrap();
    let dst_claude = dst_home.join(".claude");
    let at_lane = dst_claude.join("projects").join(key(&dst_lane));
    fs::create_dir_all(&at_lane).unwrap();
    let mut sting = old;
    sting.push_str(&transcript(SID_BOTH, &dst_lane, "STING-MORE"));
    fs::write(at_lane.join(format!("{SID_BOTH}.jsonl")), &sting).unwrap();
    fs::write(dst_claude.join("settings.json"), settings).unwrap();
    fs::create_dir_all(dst_home.join(".codex")).unwrap();
    bulkload_agent::transfer::settle_racy_window(&src_claude).unwrap();
    bulkload_agent::transfer::settle_racy_window(&dst_claude).unwrap();

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src_git,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let moved = agent(&os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"-",
        &source_state,
        &state,
    ]));
    assert!(moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    let one = at_lane.join(format!("{SID_BOTH}.jsonl"));
    let sting_branch = transcript(SID_BOTH, &dst_lane, "STING-MORE");
    assert_eq!(
        fs::read_to_string(&one).unwrap(),
        format!("{neo}{sting_branch}"),
        "one file: neo's bytes, then sting's branch"
    );
    assert!(
        !at_lane.join(format!("{SID_BOTH}.jsonl.{LABEL}")).exists(),
        "never a beside copy"
    );
    let totals = closure(&moved.stdout);
    assert_eq!(totals["diverged"], "0", "{}", moved.stdout);
    assert_eq!(totals["applied_beside"], "0", "{}", moved.stdout);
    assert_eq!(totals["reseat_held"], "none", "{}", moved.stdout);
    let closure_json = fs::read_to_string(state.join("closure.json")).unwrap();
    assert!(
        closure_json.contains("\"transfer_outcome\":\"consolidated\""),
        "{closure_json}"
    );
    assert!(
        moved.stdout.contains("estate-move gate=pass"),
        "{}",
        moved.stdout
    );
}

/// Verification of #280, N4 across passes: provenance is sticky. A neo
/// session with no lane (a Claude transcript, and a Codex rollout neo's
/// index has no row for) is `RESEAT_LANE_UNMAPPED` while neo lists it; once
/// neo prunes it, the next pass still holds it typed (the pulls' listing is
/// cumulative), and so does a pass whose listing no longer names it (the
/// re-seat ledger recorded it as neo's): never `in-place`, the gate never
/// passes on it, and its refused ledger row is never overwritten.
#[test]
fn listed_then_pruned_sessions_with_no_lane_stay_typed() {
    const SID_PRUNED: &str = "5e5e5e5e-8888-4222-8333-444444444444";
    const T_LANE: &str = "019a0000-0000-7000-8000-00000000000b";
    const T_PRUNED: &str = "019a0000-0000-7000-8000-00000000000c";
    const T_DEST: &str = "019a0000-0000-7000-8000-00000000000d";
    let base = std::env::temp_dir().join(format!("bulkload-move-pruned-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));

    // neo: a session in a scratch directory of its home, which has no lane
    // (neo's /Users/jess/scratch).
    let scratch = src_home.join("scratch");
    let scratch_projects = src_home.join(".claude/projects").join(key(&scratch));
    fs::create_dir_all(&scratch_projects).unwrap();
    let pruned_transcript = scratch_projects.join(format!("{SID_PRUNED}.jsonl"));
    fs::write(
        &pruned_transcript,
        transcript(SID_PRUNED, &scratch, "PRUNED"),
    )
    .unwrap();
    fs::write(
        src_home.join(".claude/settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    // neo's Codex: a thread of the lane its index names, and a rollout of
    // the scratch directory it has no row for.
    let src_codex = src_home.join(".codex");
    let meta = |id: &str, cwd: &Path| {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{}\"}}}}\n{{\"neo\":true}}\n",
            cwd.display()
        )
    };
    let lane_rel = format!("sessions/2026/10/19/rollout-2026-10-19T10-00-00-{T_LANE}.jsonl");
    let pruned_rel = format!("sessions/2026/10/19/rollout-2026-10-19T11-00-00-{T_PRUNED}.jsonl");
    fs::create_dir_all(src_codex.join(&lane_rel).parent().unwrap()).unwrap();
    fs::write(src_codex.join(&lane_rel), meta(T_LANE, &src_lane)).unwrap();
    fs::write(src_codex.join(&pruned_rel), meta(T_PRUNED, &scratch)).unwrap();
    codex_index(
        &src_codex.join("state_5.sqlite"),
        T_LANE,
        src_codex.join(&lane_rel).to_str().unwrap(),
        &src_lane,
    );

    // sting: its live homes, with an index naming a thread of its own.
    let dst_claude = dst_home.join(".claude");
    fs::create_dir_all(&dst_claude).unwrap();
    fs::write(
        dst_claude.join("settings.json"),
        b"{\"cleanupPeriodDays\": 3650}\n",
    )
    .unwrap();
    let dst_codex = dst_home.join(".codex");
    let dest_rollout = dst_codex.join(format!(
        "sessions/2026/10/18/rollout-2026-10-18T10-00-00-{T_DEST}.jsonl"
    ));
    fs::create_dir_all(dest_rollout.parent().unwrap()).unwrap();
    fs::write(&dest_rollout, meta(T_DEST, &dst_lane)).unwrap();
    codex_index(
        &dst_codex.join("state_5.sqlite"),
        T_DEST,
        dest_rollout.to_str().unwrap(),
        &dst_lane,
    );

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let pass = || {
        let sourced = agent(&os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &source_state,
            &src_git,
        ]));
        assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
        agent(&os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--sessions",
            &src_home,
            &"--dest-home",
            &dst_home,
            &"--map",
            &map,
            // As in the archive test: the by-ID prefixes would take a `/tmp`
            // test directory's session for a by-ID one.
            &"--reseat-arg",
            &"--no-default-by-id",
            &"-",
            &source_state,
            &state,
        ]))
    };
    let ledger = state.join("reseat").join("reseat.sqlite");
    let outcome = |tool: &str, id: &str| -> String {
        let conn = rusqlite::Connection::open_with_flags(
            &ledger,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row(
            "SELECT outcome FROM session WHERE tool = ?1 AND id = ?2",
            rusqlite::params![tool, id],
            |row| row.get(0),
        )
        .unwrap()
    };
    let held = |moved: &Ran, at: &str| {
        assert!(
            moved.stdout.contains("estate-move phase=reseat outcome=ok"),
            "{at}\n{}\n{}",
            moved.stdout,
            moved.stderr
        );
        let totals = closure(&moved.stdout);
        assert_eq!(
            totals["reseat_held"], "claude/RESEAT_LANE_UNMAPPED:1,codex/RESEAT_LANE_UNMAPPED:1",
            "{at}\n{}",
            moved.stdout
        );
        assert!(
            moved.stdout.contains("estate-move gate=fail"),
            "{at}\n{}",
            moved.stdout
        );
        assert_eq!(
            outcome("claude", SID_PRUNED),
            "RESEAT_LANE_UNMAPPED",
            "{at}"
        );
        assert_eq!(outcome("codex", T_PRUNED), "RESEAT_LANE_UNMAPPED", "{at}");
    };

    // Pass 1: neo lists both; both are typed and hold the gate.
    let first = pass();
    held(&first, "pass 1");
    let landed_transcript = dst_claude
        .join("projects")
        .join(key(&scratch))
        .join(format!("{SID_PRUNED}.jsonl"));
    assert!(landed_transcript.is_file(), "{}", first.stdout);
    assert!(dst_codex.join(&pruned_rel).is_file(), "{}", first.stdout);

    // neo prunes both. Pass 2: absent from this pass's walk, still neo's
    // (the cumulative listing names them).
    fs::remove_file(&pruned_transcript).unwrap();
    fs::remove_file(src_codex.join(&pruned_rel)).unwrap();
    let second = pass();
    held(&second, "pass 2");
    let listing = fs::read(state.join("pull/claude.listing")).unwrap();
    let rel = format!("projects/{}/{SID_PRUNED}.jsonl", key(&scratch));
    assert!(
        listing
            .split(|byte| *byte == 0)
            .any(|entry| entry == rel.as_bytes()),
        "{}",
        String::from_utf8_lossy(&listing)
    );

    // Pass 3: a listing that no longer names them (an earlier build's
    // state): the re-seat ledger's rows keep them neo's.
    fs::remove_file(state.join("pull/claude.listing")).unwrap();
    fs::remove_file(state.join("pull/codex.listing")).unwrap();
    let third = pass();
    held(&third, "pass 3");
    let listing = fs::read(state.join("pull/claude.listing")).unwrap();
    assert!(
        !listing
            .split(|byte| *byte == 0)
            .any(|entry| entry == rel.as_bytes()),
        "{}",
        String::from_utf8_lossy(&listing)
    );
}

/// Every regular file under `root`, by relative path, with its bytes.
fn tree_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path);
            } else if meta.is_file() {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((name, fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Operator ruling 2026-10-20, item 3: `estate-move --plan-only --sessions`
/// lists every file of the homes with its class, its action and the sizes
/// on both sides before anything lands, and writes nothing to them.
#[test]
fn the_homes_plan_lists_every_file_and_writes_nothing() {
    let base = std::env::temp_dir().join(format!("bulkload-move-plan-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(&dst_git).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let src_lane = src_git.join("lab");
    let dst_lane = dst_git.join("lab");
    let src_claude = src_home.join(".claude");
    let src_projects = src_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&src_projects).unwrap();
    fs::write(
        src_projects.join(format!("{SID}.jsonl")),
        transcript(SID, &src_lane, "NEO"),
    )
    .unwrap();
    fs::write(src_claude.join("settings.json"), b"{\"theme\": \"neo\"}\n").unwrap();
    fs::write(src_claude.join("same.txt"), b"same\n").unwrap();
    fs::write(src_claude.join("blob.bin"), b"neo bytes").unwrap();
    fs::write(src_home.join(".claude.json"), b"{\"numStartups\": 40}\n").unwrap();
    let src_codex = src_home.join(".codex");
    fs::create_dir_all(&src_codex).unwrap();
    fs::write(src_codex.join("config.toml"), b"model = \"gpt-5\"\n").unwrap();

    let dst_claude = dst_home.join(".claude");
    fs::create_dir_all(&dst_claude).unwrap();
    fs::write(dst_claude.join("settings.json"), b"{\"model\": \"opus\"}\n").unwrap();
    fs::write(dst_claude.join("same.txt"), b"same\n").unwrap();
    fs::write(dst_claude.join("blob.bin"), b"sting bytes").unwrap();
    fs::write(dst_home.join(".claude.json"), b"{\"numStartups\": 12}\n").unwrap();
    let dst_codex = dst_home.join(".codex");
    fs::create_dir_all(&dst_codex).unwrap();
    fs::write(dst_codex.join("config.toml"), b"# sting\n").unwrap();
    let before = tree_files(&dst_home);

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let sourced = agent(&os(&[
        &"estate-move-source",
        &"--map",
        &map,
        &source_state,
        &src_git,
    ]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let planned = agent(&os(&[
        &"estate-move",
        &"--plan-only",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--sessions",
        &src_home,
        &"--dest-home",
        &dst_home,
        &"--map",
        &map,
        &"-",
        &source_state,
        &state,
    ]));
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    let line = |path: &str, action: &str| {
        planned.stdout.lines().any(|line| {
            line.starts_with(&format!("planned {path} "))
                && line.contains(&format!(" action={action} "))
        })
    };
    for (path, action) in [
        ("settings.json", "consolidate"),
        ("same.txt", "adopt"),
        ("blob.bin", "hold"),
        (
            // Under the key map the real pass derives (#280 finding 9).
            &format!("projects/{}/{SID}.jsonl", key(&dst_lane)) as &str,
            "create",
        ),
        (".claude.json", "consolidate"),
        ("config.toml", "consolidate"),
    ] {
        assert!(line(path, action), "{path} {action}:\n{}", planned.stdout);
    }
    assert!(
        planned
            .stdout
            .contains("planned blob.bin class=opaque action=hold source_size=9 live_size=11 reason=\"opaque: the bytes differ\""),
        "{}",
        planned.stdout
    );
    // Nothing landed, merged, made or recorded.
    assert_eq!(tree_files(&dst_home), before);
    assert!(!dst_claude.join("projects").exists());
    assert!(!state.join("pull").join("claude").exists());
}

/// Every name under `root` ending in `.LABEL`, the beside form (SPEC rule 3
/// forbids it in a home).
fn label_copies(root: &Path) -> Vec<String> {
    let suffix = format!(".{LABEL}");
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if name.ends_with(&suffix) {
                found.push(name);
            }
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            }
        }
    }
    found.sort();
    found
}

/// SPEC rule 3 ("no beside copies"): `--beside` is gone from `copy` and
/// `pull`, refused before anything is written, in either spelling.
#[test]
fn no_option_writes_a_label_copy_in_a_home() {
    let base = std::env::temp_dir().join(format!("bulkload-move-nolabel-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let (src, dst) = (base.join("src"), base.join("dst"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::write(src.join("settings.json"), b"{\"a\": 1}\n").unwrap();
    fs::write(dst.join("settings.json"), b"{\"b\": 2}\n").unwrap();
    let suffix = format!(".{LABEL}");
    for flags in [
        vec![OsString::from("--beside"), OsString::from(&suffix)],
        vec![OsString::from(format!("--beside={suffix}"))],
        vec![
            OsString::from("--consolidate"),
            OsString::from("--beside"),
            OsString::from(&suffix),
        ],
    ] {
        let mut args = vec![OsString::from("copy")];
        args.extend(flags);
        args.extend([
            src.clone().into_os_string(),
            dst.clone().into_os_string(),
            base.join("src-state").into_os_string(),
            base.join("dst-state").into_os_string(),
        ]);
        let ran = agent(&args);
        assert!(!ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr.contains("FIELD_DOMAIN_VIOLATION"),
            "{}",
            ran.stderr
        );
    }
    assert!(label_copies(&dst).is_empty());
    assert_eq!(
        fs::read(dst.join("settings.json")).unwrap(),
        b"{\"b\": 2}\n"
    );
    assert!(!base.join("dst-state").exists());
}

/// Operator interview and A-055, 2026-10-21: the homes pass resolves the
/// home's links before it pulls, and folds through links.
///
/// - A home directory linking into a `--live-link-root` is the live
///   directory itself: the source's files are merged there, the link stays.
/// - A link into a `--regularize-link-root` (a repair stage) becomes a real
///   directory holding the stage's files, then the source's are merged in;
///   the stage is never written, and the regularization is counted.
/// - A neo-key link (into the archive) whose target is the very bytes at
///   the lane's key is folded: only the link name goes, never its target.
/// - The lane of a repository under the git root is derived (no per-repo
///   --map).
#[test]
fn homes_links_are_resolved_and_folded_through() {
    homes_links(false);
}

/// 2026-10-22: the same pass over a transport. A remote source refuses a
/// missing link source before any frame (the pull can only read
/// `TRANSPORT_LOST`), so a link the home's own walk never reached runs no
/// session: nothing to merge, the sessions pull completes and the re-seat
/// runs.
#[test]
fn homes_links_over_a_transport_skip_a_link_the_source_lacks() {
    homes_links(true);
}

fn homes_links(remote: bool) {
    let base = std::env::temp_dir().join(format!(
        "bulkload-move-links-{}-{remote}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let base = &world.base;
    let src_home = base.join("src/home");
    let dst_home = base.join("dst/home");
    let (src_git, dst_git) = (src_home.join("git"), dst_home.join("git"));
    fs::create_dir_all(&src_git).unwrap();
    fs::create_dir_all(dst_git.join("lab")).unwrap();
    repo(&src_git.join("lab"), "l.txt");
    let (src_lane, dst_lane) = (src_git.join("lab"), dst_git.join("lab"));
    let src_claude = src_home.join(".claude");
    let dst_claude = dst_home.join(".claude");
    fs::create_dir_all(&dst_claude).unwrap();
    fs::create_dir_all(src_home.join(".codex/sessions")).unwrap();

    // The relocated live directory and its link.
    let relocated = base.join("relocated");
    fs::create_dir_all(relocated.join("agent-notes-rescue")).unwrap();
    fs::write(relocated.join("agent-notes-rescue/a.md"), b"sting\n").unwrap();
    std::os::unix::fs::symlink(
        relocated.join("agent-notes-rescue"),
        dst_claude.join("agent-notes-rescue"),
    )
    .unwrap();
    fs::create_dir_all(src_claude.join("agent-notes-rescue")).unwrap();
    fs::write(src_claude.join("agent-notes-rescue/a.md"), b"sting\n").unwrap();
    fs::write(src_claude.join("agent-notes-rescue/b.md"), b"neo\n").unwrap();

    // Deeper live links (operator interview 2026-10-21, "Merge in place at
    // link targets"): a session directory under the lane's key, and a Codex
    // rollout file, each a link into a second live-link root.
    let second_root = base.join("stage2");
    let sid2 = "5e5e5e5e-2222-4222-8333-444444444444";
    let session_dir = second_root.join("forks").join(sid2);
    fs::create_dir_all(session_dir.join("subagents")).unwrap();
    fs::write(session_dir.join("subagents/a.jsonl"), b"{\"uuid\":\"s\"}\n").unwrap();
    let src_session = src_claude
        .join("projects")
        .join(key(&src_lane))
        .join(sid2)
        .join("subagents");
    fs::create_dir_all(&src_session).unwrap();
    fs::write(
        src_session.join("a.jsonl"),
        b"{\"uuid\":\"s\"}\n{\"uuid\":\"n\"}\n",
    )
    .unwrap();
    fs::write(src_session.join("b.jsonl"), b"{\"uuid\":\"b\"}\n").unwrap();
    let rollout = second_root.join("live/rollout.jsonl");
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    fs::write(&rollout, b"{\"r\":1}\n").unwrap();
    fs::write(
        src_home.join(".codex/sessions/rollout.jsonl"),
        b"{\"r\":1}\n{\"r\":2}\n",
    )
    .unwrap();
    let dst_codex = dst_home.join(".codex");
    fs::create_dir_all(dst_codex.join("sessions")).unwrap();
    std::os::unix::fs::symlink(&rollout, dst_codex.join("sessions/rollout.jsonl")).unwrap();
    // A stage-linked rollout neo no longer has: nothing to merge.
    let gone = second_root.join("live/gone.jsonl");
    fs::write(&gone, b"{\"g\":1}\n").unwrap();
    std::os::unix::fs::symlink(&gone, dst_codex.join("sessions/gone.jsonl")).unwrap();

    // A stage-linked session directory neo no longer has: nothing to
    // merge, and no record of the absent source left to count.
    let sid3 = "5e5e5e5e-3333-4333-8333-444444444444";
    fs::create_dir_all(second_root.join("forks").join(sid3)).unwrap();
    fs::write(
        second_root.join("forks").join(sid3).join("kept.jsonl"),
        b"{}\n",
    )
    .unwrap();

    // A file-history link into the repair stage.
    let repair_stage = base.join("stage");
    let staged = repair_stage.join("forks/x/support/file-history").join(SID);
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("v1"), b"old\n").unwrap();
    fs::create_dir_all(dst_claude.join("file-history")).unwrap();
    std::os::unix::fs::symlink(&staged, dst_claude.join("file-history").join(SID)).unwrap();
    fs::create_dir_all(src_claude.join("file-history").join(SID)).unwrap();
    fs::write(
        src_claude.join("file-history").join(SID).join("v1"),
        b"old\n",
    )
    .unwrap();
    fs::write(
        src_claude.join("file-history").join(SID).join("v2"),
        b"new\n",
    )
    .unwrap();

    // The session: neo's copy at its key; sting's at the lane's key, and an
    // older hand copy at the neo key that is a link into the archive.
    let bytes = transcript(SID, &src_lane, "NEO");
    let src_projects = src_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&src_projects).unwrap();
    fs::write(src_projects.join(format!("{SID}.jsonl")), &bytes).unwrap();
    let lane_projects = dst_claude.join("projects").join(key(&dst_lane));
    fs::create_dir_all(&lane_projects).unwrap();
    std::os::unix::fs::symlink(&session_dir, lane_projects.join(sid2)).unwrap();
    std::os::unix::fs::symlink(
        second_root.join("forks").join(sid3),
        lane_projects.join(sid3),
    )
    .unwrap();
    fs::write(lane_projects.join(format!("{SID}.jsonl")), &bytes).unwrap();
    let archive = base.join("archive");
    fs::create_dir_all(&archive).unwrap();
    fs::write(archive.join("hand-copy.jsonl"), &bytes).unwrap();
    let neo_key_dir = dst_claude.join("projects").join(key(&src_lane));
    fs::create_dir_all(&neo_key_dir).unwrap();
    std::os::unix::fs::symlink(
        archive.join("hand-copy.jsonl"),
        neo_key_dir.join(format!("{SID}.jsonl")),
    )
    .unwrap();

    let source_state = base.join("move-src");
    let state = base.join("move-dst");
    let root = base.join("src/empty-root");
    fs::create_dir_all(&root).unwrap();
    let sourced = agent(&os(&[&"estate-move-source", &source_state, &root]));
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let map = format!("{}={}", src_git.display(), dst_git.display());
    let home_map = format!("{}={}", src_home.display(), dst_home.display());
    let fake_bin = base.join("fake-bin");
    fake_ssh(&fake_bin);
    let run = |plan: bool| {
        let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"estate-move"];
        if plan {
            args.push(&"--plan-only");
        }
        args.extend::<[&dyn AsRef<std::ffi::OsStr>; 18]>([
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--sessions",
            &src_home,
            &"--dest-home",
            &dst_home,
            &"--map",
            &home_map,
            &"--map",
            &map,
            &"--live-link-root",
            &relocated,
            &"--live-link-root",
            &second_root,
            &"--regularize-link-root",
            &repair_stage,
        ]);
        args.extend::<[&dyn AsRef<std::ffi::OsStr>; 2]>([&"--reseat-arg", &"--no-default-by-id"]);
        if remote {
            args.extend::<[&dyn AsRef<std::ffi::OsStr>; 3]>([
                &"--remote-executable",
                &env!("CARGO_BIN_EXE_bulkload-agent"),
                &"fakehost",
            ]);
        } else {
            args.push(&"-");
        }
        args.extend::<[&dyn AsRef<std::ffi::OsStr>; 2]>([&source_state, &state]);
        if remote {
            agent_over_ssh(&os(&args), &fake_bin)
        } else {
            agent(&os(&args))
        }
    };

    // The plan: the lane is derived, the regularization and the live link
    // are named, nothing is written.
    let homes = || {
        [&dst_home, &relocated, &repair_stage, &archive, &second_root]
            .iter()
            .map(|root| tree_files(root))
            .collect::<Vec<_>>()
    };
    let before = homes();
    let planned = run(true);
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    assert!(
        planned.stdout.contains(&format!(
            "estate-move key-map from={} to={}",
            key(&src_lane),
            key(&dst_lane)
        )),
        "{}",
        planned.stdout
    );
    assert!(
        planned
            .stdout
            .contains("estate-move regularized-links home=claude links=1 files=1 bytes=4 remaining=1 (plan only)"),
        "{}",
        planned.stdout
    );
    assert!(
        planned
            .stdout
            .contains("estate-move live-link home=plan-claude"),
        "{}",
        planned.stdout
    );
    assert!(
        planned.stdout.lines().any(|line| line.starts_with(&format!(
            "planned projects/{}/{SID}.jsonl class=append-log action=fold-union ",
            key(&src_lane)
        ))),
        "{}",
        planned.stdout
    );
    assert_eq!(homes(), before, "the plan wrote nothing");

    let moved = run(false);
    assert!(
        moved
            .stdout
            .contains("estate-move phase=sessions-pull outcome=ok"),
        "{}\n{}",
        moved.stdout,
        moved.stderr
    );
    // The live link: merged at its real directory; the link stays.
    assert!(fs::symlink_metadata(dst_claude.join("agent-notes-rescue"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(relocated.join("agent-notes-rescue/b.md")).unwrap(),
        b"neo\n"
    );
    assert_eq!(
        fs::read(relocated.join("agent-notes-rescue/a.md")).unwrap(),
        b"sting\n"
    );
    // The stage link: a real directory with the stage's and neo's files;
    // the stage untouched.
    let history = dst_claude.join("file-history").join(SID);
    assert!(
        fs::symlink_metadata(&history).unwrap().is_dir(),
        "{}",
        moved.stdout
    );
    assert_eq!(fs::read(history.join("v1")).unwrap(), b"old\n");
    assert_eq!(fs::read(history.join("v2")).unwrap(), b"new\n");
    assert_eq!(
        tree_files(&repair_stage)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        [format!("forks/x/support/file-history/{SID}/v1")]
    );
    assert!(
        moved.stdout.contains(
            "estate-move regularized-links home=claude links=1 files=1 bytes=4 remaining=0"
        ),
        "{}",
        moved.stdout
    );
    // The deeper live links: merged into their targets through the key map
    // backwards; the links stay.
    assert!(fs::symlink_metadata(lane_projects.join(sid2))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(session_dir.join("subagents/a.jsonl")).unwrap(),
        b"{\"uuid\":\"s\"}\n{\"uuid\":\"n\"}\n"
    );
    assert_eq!(
        fs::read(session_dir.join("subagents/b.jsonl")).unwrap(),
        b"{\"uuid\":\"b\"}\n"
    );
    assert!(
        fs::symlink_metadata(dst_codex.join("sessions/rollout.jsonl"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&rollout).unwrap(), b"{\"r\":1}\n{\"r\":2}\n");
    assert!(
        moved
            .stdout
            .contains("rel=sessions/gone.jsonl source=absent (nothing to merge)"),
        "{}",
        moved.stdout
    );
    assert_eq!(fs::read(&gone).unwrap(), b"{\"g\":1}\n");
    assert!(
        moved.stdout.contains(&format!(
            "rel=projects/{}/{sid3} source=absent (nothing to merge)",
            key(&dst_lane)
        )),
        "{}",
        moved.stdout
    );
    assert_eq!(
        fs::read(second_root.join("forks").join(sid3).join("kept.jsonl")).unwrap(),
        b"{}\n"
    );
    // 2026-10-22: the typed refusals never hold the re-seat; no step
    // refused; the closure joins the session pulls only (never the
    // estate's own `plan` and `corpus` pulls, one transfer twice).
    assert!(
        !moved.stdout.contains("phase=reseat outcome=skipped"),
        "{}",
        moved.stdout
    );
    assert!(
        !moved.stdout.contains("estate-move sessions-pull step="),
        "{}",
        moved.stdout
    );
    assert!(
        !moved.stderr.contains("refused: FIELD_DOMAIN_VIOLATION"),
        "{}",
        moved.stderr
    );
    let pulls: Vec<String> = fs::read_dir(state.join("pull"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        pulls
            .iter()
            .filter(|name| name.starts_with("claude-link-"))
            .count()
            >= 1,
        "{pulls:?}"
    );
    // A retired pass's pull of the same ledgers under another name
    // (`corpus`): never joined, so the top-up's closure joins every
    // transfer once.
    let retired = state.join("pull/corpus");
    fs::create_dir_all(retired.join("transfer.closure.d")).unwrap();
    for entry in fs::read_dir(state.join("pull/plan/transfer.closure.d"))
        .unwrap()
        .flatten()
    {
        fs::copy(
            entry.path(),
            retired.join("transfer.closure.d").join(entry.file_name()),
        )
        .unwrap();
    }
    let topped = run(false);
    assert!(
        !topped.stderr.contains("refused: FIELD_DOMAIN_VIOLATION"),
        "{}\n{}",
        topped.stdout,
        topped.stderr
    );
    assert!(
        topped
            .stdout
            .contains("estate-move phase=sessions-pull outcome=ok"),
        "{}",
        topped.stdout
    );
    assert!(fs::symlink_metadata(dst_codex.join("sessions/gone.jsonl"))
        .unwrap()
        .file_type()
        .is_symlink());
    // The neo-key link folded through: its name gone, its target kept, the
    // lane's file as it was.
    assert!(fs::symlink_metadata(neo_key_dir.join(format!("{SID}.jsonl"))).is_err());
    assert_eq!(
        fs::read_to_string(archive.join("hand-copy.jsonl")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read_to_string(lane_projects.join(format!("{SID}.jsonl"))).unwrap(),
        bytes
    );
}

/// An `ssh` stand-in first on `PATH`: drops ssh's own options and the
/// host, then runs the remote command here through a shell.
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

fn agent_over_ssh(args: &[OsString], fake_bin: &Path) -> Ran {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin.to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("--min-free-percent=0")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("PATH", std::env::join_paths(paths).unwrap())
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn agent_home(args: &[OsString], home: &Path) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("--min-free-percent=0")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", home)
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

impl World {
    fn planned(&self) -> Ran {
        agent(&os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--plan-only",
            &"-",
            &self.source_state(),
            &self.state(),
        ]))
    }
}

// The `key=value` words of `line`.
fn words(line: &str) -> BTreeMap<String, String> {
    line.split(' ')
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

// The plan's line for the item at `path`.
#[allow(clippy::unnecessary_debug_formatting)]
fn plan_line<'a>(stdout: &'a str, path: &Path) -> &'a str {
    let live = format!("live={path:?}");
    stdout
        .lines()
        .find(|line| {
            line.starts_with("estate-move item=")
                && line.ends_with(&live)
                && line.contains(" action=")
        })
        .unwrap_or_else(|| panic!("no plan line for {live}:\n{stdout}"))
}

/// The coordinator, 2026-10-21 ("the plan prints the B2 decisions"):
/// `--plan-only` says, per item, what the pass would do: the action, the
/// ref union (created, tracked at refs/remotes/neo), the files to land
/// with their three-way merges and conflicts, the duplicates rule 3
/// retires, and the bytes to fetch; the per-file rows go to `diff.jsonl`,
/// read from the B1 ledgers. Nothing is written to the live repository,
/// and the state holds no payload.
#[test]
fn the_plan_prints_every_b2_decision_and_writes_nothing() {
    let world = World::new("b2plan");
    let (source, live) = (world.src().join("p"), world.dst().join("p"));
    fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(source.join("g.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "c0"]);
    git(
        &world.dst(),
        &[
            "clone",
            "--quiet",
            "--template=",
            source.to_str().unwrap(),
            "p",
        ],
    );
    git(&live, &["remote", "remove", "origin"]);
    // A branch only neo has; one both moved apart.
    git(&source, &["branch", "topic"]);
    git(&source, &["checkout", "--quiet", "topic"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "topic work"],
    );
    git(&source, &["checkout", "--quiet", "-b", "div", "main"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "neo div"],
    );
    git(&source, &["checkout", "--quiet", "main"]);
    git(&live, &["branch", "div"]);
    git(&live, &["checkout", "--quiet", "div"]);
    git(
        &live,
        &["commit", "--quiet", "--allow-empty", "-m", "sting div"],
    );
    git(&live, &["checkout", "--quiet", "main"]);
    // Uncommitted on both sides: f.txt merges, g.txt conflicts; neo's new
    // file lands.
    fs::write(source.join("f.txt"), "one\ntwo\nthree\nfour\nFIVE neo\n").unwrap();
    fs::write(live.join("f.txt"), "ONE sting\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(source.join("g.txt"), "alpha\nBETA neo\ngamma\n").unwrap();
    fs::write(live.join("g.txt"), "alpha\nBETA sting\ngamma\n").unwrap();
    fs::write(source.join("new.txt"), "neo only\n").unwrap();
    // A duplicate an older pass left beside the live path.
    fs::create_dir_all(world.dst().join("p.neo-20261016")).unwrap();
    let refs_before = out(&live, &["for-each-ref"]);
    let files_before = tree_files(&live);
    assert!(world.source().ok);
    let planned = world.planned();
    let both = format!("{}\n{}", planned.stdout, planned.stderr);
    assert!(planned.ok, "{both}");
    let row = words(plan_line(&planned.stdout, &live));
    assert_eq!(row["action"], "consolidate", "{both}");
    assert_eq!(row["refs_created"], "1", "{both}");
    assert_eq!(row["refs_tracked"], "1", "{both}");
    assert_eq!(row["merges"], "1", "{both}");
    assert_eq!(row["conflicts"], "1", "{both}");
    assert_eq!(row["land"], "1", "{both}");
    assert_eq!(row["untracked"], "1", "{both}");
    assert_eq!(row["changed"], "2", "{both}");
    assert_eq!(row["asides"], "1", "{both}");
    assert_eq!(row["worktree"], "main", "{both}");
    assert!(
        planned
            .stdout
            .contains("ref=refs/heads/div union=tracked at=refs/remotes/neo/div"),
        "{both}"
    );
    assert!(planned.stdout.contains("action=retire-beside"), "{both}");
    let repository = planned
        .stdout
        .lines()
        .find(|line| line.starts_with("estate-move repository="))
        .unwrap_or_else(|| panic!("{both}"));
    let fetch: u64 = words(repository)["fetch_bytes"].parse().unwrap();
    assert!(fetch > 0, "{repository}");
    let summary = words(
        planned
            .stdout
            .lines()
            .find(|line| line.starts_with("estate-move plan items="))
            .unwrap(),
    );
    assert_eq!(summary["consolidate"], "1", "{both}");
    assert_eq!(summary["conflicts"], "1", "{both}");
    let rows = fs::read_to_string(world.state().join("diff.jsonl")).unwrap();
    for (path, verdict) in [
        ("f.txt", "merged"),
        ("g.txt", "conflict"),
        ("new.txt", "land"),
    ] {
        assert!(
            rows.lines()
                .any(|row| row.contains(&format!("\"path\":\"{path}\""))
                    && row.contains(&format!("\"verdict\":\"{verdict}\""))),
            "{path} {verdict}:\n{rows}"
        );
    }
    // Nothing written: the live refs and files as they were, no scratch.
    assert_eq!(out(&live, &["for-each-ref"]), refs_before);
    assert_eq!(tree_files(&live), files_before);
    assert!(!world.state().join("scratch").exists());
    holds_no_payload(&world.state(), 1);
}

/// SPEC rule 3 (no duplicates; the coordinator, 2026-10-21: "no
/// carry-beside on any path"): a capture the source refused is a typed
/// hold. Nothing is carried as files, beside an occupied live path or at
/// an absent one, and the plan names the source's code.
#[test]
fn a_refused_capture_is_held_and_nothing_is_written_beside_or_live() {
    let world = World::new("nobeside");
    world.estate();
    let src = world.src();
    for name in ["other", "solo"] {
        if name == "solo" {
            repo(&src.join("solo"), "s.txt");
        }
        let at = src.join(name);
        git(&at, &["config", "core.repositoryformatversion", "1"]);
        git(&at, &["config", "remote.origin.url", "/nowhere"]);
        git(&at, &["config", "remote.origin.promisor", "true"]);
        git(&at, &["config", "extensions.partialClone", "origin"]);
    }
    let occupied = world.dst().join("other");
    repo(&occupied, "mine.txt");
    let before = tree(&world.dst());
    let sourced = world.source();
    assert!(
        sourced.stdout.contains("code=GIT_SOURCE_PARTIAL_CLONE"),
        "{}\n{}",
        sourced.stdout,
        sourced.stderr
    );
    let planned = world.planned();
    let both = format!("{}\n{}", planned.stdout, planned.stderr);
    for place in [&occupied, &world.dst().join("solo")] {
        let row = words(plan_line(&planned.stdout, place));
        assert_eq!(row["action"], "held", "{both}");
        assert_eq!(row["code"], "GIT_SOURCE_PARTIAL_CLONE", "{both}");
    }
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    for place in [&occupied, &world.dst().join("solo")] {
        assert!(
            moved
                .stdout
                .lines()
                .any(|line| line.contains("outcome=held code=CAPTURE_ABSENT")
                    && line.contains("capture_code=GIT_SOURCE_PARTIAL_CLONE")),
            "{} {both}",
            place.display()
        );
    }
    assert!(!world.dst().join("solo").exists(), "{both}");
    let after = tree(&world.dst());
    for rel in &after {
        let name = rel.to_string_lossy();
        assert!(
            !name.contains(&format!(".{LABEL}")) && !name.starts_with("other."),
            "a copy beside: {name}\n{both}"
        );
    }
    let mine: Vec<_> = before
        .iter()
        .filter(|rel| rel.starts_with("other"))
        .collect();
    let still: Vec<_> = after
        .iter()
        .filter(|rel| rel.starts_with("other"))
        .collect();
    assert_eq!(mine, still, "the occupied live path is untouched");
}

/// The coordinator, 2026-10-21: a path outside every `--map` (a worktree
/// under the source's home, outside `~/git`) is placed by the default home
/// map, `SOURCE_HOME=DEST_HOME`, its absent parents made 0700 under the
/// destination home; never at the source's raw path.
#[test]
fn a_path_outside_the_maps_lands_under_the_destination_home() {
    let world = World::new("homemap");
    let (src_home, dst_home) = (world.base.join("src"), world.base.join("dst"));
    let land = world.src().join("land");
    repo(&land, "a.txt");
    let outside = src_home.join(".cache/lanes/wt");
    git(
        &land,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            outside.to_str().unwrap(),
        ],
    );
    let map = format!("{}={}", world.src().display(), world.dst().display());
    let sourced = agent_home(
        &os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &world.source_state(),
            &world.src(),
        ]),
        &src_home,
    );
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let args = |plan: bool| {
        let mut args = os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"--dest-home",
            &dst_home,
        ]);
        if plan {
            args.push("--plan-only".into());
        }
        args.extend(os(&[&"-", &world.source_state(), &world.state()]));
        args
    };
    let mapped = dst_home.join(".cache/lanes/wt");
    let planned = agent_home(&args(true), &dst_home);
    let both = format!("{}\n{}", planned.stdout, planned.stderr);
    let line = words(plan_line(&planned.stdout, &mapped));
    assert_eq!(line["action"], "create", "{both}");
    assert_eq!(line["parents"], "2", "{both}");
    assert_eq!(line["under_home"], "true", "{both}");
    assert!(!dst_home.join(".cache").exists(), "the plan makes nothing");
    let moved = agent_home(&args(false), &dst_home);
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let totals = closure(&moved.stdout);
    assert_eq!(totals["applied"], "2", "{both}");
    assert_eq!(totals["unaccounted"], "0", "{both}");
    let report = fs::read_to_string(world.state().join("closure.json")).unwrap();
    assert!(
        report.contains("\"foreign_outcome_records\":[]"),
        "{report}"
    );
    assert!(mapped.join("a.txt").is_file(), "{both}");
    // The hand-off reads the placed path, never the plan's source path.
    assert!(!moved.stdout.contains("HANDOFF_NOT_LANDED"), "{both}");
    assert!(
        moved
            .stdout
            .contains("estate-move phase=hand-off outcome=ok"),
        "{both}"
    );
    for level in [dst_home.join(".cache"), dst_home.join(".cache/lanes")] {
        let mode = fs::symlink_metadata(&level).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o700, "{} {both}", level.display());
    }
    assert_eq!(
        out(
            &world.dst().join("land"),
            &["worktree", "list", "--porcelain"]
        )
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count(),
        2,
        "{both}"
    );
    let again = agent_home(&args(false), &dst_home);
    let both = format!("{}\n{}", again.stdout, again.stderr);
    let totals = closure(&again.stdout);
    assert_eq!(totals["applied"], "2", "{both}");
    assert_eq!(totals["unaccounted"], "0", "{both}");
    assert_eq!(
        out(
            &world.dst().join("land"),
            &["worktree", "list", "--porcelain"]
        )
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count(),
        2,
        "a repeated pass keeps each mapped worktree registered once: {both}"
    );
}

/// SPEC Done 2: a home-mapped worktree's typed refusal belongs to its
/// original plan item, including when a pass is retried.
#[test]
fn a_home_mapped_capture_refusal_keeps_its_plan_identity() {
    let world = World::new("homemap-refusal");
    let (src_home, dst_home) = (world.base.join("src"), world.base.join("dst"));
    let land = world.src().join("land");
    repo(&land, "a.txt");
    let outside = src_home.join(".cache/lanes/wt");
    git(
        &land,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            outside.to_str().unwrap(),
        ],
    );
    git(&land, &["config", "core.repositoryformatversion", "1"]);
    git(&land, &["config", "remote.origin.url", "/nowhere"]);
    git(&land, &["config", "remote.origin.promisor", "true"]);
    git(&land, &["config", "extensions.partialClone", "origin"]);
    let map = format!("{}={}", world.src().display(), world.dst().display());
    let sourced = agent_home(
        &os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &world.source_state(),
            &world.src(),
        ]),
        &src_home,
    );
    assert!(
        sourced.stdout.contains("code=GIT_SOURCE_PARTIAL_CLONE"),
        "{}\n{}",
        sourced.stdout,
        sourced.stderr
    );
    let args = os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--dest-home",
        &dst_home,
        &"-",
        &world.source_state(),
        &world.state(),
    ]);
    for _ in 0..2 {
        let moved = agent_home(&args, &dst_home);
        let both = format!("{}\n{}", moved.stdout, moved.stderr);
        let totals = closure(&moved.stdout);
        assert_eq!(totals["planned"], "2", "{both}");
        assert_eq!(totals["refused_pending_review"], "2", "{both}");
        assert_eq!(totals["unaccounted"], "0", "{both}");
        assert_eq!(totals["applied"], "0", "{both}");
        let report = fs::read_to_string(world.state().join("closure.json")).unwrap();
        assert!(
            report.contains("\"foreign_outcome_records\":[]"),
            "{report}"
        );
        assert!(!dst_home.join(".cache").exists(), "{both}");
        assert!(!world.dst().join("land").exists(), "{both}");
    }
}

/// SPEC rules 7 and 8: an older pass's placement-keyed records remain
/// untouched while a retry writes outcomes under the original plan ID.
#[test]
fn a_home_mapped_retry_preserves_foreign_outcomes_and_journals() {
    use bulkload_agent::estate::{self, streamed};

    let world = World::new("homemap-foreign");
    let (src_home, dst_home) = (world.base.join("src"), world.base.join("dst"));
    let land = world.src().join("land");
    repo(&land, "a.txt");
    let outside = src_home.join(".cache/lanes/wt");
    git(
        &land,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            outside.to_str().unwrap(),
        ],
    );
    let map = format!("{}={}", world.src().display(), world.dst().display());
    let sourced = agent_home(
        &os(&[
            &"estate-move-source",
            &"--map",
            &map,
            &world.source_state(),
            &world.src(),
        ]),
        &src_home,
    );
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    let carry = world.source_state().join("carry");
    let planned = estate::inspect(&carry.join("plan.postcard"))
        .unwrap()
        .into_iter()
        .find(|item| item.source == outside)
        .unwrap();
    let planned_id = estate::item_id(&planned).unwrap();
    let mut old = streamed::recorded(&carry, &planned_id).unwrap().unwrap();
    old.item.workspace = Some(dst_home.join(".cache/lanes/wt"));
    let foreign_id = estate::item_id(&old.item).unwrap();
    assert_ne!(planned_id, foreign_id);
    let private = world.state().join("private");
    estate::moving::private_dir(&world.state()).unwrap();
    estate::moving::private_dir(&private).unwrap();
    streamed::record_outcome(&private, &old.item, "workspace-restored", None, None).unwrap();
    streamed::journal(&private, &old, "neo").unwrap();
    let legacy: Vec<_> = fs::read_dir(&private)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert_eq!(legacy.len(), 2);
    let args = os(&[
        &"estate-move",
        &"--label",
        &LABEL,
        &"--source",
        &"neo",
        &"--dest-home",
        &dst_home,
        &"-",
        &world.source_state(),
        &world.state(),
    ]);
    for _ in 0..2 {
        let moved = agent_home(&args, &dst_home);
        let both = format!("{}\n{}", moved.stdout, moved.stderr);
        let totals = closure(&moved.stdout);
        assert_eq!(totals["applied"], "2", "{both}");
        assert_eq!(totals["unaccounted"], "0", "{both}");
        let report = fs::read_to_string(world.state().join("closure.json")).unwrap();
        assert!(
            report.contains(&format!("\"foreign_outcome_records\":[\"{foreign_id}\"]")),
            "{report}"
        );
        assert!(private.join(format!("{planned_id}.outcome")).is_file());
        for (path, bytes) in &legacy {
            assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        }
    }
}

/// SPEC rule 3 ("Duplicates already on sting are consolidated into the
/// live path and removed"; the coordinator, 2026-10-21): a copy an older
/// pass left beside a live checkout (`.neo-*`) is consolidated into it
/// through the B2 rules (its branch created, a diverged one tracked at
/// `refs/remotes/SUFFIX/`, a changed file merged three-way), then removed.
/// A copy whose file conflicts is kept, typed, and the live file is the
/// destination's.
#[test]
fn a_copy_beside_the_live_path_is_consolidated_and_removed() {
    let world = World::new("asides");
    let (_source, live) = related_pair(&world, "p");
    let dup = world.dst().join("p.neo-20261016");
    git(
        &world.dst(),
        &[
            "clone",
            "--quiet",
            "--template=",
            live.to_str().unwrap(),
            "p.neo-20261016",
        ],
    );
    git(&dup, &["remote", "remove", "origin"]);
    git(&dup, &["checkout", "--quiet", "-b", "only-in-copy"]);
    git(
        &dup,
        &["commit", "--quiet", "--allow-empty", "-m", "copy work"],
    );
    let copy_work = out(&dup, &["rev-parse", "HEAD"]);
    git(&dup, &["checkout", "--quiet", "main"]);
    git(
        &dup,
        &["commit", "--quiet", "--allow-empty", "-m", "copy main"],
    );
    git(
        &live,
        &["commit", "--quiet", "--allow-empty", "-m", "live main"],
    );
    let live_main = out(&live, &["rev-parse", "main"]);
    let copy_main = out(&dup, &["rev-parse", "main"]);
    fs::write(dup.join("f.txt"), "one\ntwo\nthree\nfour\nFIVE copy\n").unwrap();
    // A second copy whose file conflicts with the live one: kept.
    let held = world.dst().join("p.sting-1015");
    git(
        &world.dst(),
        &[
            "clone",
            "--quiet",
            "--template=",
            live.to_str().unwrap(),
            "p.sting-1015",
        ],
    );
    git(&held, &["remote", "remove", "origin"]);
    fs::write(held.join("f.txt"), "ONE held\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(live.join("f.txt"), "ONE live\ntwo\nthree\nfour\nfive\n").unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.lines().any(|line| line.contains("aside=")
            && line.contains("p.neo-20261016")
            && line.ends_with("outcome=retired")),
        "{both}"
    );
    assert!(!dup.exists(), "{both}");
    assert_eq!(
        out(&live, &["rev-parse", "refs/heads/only-in-copy"]),
        copy_work
    );
    assert_eq!(out(&live, &["rev-parse", "main"]), live_main, "{both}");
    assert_eq!(
        out(&live, &["rev-parse", "refs/remotes/neo-20261016/main"]),
        copy_main,
        "{both}"
    );
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"ONE live\ntwo\nthree\nfour\nFIVE copy\n",
        "{both}"
    );
    assert!(
        moved
            .stdout
            .lines()
            .any(|line| line.contains("p.sting-1015") && line.contains("outcome=held")),
        "{both}"
    );
    assert!(held.join("f.txt").is_file(), "the conflicting copy is kept");
    assert_eq!(
        fs::read(live.join("f.txt")).unwrap(),
        b"ONE live\ntwo\nthree\nfour\nFIVE copy\n"
    );
}

/// 2026-10-21: a plan pull never makes `carry/corpus` in the state, and an
/// empty one an older pass left (here on the source half) is removed, not
/// refused `STATE_FORMAT_RETIRED`.
#[test]
fn no_pass_leaves_an_empty_corpus_and_an_old_one_is_removed() {
    let world = World::new("nocorpus");
    world.estate();
    fs::create_dir_all(world.source_state().join("carry/corpus")).unwrap();
    for dir in [world.source_state(), world.source_state().join("carry")] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sourced = world.source();
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    assert!(!world.source_state().join("carry/corpus").exists());
    fs::create_dir_all(world.source_state().join("carry/corpus")).unwrap();
    let planned = world.planned();
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    assert!(!world.state().join("carry/corpus").exists());
    for state in [world.state(), world.source_state()] {
        assert!(
            !tree(&state).iter().any(|rel| rel.ends_with("corpus")),
            "{}: {:?}",
            state.display(),
            tree(&state)
        );
    }
}

// The receipt line of the item at `live` on a pass's stdout.
fn receipt_words(stdout: &str, live: &Path) -> BTreeMap<String, String> {
    words(item_line(stdout, live))
}

/// The coordinator, 2026-10-21: a registered worktree nested in a main's
/// checkout (`.worktrees/NAME`, tinyland.dev's layout) is its own item,
/// never the main's seats. A destination that holds other files at that
/// path leaves the main's consolidation whole.
#[test]
fn nested_worktrees_are_never_the_mains_seats() {
    let world = World::new("nestedwt");
    let (source, live) = related_pair(&world, "p");
    git(
        &source,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "side",
            ".worktrees/side",
        ],
    );
    fs::write(source.join(".worktrees/side/u.txt"), "neo's untracked\n").unwrap();
    fs::create_dir_all(live.join(".worktrees/side")).unwrap();
    fs::write(live.join(".worktrees/side/u.txt"), "sting's own\n").unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let main = receipt_words(&moved.stdout, &live);
    assert_eq!(main["seats_held"], "false", "{both}");
    assert!(
        !moved
            .stderr
            .lines()
            .any(|line| line.starts_with("held .worktrees/")),
        "{both}"
    );
    assert_eq!(
        fs::read(live.join(".worktrees/side/u.txt")).unwrap(),
        b"sting's own\n"
    );
}

/// The coordinator, 2026-10-21 (add/add, no merge base): a file both sides
/// made is adopted when equal, unioned by its class when #297 has one (an
/// append log, JSON config), and held typed otherwise.
#[test]
fn a_file_both_sides_added_is_adopted_unioned_or_held() {
    let world = World::new("addadd");
    let (source, live) = related_pair(&world, "p");
    for (side, notes, config) in [
        (&source, "{\"n\":1}\n", "{\"neo\": true}\n"),
        (&live, "{\"n\":1}\n{\"n\":2}\n", "{\"sting\": true}\n"),
    ] {
        fs::write(side.join("same.md"), "equal on both\n").unwrap();
        fs::write(side.join("notes.jsonl"), notes).unwrap();
        fs::write(side.join("settings.json"), config).unwrap();
    }
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let main = receipt_words(&moved.stdout, &live);
    assert_eq!(main["seats_held"], "false", "{both}");
    let config = fs::read_to_string(live.join("settings.json")).unwrap();
    assert!(
        config.contains("neo") && config.contains("sting"),
        "{config}\n{both}"
    );
    assert_eq!(
        fs::read_to_string(live.join("notes.jsonl")).unwrap(),
        "{\"n\":1}\n{\"n\":2}\n"
    );
    // A file with no union, differing on both sides: held, the live kept.
    fs::write(source.join("other.md"), "neo\n").unwrap();
    fs::write(live.join("other.md"), "sting\n").unwrap();
    assert!(world.source().ok);
    let again = world.moved();
    let both = format!("{}\n{}", again.stdout, again.stderr);
    assert_eq!(
        receipt_words(&again.stdout, &live)["seats_held"],
        "true",
        "{both}"
    );
    assert_eq!(fs::read(live.join("other.md")).unwrap(), b"sting\n");
}

/// SPEC rule 6 (never halt), the coordinator 2026-10-21: an object the live
/// repository names after its snapshot (the source moved under the pass) is
/// fetched by id from the live source in the same pass, not refused
/// `CAPTURE_DRIFTED`.
#[test]
fn an_object_named_after_the_snapshot_is_fetched_in_the_same_pass() {
    let world = World::new("drift");
    let (source, live) = related_pair(&world, "p");
    assert!(world.source().ok);
    git(
        &source,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "after the snapshot",
        ],
    );
    let later = out(&source, &["rev-parse", "HEAD"]);
    fs::write(
        live.join(".git/refs/heads/later"),
        format!("{}\n", later.trim()),
    )
    .unwrap();
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let main = receipt_words(&moved.stdout, &live);
    assert_eq!(main["outcome"], "workspace-restored", "{both}");
    assert!(!moved.stdout.contains("CAPTURE_DRIFTED"), "{both}");
    assert_eq!(
        out(&live, &["cat-file", "-t", later.trim()]).trim(),
        "commit"
    );
}

/// SPEC rules 7 and 8 (the coordinator, 2026-10-21): the hand-off never
/// modifies a tracked file. A block an earlier pass wrote into the tracked
/// `AGENTS.md` and `CLAUDE.md` is removed, exactly, by the next pass; the
/// note lives in `CLAUDE.local.md`, excluded through `info/exclude`. After
/// a pass and a rerun, `git status` shows no change to either tracked file,
/// and the plan holds nothing for them.
#[test]
fn the_hand_off_never_modifies_a_tracked_file() {
    let world = World::new("handofflocal");
    world.estate();
    let land = world.src().join("land");
    fs::write(land.join("AGENTS.md"), "# agents\nneo's rules\n").unwrap();
    fs::write(land.join("CLAUDE.md"), "# claude\nneo's notes\n").unwrap();
    git(&land, &["add", "AGENTS.md", "CLAUDE.md"]);
    git(&land, &["commit", "--quiet", "-m", "agent files"]);
    assert!(world.source().ok);
    // An earlier pass (the legacy targets) wrote its block into both.
    let legacy = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("--min-free-percent=0")
        .args(os(&[
            &"estate-move",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"-",
            &world.source_state(),
            &world.state(),
        ]))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("BULKLOAD_HANDOFF_TRACKED_TARGETS", "1")
        .output()
        .unwrap();
    let live = world.dst().join("land");
    let agents = fs::read_to_string(live.join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("bulkload") && agents.ends_with("# agents\nneo's rules\n"),
        "{agents}\n{}",
        String::from_utf8_lossy(&legacy.stdout)
    );
    // This pass removes the block it finds there and writes the local note.
    for pass in 0..2 {
        assert!(world.source().ok);
        let moved = world.moved();
        let both = format!("{}\n{}", moved.stdout, moved.stderr);
        assert_eq!(
            fs::read(live.join("AGENTS.md")).unwrap(),
            b"# agents\nneo's rules\n",
            "pass {pass}: {both}"
        );
        assert_eq!(
            fs::read(live.join("CLAUDE.md")).unwrap(),
            b"# claude\nneo's notes\n",
            "pass {pass}: {both}"
        );
        let status = out(
            &live,
            &["status", "--porcelain", "--", "AGENTS.md", "CLAUDE.md"],
        );
        assert!(status.is_empty(), "pass {pass}: {status}\n{both}");
        assert!(live.join("CLAUDE.local.md").is_file(), "{both}");
        assert!(
            out(&live, &["status", "--porcelain"])
                .lines()
                .all(|line| !line.contains("CLAUDE.local.md")),
            "excluded"
        );
        assert!(
            !moved
                .stderr
                .lines()
                .any(|line| line.contains("AGENTS.md") || line.contains(" CLAUDE.md")),
            "pass {pass}: {both}"
        );
    }
    let planned = world.planned();
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    let rows = fs::read_to_string(world.state().join("diff.jsonl")).unwrap();
    assert!(
        !rows
            .lines()
            .any(|row| row.contains("\"path\":\"AGENTS.md\"")
                || row.contains("\"path\":\"CLAUDE.md\"")),
        "{rows}"
    );
}

/// The estate pass's new holds, 2026-10-22 (operator: "Hold all 22, run
/// now"; the follow-up handles them): a store that borrows objects through
/// `objects/info/alternates` is carried whole by the fetch; a stale nested
/// `.git` pointer and a nested repository with its own conversion
/// attributes are their own items and never refuse the parent's capture.
#[test]
fn alternates_are_carried_and_nests_never_refuse_the_parent() {
    let world = World::new("holds22");
    world.estate();
    let src = world.src();
    git(
        &src,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--template=",
            "other",
            "alt",
        ],
    );
    assert!(src.join("alt/.git/objects/info/alternates").is_file());
    let land = src.join("land");
    fs::create_dir_all(land.join("stale")).unwrap();
    fs::write(land.join("stale/.git"), "gitdir: /nowhere\n").unwrap();
    repo(&land.join("vendor/nest"), "n.txt");
    fs::write(land.join("vendor/nest/.gitattributes"), "* text eol=crlf\n").unwrap();
    let sourced = world.source();
    let both = format!("{}\n{}", sourced.stdout, sourced.stderr);
    assert!(sourced.ok, "{both}");
    for name in ["alt", "land"] {
        let path = format!("source={:?}", src.join(name));
        assert!(
            sourced
                .stdout
                .lines()
                .any(|line| line.contains("outcome=captured") && line.ends_with(&path)),
            "{name}: {both}"
        );
    }
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let alt = world.dst().join("alt");
    assert!(
        item_line(&moved.stdout, &alt).contains("outcome=workspace-restored"),
        "{both}"
    );
    assert!(!alt.join(".git/objects/info/alternates").exists(), "{both}");
    assert_eq!(out(&alt, &["cat-file", "-t", "HEAD"]).trim(), "commit");
    assert_eq!(
        out(&alt, &["fsck", "--connectivity-only", "--no-progress"]),
        ""
    );
}

// The receipt line's words of the item at `live`.
fn receipt_at(stdout: &str, live: &Path) -> BTreeMap<String, String> {
    item_line(stdout, live)
        .split(' ')
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// SPEC rule 5 (2026-10-22): untracked `build/`, `dist/`, `.svelte-kit/`
/// and Bazel's `bazel-*` links are rebuildable output: ledgered, never
/// carried or consolidated, so they never hold the item.
#[test]
fn build_output_is_never_a_seat() {
    let world = World::new("rebuildable");
    let (source, live) = related_pair(&world, "p");
    for (side, word) in [(&source, "neo"), (&live, "sting")] {
        for dir in ["build", "dist", ".svelte-kit"] {
            fs::create_dir_all(side.join(dir)).unwrap();
            fs::write(side.join(dir).join("out.js"), format!("{word}\n")).unwrap();
        }
    }
    std::os::unix::fs::symlink("/nowhere/bazel-out", source.join("bazel-out")).unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert_eq!(
        receipt_at(&moved.stdout, &live)["seats_held"],
        "false",
        "{both}"
    );
    for dir in ["build", "dist", ".svelte-kit"] {
        assert_eq!(
            fs::read(live.join(dir).join("out.js")).unwrap(),
            b"sting\n",
            "{dir}: {both}"
        );
    }
    assert!(
        fs::symlink_metadata(live.join("bazel-out")).is_err(),
        "{both}"
    );
}

/// 2026-10-22: another writer's in-flight quarantine in the live object
/// store (`incoming-*`, `pack/tmp_pack_*`) is never this pass's payload.
#[test]
fn another_writers_quarantine_never_refuses_the_item() {
    let world = World::new("quarantine");
    let (_source, live) = related_pair(&world, "p");
    let objects = live.join(".git/objects");
    fs::create_dir_all(objects.join("incoming-foreign")).unwrap();
    fs::create_dir_all(objects.join("pack")).unwrap();
    fs::write(objects.join("pack/tmp_pack_foreign"), b"in flight").unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert_eq!(
        receipt_at(&moved.stdout, &live)["outcome"],
        "workspace-restored",
        "{both}"
    );
    assert!(objects.join("incoming-foreign").is_dir());
    assert!(objects.join("pack/tmp_pack_foreign").is_file());
}

/// 2026-10-22 (tinyland.dev's `CAPTURE_DRIFTED`): a source reflog entry
/// naming an object the source itself no longer holds is never written into
/// the live reflog, so the consolidated repository never names a missing
/// object and the item is not held as drifted.
#[test]
fn a_reflog_entry_naming_a_pruned_object_is_never_unioned() {
    let world = World::new("reflogmissing");
    let (source, live) = related_pair(&world, "p");
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "gone soon"],
    );
    let gone = out(&source, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&source, &["reset", "--quiet", "--hard", "HEAD~1"]);
    let (dir, file) = gone.split_at(2);
    fs::remove_file(source.join(".git/objects").join(dir).join(file)).unwrap();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert_eq!(
        receipt_at(&moved.stdout, &live)["outcome"],
        "workspace-restored",
        "{both}"
    );
    let logs = fs::read_to_string(live.join(".git/logs/refs/heads/main")).unwrap_or_default();
    assert!(!logs.contains(&gone), "{logs}");
}

/// 2026-10-22 (estate pass 4, TIN-6569): a streamed pass keeps no corpus,
/// so its hand-off block names the move state, never a corpus path, and
/// every command it gives runs as printed: the read-only differences
/// (`--dry-run`), `--check` and `--clear`, each with `--move-state`.
#[test]
fn a_streamed_hand_off_names_its_move_state_and_its_commands_run() {
    let world = World::new("handoffstreamed");
    world.estate();
    assert!(world.source().ok);
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved
            .stdout
            .contains("estate-move phase=hand-off outcome=ok"),
        "{both}"
    );
    let note = world.dst().join("land/CLAUDE.local.md");
    let block = fs::read_to_string(&note).unwrap();
    assert!(!block.contains("carry/corpus"), "{block}");
    assert!(!block.contains("| Corpus |"), "{block}");
    assert!(
        block.contains(&format!("| Move state | `{}`", world.state().display())),
        "{block}"
    );
    assert!(block.contains("--move-state"), "{block}");
    // Each command is the backticked text after its cue, run from `/`.
    let command = |cue: &str| -> String {
        let at = block
            .find(cue)
            .unwrap_or_else(|| panic!("no {cue} in {block}"));
        let rest = &block[at..];
        let start = rest.find('`').unwrap() + 1;
        let end = start + rest[start..].find('`').unwrap();
        rest[start..end].to_owned()
    };
    let run = |line: &str| {
        let ran = Command::new("sh")
            .arg("-c")
            .arg(line)
            .current_dir("/")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{line}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
        String::from_utf8_lossy(&ran.stdout).into_owned()
    };
    let workspace = format!("\"workspace\":\"{}\"", world.dst().join("land").display());
    let differs = run(&command("See what differs, read-only"));
    assert!(
        differs
            .lines()
            .any(|line| line.contains(&workspace) && line.contains("\"mode\":\"dry-run\"")),
        "{differs}"
    );
    let check = run(&command("**When it is done**"));
    assert!(
        check
            .lines()
            .any(|line| line.contains(&workspace) && line.contains("\"state\":\"present\"")),
        "{check}"
    );
    run(&command("Then remove this block with"));
    assert!(!note.exists(), "the created note is cleared away");
}

/// SPEC rule 5 (2026-10-22, estate pass 4): a repository nested under a
/// rebuildable root (a `SwiftPM` dependency clone in `.build/checkouts/`)
/// is rebuilt with that root: never an item, counted on the inventory
/// line, never carried, and it holds nothing.
#[test]
fn a_nest_under_a_rebuildable_root_is_never_an_item() {
    let world = World::new("rebuildablenest");
    let parent = world.src().join("app");
    repo(&parent, "Package.swift");
    repo(
        &parent.join(".build/checkouts/swift-argument-parser"),
        "Sources.swift",
    );
    let sourced = world.source();
    assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
    assert!(sourced.stdout.contains(" items=1 "), "{}", sourced.stdout);
    assert!(
        sourced.stdout.contains(" rebuildable_nests=1"),
        "{}",
        sourced.stdout
    );
    let moved = world.moved();
    let both = format!("{}\n{}", moved.stdout, moved.stderr);
    let live = world.dst().join("app");
    assert!(
        item_line(&moved.stdout, &live).contains("outcome=workspace-restored"),
        "{both}"
    );
    assert!(!live.join(".build").exists(), "{both}");
    assert!(!both.contains("swift-argument-parser"), "{both}");
    assert_eq!(closure(&moved.stdout)["not_moved"], "0", "{both}");
}

/// A source outage can end a fetch after its objects are durable but before
/// the checkout and journal exist. The native rerun retains that custody.
#[test]
fn an_owned_first_fetch_resumes_through_the_native_mover() {
    use bulkload_agent::git_carry::stream;
    use bulkload_agent::{estate, first_fetch};
    for shallow in [false, true] {
        let world = World::new(if shallow {
            "first-fetch-shallow"
        } else {
            "first-fetch-full"
        });
        let source = world.src().join("land");
        if shallow {
            let origin = world.base.join("origin");
            repo(&origin, "a.txt");
            fs::write(origin.join("a.txt"), "latest source work\n").unwrap();
            git(&origin, &["commit", "--quiet", "-am", "advance"]);
            let url = format!("file://{}", origin.display());
            git(
                &world.src(),
                &["clone", "--quiet", "--depth=1", "--template=", &url, "land"],
            );
        } else {
            repo(&source, "a.txt");
        }
        let sourced = world.source();
        assert!(sourced.ok, "{}\n{}", sourced.stdout, sourced.stderr);
        let carry = world.source_state().join("carry");
        let items = estate::inspect(&carry.join("plan.postcard")).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        let identity = estate::item_id(item).unwrap();
        let snapshot = estate::streamed::recorded(&carry, &identity)
            .unwrap()
            .unwrap();
        let private = world.state().join("private");
        fs::create_dir_all(&private).unwrap();
        fs::set_permissions(world.state(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let target = first_fetch::Target {
            item: &identity,
            source_label: "neo",
            source_host: "-",
            source_path: &item.source,
            live_path: &item.repository,
            expected_shallow: &snapshot.shallow,
        };
        first_fetch::begin(&private, &target).unwrap();
        let upload = format!("git -c {} upload-pack", stream::UPLOAD_PACK_CONFIG);
        let fetched = estate::streamed::fetch_objects(&snapshot, &private, &upload, &[]).unwrap();
        assert!(fetched.bytes > 0);
        assert!(!item.repository.join(".git/HEAD").exists());
        let marker = private.join(format!("{identity}.first-fetch"));
        let intent = fs::read(&marker).unwrap();
        let preview = agent(&os(&[
            &"estate-move",
            &"--plan-only",
            &"--label",
            &LABEL,
            &"--source",
            &"neo",
            &"-",
            &world.source_state(),
            &world.state(),
        ]));
        let shown = format!("{}\n{}", preview.stdout, preview.stderr);
        assert!(!shown.contains("code=GIT_DESTINATION_OCCUPIED"), "{shown}");
        assert_eq!(fs::read(&marker).unwrap(), intent);
        assert!(!item.repository.join(".git/HEAD").exists());

        let moved = world.moved();
        let both = format!("{}\n{}", moved.stdout, moved.stderr);
        assert!(moved.ok, "{both}");
        assert_eq!(closure(&moved.stdout)["applied"], "1", "{both}");
        assert_eq!(closure(&moved.stdout)["unaccounted"], "0", "{both}");
        assert!(!both.contains("code=GIT_DESTINATION_OCCUPIED"), "{both}");
        assert!(!marker.exists());
        assert_eq!(
            fs::read(item.repository.join("a.txt")).unwrap(),
            fs::read(source.join("a.txt")).unwrap()
        );
        assert_eq!(payload::measure(&world.state()).unwrap().payload_bytes, 0);
        let again = world.moved();
        assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
        assert_eq!(closure(&again.stdout)["unaccounted"], "0");
        assert!(again.stdout.contains("object_bytes=0"), "{}", again.stdout);
    }
}
