//! M2 item 3 (2026-10-16): bounded starvation and the `auto` class.
//!
//! - A process at background priority whose counters do not move for
//!   `--escalate-after` seconds moves its IO back to the normal class once,
//!   and says so on its counters line; `--escalate-after=0` never does.
//! - On Linux the process's own IO class is idle before and best effort
//!   after (read from outside with `ioprio_get`).
//! - `estate-capture` defaults to `auto` and records what it found.
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
use std::time::Duration;

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

/// Run `serve` idle for `hold`, then close its stdin; its stderr.
fn idle_serve(flags: &[&str], hold: Duration, probe: &dyn Fn(u32)) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(flags)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(hold);
    probe(child.id());
    drop(child.stdin.take());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    child.wait().unwrap();
    stderr
}

#[test]
fn an_idle_background_process_escalates_once_after_the_threshold() {
    #[cfg(target_os = "linux")]
    {
        let before = idle_serve(
            &["--escalate-after=60"],
            Duration::from_millis(500),
            &|pid| {
                assert_eq!(
                    io_class(pid),
                    3,
                    "background serve starts in the idle class"
                );
            },
        );
        let (line, found) = counters(&before);
        assert_eq!(found["priority_escalations"], "0", "{line}");
    }
    let stderr = idle_serve(&["--escalate-after=1"], Duration::from_secs(4), &|pid| {
        #[cfg(target_os = "linux")]
        assert_eq!(io_class(pid), 2, "escalated to best effort");
        let _ = pid;
    });
    let (line, found) = counters(&stderr);
    assert!(
        line.contains("priority=background priority_from=default"),
        "{line}"
    );
    assert_eq!(found["priority_escalations"], "1", "{line}");
    assert!(
        found["priority_escalated_threads"].parse::<u64>().unwrap() >= 1,
        "{line}"
    );
    let at: u64 = found["priority_escalated_ns"].parse().unwrap();
    assert!(at >= 1_000_000_000, "not before the threshold: {line}");
}

#[test]
fn escalate_after_zero_never_escalates() {
    let stderr = idle_serve(&["--escalate-after=0"], Duration::from_secs(3), &|pid| {
        #[cfg(target_os = "linux")]
        assert_eq!(io_class(pid), 3, "still idle");
        let _ = pid;
    });
    let (line, found) = counters(&stderr);
    assert_eq!(found["priority_escalations"], "0", "{line}");
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
    // An explicit class still wins, and a bad threshold is refused.
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(["--priority=background", "estate-capture"])
        .output()
        .unwrap();
    let (line, _) = counters(&String::from_utf8_lossy(&out.stderr));
    assert!(
        line.contains("priority=background priority_from=flag"),
        "{line}"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(["--escalate-after=soon", "estate-capture"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
