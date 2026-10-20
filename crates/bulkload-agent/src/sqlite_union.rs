//! `sqlite-union`: a source `SQLite` store merged row by row.
//!
//! The rows go into the destination's live store of the same schema
//! (operator ruling 2026-10-20: "neither 'WIN'. they must be consoldiated"
//! and "no dupes, no duplciated besides, no stage dupes"; SPEC R2.2, C10:
//! "Every run is a lossless union into DEST").
//!
//! A Codex home's stores (`state_5.sqlite`, `thread_history_1.sqlite`,
//! `logs_2.sqlite`, and any other) are merged into the destination's live
//! stores of the same name: never landed as a file, never beside one
//! (`<db>.LABEL`), and never staged as a copy on either host.
//!
//! **Read side** ([`serve_rows`], on the source, `--sqlite-rows serve`): one
//! read snapshot of the WAL-mode source is held for the whole pass, as a
//! carry holds it (operator ruling 2026-10-19 "Hold one read snapshot"),
//! within the carry wall and the source's free-space floor. The rows are
//! streamed from that snapshot over the wire (`bulkload_proto::rows`): there
//! is no temporary database on either host. A store whose identity (its
//! database and `-wal`, `stat`) is the one the destination recorded at its
//! last completed pass is not opened: nothing is read.
//!
//! **Write side** ([`consolidate`], on the destination): the destination store
//! must already exist and its schema must equal the source's entry for
//! entry, every table's `CREATE` statement, `user_version` and the applied
//! `_sqlx_migrations`; otherwise `SQLITE_SCHEMA_MISMATCH` and nothing is
//! written. Tables are merged parents first (foreign keys), in short
//! `BEGIN IMMEDIATE` transactions under a busy timeout ([`dest::BATCH_ROWS`]
//! rows or [`dest::BATCH_WALL`], whichever comes first), so a live writer on
//! the destination waits at most one short batch. Per row:
//!
//! - **A natural key** (the table's primary key): a row the destination does
//!   not hold is inserted; an equal row is adopted (nothing written); a row
//!   that differs goes in only when its version column (`updated_at_ms`,
//!   `updated_at` or `updated_at_ordinal`) is strictly later, and the
//!   destination's row is recorded in the ledger before the replacing
//!   commit; otherwise the destination's row stays and the source's is
//!   recorded. A replacement is an `UPDATE`, never a delete and insert, so
//!   no `ON DELETE CASCADE` fires.
//! - **A surrogate key** (`INTEGER PRIMARY KEY AUTOINCREMENT`, `logs.id`):
//!   the key means nothing on another host. A row is inserted with a new
//!   key, unless a `UNIQUE` constraint names a destination row: equal
//!   content is adopted, other content keeps the destination's and records
//!   the source's. Rows are read once, by rowid mark, and an insert batch is
//!   ledgered as pending before its commit and confirmed by content after,
//!   so an interrupted pass never inserts a row twice.
//! - A row a constraint refuses (a foreign key whose parent the source did
//!   not have) is recorded and not written.
//!
//! Path columns (`cwd`, `path`, `*_path`) are re-spelled through the `--map
//! SRC=DST` set as rows are inserted (A-055): a value equal to a mapped
//! source path or under it, at a component boundary, takes the longest
//! mapped prefix's destination. The ledger records every row not in the
//! destination's final state with its key, its BLAKE3 and its JSON.
//!
//! **Rerun.** Each table's marks record what the destination holds: rows
//! past the last rowid, and rows whose time column (`updated_at_ms` or
//! `updated_at`) is at least the snapshot's high value of the last pass
//! that read the table to its end. A rerun reads only those. After the pass
//! the destination's `integrity_check` and `foreign_key_check` run.

mod dest;
pub mod hold;
mod ledger;
mod source;
#[cfg(test)]
mod tests;

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

pub use dest::{consolidate, maps, Options, UnionStats};
pub use ledger::{open_store as open_ledger, Ledger, FILE as LEDGER_FILE};

/// What a store's consolidation did, in #297's terms (`Action`).
///
/// A lossless union is `Consolidated`; a store that cannot be merged is
/// `Held` with its code, nothing written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Consolidated,
    /// The destination had no store: it was made, and the rows went in.
    Created,
    Held(&'static str),
}

/// The [`Action`] of one [`consolidate`] result; `None` for a refusal that
/// is no hold (the transport's, a source's, a racing live writer's).
#[must_use]
pub const fn action(result: &Result<UnionStats>) -> Option<Action> {
    match result {
        Ok(stats) if stats.created => Some(Action::Created),
        Ok(_) => Some(Action::Consolidated),
        Err(
            refusal @ (BulkloadRefusal::SqliteSchemaMismatch
            | BulkloadRefusal::SqliteUnionSchemaUnsupported
            | BulkloadRefusal::SqliteUnionSourceUnreadable
            | BulkloadRefusal::SqliteUnionSourceNotWal),
        ) => Some(Action::Held(refusal.code())),
        Err(_) => None,
    }
}
pub use source::{serve_rows, SourceStats};

use bulkload_proto::frame::{FRAME_HEADER_BYTES, MAX_FRAME_BYTES, TAG_CONTROL};
use bulkload_proto::rows::{
    self, Column, Extra, Identity, Migration, Stat, Store, TableSchema, Value,
};
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension as _};

use crate::{BulkloadRefusal, Result};

/// Read one row stream message.
///
/// # Errors
/// `TRANSPORT_LOST` when the stream ends, `FRAME_CODEC` for a malformed
/// frame, `BUDGET_EXCEEDED` for an oversized one.
pub(crate) fn read_message<T: serde::de::DeserializeOwned, R: Read>(input: &mut R) -> Result<T> {
    let mut head = [0_u8; FRAME_HEADER_BYTES];
    input
        .read_exact(&mut head)
        .map_err(|_| BulkloadRefusal::TransportLost)?;
    let [a, b, c, d, tag] = head;
    let length = usize::try_from(u32::from_be_bytes([a, b, c, d]))
        .map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    if length > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if tag != TAG_CONTROL || length == 0 {
        return Err(BulkloadRefusal::FrameCodec);
    }
    let mut body = vec![0_u8; length - 1];
    input
        .read_exact(&mut body)
        .map_err(|_| BulkloadRefusal::TransportLost)?;
    rows::decode(&body)
}

/// Write one row stream message and flush it.
///
/// # Errors
/// `TRANSPORT_LOST` when the peer is gone; the codec's refusals.
pub(crate) fn write_message<T: serde::Serialize, W: Write>(
    output: &mut W,
    message: &T,
) -> Result<()> {
    let frame = rows::encode(message)?;
    output
        .write_all(&frame)
        .and_then(|()| output.flush())
        .map_err(|_| BulkloadRefusal::TransportLost)
}

/// A name from a schema as SQL text: `SQLite` takes statements as UTF-8.
fn text(name: &[u8]) -> Result<&str> {
    std::str::from_utf8(name).map_err(|_| BulkloadRefusal::SqliteUnionSchemaUnsupported)
}

/// `name` quoted as an identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The failure of a statement on a store being read or merged: `SQLite`'s
/// extended code (`SQLITE_BACKUP_FAILED`, the code the carry uses).
#[allow(clippy::needless_pass_by_value)] // a `map_err` adapter
fn sql(error: rusqlite::Error) -> BulkloadRefusal {
    crate::provider_sqlite::backup_refusal(error)
}

/// One value as the stream carries it.
fn value_of(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::Integer(integer),
        ValueRef::Real(real) => Value::Real(real.to_bits()),
        ValueRef::Text(bytes) => Value::Text(bytes.to_vec()),
        ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
    }
}

/// A [`Value`] bound as a parameter, by storage class: text is bound as
/// its bytes, whatever they are.
struct Bind<'v>(&'v Value);

impl rusqlite::ToSql for Bind<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self.0 {
            Value::Null => ValueRef::Null,
            Value::Integer(integer) => ValueRef::Integer(*integer),
            Value::Real(bits) => ValueRef::Real(f64::from_bits(*bits)),
            Value::Text(bytes) => ValueRef::Text(bytes),
            Value::Blob(bytes) => ValueRef::Blob(bytes),
        }))
    }
}

/// A store's schema, read inside the caller's transaction: every table but
/// `SQLite`'s own, its columns as `table_xinfo` reports them, `user_version`
/// and the applied `_sqlx_migrations`.
///
/// # Errors
/// The store's `SQLite` failure.
#[allow(
    clippy::too_many_lines,
    reason = "the schema's four parts, each one query, read top to bottom"
)]
pub(crate) fn read_store(conn: &Connection) -> Result<Store> {
    let mut listed = conn
        .prepare(
            "SELECT name, type, wr FROM pragma_table_list \
             WHERE schema = 'main' AND type IN ('table', 'virtual', 'shadow') \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )
        .map_err(sql)?;
    let entries = listed
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    let mut tables = Vec::with_capacity(entries.len());
    for (name, kind, without_rowid) in entries {
        let sql_text: Option<Vec<u8>> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [&name],
                |row| {
                    Ok(match row.get_ref(0)? {
                        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(bytes.to_vec()),
                        _ => None,
                    })
                },
            )
            .optional()
            .map_err(sql)?
            .flatten();
        // A virtual or shadow table is reported by its kind, which no
        // ordinary table's statement can equal.
        let sql_text = if kind == "table" {
            sql_text.unwrap_or_default()
        } else {
            format!("{kind}:").into_bytes()
        };
        let mut columns = conn
            .prepare(&format!("PRAGMA table_xinfo({})", quote(&name)))
            .map_err(sql)?;
        let columns = columns
            .query_map([], |row| {
                Ok(Column {
                    name: row.get::<_, String>(1)?.into_bytes(),
                    decl: row.get::<_, String>(2)?.into_bytes(),
                    notnull: row.get::<_, i64>(3)? != 0,
                    default: row.get::<_, Option<String>>(4)?.map(String::into_bytes),
                    pk: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                    hidden: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                })
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        tables.push(TableSchema {
            name: name.into_bytes(),
            sql: sql_text,
            columns,
            rowid: without_rowid == 0,
        });
    }
    let mut listed = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema \
             WHERE type IN ('index', 'trigger', 'view') AND sql IS NOT NULL \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY CASE type WHEN 'index' THEN 0 WHEN 'view' THEN 1 ELSE 2 END, name",
        )
        .map_err(sql)?;
    let extras = listed
        .query_map([], |row| {
            let bytes = |at: usize| -> rusqlite::Result<Vec<u8>> {
                Ok(match row.get_ref(at)? {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.to_vec(),
                    _ => Vec::new(),
                })
            };
            Ok(Extra {
                kind: bytes(0)?,
                name: bytes(1)?,
                sql: bytes(2)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    let user_version = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql)?;
    let migrations = if tables
        .iter()
        .any(|table| table.name == MIGRATIONS.as_bytes())
    {
        let mut applied = conn
            .prepare("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .map_err(sql)?;
        let applied = applied
            .query_map([], |row| {
                Ok(Migration {
                    version: row.get(0)?,
                    checksum: match row.get_ref(1)? {
                        ValueRef::Blob(bytes) | ValueRef::Text(bytes) => bytes.to_vec(),
                        _ => Vec::new(),
                    },
                })
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        applied
    } else {
        Vec::new()
    };
    Ok(Store {
        tables,
        extras,
        user_version,
        migrations,
    })
}

/// sqlx's migration table: compared, never merged (its rows hold each
/// host's own install times).
const MIGRATIONS: &str = "_sqlx_migrations";

/// A file's identity, or `None` when nothing is at `path`.
fn stat(path: &Path) -> Result<Option<Stat>> {
    match std::fs::symlink_metadata(path) {
        Ok(found) => Ok(Some(Stat {
            dev: found.dev(),
            ino: found.ino(),
            size: found.size(),
            mtime_ns: found
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(found.mtime_nsec()),
            ctime_ns: found
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(found.ctime_nsec()),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BulkloadRefusal::Io(error.raw_os_error())),
    }
}

/// A store's identity now ([`Identity`]); an empty `-wal` is none.
///
/// # Errors
/// `SOURCE_VANISHED` when no database is at `path`; a failed `lstat`.
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    let db = stat(path)?.ok_or(BulkloadRefusal::SourceVanished)?;
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    // An empty `-wal` holds nothing: a read-only open makes one where none
    // was, and that is no change to the store.
    Ok(Identity {
        db,
        wal: stat(Path::new(&wal))?.filter(|wal| wal.size > 0),
    })
}
