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
use std::os::unix::fs::MetadataExt as _;
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
    // The source re-stamps `diverged`'s file, its bytes unchanged: its stat
    // identity moves, so this pass captures `diverged` again whatever the
    // clock did. The mtime is set explicitly, an hour before the old one,
    // so it differs and is never in the future; the ctime moves with it.
    // Without this, `diverged` was recaptured only while its first capture
    // was racy (seats stamped within the 2 s racy allowance of that pass's
    // start), which held only when the fixture and the first pass ran
    // inside two seconds: under load the pass below reused the capture and
    // never reached the beside converge it is here to prove.
    let restamped = src.join("diverged/d.txt");
    let stamp = fs::symlink_metadata(&restamped)
        .unwrap()
        .modified()
        .unwrap();
    fs::File::open(&restamped)
        .unwrap()
        .set_modified(stamp - std::time::Duration::from_hours(1))
        .unwrap();
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
    // The new pass recaptured `diverged` too (re-stamped above), and its
    // beside landing converges to the new capture (gap C12, A-115: a beside landing used
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
///   in place, the archive untouched) and neo's copy is kept at its own key
///   (`diverged`, both kept) until the append-log union lands: never neo's
///   copy over the destination's.
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
    // its archive link in place, OI-1003-Q198), never neo's copy; neo's
    // copy is kept at its own key, `diverged`: both kept.
    let at_live = dst_projects.join(format!("{SID_FORK}.jsonl"));
    assert_eq!(fs::read_to_string(&at_live).unwrap(), fork_bytes);
    assert_eq!(
        fs::read_to_string(archive.join("fork.jsonl")).unwrap(),
        fork_bytes
    );
    assert_eq!(
        fs::read_to_string(neo_key_dir.join(format!("{SID_FORK}.jsonl"))).unwrap(),
        neo(SID_FORK)
    );
    assert!(
        moved
            .stdout
            .contains("\"refused\":\"RESEAT_DEST_DIVERGED\""),
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
