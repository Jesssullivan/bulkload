//! Source-side priority: WP0(f), `auto` and bounded starvation.
//!
//! Background CPU and IO priority for source-side verbs (WP0(f),
//! OI-1003-Q17), the class `auto`, and bounded starvation (M2 item 3,
//! 2026-10-16).
//!
//! On neo, an `estate-capture` at background priority had a Git child sit
//! in uninterruptible IO for 25 minutes while a normal-priority `serve`
//! read the same disk: a throttled (Darwin `IOPOL_THROTTLE`) or idle-class
//! (Linux) reader is served only when no other IO is pending, so a busy
//! disk starves it without bound. Two rules bound it:
//!
//! - **`auto`** (the default of `estate-capture`; `--priority=auto` for any
//!   verb): background when a person may be at the host, normal otherwise.
//!   That is when a terminal device was read or written within
//!   [`AUTO_IDLE_SECS`] (its access or modification time): Linux
//!   `/dev/pts/*` and `/dev/tty*`, Darwin `/dev/ttys*` and `/dev/console`.
//!   Output counts, so a terminal an agent writes to also reads as a
//!   person: the test errs toward background, the WP0(f) side. Activity in
//!   a graphical session alone is not seen (a stated residual; the HID idle
//!   time is the follow-up). An unreadable `/dev` counts as a person
//!   present.
//! - **Escalation** (on by default for the `auto` verbs, `estate-capture`;
//!   for any other verb that starts at background only with an explicit
//!   `--escalate-after`, so `serve`, `copy` and the S2 runs keep WP0(f) as
//!   ratified): a watcher thread samples the process's counters each
//!   second. When none has moved for the threshold (`--escalate-after=
//!   SECONDS`, default [`DEFAULT_ESCALATE_SECS`], `0` turns it off; the
//!   watcher cannot tell a starved process from an idle one, so an idle
//!   `serve` given the flag escalates too), the IO class of the
//!   process goes back to normal ([`escalate`]): on Linux every thread of
//!   the process and of each live descendant leaves the idle class for the
//!   class its nice value implies (best effort, level 7 at nice 19); on
//!   Darwin the process disk policy goes back to `IOPOL_DEFAULT`, which is
//!   unverified there and likely helps only children started afterwards
//!   (a thread's `QoS` class also bounds its IO tier and only that thread
//!   can change it). On Linux the descendants' own read and write byte
//!   counts (`/proc/<pid>/io`) also count as progress, so a long healthy
//!   Git child is not taken for a starved one. Nice stays 19 everywhere:
//!   an unprivileged process cannot raise it back. The
//!   escalation happens at most once per process and is recorded in
//!   `priority_escalations`, `priority_escalated_ns` (from start),
//!   `priority_escalated_threads` and `priority_escalation_failures`; the
//!   counters line names the threshold (`priority_escalate_after=`) when a
//!   watcher ran, and `priority=` stays the class the process started at.

use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;

pub use crate::io::PriorityClass;

/// A terminal read within this many seconds means a person is at the host.
pub const AUTO_IDLE_SECS: u64 = 300;

/// Seconds without counter movement before a background process escalates.
pub const DEFAULT_ESCALATE_SECS: u64 = 300;

/// Enter background priority for this process and everything it creates
/// afterwards. Call it before any thread exists.
///
/// # Errors
/// Refuses with the errno of the scheduling call that failed.
pub fn enter_background() -> crate::Result<()> {
    crate::io::sys::enter_background().refuse_at("priority::enter_background")
}

/// Whether the calling thread runs at background priority.
///
/// # Errors
/// Refuses with the errno of the scheduling query that failed.
pub fn in_background() -> crate::Result<bool> {
    crate::io::sys::in_background().refuse_at("priority::in_background")
}

/// What `auto` found at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// A terminal was read within [`AUTO_IDLE_SECS`]: background.
    Interactive,
    /// No terminal was read within it: normal.
    Idle,
    /// `/dev` could not be read: background.
    Unknown,
}

impl Presence {
    /// The word the counters line records (`priority_auto=`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// The class `auto` resolves to.
    #[must_use]
    pub const fn class(self) -> PriorityClass {
        match self {
            Self::Idle => PriorityClass::Normal,
            Self::Interactive | Self::Unknown => PriorityClass::Background,
        }
    }
}

/// Whether a person is at the host now (see the module docs).
#[must_use]
pub fn presence() -> Presence {
    let now = std::time::SystemTime::now();
    let mut times = Vec::new();
    for (directory, prefixes) in TERMINALS {
        let Ok(entries) = std::fs::read_dir(directory) else {
            if *directory == "/dev" {
                return Presence::Unknown;
            }
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                use std::os::unix::fs::FileTypeExt as _;
                if meta.file_type().is_char_device() {
                    // Input moves the access time, output the modification
                    // time: either counts, so a person watching an attached
                    // terminal without typing is seen too.
                    times.extend(meta.accessed().ok());
                    times.extend(meta.modified().ok());
                }
            }
        }
    }
    if recent(&times, now, AUTO_IDLE_SECS) {
        Presence::Interactive
    } else {
        Presence::Idle
    }
}

/// The terminal devices whose access time `auto` reads, by directory.
#[cfg(target_os = "linux")]
const TERMINALS: &[(&str, &[&str])] = &[("/dev/pts", &[""]), ("/dev", &["tty"])];
#[cfg(target_vendor = "apple")]
const TERMINALS: &[(&str, &[&str])] = &[("/dev", &["ttys", "console"])];

/// Whether any of `times` lies within `window` seconds before `now` (a time
/// after `now` counts: a clock step must not hide a person).
#[must_use]
pub fn recent(times: &[std::time::SystemTime], now: std::time::SystemTime, window: u64) -> bool {
    times.iter().any(|time| {
        now.duration_since(*time)
            .map_or(true, |age| age.as_secs() < window)
    })
}

/// Move the process's IO back to the normal class (see the module docs).
/// Returns how many threads moved and how many could not be.
///
/// # Errors
/// Refuses with the errno of the call that failed for this process itself.
pub fn escalate() -> crate::Result<(u64, u64)> {
    crate::io::sys::escalate_io().refuse_at("priority::escalate")
}

/// Every counter but the escalation's own, plus the bytes the process's
/// live descendants have read and written (Linux), summed wrapping so
/// movement is seen even at saturation: what "progress" is. A healthy Git
/// child that is busy but writes nothing the agent counts still moves its
/// own IO; one stuck in uninterruptible IO does not.
fn progress() -> u64 {
    let now = counters::Counters::snapshot();
    let own = Counter::ALL
        .iter()
        .filter(|counter| {
            !matches!(
                counter,
                Counter::PriorityEscalations
                    | Counter::PriorityEscalatedNs
                    | Counter::PriorityEscalatedThreads
                    | Counter::PriorityEscalationFailures
            )
        })
        .fold(0_u64, |total, counter| {
            total.wrapping_add(now.get(*counter))
        });
    own.wrapping_add(crate::io::sys::descendant_io())
}

/// One step of the watcher: given the last progress seen and when it
/// changed, the progress now and the time now, whether to escalate, and the
/// new (progress, changed-at) pair.
#[must_use]
pub fn starving(
    (last, changed): (u64, std::time::Instant),
    now_progress: u64,
    now: std::time::Instant,
    after: std::time::Duration,
) -> (bool, (u64, std::time::Instant)) {
    if now_progress == last {
        (now.duration_since(changed) >= after, (last, changed))
    } else {
        (false, (now_progress, now))
    }
}

/// Start the watcher thread for a process at background priority.
///
/// It escalates once, after `after` with no counter movement, and records
/// it. `after` of zero starts nothing. The thread is detached; the process
/// exit ends it.
pub fn watch_starvation(after: std::time::Duration, started: std::time::Instant) {
    if after.is_zero() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("bulkload-starvation".to_owned())
        .spawn(move || {
            let mut state = (progress(), std::time::Instant::now());
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let (escalate_now, next) =
                    starving(state, progress(), std::time::Instant::now(), after);
                state = next;
                if escalate_now {
                    // Recorded whatever came of it: an escalation that
                    // failed for the process itself, or for some threads,
                    // still says when it was tried.
                    let (moved, failed) = escalate().unwrap_or((0, 1));
                    counters::bump(Counter::PriorityEscalations);
                    counters::add(Counter::PriorityEscalatedNs, counters::elapsed_ns(started));
                    counters::add(Counter::PriorityEscalatedThreads, moved);
                    counters::add(Counter::PriorityEscalationFailures, failed);
                    return;
                }
            }
        });
    // A watcher that cannot start leaves the process as WP0(f) has it.
    drop(spawned);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn a_recent_terminal_read_means_a_person_is_there() {
        let now = SystemTime::now();
        assert!(recent(&[now - Duration::from_secs(10)], now, 300));
        assert!(!recent(&[now - Duration::from_secs(301)], now, 300));
        assert!(!recent(&[], now, 300));
        assert!(
            recent(&[now + Duration::from_secs(90)], now, 300),
            "a clock step never hides a person"
        );
    }

    #[test]
    fn auto_resolves_idle_to_normal_and_anything_else_to_background() {
        assert_eq!(Presence::Idle.class(), PriorityClass::Normal);
        assert_eq!(Presence::Interactive.class(), PriorityClass::Background);
        assert_eq!(Presence::Unknown.class(), PriorityClass::Background);
        // The probe itself runs without failing on this host.
        let _ = presence();
    }

    #[test]
    fn escalation_waits_for_the_threshold_without_progress() {
        let start = Instant::now();
        let after = Duration::from_secs(5);
        let (go, state) = starving((7, start), 7, start + Duration::from_secs(4), after);
        assert!(!go);
        let (go, _) = starving(state, 7, start + Duration::from_secs(5), after);
        assert!(go, "five seconds with no movement escalates");
        // Movement restarts the clock.
        let (go, state) = starving(state, 8, start + Duration::from_secs(5), after);
        assert!(!go);
        assert_eq!(state, (8, start + Duration::from_secs(5)));
        let (go, _) = starving(state, 8, start + Duration::from_secs(9), after);
        assert!(!go);
    }
}
