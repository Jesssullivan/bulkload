//! The caller retry contract for the ingest's bounded locks (#89,
//! OI-1001-Q17, R-N71).
//!
//! Every lock an ingest takes waits about two seconds and then refuses as a
//! value, so a hung git child never stalls every other finish on the
//! repository (#75 r4 N3). The work behind the per-repository fence (a
//! connectivity walk, migration and a ref transaction) routinely takes far
//! longer than that on a large repository, so a second ingest into the same
//! repository is often refused rather than kept waiting. Retrying is the
//! caller's job, and this is the contract:
//!
//! - **Retryable** ([`retryable`]): `JOURNAL_OWNERSHIP_CONFLICT` with reason
//!   `repository_fenced` (another finish or abandon holds the repository's
//!   fence), `quarantine_held` (another live session holds this session's
//!   quarantine) or `journal_held` (another live session holds this plan's
//!   journal, or the state dir's identity token). Each means another live
//!   holder that will let go; none means anything is wrong.
//! - **Not retryable**: everything else, including a bare
//!   `JOURNAL_OWNERSHIP_CONFLICT` (a journal holding another plan, or a
//!   journal for another repository) and `journal_replaced`. Retrying cannot
//!   change their answer; they go to the operator at once.
//! - **Each retry resumes.** `Ingest::finish` consumes its session, so a
//!   retry reopens it from its journal with `Ingest::resume`, which needs
//!   nothing from the sender. A resume refused by a held lock is one more
//!   attempt. [`Ingest::finish_retrying`] does exactly this.
//! - **Backoff**: before retry `n`, wait `base * 2^(n-1)`, capped at
//!   `ceiling`, then jittered uniformly into its upper half, so two callers
//!   that collided do not collide again in step. [`FenceRetry::DEFAULT`] is
//!   12 attempts, 500 ms base and a 30 s ceiling: about three minutes of
//!   waiting plus each attempt's own two-second lock window.
//! - **When to stop**: after `attempts` attempts the caller surfaces a typed
//!   refusal to the operator: `JOURNAL_OWNERSHIP_CONFLICT` with reason
//!   `repository_fenced_retries_exhausted`,
//!   `quarantine_held_retries_exhausted` or `journal_held_retries_exhausted`,
//!   naming the lock that was still held. The session's journal and
//!   quarantine are left as they are, so a later resume carries on.
//!
//! [`Ingest::finish_retrying`]: super::Ingest::finish_retrying

use std::time::Duration;

use super::super::estimate::Refused;
use super::Outcome;
use crate::BulkloadRefusal;

/// The reasons a caller retries ([`retryable`]).
pub const RETRYABLE_REASONS: [&str; 3] = ["repository_fenced", "quarantine_held", "journal_held"];

/// Whether `refused` is a held lock another live session will release, and
/// so worth retrying after a backoff. See the module docs.
#[must_use]
pub fn retryable(refused: &Refused) -> bool {
    refused.refusal == BulkloadRefusal::JournalOwnershipConflict
        && refused
            .reason
            .is_some_and(|reason| RETRYABLE_REASONS.contains(&reason))
}

/// A bounded, jittered exponential backoff for the ingest's held-lock
/// refusals (#89). See the module docs for the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceRetry {
    attempts: u32,
    base: Duration,
    ceiling: Duration,
}

impl FenceRetry {
    /// 12 attempts, 500 ms base, 30 s ceiling.
    pub const DEFAULT: Self = Self {
        attempts: 12,
        base: Duration::from_millis(500),
        ceiling: Duration::from_secs(30),
    };

    /// A policy of at most `attempts` attempts (the first included), waiting
    /// `base * 2^(n-1)` before retry `n`, capped at `ceiling`, jittered.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for no attempts, a zero base, or a ceiling
    /// below the base.
    pub fn new(attempts: u32, base: Duration, ceiling: Duration) -> crate::Result<Self> {
        if attempts == 0 || base.is_zero() || ceiling < base {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        Ok(Self {
            attempts,
            base,
            ceiling,
        })
    }

    /// The most attempts made, the first included.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// The wait before retry `retry` (1 for the first retry): the capped
    /// exponential delay `d`, moved into `[d/2, d]` by `jitter`.
    #[must_use]
    pub fn delay(&self, retry: u32, jitter: u64) -> Duration {
        let doubling = 1_u32
            .checked_shl(retry.saturating_sub(1))
            .unwrap_or(u32::MAX);
        let capped = self.base.saturating_mul(doubling).min(self.ceiling);
        let half = capped / 2;
        let span = u64::try_from(capped.saturating_sub(half).as_nanos()).unwrap_or(u64::MAX);
        let offset = span.checked_add(1).map_or(jitter, |range| jitter % range);
        half.saturating_add(Duration::from_nanos(offset))
    }

    /// Run `attempt` until it succeeds, refuses with something not
    /// [`retryable`], or the attempts run out, sleeping the jittered delay
    /// between attempts.
    ///
    /// # Errors
    /// The first refusal that is not retryable; or, once every attempt was
    /// refused by a held lock, `JOURNAL_OWNERSHIP_CONFLICT` with the
    /// `*_retries_exhausted` reason naming the last lock held.
    pub fn run<T>(&self, attempt: impl FnMut() -> Outcome<T>) -> Outcome<T> {
        self.run_with(std::thread::sleep, jitter, attempt)
    }

    pub(super) fn run_with<T>(
        &self,
        mut sleep: impl FnMut(Duration),
        mut jitter: impl FnMut() -> u64,
        mut attempt: impl FnMut() -> Outcome<T>,
    ) -> Outcome<T> {
        let mut made = 1;
        loop {
            match attempt() {
                Ok(value) => return Ok(value),
                Err(refused) if retryable(&refused) => {
                    if made >= self.attempts {
                        return Err(exhausted(&refused));
                    }
                    sleep(self.delay(made, jitter()));
                    made += 1;
                }
                Err(refused) => return Err(refused),
            }
        }
    }
}

impl Default for FenceRetry {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The typed refusal surfaced once retries run out, naming the lock still
/// held at the last attempt.
fn exhausted(refused: &Refused) -> Refused {
    let reason = match refused.reason {
        Some("repository_fenced") => "repository_fenced_retries_exhausted",
        Some("quarantine_held") => "quarantine_held_retries_exhausted",
        _ => "journal_held_retries_exhausted",
    };
    Refused::because(BulkloadRefusal::JournalOwnershipConflict, reason)
}

/// Per-call jitter: the standard library's randomly keyed hasher, so no
/// dependency is added for it.
fn jitter() -> u64 {
    use std::hash::BuildHasher as _;
    std::collections::hash_map::RandomState::new().hash_one(0_u8)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use std::time::Duration;

    use super::{retryable, FenceRetry, Refused};
    use crate::BulkloadRefusal;

    fn held(reason: &'static str) -> Refused {
        Refused::because(BulkloadRefusal::JournalOwnershipConflict, reason)
    }

    #[test]
    fn only_held_locks_are_retryable() {
        for reason in ["repository_fenced", "quarantine_held", "journal_held"] {
            assert!(retryable(&held(reason)), "{reason}");
        }
        assert!(!retryable(&held("journal_replaced")));
        assert!(!retryable(
            &BulkloadRefusal::JournalOwnershipConflict.into()
        ));
        assert!(!retryable(&Refused::because(
            BulkloadRefusal::GitHavesUnprovable,
            "repository_fenced"
        )));
    }

    #[test]
    fn a_policy_is_bounded() {
        let second = Duration::from_secs(1);
        assert!(FenceRetry::new(1, second, second).is_ok());
        for (attempts, base, ceiling) in [
            (0, second, second),
            (3, Duration::ZERO, second),
            (3, second * 2, second),
        ] {
            assert_eq!(
                FenceRetry::new(attempts, base, ceiling),
                Err(BulkloadRefusal::FieldDomainViolation)
            );
        }
        assert_eq!(FenceRetry::default(), FenceRetry::DEFAULT);
    }

    /// The delay doubles from the base, is capped at the ceiling, and its
    /// jitter keeps it in the upper half of the capped delay.
    #[test]
    fn the_delay_doubles_caps_and_jitters_into_its_upper_half() {
        let policy = FenceRetry::DEFAULT;
        let ms = Duration::from_millis;
        assert_eq!(policy.delay(1, 0), ms(250));
        assert_eq!(
            policy.delay(1, u64::MAX),
            ms(250) + Duration::from_nanos(u64::MAX % 250_000_001)
        );
        assert_eq!(policy.delay(2, 0), ms(500));
        assert_eq!(policy.delay(3, 0), ms(1000));
        assert_eq!(policy.delay(7, 0), ms(15_000));
        assert_eq!(policy.delay(8, 0), ms(15_000), "capped at 30 s");
        assert_eq!(policy.delay(64, 0), ms(15_000), "no overflow");
        for retry in 1..=20 {
            for jitter in [0, 1, 7, 1 << 40, u64::MAX] {
                let delay = policy.delay(retry, jitter);
                let capped = (ms(500) * (1 << (retry - 1).min(10))).min(Duration::from_secs(30));
                assert!(delay >= capped / 2 && delay <= capped, "{retry} {jitter}");
            }
        }
    }

    /// Held locks are retried with the backoff until the attempt succeeds.
    #[test]
    fn held_locks_are_retried_until_released() {
        let policy = FenceRetry::new(5, Duration::from_millis(10), Duration::from_secs(1)).unwrap();
        let mut slept = Vec::new();
        let mut calls = 0;
        let result = policy.run_with(
            |delay| slept.push(delay),
            || 0,
            || {
                calls += 1;
                match calls {
                    1 => Err(held("repository_fenced")),
                    2 => Err(held("journal_held")),
                    3 => Err(held("quarantine_held")),
                    _ => Ok(calls),
                }
            },
        );
        assert_eq!(result.unwrap(), 4);
        let ms = Duration::from_millis;
        assert_eq!(slept, vec![ms(5), ms(10), ms(20)]);
    }

    /// A refusal that is not a held lock is returned at once, unretried.
    #[test]
    fn other_refusals_are_not_retried() {
        let policy = FenceRetry::DEFAULT;
        let mut calls = 0;
        let mut sleeps = 0;
        let refused = policy
            .run_with(
                |_| sleeps += 1,
                || 0,
                || -> super::Outcome<()> {
                    calls += 1;
                    Err(BulkloadRefusal::JournalOwnershipConflict.into())
                },
            )
            .unwrap_err();
        assert_eq!((calls, sleeps), (1, 0));
        assert_eq!(refused.refusal, BulkloadRefusal::JournalOwnershipConflict);
        assert_eq!(refused.reason, None);
    }

    /// Once every attempt is refused by a held lock, the caller gets one
    /// typed refusal naming the lock, after exactly `attempts` attempts.
    #[test]
    fn exhausted_retries_refuse_typed_and_name_the_lock() {
        for (reason, exhausted) in [
            ("repository_fenced", "repository_fenced_retries_exhausted"),
            ("quarantine_held", "quarantine_held_retries_exhausted"),
            ("journal_held", "journal_held_retries_exhausted"),
        ] {
            let policy =
                FenceRetry::new(3, Duration::from_millis(1), Duration::from_millis(2)).unwrap();
            let mut calls = 0;
            let mut sleeps = 0;
            let refused = policy
                .run_with(
                    |_| sleeps += 1,
                    || 0,
                    || -> super::Outcome<()> {
                        calls += 1;
                        Err(held(reason))
                    },
                )
                .unwrap_err();
            assert_eq!((calls, sleeps), (3, 2), "{reason}");
            assert_eq!(refused.refusal, BulkloadRefusal::JournalOwnershipConflict);
            assert_eq!(refused.reason, Some(exhausted));
            assert!(!retryable(&refused), "an exhausted refusal is final");
        }
    }
}
