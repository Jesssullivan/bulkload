//! `estate-reseat` (R0, TIN-6587) through the binary: the CLI's S3 counter
//! and `--check` exit status, and a canary that drives the real Claude Code
//! picker over the re-seated destination home.
//!
//! The canary runs `claude --resume` in a throwaway home (the test's
//! destination home), inside `bwrap --unshare-net` (no network: nothing
//! can reach an API), with a placeholder API key and onboarding marked
//! done, on a pseudo-terminal from `script`. It reads the picker's screen,
//! then leaves with an Escape keystroke. Nothing here sends a signal to any
//! process (R-N11): a picker that does not leave is reported and left
//! running. It is skipped, and says so, where `claude`, `bwrap` or `script`
//! is missing or the sandbox cannot start.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SID: &str = "5e5e5e5e-1111-4222-8333-444444444444";
const LINKED: &str = "5e5e5e5e-9999-4999-8999-999999999999";

const fn agent() -> &'static str {
    env!("CARGO_BIN_EXE_bulkload-agent")
}

/// Claude Code's project key (`reseat::key::project_key`, short paths).
fn key(path: &Path) -> String {
    path.to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A minimal transcript in Claude Code 2.1.299's shape, titled `label`.
fn transcript(sid: &str, cwd: &str, label: &str) -> String {
    let common = format!(
        "\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"{cwd}\",\"sessionId\":\"{sid}\",\"version\":\"2.1.299\",\"gitBranch\":\"main\",\"entrypoint\":\"cli\""
    );
    let ts = "2026-10-16T12:00:00.000Z";
    format!(
        "{{\"parentUuid\":null,\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"prompt {label}\"}},\"uuid\":\"u-{label}\",\"timestamp\":\"{ts}\",{common}}}\n\
         {{\"parentUuid\":\"u-{label}\",\"type\":\"assistant\",\"message\":{{\"id\":\"msg_{label}\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"reply {label}\"}}],\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}},\"requestId\":\"req_{label}\",\"uuid\":\"a-{label}\",\"timestamp\":\"{ts}\",{common}}}\n\
         {{\"type\":\"ai-title\",\"aiTitle\":\"title {label}\",\"sessionId\":\"{sid}\"}}\n"
    )
}

struct World {
    _temp: tempfile::TempDir,
    base: PathBuf,
}

impl World {
    /// A landing holding one neo session of `~/git/lab`, and a destination
    /// home whose `git` is a symlink to the real checkout root (sting's
    /// shape).
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("bulkload-reseat-cli-")
            .tempdir()
            .unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(base.join("real-git/lab")).unwrap();
        fs::create_dir_all(base.join("dest/.claude")).unwrap();
        fs::write(
            base.join("dest/.claude/settings.json"),
            b"{\"cleanupPeriodDays\": 3650}",
        )
        .unwrap();
        fs::create_dir_all(base.join("dest/.codex")).unwrap();
        std::os::unix::fs::symlink(base.join("real-git"), base.join("dest/git")).unwrap();
        // The source used no Codex: its landed .codex is there and empty.
        fs::create_dir_all(base.join("landing/.codex")).unwrap();
        let projects = base.join("landing/.claude/projects/-Users-jess-git-lab");
        fs::create_dir_all(&projects).unwrap();
        fs::write(
            projects.join(format!("{SID}.jsonl")),
            transcript(SID, "/Users/jess/git/lab", "RESEATCANARY"),
        )
        .unwrap();
        Self { _temp: temp, base }
    }

    fn args(&self) -> Vec<PathBuf> {
        vec![
            PathBuf::from("/Users/jess"),
            self.base.join("landing"),
            self.base.join("dest"),
            self.base.join("state"),
        ]
    }

    fn reseat(&self, extra: &[&str]) -> (bool, String, String) {
        let out = Command::new(agent())
            .arg("estate-reseat")
            .arg("--managed-settings")
            .arg(self.base.join("managed-settings.json"))
            .args(extra)
            .args(self.args())
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn lane(&self) -> PathBuf {
        self.base.join("real-git/lab")
    }

    fn placed(&self) -> PathBuf {
        self.base
            .join("dest/.claude/projects")
            .join(key(&self.lane()))
            .join(format!("{SID}.jsonl"))
    }
}

fn counter(stderr: &str, name: &str) -> u64 {
    stderr
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in {stderr}"))
}

/// The CLI: `--check` fails on the landing (R0 holds for no lane today),
/// a pass places the session, `--check` then passes, and an unchanged
/// rerun reads 0 landed bytes (S3).
#[test]
fn cli_check_fails_first_then_passes_and_rerun_reads_nothing() {
    let world = World::new();
    let (ok, stdout, stderr) = world.reseat(&["--check", "--sessions-only"]);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("RESEAT_R0_FAILED"), "{stderr}");
    assert!(stdout.contains("\"verdict\":\"fail\""), "{stdout}");
    assert!(!world.base.join("state").exists(), "--check writes nothing");

    let (ok, stdout, stderr) = world.reseat(&[]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("\"outcome\":\"placed\""), "{stdout}");
    assert!(counter(&stderr, "read_reseat_landing_bytes") > 0);
    assert!(fs::symlink_metadata(world.placed()).unwrap().is_file());

    let (ok, stdout, stderr) = world.reseat(&["--check", "--sessions-only"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(stdout.contains("\"r0\":\"pass\""), "{stdout}");

    let (ok, stdout, stderr) = world.reseat(&[]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("\"outcome\":\"unchanged\""), "{stdout}");
    assert_eq!(counter(&stderr, "read_reseat_landing_bytes"), 0, "S3");
    assert_eq!(counter(&stderr, "write_reseat_dest_bytes"), 0);

    // Without --sessions-only and with no plan, Git is unchecked: R0 is
    // incomplete, never a pass.
    let (ok, stdout, _) = world.reseat(&["--check"]);
    assert!(!ok);
    assert!(stdout.contains("\"verdict\":\"incomplete\""), "{stdout}");
    let (ok, _, stderr) = world.reseat(&["--bogus"]);
    assert!(!ok);
    assert!(stderr.contains("FIELD_DOMAIN_VIOLATION"), "{stderr}");
    let (ok, _, stderr) = world.reseat(&["--map", "relative=/x"]);
    assert!(!ok);
    assert!(stderr.contains("PATH_NOT_ABSOLUTE"), "{stderr}");

    // A landed tree that is not there refuses, run and check alike
    // (review 0, finding 2): never an empty pass.
    fs::remove_dir(world.base.join("landing/.codex")).unwrap();
    let (ok, stdout, stderr) = world.reseat(&[]);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("RESEAT_LANDING_ABSENT"), "{stderr}");
    let (ok, stdout, stderr) = world.reseat(&["--check", "--sessions-only"]);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("RESEAT_LANDING_ABSENT"), "{stderr}");
    let (ok, stdout, stderr) = world.reseat(&["--check", "--sessions-only", "--no-codex"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(stdout.contains("\"state\":\"skipped\""), "{stdout}");
}

/// Re-review of `8a89f9a`, data safety (MEDIUM): re-seat had no free-space
/// check, while decompressing neo's `.zst` rollouts adds 25-30 GB to a
/// filesystem already under the engine's floor. The engine's rule
/// (`--min-free-percent`, `DESTINATION_SPACE_INSUFFICIENT`) holds before
/// anything is written; a floor the filesystem meets passes.
#[test]
fn cli_space_floor_refuses_before_writing() {
    let world = World::new();
    let run = |floor: &str| {
        Command::new(agent())
            .arg(format!("--min-free-percent={floor}"))
            .arg("estate-reseat")
            .arg("--managed-settings")
            .arg(world.base.join("managed-settings.json"))
            .args(world.args())
            .output()
            .unwrap()
    };
    let out = run("100");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("DESTINATION_SPACE_INSUFFICIENT"),
        "{stderr}"
    );
    assert!(!world.placed().exists(), "nothing written");
    assert!(
        !world.base.join("state").exists(),
        "refused before the ledger"
    );
    let out = run("0");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(world.placed().is_file());
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// Strip terminal control sequences from the picker's screen.
fn plain(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                // An OSC ends at BEL or at ST (`ESC \`): Codex ends its
                // colour queries with ST, and a BEL-only scan swallowed the
                // rest of its screen.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The real Claude Code picker lists the re-seated session in the lane's
/// destination directory (entered through the `git` symlink, keyed by its
/// real path), does not list a symlinked transcript beside it (the rule
/// that rules out symlinks), and opening the picker leaves the transcript
/// unchanged.
#[test]
fn canary_real_claude_picker_lists_the_reseated_session() {
    let (Some(claude), Some(bwrap), Some(script)) = (
        std::env::var_os("BULKLOAD_RESEAT_CANARY_CLAUDE")
            .map(PathBuf::from)
            .or_else(|| on_path("claude")),
        on_path("bwrap"),
        on_path("script"),
    ) else {
        eprintln!("canary skipped: claude, bwrap or script is not on PATH");
        return;
    };
    let probe = Command::new(&bwrap)
        .args(["--dev-bind", "/", "/", "--unshare-net", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    if !probe.status.success() {
        eprintln!(
            "canary skipped: bwrap --unshare-net cannot start here ({})",
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        return;
    }
    let claude = fs::canonicalize(claude).unwrap();
    let world = World::new();
    let (ok, _, stderr) = world.reseat(&[]);
    assert!(ok, "{stderr}");
    // A symlinked transcript in the same key directory: invisible to the
    // picker (fact 1.2), which is why re-seat never places one.
    let linked_target = world.base.join("linked.jsonl");
    fs::write(
        &linked_target,
        transcript(LINKED, world.lane().to_str().unwrap(), "RESEATSYMLINK"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        &linked_target,
        world.placed().with_file_name(format!("{LINKED}.jsonl")),
    )
    .unwrap();
    let home = world.base.join("dest");
    fs::write(
        home.join(".claude.json"),
        format!(
            "{{\"hasCompletedOnboarding\":true,\"theme\":\"dark\",\"numStartups\":5,\"projects\":{{\"{}\":{{\"hasTrustDialogAccepted\":true}}}}}}",
            world.lane().display()
        ),
    )
    .unwrap();
    let before = fs::read(world.placed()).unwrap();
    let inner = format!(
        "{} --dev-bind / / --unshare-net --chdir {} -- env -i HOME={} PATH=/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8 USER=reseat SHELL=/bin/sh DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 ANTHROPIC_API_KEY={} {} --resume",
        bwrap.display(),
        home.join("git/lab").display(),
        home.display(),
        // Not a credential: a placeholder that only skips the login
        // screen; the sandbox has no network.
        ["sk", "ant", "placeholder", "reseat", "canary"].join("-"),
        claude.display(),
    );
    let mut child = Command::new(&script)
        .args(["-qfec", &inner, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // A detached waiter reaps the picker; it is never signalled (R-N11).
    let (reaped, exit) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(child.wait());
    });
    let screen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(read) = stdout.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buffer[..read]);
        }
    });
    let deadline = Instant::now() + Duration::from_mins(1);
    let mut listed = false;
    let mut exited = false;
    while Instant::now() < deadline && !exited {
        if plain(&screen.lock().unwrap()).contains("RESEATCANARY") {
            listed = true;
            // Let the rest of the list draw before reading it whole.
            std::thread::sleep(Duration::from_millis(1500));
            break;
        }
        exited = exit.recv_timeout(Duration::from_millis(200)).is_ok();
    }
    let text = plain(&screen.lock().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !exited {
        let _ = stdin.write_all(b"\x1b");
        let _ = stdin.flush();
        exited = exit.recv_timeout(Duration::from_millis(500)).is_ok();
    }
    drop(stdin);
    if let Some(row) = text.lines().find(|row| row.contains("RESEATCANARY")) {
        eprintln!("canary: {} listed: {}", claude.display(), row.trim());
    }
    assert!(
        listed,
        "the picker did not list the re-seated session:\n{text}"
    );
    assert!(
        !text.contains("RESEATSYMLINK"),
        "a symlinked transcript was listed; the placement rule is stale:\n{text}"
    );
    assert_eq!(
        fs::read(world.placed()).unwrap(),
        before,
        "opening the picker appended"
    );
    assert!(
        exited,
        "the picker did not leave on Escape; left running, not signalled"
    );
}

/// Terminal queries a TUI waits on before it draws, and the answers a
/// terminal gives.
const TERMINAL_ANSWERS: &[(&[u8], &[u8])] = &[
    (b"\x1b[6n", b"\x1b[1;1R"),
    (b"\x1b[c", b"\x1b[?62;22c"),
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
    (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
];

/// Run `command` (a shell line) on a `script` pseudo-terminal: answer the
/// terminal queries it sends, read its screen until `done` holds or
/// `seconds` pass, then leave with Escape and Ctrl+C keystrokes. Returns
/// the plain screen and whether it exited. Nothing here sends a signal
/// (R-N11): one that does not leave is reported and left running.
fn drive(
    script: &Path,
    command: &str,
    seconds: u64,
    done: &dyn Fn(&str) -> bool,
) -> (String, bool) {
    // A sized terminal: Codex's picker draws nothing on a 0x0 one.
    let command = format!("stty rows 50 cols 200 2>/dev/null; exec {command}");
    let mut child = Command::new(script)
        .args(["-qfec", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let (reaped, exit) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(child.wait());
    });
    let screen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(read) = stdout.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buffer[..read]);
        }
    });
    let mut answered = 0;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut exited = false;
    while Instant::now() < deadline && !exited {
        let raw = screen.lock().unwrap().clone();
        let fresh = raw.get(answered..).unwrap_or_default().to_vec();
        for (query, answer) in TERMINAL_ANSWERS {
            for _ in 0..fresh.windows(query.len()).filter(|w| w == query).count() {
                let _ = stdin.write_all(answer);
            }
        }
        let _ = stdin.flush();
        answered = raw.len();
        if done(&plain(&raw)) {
            break;
        }
        exited = exit.recv_timeout(Duration::from_millis(200)).is_ok();
    }
    // Escape, then Ctrl+C twice at once (Codex's picker starts a new
    // session on Escape and quits on a double Ctrl+C), Ctrl+D, and the end
    // of input.
    for key in [&b"\x1b"[..], b"\x03\x03", b"\x04\x04", b"\x03\x03"] {
        if exited {
            break;
        }
        let _ = stdin.write_all(key);
        let _ = stdin.flush();
        exited = exit.recv_timeout(Duration::from_secs(2)).is_ok();
    }
    drop(stdin);
    if !exited {
        exited = exit.recv_timeout(Duration::from_secs(5)).is_ok();
    }
    std::thread::sleep(Duration::from_millis(200));
    let text = plain(&screen.lock().unwrap());
    (text, exited)
}

/// The real Codex resumes a re-seated **paginated** thread whose rollout
/// landed as `.jsonl.zst` (review 0, finding 1): Codex reads such a thread
/// only from the plain `.jsonl`, so a `.zst` placed as landed fails with
/// "failed to scan paginated model context lineage". The thread is opened
/// (not only listed): Codex appends to the placed rollout, which keeps the
/// placed bytes as its prefix, and the R0 check still passes (continued).
/// The destination index is the one the installed Codex creates in the
/// throwaway home. Skipped, and says so, where `codex`, `bwrap` or
/// `script` is missing.
#[test]
fn canary_real_codex_resumes_a_reseated_paginated_thread() {
    const THREAD: &str = "019a1111-2222-7333-8444-555555555555";
    let (Some(codex), Some(bwrap), Some(script)) = (
        std::env::var_os("BULKLOAD_RESEAT_CANARY_CODEX")
            .map(PathBuf::from)
            .or_else(|| on_path("codex")),
        on_path("bwrap"),
        on_path("script"),
    ) else {
        eprintln!("codex canary skipped: codex, bwrap or script is not on PATH");
        return;
    };
    let probe = Command::new(&bwrap)
        .args(["--dev-bind", "/", "/", "--unshare-net", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    if !probe.status.success() {
        eprintln!("codex canary skipped: bwrap --unshare-net cannot start here");
        return;
    }
    let codex = fs::canonicalize(codex).unwrap();
    let world = World::new();
    let home = world.base.join("dest");
    let lane = world.lane();
    fs::write(
        home.join(".codex/config.toml"),
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n[tui]\nscreen_reader_detection_done = true\n",
            lane.display()
        ),
    )
    .unwrap();
    // Not a credential: a placeholder that only skips the login screen;
    // the sandbox has no network.
    let placeholder = ["sk", "placeholder", "reseat", "canary"].join("-");
    fs::write(
        home.join(".codex/auth.json"),
        format!("{{\"auth_mode\":\"apikey\",\"OPENAI_API_KEY\":\"{placeholder}\"}}"),
    )
    .unwrap();
    let run = |args: &str| {
        format!(
            "{} --dev-bind / / --unshare-net --die-with-parent --chdir {} -- env -i HOME={} PATH=/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8 USER=reseat SHELL=/bin/sh OPENAI_API_KEY={placeholder} {} {args}",
            bwrap.display(),
            home.join("git/lab").display(),
            home.display(),
            codex.display(),
        )
    };
    // The installed Codex makes the destination's index.
    let index = home.join(".codex/state_5.sqlite");
    let (_, exited) = drive(&script, &run("resume --no-daemon --all"), 30, &|_| {
        index.is_file()
    });
    assert!(index.is_file(), "codex made no index (exited: {exited})");
    // The landing: a paginated thread of ~/git/lab, its rollout as .zst,
    // and a snapshot with the destination Codex's own schema.
    let rel = format!("sessions/2026/10/16/rollout-2026-10-16T10-00-00-{THREAD}.jsonl");
    let ts = "2026-10-16T10:00:00.000Z";
    let turn = "019a1111-2222-7333-8444-777777777777";
    let lines = [
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{THREAD}\",\"id\":\"{THREAD}\",\"timestamp\":\"{ts}\",\"cwd\":\"/Users/jess/git/lab\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0-alpha.11\",\"source\":\"cli\",\"thread_source\":\"user\",\"model_provider\":\"openai\",\"base_instructions\":{{\"text\":\"canary\"}},\"history_mode\":\"paginated\",\"context_window\":{{\"window_id\":\"019a1111-2222-7333-8444-666666666666\"}}}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"{turn}\",\"started_at\":1792144800,\"model_context_window\":258400,\"collaboration_mode_kind\":\"default\"}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"RESEATCODEXCANARY prompt\"}}]}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":3,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"RESEATCODEXREPLY\"}}],\"phase\":\"final_answer\"}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":4,\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"{turn}\",\"last_agent_message\":\"RESEATCODEXREPLY\"}}}}"),
    ];
    let plain_rollout = lines.join("\n") + "\n";
    let landed = world.base.join("landing/.codex").join(format!("{rel}.zst"));
    fs::create_dir_all(landed.parent().unwrap()).unwrap();
    fs::write(
        &landed,
        zstd::encode_all(plain_rollout.as_bytes(), 3).unwrap(),
    )
    .unwrap();
    {
        let source = rusqlite::Connection::open(&index).unwrap();
        let mut snapshot =
            rusqlite::Connection::open(world.base.join("landing/.codex/state_5.sqlite")).unwrap();
        rusqlite::backup::Backup::new(&source, &mut snapshot)
            .unwrap()
            .run_to_completion(64, Duration::from_millis(0), None)
            .unwrap();
        snapshot
            .execute(
                "INSERT INTO threads(id, rollout_path, created_at, updated_at, source,
                    model_provider, cwd, title, sandbox_policy, approval_mode, history_mode,
                    first_user_message, preview, has_user_event)
                 VALUES (?1, ?2, 1792144800, 1792144800, 'cli', 'openai', '/Users/jess/git/lab',
                    'RESEATCODEXCANARY prompt', '{\"type\":\"disabled\"}', 'never', 'paginated',
                    'RESEATCODEXCANARY prompt', 'RESEATCODEXCANARY prompt', 1)",
                rusqlite::params![THREAD, format!("/Users/jess/.codex/{rel}.zst")],
            )
            .unwrap();
    }
    let (ok, stdout, stderr) = world.reseat(&["--no-claude"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(stdout.contains("\"row\":\"registered\""), "{stdout}");
    let placed = home.join(".codex").join(&rel);
    assert_eq!(
        fs::read_to_string(&placed).unwrap(),
        plain_rollout,
        "placed decompressed"
    );
    let (ok, stdout, _) = world.reseat(&["--check", "--sessions-only", "--no-claude"]);
    assert!(ok, "{stdout}");
    let placed_len = plain_rollout.len() as u64;
    let (text, exited) = drive(
        &script,
        &run(&format!("resume --no-daemon {THREAD}")),
        40,
        &|text| {
            text.contains("ailed to resume")
                || fs::metadata(&placed).is_ok_and(|meta| meta.len() > placed_len)
        },
    );
    eprintln!(
        "codex canary: {} resumed {THREAD}: {}",
        codex.display(),
        text.lines().last().unwrap_or_default().trim()
    );
    assert!(
        !text.contains("ailed to resume") && !text.contains("paginated model context"),
        "the re-seated paginated thread did not resume:\n{text}"
    );
    let after = fs::read(&placed).unwrap();
    assert!(
        after.len() > plain_rollout.len() && after.starts_with(plain_rollout.as_bytes()),
        "Codex did not open the thread (no append to its rollout):\n{text}"
    );
    let (ok, stdout, _) = world.reseat(&["--check", "--sessions-only", "--no-claude"]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("\"continued\":1"), "{stdout}");
    assert!(exited, "codex did not leave; left running, not signalled");
}

/// One Codex 0.163 rollout of `~/git/lab` in the canary's shape, with a
/// `user_message` event: `marker` is its prompt.
fn canary_rollout(thread: &str, turn: &str, marker: &str) -> String {
    let ts = "2026-10-16T10:00:00.000Z";
    [
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{thread}\",\"id\":\"{thread}\",\"timestamp\":\"{ts}\",\"cwd\":\"/Users/jess/git/lab\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0-alpha.11\",\"source\":\"cli\",\"thread_source\":\"user\",\"model_provider\":\"openai\",\"base_instructions\":{{\"text\":\"canary\"}},\"history_mode\":\"paginated\",\"context_window\":{{\"window_id\":\"{turn}\"}}}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"{turn}\",\"started_at\":1792144800,\"model_context_window\":258400,\"collaboration_mode_kind\":\"default\"}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{marker} prompt\"}}]}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"{marker} prompt\",\"images\":[]}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":4,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{marker} reply\"}}],\"phase\":\"final_answer\"}}}}"),
        format!("{{\"timestamp\":\"{ts}\",\"ordinal\":5,\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"{turn}\",\"last_agent_message\":\"{marker} reply\"}}}}"),
    ]
    .join("\n")
        + "\n"
}

/// OI-1003-Q198 against the installed Codex, in a throwaway home with no
/// network: (D) a landed rollout with no snapshot row gets a row from its
/// header, which the lane's own picker (`codex resume` with no `--all`)
/// lists, and which resumes by ID with no working-directory question
/// (Codex appends to the placed rollout); (1) a destination row spelled
/// through the home's `git` symlink, hidden from that picker, is listed
/// once re-seat re-spells its `cwd`; `--undo` puts the spelling back.
/// Skipped, and says so, where `codex`, `bwrap` or `script` is missing.
#[test]
fn canary_real_codex_lists_header_rows_and_respelled_rows() {
    const NOROW: &str = "019a1111-2222-7333-8444-aaaaaaaaaaa1";
    const SPELL: &str = "019a1111-2222-7333-8444-aaaaaaaaaaa2";
    let (Some(codex), Some(bwrap), Some(script)) = (
        std::env::var_os("BULKLOAD_RESEAT_CANARY_CODEX")
            .map(PathBuf::from)
            .or_else(|| on_path("codex")),
        on_path("bwrap"),
        on_path("script"),
    ) else {
        eprintln!("codex q198 canary skipped: codex, bwrap or script is not on PATH");
        return;
    };
    let probe = Command::new(&bwrap)
        .args(["--dev-bind", "/", "/", "--unshare-net", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    if !probe.status.success() {
        eprintln!("codex q198 canary skipped: bwrap --unshare-net cannot start here");
        return;
    }
    let codex = fs::canonicalize(codex).unwrap();
    let world = World::new();
    let home = world.base.join("dest");
    let lane = world.lane();
    fs::write(
        home.join(".codex/config.toml"),
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n[tui]\nscreen_reader_detection_done = true\n",
            lane.display()
        ),
    )
    .unwrap();
    // Not a credential: a placeholder that only skips the login screen;
    // the sandbox has no network.
    let placeholder = ["sk", "placeholder", "reseat", "canary"].join("-");
    fs::write(
        home.join(".codex/auth.json"),
        format!("{{\"auth_mode\":\"apikey\",\"OPENAI_API_KEY\":\"{placeholder}\"}}"),
    )
    .unwrap();
    let run = |args: &str| {
        format!(
            "{} --dev-bind / / --unshare-net --die-with-parent --chdir {} -- env -i HOME={} PATH=/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8 USER=reseat SHELL=/bin/sh OPENAI_API_KEY={placeholder} {} {args}",
            bwrap.display(),
            home.join("git/lab").display(),
            home.display(),
            codex.display(),
        )
    };
    let index = home.join(".codex/state_5.sqlite");
    let (_, exited) = drive(&script, &run("resume --no-daemon --all"), 30, &|_| {
        index.is_file()
    });
    assert!(index.is_file(), "codex made no index (exited: {exited})");
    let rel =
        |thread: &str| format!("sessions/2026/10/16/rollout-2026-10-16T10-00-00-{thread}.jsonl");
    let norow = canary_rollout(NOROW, "019a1111-2222-7333-8444-bbbbbbbbbbb1", "RESEATNOROW");
    let spell = canary_rollout(SPELL, "019a1111-2222-7333-8444-bbbbbbbbbbb2", "RESEATSPELL");
    for (thread, bytes) in [(NOROW, &norow), (SPELL, &spell)] {
        let landed = world.base.join("landing/.codex").join(rel(thread));
        fs::create_dir_all(landed.parent().unwrap()).unwrap();
        fs::write(&landed, bytes).unwrap();
    }
    // The snapshot: the destination Codex's own schema, no rows.
    {
        let source = rusqlite::Connection::open(&index).unwrap();
        let mut snapshot =
            rusqlite::Connection::open(world.base.join("landing/.codex/state_5.sqlite")).unwrap();
        rusqlite::backup::Backup::new(&source, &mut snapshot)
            .unwrap()
            .run_to_completion(64, Duration::from_millis(0), None)
            .unwrap();
        snapshot.execute("DELETE FROM threads", []).unwrap();
    }
    // SPELL is already on the destination, its row spelled through the
    // home's git symlink (sting's /home/jess/git/... rows).
    let spelled = home.join("git/lab").to_str().unwrap().to_owned();
    let spell_dest = home.join(".codex").join(rel(SPELL));
    fs::create_dir_all(spell_dest.parent().unwrap()).unwrap();
    fs::write(&spell_dest, &spell).unwrap();
    rusqlite::Connection::open(&index)
        .unwrap()
        .execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, model_provider,
                cwd, title, sandbox_policy, approval_mode, history_mode, first_user_message,
                preview, has_user_event)
             VALUES (?1, ?2, 1792144800, 1792144800, 'cli', 'openai', ?3, 'RESEATSPELL prompt',
                '{\"type\":\"disabled\"}', 'never', 'paginated', 'RESEATSPELL prompt',
                'RESEATSPELL prompt', 1)",
            rusqlite::params![SPELL, spell_dest.to_str().unwrap(), spelled],
        )
        .unwrap();
    let (before, _) = drive(&script, &run("resume --no-daemon"), 25, &|text| {
        text.contains("RESEATSPELL")
    });
    let (ok, stdout, stderr) = world.reseat(&["--no-claude"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(stdout.contains("\"row\":\"synthesized\""), "{stdout}");
    assert!(stdout.contains("\"outcome\":\"respelled\""), "{stdout}");
    let (ok, stdout, _) = world.reseat(&["--check", "--sessions-only", "--no-claude"]);
    assert!(ok, "{stdout}");
    let (picker, exited) = drive(&script, &run("resume --no-daemon"), 40, &|text| {
        text.contains("RESEATNOROW") && text.contains("RESEATSPELL")
    });
    eprintln!(
        "codex q198 canary: {} lane picker before: spelled row listed {}; after: header row {}, respelled row {}",
        codex.display(),
        before.contains("RESEATSPELL"),
        picker.contains("RESEATNOROW"),
        picker.contains("RESEATSPELL"),
    );
    assert!(
        !before.contains("RESEATSPELL"),
        "a row spelled through the symlink was listed before re-spelling; the fact is stale"
    );
    assert!(
        picker.contains("RESEATNOROW") && picker.contains("RESEATSPELL"),
        "the lane's picker did not list both threads:\n{picker}"
    );
    assert!(
        exited,
        "the picker did not leave; left running, not signalled"
    );
    let placed = home.join(".codex").join(rel(NOROW));
    let placed_len = norow.len() as u64;
    let (text, exited) = drive(
        &script,
        &run(&format!("resume --no-daemon {NOROW}")),
        40,
        &|text| {
            text.contains("ailed to resume")
                || text.contains("session directory")
                || fs::metadata(&placed).is_ok_and(|meta| meta.len() > placed_len)
        },
    );
    assert!(
        !text.contains("ailed to resume") && !text.contains("session directory"),
        "the header-row thread did not resume cleanly:\n{text}"
    );
    let after = fs::read(&placed).unwrap();
    assert!(
        after.len() > norow.len() && after.starts_with(norow.as_bytes()),
        "Codex did not open the thread (no append):\n{text}"
    );
    assert!(exited, "codex did not leave; left running, not signalled");
    // --undo puts the old spelling back.
    let (ok, stdout, stderr) = world.reseat(&["--undo"]);
    assert!(ok, "{stdout}{stderr}");
    let cwd: String = rusqlite::Connection::open(&index)
        .unwrap()
        .query_row(
            "SELECT cwd FROM threads WHERE id = ?1",
            rusqlite::params![SPELL],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cwd, spelled, "{stdout}");
}

/// OI-1003-Q198 (2) against the installed Claude Code picker: a symlinked
/// transcript in the lane's key (sting's own, into an archive) is not
/// listed; after re-seat regularizes it, it is, and the archive copy is
/// unchanged. Skipped, and says so, where `claude`, `bwrap` or `script` is
/// missing.
#[test]
fn canary_real_claude_picker_lists_a_regularized_transcript() {
    let (Some(claude), Some(bwrap), Some(script)) = (
        std::env::var_os("BULKLOAD_RESEAT_CANARY_CLAUDE")
            .map(PathBuf::from)
            .or_else(|| on_path("claude")),
        on_path("bwrap"),
        on_path("script"),
    ) else {
        eprintln!("claude q198 canary skipped: claude, bwrap or script is not on PATH");
        return;
    };
    let probe = Command::new(&bwrap)
        .args(["--dev-bind", "/", "/", "--unshare-net", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    if !probe.status.success() {
        eprintln!("claude q198 canary skipped: bwrap --unshare-net cannot start here");
        return;
    }
    let claude = fs::canonicalize(claude).unwrap();
    let world = World::new();
    let home = world.base.join("dest");
    let key_dir = world.placed().parent().unwrap().to_path_buf();
    fs::create_dir_all(&key_dir).unwrap();
    let archive = world.base.join("archive.jsonl");
    let archived = transcript(LINKED, world.lane().to_str().unwrap(), "RESEATREGULAR");
    fs::write(&archive, &archived).unwrap();
    std::os::unix::fs::symlink(&archive, key_dir.join(format!("{LINKED}.jsonl"))).unwrap();
    fs::write(
        home.join(".claude.json"),
        format!(
            "{{\"hasCompletedOnboarding\":true,\"theme\":\"dark\",\"numStartups\":5,\"projects\":{{\"{}\":{{\"hasTrustDialogAccepted\":true}}}}}}",
            world.lane().display()
        ),
    )
    .unwrap();
    let picker = || {
        format!(
            "{} --dev-bind / / --unshare-net --die-with-parent --chdir {} -- env -i HOME={} PATH=/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8 USER=reseat SHELL=/bin/sh DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 ANTHROPIC_API_KEY={} {} --resume",
            bwrap.display(),
            home.join("git/lab").display(),
            home.display(),
            ["sk", "ant", "placeholder", "reseat", "canary"].join("-"),
            claude.display(),
        )
    };
    let (before, _) = drive(&script, &picker(), 25, &|text| {
        text.contains("RESEATREGULAR") || text.contains("No conversations")
    });
    let (ok, stdout, stderr) = world.reseat(&["--no-codex"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(stdout.contains("\"outcome\":\"regularized\""), "{stdout}");
    let (after, exited) = drive(&script, &picker(), 40, &|text| {
        text.contains("RESEATREGULAR") && text.contains("RESEATCANARY")
    });
    eprintln!(
        "claude q198 canary: {} symlinked listed before: {}; regularized listed after: {}",
        claude.display(),
        before.contains("RESEATREGULAR"),
        after.contains("RESEATREGULAR"),
    );
    assert!(
        !before.contains("RESEATREGULAR"),
        "a symlinked transcript was listed; the fact is stale:\n{before}"
    );
    assert!(
        after.contains("RESEATREGULAR"),
        "the regularized transcript is not listed:\n{after}"
    );
    assert_eq!(
        fs::read_to_string(&archive).unwrap(),
        archived,
        "archive kept"
    );
    assert!(
        exited,
        "the picker did not leave; left running, not signalled"
    );
}
