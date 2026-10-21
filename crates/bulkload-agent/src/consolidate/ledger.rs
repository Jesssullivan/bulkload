//! The consolidation ledger (`consolidate.sqlite` in the state): every
//! value a union did not keep, by file and key path.
//!
//! A class's union may keep its own tables here
//! ([`Ledger::connection`]).

use std::path::Path;

use super::config::Format;
use super::value::{pointer, Conflict, Side};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The consolidation ledger: each value a merge did not keep, with its
/// key path (`consolidate.sqlite` in the move's state).
pub struct Ledger {
    conn: rusqlite::Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS value_conflict(
    file BLOB NOT NULL,
    key_path TEXT NOT NULL,
    format TEXT NOT NULL,
    other_side TEXT NOT NULL,
    other TEXT NOT NULL,
    kept_side TEXT NOT NULL,
    kept TEXT NOT NULL,
    first_seen_ns INTEGER NOT NULL,
    last_seen_ns INTEGER NOT NULL,
    PRIMARY KEY(file, key_path, other_side, other)
);";

/// One recorded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub file: Vec<u8>,
    pub key_path: String,
    pub other_side: String,
    pub other: String,
    pub kept_side: String,
    pub kept: String,
}

impl Ledger {
    /// Open (or create) the ledger in `state`.
    ///
    /// # Errors
    /// The I/O or `SQLite` refusal.
    pub fn open(state: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        match std::fs::DirBuilder::new().mode(0o700).create(state) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).refuse_at("consolidate::config::ledger_dir"),
        }
        let conn = rusqlite::Connection::open(state.join("consolidate.sqlite"))
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        conn.execute_batch(SCHEMA)
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        Ok(Self { conn })
    }

    /// The ledger's database, for a class union's own tables.
    #[must_use]
    pub const fn connection(&self) -> &rusqlite::Connection {
        &self.conn
    }

    /// Record `conflicts` of the file `rel`; a value already recorded is
    /// seen again, never duplicated.
    ///
    /// # Errors
    /// The `SQLite` refusal.
    pub fn record(&self, rel: &[u8], format: Format, conflicts: &[Conflict]) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
            });
        for conflict in conflicts {
            let other_side = match conflict.kept_side {
                Side::Live => Side::Source,
                Side::Source => Side::Live,
            };
            self.conn
                .execute(
                    "INSERT INTO value_conflict(file, key_path, format, other_side, other, kept_side, kept, first_seen_ns, last_seen_ns)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                     ON CONFLICT(file, key_path, other_side, other)
                     DO UPDATE SET kept_side = excluded.kept_side, kept = excluded.kept, last_seen_ns = excluded.last_seen_ns",
                    rusqlite::params![
                        rel,
                        pointer(&conflict.path),
                        format.word(),
                        other_side.word(),
                        format.one_line(&conflict.other),
                        conflict.kept_side.word(),
                        format.one_line(&conflict.kept),
                        now,
                    ],
                )
                .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        }
        Ok(())
    }

    /// Every recorded value, by file and key path.
    ///
    /// # Errors
    /// The `SQLite` refusal.
    pub fn recorded(&self) -> Result<Vec<Recorded>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT file, key_path, other_side, other, kept_side, kept FROM value_conflict
                 ORDER BY file, key_path, other",
            )
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        let rows = statement
            .query_map([], |row| {
                Ok(Recorded {
                    file: row.get(0)?,
                    key_path: row.get(1)?,
                    other_side: row.get(2)?,
                    other: row.get(3)?,
                    kept_side: row.get(4)?,
                    kept: row.get(5)?,
                })
            })
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)
    }
}
