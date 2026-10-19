//! M2 item 3 (2026-10-16): bounded starvation and the `auto` class.
//!
//! - A process at background priority given `--escalate-after` whose
//!   counters do not move for that long moves its IO class once, and says so
//!   on its counters line; `--escalate-after=0` never does, and a verb that
//!   is not `auto` runs no watcher unless asked.
//! - On Linux the process's IO class is idle before and not idle after
//!   (read from outside with `ioprio_get`, polled to a deadline so a loaded
//!   host does not make it flaky).
//! - `estate-capture` defaults to `auto` with the watcher, and records both.
//!
//! `serve` with an open, silent stdin is the idle process: it waits for its
//! first frame and moves no counter. Closing stdin ends it; nothing here
//! sends a signal (R-N11).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn counters(stderr: &str) -> (String, BTreeMap<String, String>) {
    let line = stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .unwrap_or_else(|| panic!("no counters line in {stderr}"))
        .to_owned();
    let pairs = line
        .split(' ')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    (line, pairs)
}

/// The IO priority class of process `pid` (its main thread), Linux.
#[cfg(target_os = "linux")]
fn io_class(pid: u32) -> i64 {
    // SAFETY: `ioprio_get` takes two integers and no pointers; 1 is
    // IOPRIO_WHO_PROCESS and `pid` names this test's own child.
    let value = unsafe { libc::syscall(libc::SYS_ioprio_get, 1, pid) };
    assert!(value >= 0, "{}", std::io::Error::last_os_error());
    value >> 13
}

/// Poll `check` every 50 ms until it holds or `deadline` passes.
#[cfg(target_os = "linux")]
fn eventually(deadline: Duration, check: &dyn Fn() -> bool) -> bool {
    let until = Instant::now() + deadline;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `serve` with `flags` and an open, silent stdin; `probe` sees its
/// PID while it waits. Then close its stdin; its stderr.
fn idle_serve(flags: &[&str], probe: &dyn Fn(u32)) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(flags)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let probed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(pid)));
    // Close stdin before any assertion fails, so the child always ends.
    drop(child.stdin.take());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    child.wait().unwrap();
    if let Err(panic) = probed {
        std::panic::resume_unwind(panic);
    }
    stderr
}

#[test]
fn an_idle_background_process_escalates_once_after_the_threshold() {
    let stderr = idle_serve(&["--escalate-after=1"], &|pid| {
        #[cfg(target_os = "linux")]
        {
            // Background first (idle class 3), then moved off it.
            assert!(
                eventually(Duration::from_secs(30), &|| io_class(pid) == 3),
                "serve never entered the idle class"
            );
            assert!(
                eventually(Duration::from_secs(90), &|| io_class(pid) != 3),
                "no escalation within 90 s"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            std::thread::sleep(Duration::from_secs(4));
        }
    });
    let (line, found) = counters(&stderr);
    assert!(
        line.contains("priority=background priority_from=default priority_escalate_after=1"),
        "{line}"
    );
    assert_eq!(found["priority_escalations"], "1", "{line}");
    assert!(
        found["priority_escalated_threads"].parse::<u64>().unwrap() >= 1,
        "{line}"
    );
    assert_eq!(found["priority_escalation_failures"], "0", "{line}");
    let at: u64 = found["priority_escalated_ns"].parse().unwrap();
    assert!(at >= 1_000_000_000, "not before the threshold: {line}");
}

#[test]
fn no_watcher_runs_unless_asked_or_auto() {
    for flags in [&["--escalate-after=0"][..], &[][..]] {
        let stderr = idle_serve(flags, &|_| std::thread::sleep(Duration::from_millis(1_500)));
        let (line, found) = counters(&stderr);
        assert_eq!(found["priority_escalations"], "0", "{line}");
        assert!(!line.contains("priority_escalate_after="), "{line}");
    }
}

#[test]
fn estate_capture_defaults_to_auto_and_records_what_it_found() {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("estate-capture")
        .output()
        .unwrap();
    let (line, found) = counters(&String::from_utf8_lossy(&out.stderr));
    assert_eq!(found["priority_from"], "auto", "{line}");
    let class = match found["priority_auto"].as_str() {
        "idle" => "normal",
        "interactive" | "unknown" => "background",
        other => panic!("priority_auto={other}: {line}"),
    };
    assert_eq!(found["priority"], class, "{line}");
    // A background start runs the watcher at its default threshold.
    assert_eq!(
        found.get("priority_escalate_after").map(String::as_str),
        (class == "background").then_some("300"),
        "{line}"
    );
    // An explicit class still wins and keeps the default watcher of the verb;
    // a bad threshold is refused.
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(["--priority=background", "estate-capture"])
        .output()
        .unwrap();
    let (line, _) = counters(&String::from_utf8_lossy(&out.stderr));
    assert!(
        line.contains("priority=background priority_from=flag priority_escalate_after=300"),
        "{line}"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(["--escalate-after=soon", "estate-capture"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
