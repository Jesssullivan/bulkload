//! The union's ledger tables, `row_union_*`, in the homes'
//! consolidation ledger `STATE/consolidate.sqlite` (a bulkload store: WAL,
//! `synchronous=FULL`, `io::durable::configure_sqlite`). The ledger borrows
//! its connection, so #297's `Ledger::connection()` serves it as well as
//! [`open_store`].
//!
//! - `stores`: per source store and destination store, the source identity
//!   of the last pass that ended (`End`), so an unchanged store is not read.
//! - `marks`: per table, the rowid and time marks of what the destination
//!   holds ([`Marks`]).
//! - `pending`: a surrogate-key insert batch ledgered before its commit and
//!   confirmed after it ([`Pending`]).
//! - `rows`: every row not in the destination's final state, kept or
//!   displaced, with its key, BLAKE3 and JSON ([`Entry`]).

use std::path::Path;

use bulkload_proto::rows::{Identity, Value};
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{BulkloadRefusal, Result};

/// The ledger's file name in the state directory.
pub const FILE: &str = "consolidate.sqlite";

/// What the destination holds of one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    /// Every source row up to this rowid is held.
    pub after_rowid: Option<i64>,
    /// Every source row whose time column is under this, and whose rowid is
    /// under `after_rowid`, is held in its current version.
    pub after_time: Option<Value>,
}

/// A surrogate-key batch inserted but not yet known committed: its source
/// rows up to `to_rowid` were inserted at destination rowids `low..=high`,
/// whose non-key values have BLAKE3 `digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub to_rowid: i64,
    pub low: i64,
    pub high: i64,
    pub digest: [u8; 32],
}

/// One row not in the destination's final state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub table: String,
    /// The row's key, as JSON.
    pub key: String,
    /// Whose row stayed: `dest` (this is the source's row, set aside) or
    /// `source` (this is the destination's row, displaced by the source's).
    pub kept: &'static str,
    /// Why: `older`, `same-version`, `unversioned`, `unique`, or
    /// `constraint-N` (`SQLite`'s extended code).
    pub reason: String,
    /// BLAKE3 of this row's values.
    pub digest: [u8; 32],
    /// This row, as JSON.
    pub row: String,
    /// BLAKE3 of the row that stayed, when one did.
    pub other: Option<[u8; 32]>,
}

pub struct Ledger<'c> {
    conn: &'c Connection,
    source: String,
    dest: String,
}

#[allow(clippy::needless_pass_by_value)] // a `map_err` adapter
fn sql(_: rusqlite::Error) -> BulkloadRefusal {
    BulkloadRefusal::SqliteIntegrityCheckFailed
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_stdvec(value).map_err(|_| BulkloadRefusal::FrameCodec)
}

/// Open (or create) the consolidation ledger store in `state`.
///
/// # Errors
/// The ledger store's failure.
pub fn open_store(state: &Path) -> Result<Connection> {
    let conn = Connection::open(state.join(FILE)).map_err(sql)?;
    crate::io::durable::configure_sqlite(&conn)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))
        .map_err(sql)?;
    Ok(conn)
}

impl<'c> Ledger<'c> {
    /// The union's tables on `conn`, for one source store and one
    /// destination store.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn new(conn: &'c Connection, source: &str, dest: &str) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS row_union_stores (
                 source TEXT NOT NULL, dest TEXT NOT NULL,
                 identity BLOB NOT NULL, at_ns INTEGER NOT NULL,
                 PRIMARY KEY (source, dest));
             CREATE TABLE IF NOT EXISTS row_union_marks (
                 source TEXT NOT NULL, dest TEXT NOT NULL, tbl TEXT NOT NULL,
                 after_rowid INTEGER, after_time BLOB,
                 PRIMARY KEY (source, dest, tbl));
             CREATE TABLE IF NOT EXISTS row_union_pending (
                 source TEXT NOT NULL, dest TEXT NOT NULL, tbl TEXT NOT NULL,
                 to_rowid INTEGER NOT NULL, low INTEGER NOT NULL,
                 high INTEGER NOT NULL, digest BLOB NOT NULL,
                 PRIMARY KEY (source, dest, tbl));
             CREATE TABLE IF NOT EXISTS row_union_rows (
                 id INTEGER PRIMARY KEY, at_ns INTEGER NOT NULL,
                 source TEXT NOT NULL, dest TEXT NOT NULL, tbl TEXT NOT NULL,
                 key TEXT NOT NULL, kept TEXT NOT NULL, reason TEXT NOT NULL,
                 digest BLOB NOT NULL, row TEXT NOT NULL, other BLOB);",
        )
        .map_err(sql)?;
        Ok(Self {
            conn,
            source: source.to_owned(),
            dest: dest.to_owned(),
        })
    }

    /// The source identity of the last pass that ended.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn identity(&self) -> Result<Option<Identity>> {
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM row_union_stores WHERE source = ?1 AND dest = ?2",
                params![self.source, self.dest],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        Ok(found.and_then(|bytes| bulkload_proto::frame::decode_exact(&bytes).ok()))
    }

    /// Record the source identity a pass ended with.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn set_identity(&self, identity: &Identity) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO row_union_stores (source, dest, identity, at_ns) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (source, dest) DO UPDATE SET identity = ?3, at_ns = ?4",
                params![self.source, self.dest, encode(identity)?, now_ns()],
            )
            .map(drop)
            .map_err(sql)
    }

    /// A table's marks.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn marks(&self, table: &str) -> Result<Marks> {
        let found: Option<(Option<i64>, Option<Vec<u8>>)> = self
            .conn
            .query_row(
                "SELECT after_rowid, after_time FROM row_union_marks
                 WHERE source = ?1 AND dest = ?2 AND tbl = ?3",
                params![self.source, self.dest, table],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        Ok(
            found.map_or_else(Marks::default, |(after_rowid, after_time)| Marks {
                after_rowid,
                after_time: after_time
                    .and_then(|bytes| bulkload_proto::frame::decode_exact(&bytes).ok()),
            }),
        )
    }

    /// A table's pending batch.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn pending(&self, table: &str) -> Result<Option<Pending>> {
        self.conn
            .query_row(
                "SELECT to_rowid, low, high, digest FROM row_union_pending
                 WHERE source = ?1 AND dest = ?2 AND tbl = ?3",
                params![self.source, self.dest, table],
                |row| {
                    let digest: Vec<u8> = row.get(3)?;
                    Ok(Pending {
                        to_rowid: row.get(0)?,
                        low: row.get(1)?,
                        high: row.get(2)?,
                        digest: digest.try_into().unwrap_or([0; 32]),
                    })
                },
            )
            .optional()
            .map_err(sql)
    }

    /// Before the destination's commit: the rows set aside or displaced, and
    /// the batch's pending inserts, in one durable commit.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn before_commit(
        &self,
        entries: &[Entry],
        pending: Option<(&str, &Pending)>,
    ) -> Result<()> {
        if entries.is_empty() && pending.is_none() {
            return Ok(());
        }
        let at = now_ns();
        let tx = self.conn.unchecked_transaction().map_err(sql)?;
        for entry in entries {
            tx.execute(
                "INSERT INTO row_union_rows (at_ns, source, dest, tbl, key, kept, reason, digest, row, other)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    at,
                    self.source,
                    self.dest,
                    entry.table,
                    entry.key,
                    entry.kept,
                    entry.reason,
                    entry.digest.as_slice(),
                    entry.row,
                    entry.other.as_ref().map(<[u8; 32]>::as_slice),
                ],
            )
            .map_err(sql)?;
        }
        if let Some((table, pending)) = pending {
            tx.execute(
                "INSERT INTO row_union_pending (source, dest, tbl, to_rowid, low, high, digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (source, dest, tbl)
                 DO UPDATE SET to_rowid = ?4, low = ?5, high = ?6, digest = ?7",
                params![
                    self.source,
                    self.dest,
                    table,
                    pending.to_rowid,
                    pending.low,
                    pending.high,
                    pending.digest.as_slice()
                ],
            )
            .map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    /// After the destination's commit, or after a pending batch is found
    /// committed: the table's marks, with its pending batch cleared.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn confirm(&self, table: &str, marks: &Marks) -> Result<()> {
        let time = marks.after_time.as_ref().map(encode).transpose()?;
        let tx = self.conn.unchecked_transaction().map_err(sql)?;
        tx.execute(
            "DELETE FROM row_union_pending WHERE source = ?1 AND dest = ?2 AND tbl = ?3",
            params![self.source, self.dest, table],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO row_union_marks (source, dest, tbl, after_rowid, after_time)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (source, dest, tbl) DO UPDATE SET after_rowid = ?4, after_time = ?5",
            params![self.source, self.dest, table, marks.after_rowid, time],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)
    }

    /// Drop a pending batch found not committed: its rows are read again.
    ///
    /// # Errors
    /// The ledger store's failure.
    pub fn abandon(&self, table: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM row_union_pending WHERE source = ?1 AND dest = ?2 AND tbl = ?3",
                params![self.source, self.dest, table],
            )
            .map(drop)
            .map_err(sql)
    }

    /// The ledgered rows of this source and destination, for a test:
    /// table, key, kept, reason and row.
    ///
    /// # Errors
    /// The ledger store's failure.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pub fn entries(&self) -> Result<Vec<(String, String, String, String, String)>> {
        let mut rows = self
            .conn
            .prepare(
                "SELECT tbl, key, kept, reason, row FROM row_union_rows
                 WHERE source = ?1 AND dest = ?2 ORDER BY id",
            )
            .map_err(sql)?;
        let found = rows
            .query_map(params![self.source, self.dest], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        Ok(found)
    }
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}
