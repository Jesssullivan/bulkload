//! The `SQLite` row stream (`sqlite-union`).
//!
//! A store's rows are read from one held read snapshot on the source and
//! merged row by row into the destination's live store of the same schema
//! (operator ruling 2026-10-20: "they must be consoldiated" and "no dupes,
//! no duplciated besides, no stage dupes").
//!
//! It is its own protocol, versioned by [`ROWS_VERSION`] and
//! [`rows_id`]: the transfer wire (`frame`, v6) is unchanged. It reuses the
//! transfer wire's framing, a 4-byte big-endian length, the
//! [`crate::frame::TAG_CONTROL`] tag and one postcard value, bounded by
//! [`crate::frame::MAX_FRAME_BYTES`].
//!
//! The exchange, destination first:
//!
//! 1. [`Request`]: the database, and the source identity the destination
//!    recorded at its last completed pass, if any.
//! 2. [`Reply::Hello`]: the source's identity now and, unless it is the one
//!    recorded (then nothing is read and the session ends), the store's
//!    schema as the held snapshot reads it.
//! 3. [`Go`]: the tables the destination wants, in its order, each with the
//!    marks of what it already holds; or [`Go::Stop`], and nothing is sent.
//! 4. Per table, [`Reply::Table`], [`Reply::Rows`] batches and
//!    [`Reply::TableEnd`]; then [`Reply::End`]. A refusal is
//!    [`Reply::Refused`], at any point after the request.

use serde::{Deserialize, Serialize};

use crate::frame::{decode_exact, frame_header, FRAME_HEADER_BYTES, TAG_CONTROL};
use crate::refusal::BulkloadRefusal;
use crate::Result;

/// Row stream protocol version.
pub const ROWS_VERSION: u16 = 1;

/// The row stream schema whose BLAKE3 is [`rows_id`]. Any change to a
/// message changes this text, and a peer with another id refuses.
pub const ROWS_SCHEMA: &str = "bulkload rows v1 (2026-10-20)
framing: u32be length (tag + body), tag 0x01, postcard body
request {version u16, schema [u8;32], db bytes, last Option<Identity>, clock Option<bytes>, plan bool}
identity {db Stat, wal Option<Stat>}
stat {dev u64, ino u64, size u64, mtime_ns i64, ctime_ns i64}
value 0 Null, 1 Integer i64, 2 Real u64 (f64 bits), 3 Text bytes, 4 Blob bytes
store {tables [TableSchema], extras [Extra], user_version i64, migrations [Migration]}
extra {kind bytes, name bytes, sql bytes}
tableschema {name bytes, sql bytes, columns [Column], rowid bool}
column {name bytes, decl bytes, notnull bool, default Option<bytes>, pk u32, hidden u32}
migration {version i64, checksum bytes}
go 0 Stop, 1 Tables [Want]
want {table bytes, time Option<bytes>, thread Option<bytes>, after_rowid Option<i64>, after_time Option<Value>}
row {rowid Option<i64>, values [Value]}
reply 0 Hello{version u16, schema [u8;32], identity Identity, store Option<Store>}, 1 Table{name bytes, columns [bytes], time_high Option<Value>}, 2 Rows [Row], 3 TableEnd{rows u64}, 4 End{rows u64, held_ns u64}, 5 Refused{code string, sqlite_code Option<i32>}, 6 Planned{sqlite bool, wal bool}";

/// BLAKE3 of [`ROWS_SCHEMA`]: both peers must agree on it.
#[must_use]
pub fn rows_id() -> [u8; 32] {
    *blake3::hash(ROWS_SCHEMA.as_bytes()).as_bytes()
}

/// A file's identity as `stat` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
}

/// A store's identity: its database file and its `-wal`, if one exists. In
/// WAL mode every commit changes the `-wal` and every checkpoint the
/// database, so an equal identity is an unchanged store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub db: Stat,
    pub wal: Option<Stat>,
}

/// One `SQLite` value, by storage class. A real crosses as its bits, so
/// equality is exact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Value {
    Null,
    Integer(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

/// One column as `PRAGMA table_xinfo` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: Vec<u8>,
    /// The declared type.
    pub decl: Vec<u8>,
    pub notnull: bool,
    /// The default expression's text.
    pub default: Option<Vec<u8>>,
    /// Position in the primary key, 0 when not in it.
    pub pk: u32,
    /// 0 for an ordinary column; a generated or hidden column otherwise.
    pub hidden: u32,
}

/// One table of a store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: Vec<u8>,
    /// Its `CREATE` statement as `sqlite_schema` holds it.
    pub sql: Vec<u8>,
    pub columns: Vec<Column>,
    /// Whether it has a rowid (not `WITHOUT ROWID`).
    pub rowid: bool,
}

/// One applied migration (`_sqlx_migrations`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub version: i64,
    pub checksum: Vec<u8>,
}

/// An index, trigger or view of a store, as `sqlite_schema` holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    /// `index`, `trigger` or `view`.
    pub kind: Vec<u8>,
    pub name: Vec<u8>,
    pub sql: Vec<u8>,
}

/// A store's schema: what the destination's must equal, entry for entry,
/// before any row is written, and what a store the union creates is made
/// with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    /// Every table but `SQLite`'s own, by name.
    pub tables: Vec<TableSchema>,
    /// Every index, trigger and view with a statement, tables' first.
    pub extras: Vec<Extra>,
    /// `PRAGMA user_version`.
    pub user_version: i64,
    /// `_sqlx_migrations`, by version, when the store has it.
    pub migrations: Vec<Migration>,
}

/// The destination's opening message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    /// [`rows_id`].
    pub schema: [u8; 32],
    /// The source database's absolute path.
    pub db: Vec<u8>,
    /// The source identity the destination recorded at its last completed
    /// pass of this store.
    pub last: Option<Identity>,
    /// The source store that holds `threads(id, updated_at[_ms])`, when it
    /// is another store than `db`: a thread-scoped table's rows are then
    /// wanted by their thread's change ([`Want::thread`]).
    pub clock: Option<Vec<u8>>,
    /// `--plan-only`: only the store's header is read (a plain file read,
    /// no `SQLite` lock), and [`Reply::Planned`] says whether it can be
    /// merged.
    pub plan: bool,
}

/// What the destination wants of one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Want {
    pub table: Vec<u8>,
    /// The table's time column, whose high value the source reports at the
    /// start of the table.
    pub time: Option<Vec<u8>>,
    /// A thread-scoped table without a time column of its own: its thread
    /// column. Its rows are wanted when their thread's `updated_at` (in the
    /// clock store) is at least `after_time`, and the source reports the
    /// clock's high value as the table's.
    pub thread: Option<Vec<u8>>,
    /// Rows past this rowid are wanted...
    pub after_rowid: Option<i64>,
    /// ... and rows whose time column is at least this. With neither mark
    /// the whole table is wanted.
    pub after_time: Option<Value>,
}

/// The destination's answer to [`Reply::Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Go {
    /// Nothing is wanted (the schemas differ, or the destination refused).
    Stop,
    /// These tables, in this order.
    Tables(Vec<Want>),
}

/// One row: its rowid when the table has one, and its values in the
/// order [`Reply::Table`] named the columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub rowid: Option<i64>,
    pub values: Vec<Value>,
}

/// The source's messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Hello {
        version: u16,
        schema: [u8; 32],
        identity: Identity,
        /// `None` when `identity` is the request's `last`: nothing is read.
        store: Option<Store>,
    },
    /// A table's rows follow, in rowid order when it has a rowid.
    Table {
        name: Vec<u8>,
        columns: Vec<Vec<u8>>,
        /// The time column's highest value in the snapshot.
        time_high: Option<Value>,
    },
    Rows(Vec<Row>),
    TableEnd {
        rows: u64,
    },
    End {
        rows: u64,
        /// How long the read snapshot was held.
        held_ns: u64,
    },
    Refused {
        code: String,
        sqlite_code: Option<i32>,
    },
    /// The answer to a plan request: the file is a `SQLite` database, and
    /// its header says WAL mode.
    Planned {
        sqlite: bool,
        wal: bool,
    },
}

/// Encode one message as a frame.
///
/// # Errors
/// [`BulkloadRefusal::FrameCodec`] if postcard declines it, and
/// [`BulkloadRefusal::BudgetExceeded`] past
/// [`crate::frame::MAX_FRAME_BYTES`].
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let body = postcard::to_stdvec(message).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let header = frame_header(TAG_CONTROL, body.len())?;
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode one frame's body (the bytes after the tag) as a message.
///
/// # Errors
/// [`BulkloadRefusal::FrameCodec`] for a malformed value or trailing bytes.
pub fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    decode_exact(body)
}

/// The encoded size of one row, for batching under the frame bound.
#[must_use]
pub fn row_bytes(row: &Row) -> usize {
    postcard::to_stdvec(row).map_or(usize::MAX, |bytes| bytes.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips_through_its_frame() {
        let value = Reply::Rows(vec![Row {
            rowid: Some(7),
            values: vec![
                Value::Null,
                Value::Integer(-1),
                Value::Real(1.5_f64.to_bits()),
                Value::Text(b"a\xff".to_vec()),
                Value::Blob(vec![0, 1]),
            ],
        }]);
        let frame = encode(&value).unwrap();
        let body = frame.get(FRAME_HEADER_BYTES..).unwrap();
        assert_eq!(decode::<Reply>(body).unwrap(), value);
        let mut trailing = body.to_vec();
        trailing.push(0);
        assert_eq!(decode::<Reply>(&trailing), Err(BulkloadRefusal::FrameCodec));
    }

    #[test]
    fn the_schema_id_is_pinned() {
        assert_eq!(rows_id(), *blake3::hash(ROWS_SCHEMA.as_bytes()).as_bytes());
        assert_ne!(rows_id(), crate::frame::wire_id());
    }
}
