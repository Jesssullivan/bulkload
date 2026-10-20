//! The read side: `serve` in row mode (`--sqlite-rows serve`).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bulkload_proto::rows::{self, Go, Reply, Request, Row, TableSchema, Value, Want, ROWS_VERSION};
use rusqlite::{Connection, OpenFlags};

use super::{quote, read_message, read_store, sql, text, value_of, write_message};
use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// Rows per [`Reply::Rows`] at most.
const FRAME_ROWS: usize = 1024;

/// Encoded bytes per [`Reply::Rows`] before it is sent: well under the
/// frame bound, so one more row (at most a frame's worth) never crosses it.
const FRAME_BYTES: usize = 1 << 20;

/// How often the wall and the free-space floor are looked at while the
/// snapshot is held (as a carry's held backup probes them).
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// What one source pass read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// The store was the destination's recorded one: nothing was read.
    pub unchanged: bool,
    pub tables: u64,
    pub rows: u64,
    /// How long the read snapshot was held.
    pub held: Duration,
}

/// Serve one store's rows: read the [`Request`], answer from one held read
/// snapshot, and release it before returning, refused or not. A refusal is
/// sent as [`Reply::Refused`] too.
///
/// # Errors
/// The pass's refusal: `SOURCE_VANISHED`, `SQLITE_SOURCE_AS_ROOT`,
/// `SQLITE_SOURCE_NOT_OWNER`, `SQLITE_UNION_SOURCE_NOT_WAL`,
/// `BUDGET_EXCEEDED` past the carry wall or under the free-space floor,
/// `SQLITE_STATE_CHANGED` when the snapshot could not be taken within the
/// wall, `SQLITE_UNION_SOURCE_UNREADABLE` for a store `SQLite` finds corrupt
/// or not a database (a hold: nothing is written, the operator rules),
/// `SQLITE_BACKUP_FAILED` for another read failure, or the transport's.
pub fn serve_rows<R: Read, W: Write>(input: &mut R, output: &mut W) -> Result<SourceStats> {
    let request: Request = read_message(input)?;
    let served = serve(input, output, &request).map_err(unreadable);
    if let Err(refusal) = &served {
        let _ = write_message(
            output,
            &Reply::Refused {
                code: refusal.code().to_owned(),
                sqlite_code: refusal.sqlite_code(),
            },
        );
    }
    served
}

/// A read that found the store corrupt (`SQLITE_CORRUPT`, 11) or not a
/// database (`SQLITE_NOTADB`, 26) is the store's hold, not a failure.
#[allow(clippy::needless_pass_by_value)] // a `map_err` adapter
fn unreadable(refusal: BulkloadRefusal) -> BulkloadRefusal {
    match refusal {
        BulkloadRefusal::SqliteBackupFailed(Some(code)) if matches!(code & 0xff, 11 | 26) => {
            BulkloadRefusal::SqliteUnionSourceUnreadable
        }
        other => other,
    }
}

/// Where a thread's `updated_at` is read: the store's own `threads`, or
/// the clock store attached as `clock`.
struct Clock {
    schema: &'static str,
    time: String,
}

/// The clock of this pass, if one is found: `clock` when the request names
/// another store (attached read-only before the snapshot is taken, so its
/// first read joins it), else the store itself when it has `threads`.
fn clock(conn: &Connection, db: &[u8], clock: Option<&[u8]>) -> Option<Clock> {
    let schema = match clock {
        Some(path) if path != db => {
            let attached = conn.execute(
                "ATTACH DATABASE ?1 AS clock",
                [super::Bind(&Value::Text(path.to_vec()))],
            );
            if attached.is_err() {
                return None;
            }
            "clock"
        }
        _ => "main",
    };
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('threads', ?1)")
        .ok()?;
    let names = statement
        .query_map([schema], |row| row.get::<_, String>(0))
        .ok()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .ok()?;
    if !names.iter().any(|name| name == "id") {
        return None;
    }
    let time = ["updated_at_ms", "updated_at"]
        .into_iter()
        .find(|wanted| names.iter().any(|name| name == wanted))?;
    Some(Clock {
        schema,
        time: quote(time),
    })
}

fn serve<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    request: &Request,
) -> Result<SourceStats> {
    if request.version != ROWS_VERSION || request.schema != rows::rows_id() {
        return Err(BulkloadRefusal::SchemaMismatch);
    }
    let path = PathBuf::from(OsString::from_vec(request.db.clone()));
    if !path.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let identity = super::identity(&path)?;
    if request.last == Some(identity) {
        counters::bump(Counter::SourceSqliteRowsUnchanged);
        write_message(
            output,
            &Reply::Hello {
                version: ROWS_VERSION,
                schema: rows::rows_id(),
                identity,
                store: None,
            },
        )?;
        return Ok(SourceStats {
            unchanged: true,
            ..SourceStats::default()
        });
    }
    if crate::io::sys::effective_uid() == 0 {
        return Err(BulkloadRefusal::SqliteSourceAsRoot);
    }
    let owner = std::fs::symlink_metadata(&path)
        .map_err(|error| BulkloadRefusal::Io(error.raw_os_error()))?
        .uid();
    if owner != crate::io::sys::effective_uid() {
        return Err(BulkloadRefusal::SqliteSourceNotOwner);
    }
    let bytes = identity
        .db
        .size
        .saturating_add(identity.wal.map_or(0, |wal| wal.size));
    let wall = super::hold::wall(bytes);
    let footprint = crate::provider_sqlite::Footprint::observe(&path);
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql)?;
    conn.busy_timeout(Duration::ZERO).map_err(sql)?;
    let clock = clock(&conn, &request.db, request.clock.as_deref());
    let started = Instant::now();
    let held = super::hold::hold_read_snapshot(&conn, started, wall);
    let streamed = match held {
        Ok(true) => {
            let held_from = Instant::now();
            let streamed = Pass {
                conn: &conn,
                started,
                wall,
                dir: path.parent().unwrap_or_else(|| Path::new("/")),
                rows: 0,
                probed: Instant::now(),
                clock: clock.as_ref(),
            }
            .stream(input, output, identity, held_from);
            // Released whatever the pass did: never held past it.
            let _ = conn.execute_batch("ROLLBACK");
            let held = held_from.elapsed();
            counters::bump(Counter::SourceSqliteSnapshotHolds);
            counters::add(
                Counter::SourceSqliteSnapshotHeldNs,
                u64::try_from(held.as_nanos()).unwrap_or(u64::MAX),
            );
            streamed.map(|(tables, rows)| SourceStats {
                unchanged: false,
                tables,
                rows,
                held,
            })
        }
        Ok(false) => Err(BulkloadRefusal::SqliteUnionSourceNotWal),
        Err(refusal) => Err(refusal),
    };
    drop(conn);
    footprint.settle();
    streamed
}

/// One held pass over a store.
struct Pass<'c> {
    conn: &'c Connection,
    started: Instant,
    wall: Duration,
    dir: &'c Path,
    rows: u64,
    probed: Instant,
    clock: Option<&'c Clock>,
}

impl Pass<'_> {
    fn stream<R: Read, W: Write>(
        &mut self,
        input: &mut R,
        output: &mut W,
        identity: rows::Identity,
        held_from: Instant,
    ) -> Result<(u64, u64)> {
        let store = read_store(self.conn)?;
        write_message(
            output,
            &Reply::Hello {
                version: ROWS_VERSION,
                schema: rows::rows_id(),
                identity,
                store: Some(store.clone()),
            },
        )?;
        let wants = match read_message::<Go, R>(input)? {
            Go::Stop => return Ok((0, 0)),
            Go::Tables(wants) => wants,
        };
        let mut tables = 0_u64;
        for want in &wants {
            let table = store
                .tables
                .iter()
                .find(|table| table.name == want.table)
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            self.table(output, table, want)?;
            tables += 1;
        }
        write_message(
            output,
            &Reply::End {
                rows: self.rows,
                held_ns: u64::try_from(held_from.elapsed().as_nanos()).unwrap_or(u64::MAX),
            },
        )?;
        Ok((tables, self.rows))
    }

    /// Send one table's wanted rows, in rowid order when it has a rowid.
    #[allow(
        clippy::too_many_lines,
        reason = "one table's query, built and streamed, read top to bottom"
    )]
    fn table<W: Write>(&mut self, output: &mut W, table: &TableSchema, want: &Want) -> Result<()> {
        let name = quote(text(&table.name)?);
        let mut columns = Vec::with_capacity(table.columns.len());
        for column in &table.columns {
            if column.hidden != 0 {
                return Err(BulkloadRefusal::SqliteUnionSchemaUnsupported);
            }
            columns.push(quote(text(&column.name)?));
        }
        let time = match &want.time {
            Some(time) => {
                if !table.columns.iter().any(|column| &column.name == time) {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
                Some(quote(text(time)?))
            }
            None => None,
        };
        // A thread-scoped table without a time of its own changes with its
        // thread: its rows are wanted by the thread's `updated_at`.
        let by_thread = match (&time, &want.thread, self.clock) {
            (None, Some(column), Some(clock)) => {
                if !table.columns.iter().any(|found| &found.name == column) {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
                Some((quote(text(column)?), clock))
            }
            _ => None,
        };
        let high = match (&time, &by_thread) {
            (Some(time), _) => Some(format!("SELECT max({time}) FROM {name}")),
            (None, Some((_, clock))) => Some(format!(
                "SELECT max({}) FROM {}.\"threads\"",
                clock.time, clock.schema
            )),
            (None, None) => None,
        };
        let time_high = match high {
            Some(high) => Some(
                self.conn
                    .query_row(&high, [], |row| row.get_ref(0).map(value_of))
                    .map_err(sql)?,
            ),
            None => None,
        };
        let changed = match (&time, &by_thread) {
            (Some(time), _) => Some(format!("{time} >= ?")),
            (None, Some((column, clock))) => Some(format!(
                "{column} IN (SELECT \"id\" FROM {}.\"threads\" WHERE {} >= ?)",
                clock.schema, clock.time
            )),
            (None, None) => None,
        };
        write_message(
            output,
            &Reply::Table {
                name: table.name.clone(),
                columns: table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                time_high,
            },
        )?;
        let after_rowid = want.after_rowid.filter(|_| table.rowid);
        let after_time = want.after_time.as_ref().filter(|_| changed.is_some());
        let mut query = format!(
            "SELECT {}{} FROM {name}",
            if table.rowid { "rowid, " } else { "" },
            columns.join(", ")
        );
        let mut bound: Vec<Value> = Vec::new();
        match (after_rowid, after_time, &changed) {
            (Some(rowid), Some(at), Some(changed)) => {
                let _ = write!(query, " WHERE rowid > ? OR {changed}");
                bound.push(Value::Integer(rowid));
                bound.push(at.clone());
            }
            (Some(rowid), None, _) => {
                query.push_str(" WHERE rowid > ?");
                bound.push(Value::Integer(rowid));
            }
            (None, Some(at), Some(changed)) => {
                let _ = write!(query, " WHERE {changed}");
                bound.push(at.clone());
            }
            _ => {}
        }
        if table.rowid {
            query.push_str(" ORDER BY rowid");
        }
        let mut statement = self.conn.prepare(&query).map_err(sql)?;
        let params: Vec<super::Bind<'_>> = bound.iter().map(super::Bind).collect();
        let mut found = statement
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(sql)?;
        let width = table.columns.len();
        let mut batch = Vec::new();
        let mut batch_bytes = 0_usize;
        let mut sent = 0_u64;
        while let Some(row) = found.next().map_err(sql)? {
            let (rowid, offset) = if table.rowid {
                (Some(row.get::<_, i64>(0).map_err(sql)?), 1)
            } else {
                (None, 0)
            };
            let mut values = Vec::with_capacity(width);
            for at in 0..width {
                values.push(value_of(row.get_ref(at + offset).map_err(sql)?));
            }
            let row = Row { rowid, values };
            batch_bytes = batch_bytes.saturating_add(rows::row_bytes(&row));
            batch.push(row);
            sent += 1;
            if batch.len() >= FRAME_ROWS || batch_bytes >= FRAME_BYTES {
                self.send(output, &mut batch)?;
                batch_bytes = 0;
            }
            self.probe()?;
        }
        self.send(output, &mut batch)?;
        write_message(output, &Reply::TableEnd { rows: sent })?;
        Ok(())
    }

    fn send<W: Write>(&mut self, output: &mut W, batch: &mut Vec<Row>) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(batch);
        let count = u64::try_from(rows.len()).unwrap_or(u64::MAX);
        write_message(output, &Reply::Rows(rows))?;
        self.rows = self.rows.saturating_add(count);
        counters::add(Counter::SourceSqliteRowsRead, count);
        Ok(())
    }

    /// The wall and the floor, every [`PROBE_EVERY`]: a held snapshot is
    /// released at either (operator ruling 2026-10-19).
    fn probe(&mut self) -> Result<()> {
        if self.probed.elapsed() < PROBE_EVERY {
            return Ok(());
        }
        self.probed = Instant::now();
        if self.started.elapsed() > self.wall {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        if super::hold::floor_crossed(self.dir) {
            counters::bump(Counter::SourceSqliteHeldFloorRefusals);
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        Ok(())
    }
}
