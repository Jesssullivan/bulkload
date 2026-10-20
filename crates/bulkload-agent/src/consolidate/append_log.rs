//! The append-log [`Union`](super::Union), the #294 lane's.
//!
//! Claude transcripts and their subagents, Codex rollouts, `history.jsonl`,
//! any `*.jsonl` (operator ruling 2026-10-20: "neither WIN. they must be
//! consolidated"; "no dupes, no duplicated besides, no stage dupes").
//!
//! One file at the path, never a beside copy. The live file and the
//! source's are compared:
//! - equal: adopted;
//! - the live file holds the source's bytes as its prefix, or every one of
//!   its records: covered, nothing written;
//! - the live file is a prefix of the source's, at a line end: extended to
//!   the source's bytes;
//! - else consolidated: the source's bytes whole, then the live file's
//!   records the source lacks, in the live file's order. A record's
//!   identity is [`crate::transfer::consolidate`]'s (the receive path writes
//!   the same union): a Claude transcript record's `uuid`, else the exact
//!   line, counted. Both branches of a session continued on both hosts are
//!   kept.
//!
//! The union is written under a private name, made durable and traded in for
//! the live file by [`super::config`]'s crash-safe trade, checked against
//! the live file as read (a writer that changed it is merged again; one that
//! changes it on every read is `HOMES_CONSOLIDATE_RACED`). A live file a
//! process holds open ([`crate::procs::inside`], or where that cannot be
//! said) or that ends inside a line (a writer at it) is
//! `HOMES_CONSOLIDATE_RACED` too, retryable, nothing written: a writer's
//! open handle would otherwise write on into the file traded out. Each
//! extension and consolidation is ledgered (`append_log_union`: both
//! inputs' and the output's BLAKE3, the record counts) once its bytes are
//! durable.

use std::ffi::CStr;
use std::fs::File;
use std::path::Path;

use super::config::{self, Target};
use super::{Action, Ledger, Live, Outcome, Source};
use crate::io::{self, sys, Stat};
use crate::refuse::RefuseAt as _;
use crate::transfer::consolidate::merge_jsonl;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "consolidate::append_log";

/// The largest append log merged, either side; a larger one is held.
pub const SIZE_LIMIT: u64 = 1 << 30;

/// The ledger's record of each append log written.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS append_log_union(
    file BLOB NOT NULL,
    action TEXT NOT NULL,
    live_blake3 BLOB NOT NULL,
    source_blake3 BLOB NOT NULL,
    output_blake3 BLOB NOT NULL,
    source_records INTEGER NOT NULL,
    live_extra INTEGER NOT NULL,
    output_size INTEGER NOT NULL,
    at_ns INTEGER NOT NULL,
    PRIMARY KEY(file, live_blake3, source_blake3)
);";

/// A union of two append logs, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub action: Action,
    /// What the live file is to hold ([`Action::Extended`],
    /// [`Action::Consolidated`]).
    pub bytes: Option<Vec<u8>>,
    /// The source's records, and the live file's records kept after them.
    pub source_records: u64,
    pub live_extra: u64,
}

/// Whether the records of the append log at `rel` are identified by their
/// `uuid`: a Claude transcript, under `.claude/projects/` (or `projects/`
/// of a `.claude` root).
#[must_use]
pub fn by_uuid(rel: &[u8]) -> bool {
    const UNDER: &[u8] = b".claude/projects/";
    rel.starts_with(b"projects/")
        || rel.starts_with(UNDER)
        || rel
            .windows(UNDER.len() + 1)
            .any(|window| window.first() == Some(&b'/') && window.get(1..) == Some(UNDER))
}

/// The union of `source` into `live` (see the module), nothing read or
/// written; `None` when either ends inside a line and neither holds the
/// other.
#[must_use]
pub fn plan(live: &[u8], source: &[u8], by_uuid: bool) -> Option<Plan> {
    let nothing = |action| Plan {
        action,
        bytes: None,
        source_records: 0,
        live_extra: 0,
    };
    if live == source {
        return Some(nothing(Action::Adopted));
    }
    if live.starts_with(source) {
        return Some(nothing(Action::Covered));
    }
    let whole = |bytes: &[u8]| bytes.is_empty() || bytes.ends_with(b"\n");
    if !whole(live) || !whole(source) {
        return None;
    }
    if source.starts_with(live) {
        return Some(Plan {
            action: Action::Extended,
            bytes: Some(source.to_vec()),
            source_records: source.split_inclusive(|byte| *byte == b'\n').count() as u64,
            live_extra: 0,
        });
    }
    // Every record of the source's already held: nothing to add.
    if merge_jsonl(live, source, by_uuid)?.extra.is_empty() {
        return Some(nothing(Action::Covered));
    }
    let merged = merge_jsonl(source, live, by_uuid)?;
    let mut bytes = source.to_vec();
    bytes.extend_from_slice(&merged.extra);
    Some(Plan {
        action: Action::Consolidated,
        bytes: Some(bytes),
        source_records: merged.seat_records,
        live_extra: merged.live_extra,
    })
}

/// The `--plan-only` row of the append log at `rel`.
///
/// #297's homes listing (class, action, sizes) takes it; the #294 lane's
/// would-write listing is this row, not a listing of its own. It is
/// [`plan`] keyed as [`union`] keys it, and a writer at either side's last
/// line is held `append-log: ends inside a line` (the union refuses that
/// `HOMES_CONSOLIDATE_RACED`, retryable).
#[must_use]
pub fn plan_at(rel: &[u8], live: &[u8], source: &[u8]) -> Plan {
    plan(live, source, by_uuid(rel)).unwrap_or(Plan {
        action: Action::Held("append-log: ends inside a line"),
        bytes: None,
        source_records: 0,
        live_extra: 0,
    })
}

/// The append-log [`super::Union`]: merge `source` into `live` and trade the
/// union in, crash-safely (see the module).
///
/// # Errors
/// - `HOMES_CONSOLIDATE_RACED`: a process holds the live file open (or this
///   platform cannot say), either side ends inside a line, or a writer
///   changed the file on every read.
/// - `DESTINATION_KIND_MISMATCH` for a live name that is no regular file (a
///   link is never followed); `PATH_ESCAPES_ROOT` for another user's file;
///   `DESTINATION_EXCHANGE_UNSUPPORTED` where two names cannot be traded.
/// - `SOURCE_CHANGED_AFTER_SNAPSHOT` for a staged source that reads short;
///   the I/O and ledger refusals.
pub fn union(live: &Live<'_>, source: &Source<'_>, ledger: &Ledger) -> Result<Outcome> {
    let held = |reason| {
        Ok(Outcome {
            action: Action::Held(reason),
            written: 0,
            recorded: 0,
        })
    };
    if source.size > SIZE_LIMIT {
        return held("append-log: over the size limit");
    }
    let offered = read_source(source)?;
    let name = io::c_name(live.name).refuse_at(SITE)?;
    let keyed = by_uuid(live.rel);
    for attempt in 1..=config::ATTEMPTS {
        let (stat, bytes) = match read_live(live.dir, &name)? {
            Read::Absent => {
                let target = Target {
                    dir: live.dir,
                    name: live.name,
                    rel: live.rel,
                };
                let created = config::create(&target, &name, &offered, attempt)?;
                return Ok(Outcome {
                    action: created.action,
                    written: created.written,
                    recorded: 0,
                });
            }
            Read::Over => return held("append-log: over the size limit"),
            Read::Changed => continue,
            Read::Live(stat, bytes) => (stat, bytes),
        };
        let Some(plan) = plan(&bytes, &offered, keyed) else {
            return Err(BulkloadRefusal::HomesConsolidateRaced);
        };
        let Some(output) = plan.bytes.as_deref() else {
            return Ok(Outcome {
                action: plan.action,
                written: 0,
                recorded: 0,
            });
        };
        if !free(live.path)? {
            return Err(BulkloadRefusal::HomesConsolidateRaced);
        }
        if config::swap_in(live.dir, &name, &stat, output, attempt)? {
            record(ledger, live.rel, &plan, (&bytes, &offered, output))?;
            return Ok(Outcome {
                action: plan.action,
                written: output.len() as u64,
                recorded: 1,
            });
        }
    }
    Err(BulkloadRefusal::HomesConsolidateRaced)
}

/// No process holds `path` open (or works under it); `false` where this
/// platform cannot say.
fn free(path: &Path) -> Result<bool> {
    Ok(matches!(crate::procs::inside(&[path])?, Some(pids) if pids.is_empty()))
}

fn read_source(source: &Source<'_>) -> Result<Vec<u8>> {
    let mut bytes =
        vec![0_u8; usize::try_from(source.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?];
    let read = sys::pread_full(source.file, &mut bytes, 0).refuse_at(SITE)?;
    if read as u64 != source.size {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    Ok(bytes)
}

/// A read of the live file.
enum Read {
    Absent,
    Over,
    /// A writer changed it while it was read.
    Changed,
    Live(Stat, Vec<u8>),
}

/// The live file's identity and bytes: this user's own regular file, never
/// through a link.
fn read_live(dir: &File, name: &CStr) -> Result<Read> {
    let fd = match sys::open_read_at(dir, name) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(Read::Absent),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(BulkloadRefusal::DestinationKindMismatch);
        }
        Err(error) => return Err(error).refuse_at(SITE),
    };
    let stat = sys::fstat(&fd).refuse_at(SITE)?;
    if !stat.is_file() {
        return Err(BulkloadRefusal::DestinationKindMismatch);
    }
    if stat.uid != sys::effective_uid() {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    if stat.size > SIZE_LIMIT {
        return Ok(Read::Over);
    }
    let mut bytes = vec![0_u8; usize::try_from(stat.size).unwrap_or(0)];
    let read = sys::pread_full(&fd, &mut bytes, 0).refuse_at(SITE)?;
    let after = sys::fstat(&fd).refuse_at(SITE)?;
    if read as u64 != stat.size || after.size != stat.size || after.mtime_ns != stat.mtime_ns {
        return Ok(Read::Changed);
    }
    Ok(Read::Live(stat, bytes))
}

/// Ledger an append log written: both inputs' and the output's digests and
/// the record counts. A union already recorded is not recorded twice.
fn record(
    ledger: &Ledger,
    rel: &[u8],
    plan: &Plan,
    (live, source, output): (&[u8], &[u8], &[u8]),
) -> Result<()> {
    let wide = |count: u64| i64::try_from(count).unwrap_or(i64::MAX);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        });
    let conn = ledger.connection();
    conn.execute_batch(SCHEMA)
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    conn.execute(
        "INSERT OR IGNORE INTO append_log_union(file, action, live_blake3, source_blake3, output_blake3, source_records, live_extra, output_size, at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            rel,
            plan.action.word(),
            blake3::hash(live).as_bytes().as_slice(),
            blake3::hash(source).as_bytes().as_slice(),
            blake3::hash(output).as_bytes().as_slice(),
            wide(plan.source_records),
            wide(plan.live_extra),
            wide(output.len() as u64),
            now,
        ],
    )
    .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests;
