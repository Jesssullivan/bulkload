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
        fs::create_dir_all(base.join("dest/.codex")).unwrap();
        std::os::unix::fs::symlink(base.join("real-git"), base.join("dest/git")).unwrap();
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
                for c in chars.by_ref() {
                    if c == '\u{7}' {
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
