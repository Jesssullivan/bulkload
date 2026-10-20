//! The source's held read snapshot and its bounds (operator ruling
//! 2026-10-19, "Hold one read snapshot"; the pieces of #278 the row union
//! needs).
//!
//! A WAL-mode store is read inside one read transaction for the whole pass:
//! in WAL mode a reader blocks no writer, it only pins checkpoints (SPEC
//! goal 6). The snapshot is never held past the wall, or once the source's
//! free-space floor is crossed (the `-wal` grows while it is held).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// The wall's base without `--sqlite-carry-wall`.
pub const CARRY_WALL: Duration = Duration::from_secs(30);

/// No wall is longer than this, the flag's included.
pub const CARRY_WALL_MAX: Duration = Duration::from_mins(30);

/// The least source rate a pass is allowed, in bytes per second, so a
/// large store's wall grows with its size. Engineering default, unruled.
const FLOOR_RATE: u64 = 32 * 1024 * 1024;

/// A store's wall allows this many reads of it at [`FLOOR_RATE`].
const PASSES: u64 = 3;

/// The first wait after a busy try, doubled up to [`BUSY_BACKOFF_MAX`].
const BUSY_BACKOFF: Duration = Duration::from_millis(1);
const BUSY_BACKOFF_MAX: Duration = Duration::from_millis(256);

static CARRY_WALL_SECS: AtomicU64 = AtomicU64::new(0);

/// Set `--sqlite-carry-wall` for this process.
pub fn set_carry_wall(wall: Duration) {
    CARRY_WALL_SECS.store(wall.as_secs(), Ordering::Relaxed);
}

/// The process's wall base: `--sqlite-carry-wall`'s, or [`CARRY_WALL`].
#[must_use]
pub fn carry_wall_base() -> Duration {
    match CARRY_WALL_SECS.load(Ordering::Relaxed) {
        0 => CARRY_WALL,
        secs => Duration::from_secs(secs),
    }
}

/// Parse `--sqlite-carry-wall=SECONDS`: whole seconds, 1 to 1800.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for anything else.
pub fn parse_carry_wall(value: &str) -> Result<Duration> {
    value
        .parse::<u64>()
        .ok()
        .filter(|secs| (1..=CARRY_WALL_MAX.as_secs()).contains(secs))
        .map(Duration::from_secs)
        .ok_or(BulkloadRefusal::FieldDomainViolation)
}

/// The wall of a pass over a store of `bytes`: the base, plus [`PASSES`]
/// reads of it at [`FLOOR_RATE`], at most [`CARRY_WALL_MAX`].
#[must_use]
pub fn wall(bytes: u64) -> Duration {
    let base = carry_wall_base();
    let reads = Duration::from_secs(bytes.saturating_mul(PASSES) / FLOOR_RATE);
    base.saturating_add(reads).max(base).min(CARRY_WALL_MAX)
}

/// A lock another connection holds: busy or locked, extended codes
/// included.
#[must_use]
pub const fn busy_failure(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

/// Start one read snapshot of a WAL-mode `source` and keep it: `true` when
/// held, `false` for a store not in WAL mode (nothing is held). A busy try
/// is retried with backoff inside `wall` from `started`.
///
/// # Errors
/// `SQLITE_STATE_CHANGED` when still busy at the wall; the store's failure.
pub fn hold_read_snapshot(source: &Connection, started: Instant, wall: Duration) -> Result<bool> {
    let mut backoff = BUSY_BACKOFF;
    loop {
        let attempt = source
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .and_then(|mode| {
                if !mode.eq_ignore_ascii_case("wal") {
                    return Ok(false);
                }
                source.execute_batch("BEGIN")?;
                // A deferred transaction reads nothing until asked: this
                // read takes the snapshot.
                let started = source
                    .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map(|_| true);
                if started.is_err() {
                    let _ = source.execute_batch("ROLLBACK");
                }
                started
            });
        match attempt {
            Ok(held) => return Ok(held),
            Err(error) if busy_failure(&error) => {
                if started.elapsed().saturating_add(backoff) > wall {
                    return Err(BulkloadRefusal::SqliteStateChanged);
                }
                counters::bump(Counter::SourceSqliteBusyRetries);
                std::thread::sleep(backoff);
                backoff = backoff.saturating_mul(2).min(BUSY_BACKOFF_MAX);
            }
            Err(error) => return Err(super::sql(error)),
        }
    }
}

/// Whether the source filesystem is under the free-space floor
/// (`--min-free-percent`) now. A probe that fails counts as under it: a
/// held snapshot is never kept on a guess.
#[must_use]
pub fn floor_crossed(source_dir: &Path) -> bool {
    crate::space::probe_limits(source_dir).map_or(true, |limits| {
        crate::space::bound(0, 0, limits, crate::space::min_free_percent()).is_some()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_grows_with_the_store_and_stops_at_its_cap() {
        assert_eq!(wall(0), CARRY_WALL);
        assert_eq!(wall(FLOOR_RATE), CARRY_WALL + Duration::from_secs(3));
        assert_eq!(wall(u64::MAX), CARRY_WALL_MAX);
        assert_eq!(parse_carry_wall("1800").unwrap(), CARRY_WALL_MAX);
        assert!(parse_carry_wall("1801").is_err());
        assert!(parse_carry_wall("0").is_err());
    }
}
