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
pub use ledger::Ledger;
pub use source::{serve_rows, SourceStats};

/// The homes' consolidation outcome (#297) of one [`consolidate`] result.
///
/// A lossless union is `Consolidated` (`Covered` when the store was
/// unchanged and nothing was read), a store made here is `Created`, and a
/// store that cannot be merged is `Held` with its code, nothing written.
/// `None` for a refusal that is no hold: the transport's, the source's, or
/// a live writer's that kept racing (`HOMES_CONSOLIDATE_RACED`,
/// retryable).
#[must_use]
pub const fn action(result: &Result<UnionStats>) -> Option<crate::consolidate::Action> {
    use crate::consolidate::Action;
    match result {
        Ok(stats) if stats.created => Some(Action::Created),
        Ok(stats) if stats.unchanged => Some(Action::Covered),
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

/// One store routed to the row union by a `--consolidate` session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    /// The store, relative to the session's roots.
    pub store: Vec<u8>,
    /// Its `-wal`, `-shm` and `-journal` the session skipped: they follow
    /// the store's outcome.
    pub sidecars: Vec<Vec<u8>>,
    /// The store with `threads` in the same directory (the home's
    /// `state_5.sqlite`), when it is another: this store's thread clock.
    pub clock: Option<Vec<u8>>,
}

/// The stores of a session's skipped seats, state stores first.
///
/// The seats are [`crate::transfer::TransferStats::sqlite_routed`]. A
/// home's `state_*.sqlite` comes first in each directory, so its threads'
/// winners are decided before its other stores follow them.
#[must_use]
pub fn routed(paths: &[Vec<u8>]) -> Vec<Routed> {
    const SIDECARS: [&[u8]; 3] = [b"-wal", b"-shm", b"-journal"];
    let base = |path: &[u8]| -> Vec<u8> {
        SIDECARS
            .iter()
            .find_map(|suffix| path.strip_suffix(*suffix))
            .unwrap_or(path)
            .to_vec()
    };
    let split = |path: &[u8]| -> (Vec<u8>, Vec<u8>) {
        path.iter().rposition(|byte| *byte == b'/').map_or_else(
            || (Vec::new(), path.to_vec()),
            |at| {
                (
                    path.get(..at).unwrap_or_default().to_vec(),
                    path.get(at + 1..).unwrap_or_default().to_vec(),
                )
            },
        )
    };
    let is_state = |path: &[u8]| split(path).1.starts_with(b"state_");
    let mut stores: Vec<Routed> = Vec::new();
    for path in paths {
        let store = base(path);
        let at = stores.iter().position(|routed| routed.store == store);
        let at = at.unwrap_or_else(|| {
            stores.push(Routed {
                store: store.clone(),
                sidecars: Vec::new(),
                clock: None,
            });
            stores.len() - 1
        });
        if *path != store {
            if let Some(routed) = stores.get_mut(at) {
                routed.sidecars.push(path.clone());
            }
        }
    }
    // A store whose own seat was not offered (only a sidecar was) is still
    // the union's: the live store is merged from the source's.
    let states: Vec<Vec<u8>> = stores
        .iter()
        .filter(|routed| is_state(&routed.store))
        .map(|routed| routed.store.clone())
        .collect();
    for routed in &mut stores {
        if !is_state(&routed.store) {
            let dir = split(&routed.store).0;
            routed.clock = states.iter().find(|state| split(state).0 == dir).cloned();
        }
    }
    stores.sort_by(|a, b| {
        is_state(&b.store)
            .cmp(&is_state(&a.store))
            .then_with(|| a.store.cmp(&b.store))
    });
    stores
}

/// Record one routed store's union in the session's stats, so its transfer
/// record accounts for it and its sidecars: consolidated (with #297's
/// action word), or held / refused with the typed code.
pub fn fold(
    stats: &mut crate::transfer::TransferStats,
    routed: &Routed,
    result: &Result<UnionStats>,
) {
    let paths = std::iter::once(&routed.store).chain(routed.sidecars.iter());
    match (action(result), result) {
        (Some(crate::consolidate::Action::Held(code)), _) => {
            for path in paths {
                stats.held.push((path.clone(), code));
                stats.refusals.push((path.clone(), code.to_owned()));
            }
        }
        (Some(action), _) => {
            for path in paths {
                stats
                    .consolidated
                    .push((path.clone(), action.word().to_owned()));
            }
        }
        (None, Err(refusal)) => {
            for path in paths {
                stats
                    .refusals
                    .push((path.clone(), refusal.code().to_owned()));
            }
        }
        (None, Ok(_)) => {}
    }
}

/// Merge a `--consolidate` session's routed stores, one report line each.
///
/// Each store ([`routed`]) is merged by `run`, one serve session per
/// store, a home's state store first, and every outcome is folded into
/// `stats`. The roots are the session's source and destination roots; the
/// host is the source host's name (`None`: this host, `local`), and
/// `source_key` names a source store in the ledger.
/// How a source store is named in the ledger.
pub type SourceKey<'k> = &'k dyn Fn(&Path) -> String;

pub fn union_routed(
    stats: &mut crate::transfer::TransferStats,
    settings: &crate::transfer::Consolidation,
    (source_root, dest_root): (&Path, &Path),
    (host, source_key): (Option<&str>, SourceKey<'_>),
    mut run: impl FnMut(&Options) -> Result<UnionStats>,
) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt as _;
    let routed = routed(&std::mem::take(&mut stats.sqlite_routed));
    if settings.plan_only {
        return Vec::new();
    }
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = settings
        .maps
        .pairs()
        .iter()
        .map(|(from, to)| (from.as_bytes().to_vec(), to.as_bytes().to_vec()))
        .collect();
    let maps = maps(&pairs);
    let join = |root: &Path, rel: &[u8]| root.join(std::ffi::OsStr::from_bytes(rel));
    let mut lines = Vec::with_capacity(routed.len());
    for store in &routed {
        let source = join(source_root, &store.store);
        let options = Options {
            host: host.unwrap_or("local").to_owned(),
            source: source_key(&source),
            source_db: source.as_os_str().as_bytes().to_vec(),
            source_clock: store
                .clock
                .as_ref()
                .map(|clock| join(source_root, clock).as_os_str().as_bytes().to_vec()),
            dest: join(dest_root, &store.store),
            state: settings.ledger.clone(),
            maps: maps.clone(),
            full: false,
        };
        let result = run(&options);
        let word = match (action(&result), &result) {
            (Some(action), _) => action.word().to_owned(),
            (None, Err(refusal)) => refusal.code().to_owned(),
            (None, Ok(_)) => "none".to_owned(),
        };
        lines.push(format!(
            "sqlite-union-store {} {word}",
            store.store.escape_ascii()
        ));
        fold(stats, store, &result);
    }
    lines
}

/// `sqlite-union --local`, and a `copy --consolidate`'s stores: the source
/// half on a thread of this process, over a socket pair.
///
/// # Errors
/// The union's refusal, or the source half's.
pub fn local(options: &Options) -> Result<UnionStats> {
    let io = |error: std::io::Error| BulkloadRefusal::Io(error.raw_os_error());
    let (source, mut dest) = std::os::unix::net::UnixStream::pair().map_err(io)?;
    let mut source_in = source.try_clone().map_err(io)?;
    let mut source_out = source;
    let served = std::thread::spawn(move || serve_rows(&mut source_in, &mut source_out));
    let mut dest_in = dest.try_clone().map_err(io)?;
    let merged = consolidate(&mut dest_in, &mut dest, options);
    let _ = dest.shutdown(std::net::Shutdown::Both);
    let served = served.join().map_err(|_| BulkloadRefusal::TransportLost)?;
    let stats = merged?;
    // The source's own refusal reached the merge as `Refused`; one after
    // the merge ended is still this run's failure.
    served?;
    Ok(stats)
}

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
