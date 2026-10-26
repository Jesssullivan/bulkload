//! SPEC rules 6 and 8: a rebooted source can return to the same durable
//! transfer states without an operator restarting the pull.
//!
//! The budget is opt-in. Only a file pull's lost transport is retried. The
//! receive attempt has joined its workers and released its publisher before
//! the next attempt.
//!
//! A remote `serve` that refuses before its first frame (an absent root is
//! `SOURCE_ROOT_MISSING`) closes the stream with no frame, which the receive
//! alone reads as a lost transport. The remote agent names its refusal on
//! its last stderr line and exits 1, so [`settle`] types it from that line
//! and the item ends at once with its own code (MG-1, 2026-10-26). A served
//! `TRANSPORT_LOST`, an ssh failure (255) or a signal stays a lost transport.

use std::io::{BufRead as _, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bulkload_proto::{BulkloadRefusal, Result};

static SECONDS: AtomicU64 = AtomicU64::new(0);

pub fn set_budget(seconds: u64) {
    SECONDS.store(seconds, Ordering::Relaxed);
}

/// Whether this process opted into reconnect and must own its SSH transport.
#[must_use]
pub fn enabled() -> bool {
    SECONDS.load(Ordering::Relaxed) > 0
}

fn wait(budget: Duration, elapsed: Duration, failures: u32) -> Option<Duration> {
    let left = budget.checked_sub(elapsed)?;
    let seconds = 1_u64
        .checked_shl(failures.saturating_sub(1))
        .unwrap_or(15)
        .min(15);
    let delay = Duration::from_secs(seconds);
    (delay < left).then_some(delay)
}

/// Retry only a lost file-transfer transport within the opted-in budget.
///
/// The budget starts at the first transport loss. An attempt reporting
/// newly committed payload resets it, so productive transfers between repeated
/// reboots do not consume a later outage's budget. Failed dials never
/// reset it. Each attempt uses unchanged roots and durable states.
/// The budget limits admission and backoff, not an in-flight attempt's
/// wall time. No process is canceled when the window expires.
///
/// # Errors
/// Returns the attempt's refusal. A lost transport is returned when its
/// reconnect budget expires.
pub fn pull<T>(attempt: impl FnMut() -> (Result<T>, bool)) -> Result<T> {
    let budget = Duration::from_secs(SECONDS.load(Ordering::Relaxed));
    let started = Instant::now();
    retry(budget, attempt, || started.elapsed(), std::thread::sleep)
}

fn retry<T>(
    budget: Duration,
    mut attempt: impl FnMut() -> (Result<T>, bool),
    mut now: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<T> {
    let mut started = None;
    let mut failures = 0_u32;
    loop {
        if started.is_some_and(|start| now().saturating_sub(start) >= budget) {
            return Err(BulkloadRefusal::TransportLost);
        }
        let (outcome, progressed) = attempt();
        match outcome {
            Err(BulkloadRefusal::TransportLost) => {
                let at = now();
                if progressed {
                    started = Some(at);
                    failures = 0;
                }
                let elapsed = at.saturating_sub(*started.get_or_insert(at));
                failures = failures.saturating_add(1);
                let Some(delay) = wait(budget, elapsed, failures) else {
                    return Err(BulkloadRefusal::TransportLost);
                };
                eprintln!(
                    "bulkload-agent: reconnect transport=lost attempt={failures} elapsed_ms={} wait_ms={} budget_seconds={}",
                    elapsed.as_millis(),
                    delay.as_millis(),
                    budget.as_secs(),
                );
                sleep(delay);
            }
            outcome => return outcome,
        }
    }
}

/// What a remote agent's stderr line says it refused: `bulkload-agent:
/// refused: CODE`, with `(errno N)` after `IO` or `SOURCE_ROOT_UNREADABLE`.
///
/// Only a code of the taxonomy is read. The codes a source half raises are
/// typed; any other taxonomy code is carried as `PROTOCOL_STATE_VIOLATION`,
/// its own line already relayed. `None` for any other line.
#[must_use]
pub fn remote_refusal(line: &[u8]) -> Option<BulkloadRefusal> {
    let said = std::str::from_utf8(line)
        .ok()?
        .trim_end()
        .strip_prefix("bulkload-agent: refused: ")?;
    let (code, detail) = said.split_once(' ').unwrap_or((said, ""));
    if !BulkloadRefusal::is_code(code) {
        return None;
    }
    let errno = detail
        .strip_prefix("(errno ")
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|digits| digits.parse().ok());
    Some(
        [
            BulkloadRefusal::SourceRootMissing,
            BulkloadRefusal::SourceRootUnreadable(errno),
            BulkloadRefusal::SourceRootKindMismatch,
            BulkloadRefusal::SourceVanished,
            BulkloadRefusal::SourceChangedAfterSnapshot,
            BulkloadRefusal::SnapshotRootsOverlap,
            BulkloadRefusal::PathNotAbsolute,
            BulkloadRefusal::PathNotPortable,
            BulkloadRefusal::PathEscapesRoot,
            BulkloadRefusal::PathDepthExceeded,
            BulkloadRefusal::PathTooLong,
            BulkloadRefusal::SchemaMismatch,
            BulkloadRefusal::RequiredFieldMissing,
            BulkloadRefusal::FieldDomainViolation,
            BulkloadRefusal::FrameCodec,
            BulkloadRefusal::WorkerLost,
            BulkloadRefusal::BudgetExceeded,
            BulkloadRefusal::DestinationSpaceInsufficient,
            BulkloadRefusal::SqliteSourceAsRoot,
            BulkloadRefusal::SqliteSourceNotOwner,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::SqliteUnionSourceNotWal,
            BulkloadRefusal::SqliteUnionSourceUnreadable,
            BulkloadRefusal::SqliteUnionSchemaUnsupported,
            BulkloadRefusal::TransportLost,
            BulkloadRefusal::Io(errno),
        ]
        .into_iter()
        .find(|refusal| refusal.code() == code)
        .unwrap_or(BulkloadRefusal::ProtocolStateViolation),
    )
}

/// Relay a remote session's stderr to this process's stderr, line by line
/// and byte for byte, and return the last refusal it named
/// ([`remote_refusal`]). Ends when the stream does.
pub fn relay_stderr(from: impl std::io::Read) -> Option<BulkloadRefusal> {
    let mut reader = std::io::BufReader::new(from);
    let mut line = Vec::new();
    let mut said = None;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return said,
            Ok(_) => {
                let _ = std::io::stderr().lock().write_all(&line);
                if let Some(refusal) = remote_refusal(&line) {
                    said = Some(refusal);
                }
            }
        }
    }
}

/// One remote session's result: what this side `received`, the ssh
/// child's `exit` code, and the refusal the remote agent `said`.
///
/// An agent refusal is exit 1 with its line. It is the cause when this side
/// only saw the stream end (`TRANSPORT_LOST`, or an `IO` echo), and when
/// this side completed but the remote did not. A refusal of this side's own
/// stays this side's. A served `TRANSPORT_LOST`, or any other unclean exit
/// (ssh's 255, a signal), is a lost transport, which [`pull`] may retry.
///
/// # Errors
/// The remote's refusal, this side's own, or `TRANSPORT_LOST`.
pub fn settle<T>(
    received: Result<T>,
    exit: Option<i32>,
    said: Option<BulkloadRefusal>,
) -> Result<T> {
    let remote =
        said.filter(|refusal| exit == Some(1) && *refusal != BulkloadRefusal::TransportLost);
    match (received, remote) {
        (Err(BulkloadRefusal::TransportLost | BulkloadRefusal::Io(_)) | Ok(_), Some(refusal)) => {
            Err(refusal)
        }
        (Err(refusal), _) => Err(refusal),
        (Ok(value), None) if exit == Some(0) => Ok(value),
        (Ok(_), None) => Err(BulkloadRefusal::TransportLost),
    }
}

#[cfg(test)]
mod tests {
    use super::{remote_refusal, retry, settle, wait};
    use bulkload_proto::BulkloadRefusal;
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn backoff_is_bounded_and_never_sleeps_past_the_budget() {
        let budget = Duration::from_secs(20);
        for (failures, seconds) in [(1, 1), (2, 2), (3, 4), (4, 8), (5, 15), (100, 15)] {
            assert_eq!(
                wait(budget, Duration::ZERO, failures),
                Some(Duration::from_secs(seconds))
            );
        }
        assert_eq!(wait(Duration::ZERO, Duration::ZERO, 1), None);
        assert_eq!(wait(budget, budget, 1), None);
        assert_eq!(wait(budget, budget + Duration::from_secs(1), 1), None);
        assert_eq!(wait(budget, Duration::from_secs(19), 1), None);
        assert_eq!(
            wait(Duration::from_secs(u64::MAX), Duration::ZERO, 1),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn verified_progress_resets_each_outage_but_failed_dials_do_not() {
        let clock = Cell::new(Duration::ZERO);
        let mut attempts = 0;
        let mut waits = Vec::new();
        let result = retry(
            Duration::from_secs(4),
            || {
                attempts += 1;
                match attempts {
                    1 | 3 => {
                        // Useful transfer time exceeds the whole outage
                        // budget, then the source reboots again.
                        clock.set(clock.get() + Duration::from_secs(6));
                        (Err(BulkloadRefusal::TransportLost), true)
                    }
                    2 => (Err(BulkloadRefusal::TransportLost), false),
                    _ => (Ok(()), false),
                }
            },
            || clock.get(),
            |delay| {
                waits.push(delay.as_secs());
                clock.set(clock.get() + delay);
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(attempts, 4);
        assert_eq!(waits, [1, 2, 1]);

        let mut dials = 0;
        let stopped = retry(
            Duration::from_secs(4),
            || {
                dials += 1;
                (Err::<(), _>(BulkloadRefusal::TransportLost), false)
            },
            || clock.get(),
            |delay| clock.set(clock.get() + delay),
        );
        assert_eq!(stopped, Err(BulkloadRefusal::TransportLost));
        assert_eq!(dials, 3, "failed dials cannot extend the budget");
    }

    #[test]
    fn a_remote_refusal_line_is_typed_with_its_code() {
        let typed = |line: &str| remote_refusal(line.as_bytes());
        assert_eq!(
            typed("bulkload-agent: refused: SOURCE_ROOT_MISSING\n"),
            Some(BulkloadRefusal::SourceRootMissing)
        );
        assert_eq!(
            typed("bulkload-agent: refused: SOURCE_ROOT_UNREADABLE (errno 13)\n"),
            Some(BulkloadRefusal::SourceRootUnreadable(Some(13)))
        );
        assert_eq!(
            typed("bulkload-agent: refused: IO (errno 5)"),
            Some(BulkloadRefusal::Io(Some(5)))
        );
        assert_eq!(
            typed("bulkload-agent: refused: TRANSPORT_LOST\n"),
            Some(BulkloadRefusal::TransportLost)
        );
        // A taxonomy code a source half does not raise keeps the session
        // refused, never retried; its own line is relayed.
        assert_eq!(
            typed("bulkload-agent: refused: RESEAT_LOCKED\n"),
            Some(BulkloadRefusal::ProtocolStateViolation)
        );
        for other in [
            "bulkload-agent: refused: not a code\n",
            "bulkload-agent: reconnect transport=lost attempt=1\n",
            "counters verb=serve side=source\n",
            "  bulkload-agent: refused: SOURCE_ROOT_MISSING\n",
        ] {
            assert_eq!(typed(other), None, "{other:?}");
        }
        assert_eq!(remote_refusal(b"bulkload-agent: refused: \xff\n"), None);
    }

    #[test]
    fn an_explicit_remote_refusal_is_terminal_and_a_lost_transport_is_not() {
        let missing = Some(BulkloadRefusal::SourceRootMissing);
        // MG-1: the remote refused its absent root before any frame. The
        // receive saw only the stream end; the item ends with the code.
        for received in [
            Err::<(), _>(BulkloadRefusal::TransportLost),
            Err(BulkloadRefusal::Io(None)),
            Ok(()),
        ] {
            assert_eq!(
                settle(received, Some(1), missing.clone()),
                Err(BulkloadRefusal::SourceRootMissing)
            );
        }
        // This side's own refusal stays this side's.
        assert_eq!(
            settle::<()>(
                Err(BulkloadRefusal::DestinationOccupied),
                Some(1),
                missing.clone()
            ),
            Err(BulkloadRefusal::DestinationOccupied)
        );
        // ssh's own failure, a signal, or no refusal line: transport loss.
        for (exit, said) in [
            (Some(255), missing.clone()),
            (None, missing),
            (Some(1), None),
            (Some(1), Some(BulkloadRefusal::TransportLost)),
        ] {
            assert_eq!(
                settle::<()>(Err(BulkloadRefusal::TransportLost), exit, said.clone()),
                Err(BulkloadRefusal::TransportLost),
                "{exit:?} {said:?}"
            );
            assert_eq!(
                settle(Ok(()), exit, said),
                Err(BulkloadRefusal::TransportLost)
            );
        }
        assert_eq!(settle(Ok(7), Some(0), None), Ok(7));
    }
}
