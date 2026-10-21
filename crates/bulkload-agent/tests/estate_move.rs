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
//! The P-MOVE-1..3 landing proofs this file held (M2 item 7), and the four
//! `--sessions` passes over a Git lane below (plan of record 2026-10-19,
//! C9 to C11), wait for the streamed capture (PR B), which brings them back
//! over live-store transfers: until then they are `#[ignore]`d with that
//! reason.

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
#[ignore = "waits for the streamed capture (PR B): SPEC rule 2 refuses the git capture and corpus pull this pass needs"]
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
    codex_index(
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
    // The Codex store is the row union's (#278), held typed until it lands,
    // so the sessions pull ends ITEMS_REFUSED; the re-seat still runs.
    let moved = move_pass();
    assert!(!moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved
            .stdout
            .contains("estate-move phase=sessions-pull outcome=refused:ITEMS_REFUSED"),
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
    assert_eq!(
        fs::read_to_string(
            dst_claude
                .join("projects")
                .join(key(&src_lane))
                .join(format!("{SID}.jsonl"))
        )
        .unwrap(),
        neo_transcript,
        "the source's session landed under its own key, byte for byte"
    );
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{SID}.jsonl"))).unwrap(),
        neo_transcript,
        "and was re-seated under the lane's destination key"
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
    // Codex: the live index and its sidecar are never written by the pull,
    // and nothing lands beside them; the source's rollout lands in place and
    // the re-seat registers its thread for the lane from its header.
    assert!(dst_codex.join("state_5.sqlite-wal").exists());
    assert!(!dst_codex.join(format!("state_5.sqlite.{LABEL}")).exists());
    assert!(
        moved
            .stderr
            .contains("held state_5.sqlite: HOMES_CONSOLIDATE_UNSUPPORTED"),
        "{}",
        moved.stderr
    );
    assert_eq!(
        fs::read_to_string(dst_codex.join(&rollout_rel)).unwrap(),
        rollout_bytes
    );
    let cwds = thread_cwds(&dst_codex.join("state_5.sqlite"));
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
    // The closure: the session transfers joined, the merged configs
    // accounted (covered), the held store pending review: the gate fails.
    let first = closure(&moved.stdout);
    assert_eq!(first["sessions"], "claude,codex", "{}", moved.stdout);
    assert_eq!(first["applied_beside"], "0", "{}", moved.stdout);
    assert_eq!(first["refused_pending_review"], "1", "{}", moved.stdout);
    assert_eq!(first["unaccounted"], "0", "{}", moved.stdout);
    assert!(
        moved.stdout.contains("estate-move gate=fail"),
        "{}",
        moved.stdout
    );
    let closure_json = fs::read_to_string(state.join("closure.json")).unwrap();
    assert!(
        closure_json.contains("\"transfer_outcome\":\"consolidated\""),
        "{closure_json}"
    );
    assert!(
        closure_json.contains("\"consolidation\":\"consolidate\""),
        "{closure_json}"
    );
    assert!(
        closure_json.contains("\"HOMES_CONSOLIDATE_UNSUPPORTED\""),
        "{closure_json}"
    );

    // Pass 2: unchanged. The merged configs are reused, nothing is
    // rewritten, and the store is still held.
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
    let second = closure(&again.stdout);
    assert_eq!(second["refused_pending_review"], "1", "{}", again.stdout);
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
#[ignore = "waits for the streamed capture (PR B): SPEC rule 2 refuses the git capture and corpus pull this pass needs"]
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
#[ignore = "waits for the streamed capture (PR B): SPEC rule 2 refuses the git capture and corpus pull this pass needs"]
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
        &"-",
        &source_state,
        &state,
    ]));
    assert!(planned.ok, "{}\n{}", planned.stdout, planned.stderr);
    assert!(
        planned.stdout.lines().any(|line| line.starts_with(&format!(
            "planned projects/{}/{SID_FORK}.jsonl class=append-log action=hold ",
            key(&src_lane)
        )) && line.contains("under another key")),
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
    // A-106 is a union now (operator ruling 2026-10-20), never neo's copy
    // replacing the destination's: until the append-log union (#294)
    // lands, the archive link at the lane's key stays, the archived fork is
    // untouched, and the placement is `occupied`, holding the gate.
    assert!(
        moved.stdout.contains("estate-move a106=union-pending"),
        "{}",
        moved.stdout
    );
    // The live name holds the destination's fork (the re-seat regularizes
    // its archive link in place, OI-1003-Q198), never neo's copy. Neo's
    // copy is held at the pull, typed, and written nowhere: one session,
    // one file on sting (SPEC rule 3), until the append-log union merges
    // them.
    let at_live = dst_projects.join(format!("{SID_FORK}.jsonl"));
    assert_eq!(fs::read_to_string(&at_live).unwrap(), fork_bytes);
    assert_eq!(
        fs::read_to_string(archive.join("fork.jsonl")).unwrap(),
        fork_bytes
    );
    assert!(
        moved.stderr.contains(&format!(
            "held projects/{}/{SID_FORK}.jsonl: HOMES_CONSOLIDATE_UNSUPPORTED",
            key(&src_lane)
        )),
        "{}",
        moved.stderr
    );
    let copies: Vec<String> = tree_files(&dst_claude)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.contains(SID_FORK))
        .collect();
    assert_eq!(
        copies,
        [format!("projects/{}/{SID_FORK}.jsonl", key(&dst_lane))],
        "nothing new at any key"
    );
    assert!(
        !moved.stdout.contains("RESEAT_DEST_DIVERGED"),
        "{}",
        moved.stdout
    );
    // Under the neo key: the destination's link is held at the pull (a live
    // path of another kind), never landed beside; the live name keeps the
    // destination's archived bytes, never neo's copy.
    assert_eq!(
        fs::read_to_string(neo_key_dir.join(format!("{SID_NEO_KEY}.jsonl"))).unwrap(),
        transcript(SID_NEO_KEY, &src_lane, "OLD")
    );
    assert!(
        moved.stderr.contains(&format!(
            "held projects/{}/{SID_NEO_KEY}.jsonl: HOMES_CONSOLIDATE_UNSUPPORTED",
            key(&src_lane)
        )),
        "{}",
        moved.stderr
    );
    assert!(!neo_key_dir
        .join(format!("{SID_NEO_KEY}.jsonl.{LABEL}"))
        .exists());
    // The destination's own session keeps its own bytes.
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{SID_DEST}.jsonl"))).unwrap(),
        transcript(SID_DEST, &dst_lane, "OWN")
    );
    // Codex: the archive link at neo's rollout path is held too; it and the
    // archived rollout are untouched.
    let live_rollout = dst_codex.join(&fork_rel);
    assert!(
        fs::symlink_metadata(&live_rollout)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{}",
        moved.stdout
    );
    assert_eq!(
        fs::read(archive.join("rollout-fork.jsonl")).unwrap(),
        archived_rollout
    );
    assert_eq!(fs::read(&gz).unwrap(), b"\x1f\x8b old");
    assert_eq!(fs::read(&odd).unwrap(), b"an odd name\n");
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

/// Verification of #280, N4 across passes: provenance is sticky. A neo
/// session with no lane (a Claude transcript, and a Codex rollout neo's
/// index has no row for) is `RESEAT_LANE_UNMAPPED` while neo lists it; once
/// neo prunes it, the next pass still holds it typed (the pulls' listing is
/// cumulative), and so does a pass whose listing no longer names it (the
/// re-seat ledger recorded it as neo's): never `in-place`, the gate never
/// passes on it, and its refused ledger row is never overwritten.
#[test]
#[ignore = "waits for the streamed capture (PR B): SPEC rule 2 refuses the git capture and corpus pull this pass needs"]
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
#[ignore = "waits for the streamed capture (PR B): SPEC rule 2 refuses the git capture and corpus pull this pass needs"]
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
            &format!("projects/{}/{SID}.jsonl", key(&src_lane)) as &str,
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
