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
//! - `occupied`: a non-Git tree at the live path: refused typed, untouched;
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
    // The new pass recaptured `diverged` too, and its beside landing
    // converges to the new capture (gap C12, A-115: a beside landing used
    // to refuse every later pass `GIT_DESTINATION_OCCUPIED`): the live
    // checkout and its refs as they were, the beside checkout still
    // detached at the captured head. Everything accounts as before.
    assert!(
        converged.stdout.contains("beside-converged=1"),
        "{}",
        converged.stdout
    );
    let mut want = first;
    let mut got = closure(&converged.stdout);
    assert!(
        got.remove("refusals")
            .is_some_and(|codes| codes.contains("GIT_DESTINATION_OCCUPIED:1")),
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

/// `estate-move --sessions` lands the source's `.claude` and `.codex` at the
/// LIVE destination home with `--beside .LABEL` (C9: no landing stage):
/// nothing there is overwritten, a differing settings file and the live
/// Codex index's snapshot land beside, new transcripts and rollouts land in
/// place, the re-seat runs although one path was refused (C10) and re-keys
/// the source's session under the lane's destination key while the
/// destination's own session is in place, and the closure counts the
/// session transfers and the re-seat (C11): the refused path holds the gate
/// until it is resolved, and the beside placements stay accounted on the
/// rerun that then passes.
#[test]
fn sessions_land_in_the_live_home_beside_and_the_gate_reads_them() {
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

    // Pass 1: the plugin file is refused (its beside path is taken), so the
    // sessions pull ends ITEMS_REFUSED; the re-seat still runs.
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
    assert!(
        !moved.stdout.contains("skipped:sessions-pull-refused"),
        "{}",
        moved.stdout
    );
    // Nothing live was overwritten; the differing files landed beside.
    assert_eq!(
        fs::read(dst_claude.join("settings.json")).unwrap(),
        dst_settings
    );
    assert_eq!(
        fs::read(dst_claude.join(format!("settings.json.{LABEL}"))).unwrap(),
        src_settings
    );
    assert_eq!(
        fs::read(dst_claude.join("plugins/config.json")).unwrap(),
        b"{\"sting\":true}\n"
    );
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
    assert!(
        !moved.stdout.contains("RESEAT_LANE_UNMAPPED"),
        "{}",
        moved.stdout
    );
    assert!(!state.join("landing").exists(), "no intermediary stage");
    // Codex: the live index kept with its sidecar, the snapshot beside it,
    // the rollout in place, and the thread registered for the lane.
    assert_ne!(
        fs::read(dst_codex.join("state_5.sqlite")).unwrap(),
        live_index,
        "the live index gained the re-seated row"
    );
    assert!(dst_codex.join("state_5.sqlite-wal").exists());
    assert!(dst_codex.join(format!("state_5.sqlite.{LABEL}")).is_file());
    assert_eq!(
        fs::read_to_string(dst_codex.join(&rollout_rel)).unwrap(),
        rollout_bytes
    );
    let cwds = thread_cwds(&dst_codex.join("state_5.sqlite"));
    assert_eq!(
        cwds,
        [
            (THREAD.to_owned(), dst_lane.to_str().unwrap().to_owned()),
            (DEST_THREAD.to_owned(), dst_lane.to_str().unwrap().to_owned()),
        ],
        "the source's thread registered for the lane; the rowless destination rollout left as it was:\n{}",
        moved.stdout
    );
    assert_eq!(
        fs::read_to_string(&dest_rollout).unwrap(),
        dest_meta(DEST_THREAD)
    );
    assert_eq!(
        fs::read_to_string(&rowless_rollout).unwrap(),
        dest_meta(ROWLESS_THREAD)
    );
    assert!(
        moved
            .stdout
            .lines()
            .filter(|line| line.contains("\"kind\":\"thread\"")
                && line.contains("\"outcome\":\"in-place\""))
            .count()
            == 2,
        "the destination's own rollouts are in place:\n{}",
        moved.stdout
    );
    // The closure: the two session transfers joined, their beside rows
    // accounted, the refused plugin pending review, the re-seat accounted.
    let first = closure(&moved.stdout);
    assert_eq!(first["sessions"], "claude,codex", "{}", moved.stdout);
    assert_eq!(first["applied_beside"], "2", "{}", moved.stdout);
    assert_eq!(first["refused_pending_review"], "1", "{}", moved.stdout);
    assert_eq!(first["unaccounted"], "0", "{}", moved.stdout);
    assert_eq!(first["reseat_unaccounted"], "0", "{}", moved.stdout);
    assert_eq!(first["reseat_held"], "none", "{}", moved.stdout);
    assert!(
        first["transfer_rows"].parse::<u64>().unwrap() >= 5,
        "{}",
        moved.stdout
    );
    assert!(
        moved.stdout.contains("estate-move gate=fail"),
        "{}",
        moved.stdout
    );
    let closure_json = fs::read_to_string(state.join("closure.json")).unwrap();
    assert!(
        closure_json.contains("\"transfer_outcome\":\"applied-beside\""),
        "{closure_json}"
    );
    assert!(
        closure_json.contains(&format!("\"beside\":\"settings.json.{LABEL}\"")),
        "{closure_json}"
    );

    // Pass 2: the foreign beside file resolved by its owner; the plugin
    // file lands beside, every placement is reused, and the gate passes.
    fs::remove_file(&foreign).unwrap();
    source_pass();
    let again = move_pass();
    assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
    assert!(
        again
            .stdout
            .contains("estate-move phase=sessions-pull outcome=ok"),
        "{}",
        again.stdout
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"{\"neo\":true}\n");
    let second = closure(&again.stdout);
    assert_eq!(second["applied_beside"], "3", "{}", again.stdout);
    assert_eq!(second["refused_pending_review"], "0", "{}", again.stdout);
    assert_eq!(second["unaccounted"], "0", "{}", again.stdout);
    assert_eq!(second["reseat_unaccounted"], "0", "{}", again.stdout);
    assert!(
        again.stdout.contains("estate-move gate=pass"),
        "{}",
        again.stdout
    );
    assert_eq!(
        fs::read(dst_claude.join("settings.json")).unwrap(),
        dst_settings
    );
    assert_eq!(
        fs::read_to_string(dst_projects.join(format!("{DEST_SID}.jsonl"))).unwrap(),
        dest_transcript
    );
    assert_eq!(thread_cwds(&dst_codex.join("state_5.sqlite")), cwds);
}

/// Review of #280, findings 6 and 7: a destination with no Codex index.
/// The pull never writes the live index: the source's snapshot lands
/// beside it as `state_5.sqlite.LABEL`, and the re-seat seeds the live
/// index from that snapshot (its thread rows left out) and registers the
/// lane's thread under the destination's `cwd` and rollout path, never
/// reading a live index as the snapshot. A destination session with no
/// `cwd` under a destination key is in place and holds nothing.
#[test]
fn sessions_into_a_destination_with_no_codex_index_seed_it() {
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
    // cwd, and no .codex at all.
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
    assert!(moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    assert!(
        moved.stdout.contains("estate-move gate=pass"),
        "{}",
        moved.stdout
    );
    let snapshot = dst_codex.join(format!("state_5.sqlite.{LABEL}"));
    assert!(
        snapshot.is_file(),
        "the snapshot lands beside, live path absent"
    );
    assert!(
        moved.stdout.contains("\"kind\":\"index\"")
            && moved.stdout.contains("\"outcome\":\"seeded\""),
        "{}",
        moved.stdout
    );
    let live = dst_codex.join("state_5.sqlite");
    assert_eq!(
        thread_cwds(&live),
        [(THREAD.to_owned(), dst_lane.to_str().unwrap().to_owned())],
        "registered for the lane, not copied with the source's cwd:\n{}",
        moved.stdout
    );
    let rollout_path: String = rusqlite::Connection::open_with_flags(
        format!("file:{}?immutable=1", live.display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
    .query_row("SELECT rollout_path FROM threads", [], |row| row.get(0))
    .unwrap();
    assert_eq!(
        rollout_path,
        fs::canonicalize(&dst_codex)
            .unwrap()
            .join(&rollout_rel)
            .to_str()
            .unwrap()
    );
    let backfill: String = rusqlite::Connection::open_with_flags(
        format!("file:{}?immutable=1", live.display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
    .query_row("SELECT status FROM backfill_state", [], |row| row.get(0))
    .unwrap();
    assert_eq!(
        backfill, "complete",
        "the destination Codex never backfills"
    );
    assert_eq!(
        thread_cwds(&snapshot),
        [(THREAD.to_owned(), src_lane.to_str().unwrap().to_owned())],
        "the snapshot itself is the source's, untouched"
    );
    // The destination's own cwd-less session: in place, nothing held.
    assert_eq!(
        fs::read_to_string(own_projects.join(format!("{DEST_SID}.jsonl"))).unwrap(),
        no_cwd
    );
    let totals = closure(&moved.stdout);
    assert_eq!(totals["reseat_unaccounted"], "0", "{}", moved.stdout);
    assert_eq!(totals["unaccounted"], "0", "{}", moved.stdout);

    // The rerun reads the snapshot this pass reused, and changes nothing.
    let again = agent(&os(&[
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
    assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
    assert!(
        !again.stdout.contains("\"outcome\":\"seeded\""),
        "{}",
        again.stdout
    );
    assert_eq!(
        thread_cwds(&live),
        [(THREAD.to_owned(), dst_lane.to_str().unwrap().to_owned())]
    );
}
