//! SPEC rules 6 and 8: a rebooted source can return to the same durable
//! transfer states without an operator restarting the pull.
//!
//! The budget is opt-in because an absent remote root also closes a pre-Start stream.
//! Only a file pull's lost transport is retried. The receive attempt has
//! joined its workers and released its publisher before the next attempt.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bulkload_proto::{BulkloadRefusal, Result};

static SECONDS: AtomicU64 = AtomicU64::new(0);

pub fn set_budget(seconds: u64) {
    SECONDS.store(seconds, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::{retry, wait};
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
}
