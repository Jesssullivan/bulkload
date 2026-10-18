//! The re-seat ledger (R0, TIN-6587): one `SQLite` store in the run's
//! private state, `STATE/reseat.sqlite`, bulkload's own (WAL,
//! `synchronous=FULL`, `io::durable::configure_sqlite`).
//!
//! - `placement`: one row per destination file a pass placed, adopted or
//!   kept, keyed by its destination path, naming the one landed file it
//!   belongs to. It holds the landed file's identity at the last pass (so
//!   an unchanged landed file is never read again, S3), the destination
//!   file's identity after bulkload's last write (so a destination resume
//!   is seen without reading it), the bytes placed and the BLAKE3 of all of
//!   them (so a grown landed file is proven to extend them, byte for byte,
//!   before its tail is appended), and an intent while an extension or a
//!   supersede is in flight (a supersede's intent holds the staged file's
//!   whole identity).
//! - `session`: one row per session or thread: its recorded working
//!   directory, its lane, its destination key and its plan slug.
//! - `thread_row`: one row per Codex thread row this ledger registered or
//!   adopted in a destination index.
//! - `snapshot`, `snapshot_thread`: the landed Codex snapshot's identity
//!   and its threads' identity columns, so a pass over an unchanged
//!   snapshot does not open it (S3).
//! - `pass`: one row per pass.
//! - `meta`: this ledger's temporary-name token (`temp_token`): a staged
//!   file is `.<name>.<token>.bulkload-reseat`, so two ledgers never share
//!   one temporary (OI-1003-Q198 review). A ledger migrated from version 2
//!   keeps the old shared name (an empty token) for what it left in flight.
//! - `cwd_update`: one row per existing destination Codex row whose `cwd`
//!   column this ledger re-spelled to the lane's canonical path
//!   (OI-1003-Q198 (1)), with the old value, so `--undo` puts it back.
//! - `regularized`: one row per symlinked destination transcript this
//!   ledger replaced with a regular-file copy of its target
//!   (OI-1003-Q198 (2)), with the link text and both identities, so
//!   `--undo` puts the symlink back; with kind `replace`, a
//!   `--replace-archived-dest` replacement by the landed file (A-106).
//! - `rebind_history`: one row per `--rebind-landing` rebind (append-only;
//!   a chained one adds a row): the old and new landed file and identity,
//!   and the placement row's bytes, destination identity and digest at the
//!   rebind, so `--undo` puts it back only over an unchanged row.
//! - `rebind_unproved`: per destination, the new landed file a rebind's
//!   byte proof refused and its identity then, so a pass does not read it
//!   again while it is unchanged (S3).
//!
//! A row is committed only after the bytes it describes are durable at the
//! destination (the engine rule).

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use crate::{BulkloadRefusal, Result};

/// The ledger file inside STATE.
pub const FILE: &str = "reseat.sqlite";
/// The ledger's schema version (`PRAGMA user_version`). Version 1 (the
/// first build, `550396c`: window digests, inode-only supersede intents)
/// is refused, never read as this one; version 2 (`8a89f9a`) is migrated
/// in place (new tables and session columns only).
const VERSION: i64 = 5;
/// Version 2's tables are a subset of version 3's.
const MIGRATE_FROM: i64 = 2;
/// Version 3 lacks only `regularized.kind` (A-106): migrated in place. A
/// build of version 3 refuses a version 4 ledger, so it never reads a
/// replacement as a regularization (review B11).
const MIGRATE_FROM_V3: i64 = 3;
/// Version 4 lacks only the rebind tables (A-106 follow-up): migrated in
/// place. A build of version 4 refuses a version 5 ledger, so it never
/// ignores a rebind, on a pass or on `--undo` (review 8).
const MIGRATE_FROM_V4: i64 = 4;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS placement(
    dest BLOB PRIMARY KEY,
    landing BLOB NOT NULL,
    kind TEXT NOT NULL,
    session TEXT NOT NULL,
    state TEXT NOT NULL,
    l_dev INTEGER NOT NULL, l_ino INTEGER NOT NULL,
    l_size INTEGER NOT NULL, l_mtime INTEGER NOT NULL,
    d_dev INTEGER NOT NULL, d_ino INTEGER NOT NULL,
    d_size INTEGER NOT NULL, d_mtime INTEGER NOT NULL,
    placed INTEGER NOT NULL,
    digest BLOB,
    intent TEXT, i_dev INTEGER, i_ino INTEGER, i_size INTEGER, i_mtime INTEGER,
    pass INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session(
    tool TEXT NOT NULL, id TEXT NOT NULL,
    landing BLOB NOT NULL,
    source_cwd TEXT, dest_cwd BLOB, dest_key TEXT, slug TEXT,
    outcome TEXT NOT NULL, pass INTEGER NOT NULL,
    PRIMARY KEY(tool, id)
);
CREATE TABLE IF NOT EXISTS thread_row(
    id TEXT PRIMARY KEY, db BLOB NOT NULL, cwd TEXT NOT NULL,
    rollout BLOB NOT NULL, state TEXT NOT NULL, pass INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshot(
    path BLOB PRIMARY KEY,
    dev INTEGER NOT NULL, ino INTEGER NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshot_thread(
    ord INTEGER NOT NULL, id TEXT PRIMARY KEY, rollout TEXT NOT NULL, cwd TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pass(
    pass INTEGER PRIMARY KEY, started_ns INTEGER NOT NULL, mode TEXT NOT NULL
);
";
/// Version 3's additions, applied to a new ledger and to a version 2 one.
const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS cwd_update(
    id TEXT PRIMARY KEY, db BLOB NOT NULL, old_cwd TEXT NOT NULL, new_cwd TEXT NOT NULL,
    state TEXT NOT NULL, pass INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS regularized(
    link BLOB PRIMARY KEY, target BLOB NOT NULL, resolved BLOB NOT NULL,
    k_dev INTEGER NOT NULL, k_ino INTEGER NOT NULL, k_size INTEGER NOT NULL, k_mtime INTEGER NOT NULL,
    t_dev INTEGER NOT NULL, t_ino INTEGER NOT NULL, t_size INTEGER NOT NULL, t_mtime INTEGER NOT NULL,
    r_dev INTEGER NOT NULL, r_ino INTEGER NOT NULL, r_size INTEGER NOT NULL, r_mtime INTEGER NOT NULL,
    state TEXT NOT NULL, pass INTEGER NOT NULL,
    digest BLOB
);
CREATE TABLE IF NOT EXISTS rebind_history(
    seq INTEGER PRIMARY KEY AUTOINCREMENT, dest BLOB NOT NULL,
    old_landing BLOB NOT NULL, new_landing BLOB NOT NULL,
    o_dev INTEGER NOT NULL, o_ino INTEGER NOT NULL, o_size INTEGER NOT NULL, o_mtime INTEGER NOT NULL,
    n_dev INTEGER NOT NULL, n_ino INTEGER NOT NULL, n_size INTEGER NOT NULL, n_mtime INTEGER NOT NULL,
    placed INTEGER NOT NULL,
    d_dev INTEGER NOT NULL, d_ino INTEGER NOT NULL, d_size INTEGER NOT NULL, d_mtime INTEGER NOT NULL,
    digest BLOB,
    state TEXT NOT NULL, pass INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rebind_unproved(
    dest BLOB PRIMARY KEY, new_landing BLOB NOT NULL,
    n_dev INTEGER NOT NULL, n_ino INTEGER NOT NULL, n_size INTEGER NOT NULL, n_mtime INTEGER NOT NULL,
    pass INTEGER NOT NULL
);
";
/// A file's identity: what a pass compares instead of reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// Modification time, nanoseconds since the epoch.
    pub mtime: i64,
}

impl Identity {
    /// The identity in `meta`.
    #[must_use]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime: meta
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.mtime_nsec()),
        }
    }
}

/// What a placement row says the destination file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Written by this ledger (placed, extended or superseded).
    Ours,
    /// Found identical to the landed file and adopted.
    Adopted,
    /// A different file the destination holds: kept, reported.
    Diverged,
    /// Changed at the destination after bulkload's last write: the
    /// destination owns it.
    Continued,
    /// Not a regular file, or under a symlink: never written.
    Occupied,
    /// The landed file stopped extending the bytes placed from it.
    Rewritten,
    /// The landed file cannot be read as its form (a truncated `.zst`):
    /// nothing placed, and not read again while its identity holds.
    Unreadable,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Adopted => "adopted",
            Self::Diverged => "diverged",
            Self::Continued => "continued",
            Self::Occupied => "occupied",
            Self::Rewritten => "rewritten",
            Self::Unreadable => "unreadable",
        }
    }

    fn parse(word: &str) -> Result<Self> {
        Ok(match word {
            "ours" => Self::Ours,
            "adopted" => Self::Adopted,
            "diverged" => Self::Diverged,
            "continued" => Self::Continued,
            "occupied" => Self::Occupied,
            "rewritten" => Self::Rewritten,
            "unreadable" => Self::Unreadable,
            _ => return Err(BulkloadRefusal::SchemaMismatch),
        })
    }

    /// Whether the destination file holds this ledger's bytes, so a later
    /// pass may extend or supersede it.
    #[must_use]
    pub const fn owned(self) -> bool {
        matches!(self, Self::Ours | Self::Adopted)
    }
}

/// An extension or supersede in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Appending landed bytes, whole lines, to the destination file after
    /// `placed`.
    Extend,
    /// Exchanging in a staged file with this identity (device, inode, size
    /// and modification time: a staged file edited in place after the
    /// exchange is not the staged file any more).
    Supersede(Identity),
}

/// One placement row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub dest: PathBuf,
    pub landing: PathBuf,
    pub kind: &'static str,
    pub session: String,
    pub state: State,
    pub landed: Identity,
    pub placed_identity: Identity,
    /// Landed bytes the destination file holds (for an owned row).
    pub placed: u64,
    /// BLAKE3 of those `placed` bytes (for an owned row).
    pub digest: Option<[u8; 32]>,
    pub intent: Option<Intent>,
}

/// One session row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub tool: String,
    pub id: String,
    pub landing: PathBuf,
    /// The recorded working directory; `Some("")` when the landed file
    /// records none (cached so a rerun does not scan it again, S3).
    pub source_cwd: Option<String>,
    pub dest_cwd: Option<PathBuf>,
    pub dest_key: Option<String>,
    pub slug: Option<String>,
    pub outcome: String,
    /// The landed file's size and modification time when the row was
    /// written: a session that recorded no `cwd` is scanned again only
    /// when its landed file changed.
    pub landed_size: Option<u64>,
    pub landed_mtime: Option<i64>,
}

/// One rebind of a placement to the same landed file under a new root.
///
/// `--rebind-landing` (A-106 follow-up): the bytes it placed proved the
/// new landing's. Rows are history, one per rebind (a chained one adds a
/// row), never overwritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebind {
    /// Its order in the history (0 before it is written).
    pub seq: i64,
    pub dest: PathBuf,
    pub old_landing: PathBuf,
    pub new_landing: PathBuf,
    /// The placement row's landed identity before and after the rebind.
    pub old_landed: Identity,
    pub new_landed: Identity,
    /// The placement row's bytes, destination identity and digest at the
    /// rebind: `--undo` puts the old binding back only while the row still
    /// holds exactly these (else `undo-kept`).
    pub placed: u64,
    pub placed_identity: Identity,
    pub digest: Option<[u8; 32]>,
    /// `done`, `undone` or `undo-kept`.
    pub state: String,
}

/// One re-spelled destination Codex row (OI-1003-Q198 (1)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdUpdate {
    pub id: String,
    pub db: PathBuf,
    pub old_cwd: String,
    pub new_cwd: String,
    /// `intent` (recorded before the destination's transaction),
    /// `applied`, `undone` or `undo-kept`.
    pub state: String,
}

/// What a regularization row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegularState {
    /// Staged (the regular copy at its temporary name), not yet exchanged
    /// or settled.
    Intent,
    /// The regular copy is at the transcript's path; the symlink removed.
    Done,
    /// The target changed during the exchange and so did the copy: both
    /// kept, the copy at the path.
    Raced,
    /// `--undo` put the symlink back.
    Undone,
}

impl RegularState {
    const fn word(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Done => "done",
            Self::Raced => "raced",
            Self::Undone => "undone",
        }
    }

    fn parse(word: &str) -> Result<Self> {
        Ok(match word {
            "intent" => Self::Intent,
            "done" => Self::Done,
            "raced" => Self::Raced,
            "undone" => Self::Undone,
            _ => return Err(BulkloadRefusal::SchemaMismatch),
        })
    }
}

/// One regularized destination transcript (OI-1003-Q198 (2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regular {
    /// The transcript's path (where the symlink was).
    pub link: PathBuf,
    /// The symlink's text, as `readlink` returned it.
    pub target: PathBuf,
    /// The target's real path when it was copied.
    pub resolved: PathBuf,
    /// The symlink's own identity (`lstat`).
    pub link_identity: Identity,
    /// The target's identity when it was copied.
    pub target_identity: Identity,
    /// The regular copy's identity (staged, then at the path).
    pub regular: Identity,
    /// BLAKE3 of the bytes copied (the target's then, the copy's as made);
    /// `--undo` puts the link back only over a target that holds them.
    /// `None` on a row written before it was recorded: never undone.
    pub digest: Option<[u8; 32]>,
    pub state: RegularState,
    /// A `--replace-archived-dest` replacement (A-106): the regular file
    /// at the path is the landed file, not a copy of the target; its
    /// placement row is written once the row is `done` (a crash before
    /// that leaves none, and the next pass adopts the file at the path).
    pub replace: bool,
}

/// The `regularized.kind` of a `--replace-archived-dest` row (NULL for a
/// regularization).
const REPLACE_KIND: &str = "replace";

/// The open ledger.
pub struct Ledger {
    conn: Connection,
    pass: i64,
    read_only: bool,
    /// Whether the version 3 tables are there (a version 2 ledger opened
    /// read-only has none).
    v3: bool,
    /// The temporary-name token (empty: the shared name of version 2).
    token: String,
}

fn sql<T>(outcome: rusqlite::Result<T>) -> Result<T> {
    outcome.map_err(|_| BulkloadRefusal::SqliteStateChanged)
}

fn blob(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_of(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

fn int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn uint(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn digest(value: Option<Vec<u8>>) -> Option<[u8; 32]> {
    value.and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
}

impl Ledger {
    /// Open (creating) the ledger in `state` for a new pass.
    ///
    /// # Errors
    /// The I/O refusal of creating `state`; `SQLITE_STATE_CHANGED` when the
    /// store cannot be opened or configured; `SCHEMA_MISMATCH` for a ledger
    /// of another version (the first build's is not read as this one).
    pub fn open(state: &Path, now_ns: i64, mode: &str) -> Result<Self> {
        use crate::refuse::RefuseAt as _;
        use std::os::unix::fs::DirBuilderExt as _;
        match std::fs::DirBuilder::new().mode(0o700).create(state) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).refuse_at("reseat::ledger::open"),
        }
        let conn = sql(Connection::open(state.join(FILE)))?;
        crate::io::durable::configure_sqlite(&conn)?;
        sql(conn.busy_timeout(std::time::Duration::from_secs(30)))?;
        let version: i64 = sql(conn.query_row("PRAGMA user_version", [], |row| row.get(0)))?;
        let tables: i64 = sql(conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        ))?;
        let fresh = version == 0 && tables == 0;
        if !fresh
            && version != VERSION
            && version != MIGRATE_FROM
            && version != MIGRATE_FROM_V3
            && version != MIGRATE_FROM_V4
        {
            return Err(BulkloadRefusal::SchemaMismatch);
        }
        sql(conn.execute_batch(SCHEMA))?;
        sql(conn.execute_batch(SCHEMA_V3))?;
        for (table, column, kind) in [
            ("session", "l_size", "INTEGER"),
            ("session", "l_mtime", "INTEGER"),
            ("regularized", "digest", "BLOB"),
            ("regularized", "kind", "TEXT"),
        ] {
            let present: i64 = sql(conn.query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |row| row.get(0),
            ))?;
            if present == 0 {
                sql(conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}")))?;
            }
        }
        let token: Option<String> = sql(conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'temp_token'",
                [],
                |row| row.get(0),
            )
            .optional())?;
        let token = if let Some(token) = token {
            token
        } else {
            // A migrated version 2 ledger keeps the shared name for what it
            // may have left in flight.
            let token = if version == MIGRATE_FROM {
                String::new()
            } else {
                new_token(state, now_ns)
            };
            sql(conn.execute(
                "INSERT INTO meta(key, value) VALUES ('temp_token', ?1)",
                params![token],
            ))?;
            token
        };
        sql(conn.execute_batch(&format!("PRAGMA user_version = {VERSION}")))?;
        sql(conn.execute(
            "INSERT INTO pass(started_ns, mode) VALUES (?1, ?2)",
            params![now_ns, mode],
        ))?;
        let pass = conn.last_insert_rowid();
        Ok(Self {
            conn,
            pass,
            read_only: false,
            v3: true,
            token,
        })
    }

    /// Open an existing ledger read-only (`--check`, and the checks a run
    /// makes before it writes anything), or `None` when there is none. A
    /// version 2 ledger is read as it is (no version 3 rows yet).
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` when it exists and cannot be read;
    /// `SCHEMA_MISMATCH` for a ledger of another version.
    pub fn open_read_only(state: &Path) -> Result<Option<Self>> {
        let path = state.join(FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let conn = sql(Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        let version: i64 = sql(conn.query_row("PRAGMA user_version", [], |row| row.get(0)))?;
        if version != VERSION
            && version != MIGRATE_FROM
            && version != MIGRATE_FROM_V3
            && version != MIGRATE_FROM_V4
        {
            return Err(BulkloadRefusal::SchemaMismatch);
        }
        Ok(Some(Self {
            conn,
            pass: 0,
            read_only: true,
            v3: version >= MIGRATE_FROM_V3,
            token: String::new(),
        }))
    }

    /// The token in this ledger's temporary names.
    #[must_use]
    pub fn temp_token(&self) -> &str {
        &self.token
    }

    /// This pass's number.
    #[must_use]
    pub const fn pass(&self) -> i64 {
        self.pass
    }

    /// The placement row for `dest`.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read; `SCHEMA_MISMATCH` on a row
    /// this build cannot read.
    pub fn placement(&self, dest: &Path) -> Result<Option<Placement>> {
        let row = sql(self
            .conn
            .query_row(
                "SELECT landing, kind, session, state, l_dev, l_ino, l_size, l_mtime,
                        d_dev, d_ino, d_size, d_mtime, placed, digest,
                        intent, i_dev, i_ino, i_size, i_mtime
                 FROM placement WHERE dest = ?1",
                params![blob(dest)],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        [
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, i64>(9)?,
                            row.get::<_, i64>(10)?,
                            row.get::<_, i64>(11)?,
                            row.get::<_, i64>(12)?,
                        ],
                        row.get::<_, Option<Vec<u8>>>(13)?,
                        (
                            row.get::<_, Option<String>>(14)?,
                            [
                                row.get::<_, Option<i64>>(15)?,
                                row.get::<_, Option<i64>>(16)?,
                                row.get::<_, Option<i64>>(17)?,
                                row.get::<_, Option<i64>>(18)?,
                            ],
                        ),
                    ))
                },
            )
            .optional())?;
        let Some((landing, kind, session, state, numbers, stored_digest, intent)) = row else {
            return Ok(None);
        };
        let [l_dev, l_ino, l_size, l_mtime, d_dev, d_ino, d_size, d_mtime, placed] = numbers;
        let intent = match intent {
            (Some(word), _) if word == "extend" => Some(Intent::Extend),
            (Some(word), [Some(dev), Some(ino), Some(size), Some(mtime)])
                if word == "supersede" =>
            {
                Some(Intent::Supersede(Identity {
                    dev: uint(dev),
                    ino: uint(ino),
                    size: uint(size),
                    mtime,
                }))
            }
            (None, _) => None,
            _ => return Err(BulkloadRefusal::SchemaMismatch),
        };
        Ok(Some(Placement {
            dest: dest.to_path_buf(),
            landing: path_of(landing),
            kind: super::Kind::parse(&kind)?.word(),
            session,
            state: State::parse(&state)?,
            landed: Identity {
                dev: uint(l_dev),
                ino: uint(l_ino),
                size: uint(l_size),
                mtime: l_mtime,
            },
            placed_identity: Identity {
                dev: uint(d_dev),
                ino: uint(d_ino),
                size: uint(d_size),
                mtime: d_mtime,
            },
            placed: uint(placed),
            digest: digest(stored_digest),
            intent,
        }))
    }

    /// Every destination path with a placement row.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn placement_dests(&self) -> Result<Vec<PathBuf>> {
        let mut statement = sql(self
            .conn
            .prepare("SELECT dest FROM placement ORDER BY dest"))?;
        let rows = sql(statement.query_map([], |row| row.get::<_, Vec<u8>>(0)))?;
        Ok(sql(rows.collect::<rusqlite::Result<Vec<_>>>())?
            .into_iter()
            .map(path_of)
            .collect())
    }

    fn has_table(&self, name: &str) -> Result<bool> {
        sql(self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        ))
        .map(|count| count > 0)
    }

    /// The rebind history of `dest`, oldest first.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn rebinds_of(&self, dest: &Path) -> Result<Vec<Rebind>> {
        Ok(self
            .rebinds()?
            .into_iter()
            .filter(|row| row.dest == dest)
            .collect())
    }

    /// Every rebind, oldest first.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn rebinds(&self) -> Result<Vec<Rebind>> {
        if !self.has_table("rebind_history")? {
            return Ok(Vec::new());
        }
        let mut statement = sql(self.conn.prepare(
            "SELECT seq, dest, old_landing, new_landing, o_dev, o_ino, o_size, o_mtime,
                    n_dev, n_ino, n_size, n_mtime, placed, d_dev, d_ino, d_size, d_mtime,
                    digest, state
             FROM rebind_history ORDER BY seq",
        ))?;
        let identity = |row: &rusqlite::Row<'_>, at: usize| -> rusqlite::Result<Identity> {
            Ok(Identity {
                dev: uint(row.get(at)?),
                ino: uint(row.get(at + 1)?),
                size: uint(row.get(at + 2)?),
                mtime: row.get(at + 3)?,
            })
        };
        let rows = sql(statement.query_map([], |row| {
            Ok(Rebind {
                seq: row.get(0)?,
                dest: path_of(row.get(1)?),
                old_landing: path_of(row.get(2)?),
                new_landing: path_of(row.get(3)?),
                old_landed: identity(row, 4)?,
                new_landed: identity(row, 8)?,
                placed: uint(row.get(12)?),
                placed_identity: identity(row, 13)?,
                digest: digest(row.get(17)?),
                state: row.get(18)?,
            })
        }))?;
        sql(rows.collect::<rusqlite::Result<Vec<_>>>())
    }

    /// The refused rebind of `dest` remembered: the new landed file and its
    /// identity when the proof refused it.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn rebind_unproved(&self, dest: &Path) -> Result<Option<(PathBuf, Identity)>> {
        if !self.has_table("rebind_unproved")? {
            return Ok(None);
        }
        sql(self
            .conn
            .query_row(
                "SELECT new_landing, n_dev, n_ino, n_size, n_mtime FROM rebind_unproved
                 WHERE dest = ?1",
                params![blob(dest)],
                |row| {
                    Ok((
                        path_of(row.get(0)?),
                        Identity {
                            dev: uint(row.get(1)?),
                            ino: uint(row.get(2)?),
                            size: uint(row.get(3)?),
                            mtime: row.get(4)?,
                        },
                    ))
                },
            )
            .optional())
    }

    /// Re-bind the placement row of `rebind.dest` from `old_landing` to
    /// `new_landing` (its landed identity set to `new_landed`) and append
    /// the rebind to the history, in one commit; only while the row still
    /// names the old landed file. A refused-proof memo of the path is
    /// cleared. Returns whether it did.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit; the refusal of a
    /// read-only ledger.
    pub fn put_rebind(&self, rebind: &Rebind) -> Result<bool> {
        if self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute_batch("BEGIN IMMEDIATE"))?;
        let outcome = (|| -> rusqlite::Result<bool> {
            let changed = self.conn.execute(
                "UPDATE placement SET landing = ?2, l_dev = ?3, l_ino = ?4, l_size = ?5,
                    l_mtime = ?6, pass = ?7
                 WHERE dest = ?1 AND landing = ?8",
                params![
                    blob(&rebind.dest),
                    blob(&rebind.new_landing),
                    int(rebind.new_landed.dev),
                    int(rebind.new_landed.ino),
                    int(rebind.new_landed.size),
                    rebind.new_landed.mtime,
                    self.pass,
                    blob(&rebind.old_landing),
                ],
            )?;
            if changed != 1 {
                return Ok(false);
            }
            self.conn.execute(
                "INSERT INTO rebind_history(dest, old_landing, new_landing,
                    o_dev, o_ino, o_size, o_mtime, n_dev, n_ino, n_size, n_mtime,
                    placed, d_dev, d_ino, d_size, d_mtime, digest, state, pass)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, 'done', ?18)",
                params![
                    blob(&rebind.dest),
                    blob(&rebind.old_landing),
                    blob(&rebind.new_landing),
                    int(rebind.old_landed.dev),
                    int(rebind.old_landed.ino),
                    int(rebind.old_landed.size),
                    rebind.old_landed.mtime,
                    int(rebind.new_landed.dev),
                    int(rebind.new_landed.ino),
                    int(rebind.new_landed.size),
                    rebind.new_landed.mtime,
                    int(rebind.placed),
                    int(rebind.placed_identity.dev),
                    int(rebind.placed_identity.ino),
                    int(rebind.placed_identity.size),
                    rebind.placed_identity.mtime,
                    rebind.digest.as_ref().map(<[u8; 32]>::as_slice),
                    self.pass,
                ],
            )?;
            self.conn.execute(
                "DELETE FROM rebind_unproved WHERE dest = ?1",
                params![blob(&rebind.dest)],
            )?;
            Ok(true)
        })();
        if let Ok(done) = outcome {
            sql(self
                .conn
                .execute_batch(if done { "COMMIT" } else { "ROLLBACK" }))?;
            Ok(done)
        } else {
            let _ = self.conn.execute_batch("ROLLBACK");
            Err(BulkloadRefusal::SqliteStateChanged)
        }
    }

    /// Remember a rebind the byte proof refused, by the new landed file's
    /// identity, so a pass does not read it again while it is unchanged
    /// (S3). Never touches the history.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put_rebind_unproved(
        &self,
        dest: &Path,
        landing: &Path,
        identity: Identity,
    ) -> Result<()> {
        if self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute(
            "INSERT OR REPLACE INTO rebind_unproved(dest, new_landing,
                n_dev, n_ino, n_size, n_mtime, pass)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                blob(dest),
                blob(landing),
                int(identity.dev),
                int(identity.ino),
                int(identity.size),
                identity.mtime,
                self.pass,
            ],
        ))
        .map(drop)
    }

    /// Put one rebind back (`--undo`, ledger only, newest first): the
    /// placement row names the old landed file and identity again, only
    /// while it still holds exactly what the rebind left (the new landed
    /// file and identity, the bytes, destination identity and digest);
    /// the history row `undone`, else `undo-kept`. Returns whether the row
    /// was put back.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn undo_rebind(&self, rebind: &Rebind) -> Result<bool> {
        if self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute_batch("BEGIN IMMEDIATE"))?;
        let outcome = (|| -> rusqlite::Result<bool> {
            let changed = self.conn.execute(
                "UPDATE placement SET landing = ?2, l_dev = ?3, l_ino = ?4, l_size = ?5,
                    l_mtime = ?6
                 WHERE dest = ?1 AND landing = ?7
                   AND l_dev = ?8 AND l_ino = ?9 AND l_size = ?10 AND l_mtime = ?11
                   AND placed = ?12 AND d_dev = ?13 AND d_ino = ?14 AND d_size = ?15
                   AND d_mtime = ?16 AND digest IS ?17",
                params![
                    blob(&rebind.dest),
                    blob(&rebind.old_landing),
                    int(rebind.old_landed.dev),
                    int(rebind.old_landed.ino),
                    int(rebind.old_landed.size),
                    rebind.old_landed.mtime,
                    blob(&rebind.new_landing),
                    int(rebind.new_landed.dev),
                    int(rebind.new_landed.ino),
                    int(rebind.new_landed.size),
                    rebind.new_landed.mtime,
                    int(rebind.placed),
                    int(rebind.placed_identity.dev),
                    int(rebind.placed_identity.ino),
                    int(rebind.placed_identity.size),
                    rebind.placed_identity.mtime,
                    rebind.digest.as_ref().map(<[u8; 32]>::as_slice),
                ],
            )?;
            self.conn.execute(
                "UPDATE rebind_history SET state = ?2, pass = ?3 WHERE seq = ?1",
                params![
                    rebind.seq,
                    if changed == 1 { "undone" } else { "undo-kept" },
                    self.pass
                ],
            )?;
            Ok(changed == 1)
        })();
        if let Ok(done) = outcome {
            sql(self.conn.execute_batch("COMMIT"))?;
            Ok(done)
        } else {
            let _ = self.conn.execute_batch("ROLLBACK");
            Err(BulkloadRefusal::SqliteStateChanged)
        }
    }

    /// Write `row` (insert or replace), committed before this returns.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put(&self, row: &Placement) -> Result<()> {
        let (intent, staged) = match row.intent {
            None => (None, None),
            Some(Intent::Extend) => (Some("extend"), None),
            Some(Intent::Supersede(staged)) => (Some("supersede"), Some(staged)),
        };
        let started = std::time::Instant::now();
        let outcome = self.conn.execute(
            "INSERT OR REPLACE INTO placement(dest, landing, kind, session, state,
                l_dev, l_ino, l_size, l_mtime, d_dev, d_ino, d_size, d_mtime, placed,
                digest, intent, i_dev, i_ino, i_size, i_mtime, pass)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                blob(&row.dest),
                blob(&row.landing),
                row.kind,
                row.session,
                row.state.word(),
                int(row.landed.dev),
                int(row.landed.ino),
                int(row.landed.size),
                row.landed.mtime,
                int(row.placed_identity.dev),
                int(row.placed_identity.ino),
                int(row.placed_identity.size),
                row.placed_identity.mtime,
                int(row.placed),
                row.digest.as_ref().map(<[u8; 32]>::as_slice),
                intent,
                staged.map(|staged| int(staged.dev)),
                staged.map(|staged| int(staged.ino)),
                staged.map(|staged| int(staged.size)),
                staged.map(|staged| staged.mtime),
                self.pass,
            ],
        );
        crate::counters::sqlite_commit(
            crate::counters::Counter::SqliteGroupDest,
            started,
            &outcome,
        );
        sql(outcome).map(drop)
    }

    /// The session row of `id` under `tool`.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn session(&self, tool: &str, id: &str) -> Result<Option<Session>> {
        sql(self
            .conn
            .query_row(
                if self.v3 {
                    "SELECT landing, source_cwd, dest_cwd, dest_key, slug, outcome, l_size, l_mtime
                     FROM session WHERE tool = ?1 AND id = ?2"
                } else {
                    "SELECT landing, source_cwd, dest_cwd, dest_key, slug, outcome, NULL, NULL
                     FROM session WHERE tool = ?1 AND id = ?2"
                },
                params![tool, id],
                |row| {
                    Ok(Session {
                        tool: tool.to_owned(),
                        id: id.to_owned(),
                        landing: path_of(row.get(0)?),
                        source_cwd: row.get(1)?,
                        dest_cwd: row.get::<_, Option<Vec<u8>>>(2)?.map(path_of),
                        dest_key: row.get(3)?,
                        slug: row.get(4)?,
                        outcome: row.get(5)?,
                        landed_size: row.get::<_, Option<i64>>(6)?.map(uint),
                        landed_mtime: row.get(7)?,
                    })
                },
            )
            .optional())
    }

    /// Write a session row.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit; the refusal of a
    /// read-only ledger.
    pub fn put_session(&self, row: &Session) -> Result<()> {
        if self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute(
            "INSERT OR REPLACE INTO session(tool, id, landing, source_cwd, dest_cwd,
                dest_key, slug, outcome, pass, l_size, l_mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.tool,
                row.id,
                blob(&row.landing),
                row.source_cwd,
                row.dest_cwd.as_deref().map(blob),
                row.dest_key,
                row.slug,
                row.outcome,
                self.pass,
                row.landed_size.map(int),
                row.landed_mtime,
            ],
        ))
        .map(drop)
    }

    /// The registered row of thread `id`: (db, cwd, rollout).
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn thread_row(&self, id: &str) -> Result<Option<(PathBuf, String, PathBuf)>> {
        sql(self
            .conn
            .query_row(
                "SELECT db, cwd, rollout FROM thread_row WHERE id = ?1",
                params![id],
                |row| Ok((path_of(row.get(0)?), row.get(1)?, path_of(row.get(2)?))),
            )
            .optional())
    }

    /// Record thread rows registered (or adopted) in `db`, in one commit.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put_thread_rows(
        &mut self,
        db: &Path,
        rows: &[(String, String, PathBuf, &str)],
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let outcome = (|| {
            let transaction = self.conn.transaction()?;
            for (id, cwd, rollout, state) in rows {
                transaction.execute(
                    "INSERT OR REPLACE INTO thread_row(id, db, cwd, rollout, state, pass)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, blob(db), cwd, blob(rollout), state, self.pass],
                )?;
            }
            transaction.commit()
        })();
        crate::counters::sqlite_commit(
            crate::counters::Counter::SqliteGroupDest,
            started,
            &outcome,
        );
        sql(outcome)
    }

    /// The threads cached for the landed snapshot at `path` when its
    /// identity is still `identity`: `(id, rollout, cwd)`, oldest first.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn snapshot_threads(
        &self,
        path: &Path,
        identity: Identity,
    ) -> Result<Option<Vec<(String, String, String)>>> {
        let cached: Option<[i64; 4]> = sql(self
            .conn
            .query_row(
                "SELECT dev, ino, size, mtime FROM snapshot WHERE path = ?1",
                params![blob(path)],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
            )
            .optional())?;
        let Some([dev, ino, size, mtime]) = cached else {
            return Ok(None);
        };
        if (uint(dev), uint(ino), uint(size), mtime)
            != (identity.dev, identity.ino, identity.size, identity.mtime)
        {
            return Ok(None);
        }
        let mut statement = sql(self
            .conn
            .prepare("SELECT id, rollout, cwd FROM snapshot_thread ORDER BY ord"))?;
        let rows = sql(statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))))?;
        sql(rows.collect::<rusqlite::Result<Vec<_>>>()).map(Some)
    }

    /// Replace the cached threads of the landed snapshot (one snapshot per
    /// ledger), in one commit.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put_snapshot_threads(
        &mut self,
        path: &Path,
        identity: Identity,
        threads: &[(String, String, String)],
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let outcome = (|| {
            let transaction = self.conn.transaction()?;
            transaction.execute("DELETE FROM snapshot", [])?;
            transaction.execute("DELETE FROM snapshot_thread", [])?;
            for (ord, (id, rollout, cwd)) in threads.iter().enumerate() {
                transaction.execute(
                    "INSERT OR REPLACE INTO snapshot_thread(ord, id, rollout, cwd)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![i64::try_from(ord).unwrap_or(i64::MAX), id, rollout, cwd],
                )?;
            }
            transaction.execute(
                "INSERT INTO snapshot(path, dev, ino, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    blob(path),
                    int(identity.dev),
                    int(identity.ino),
                    int(identity.size),
                    identity.mtime
                ],
            )?;
            transaction.commit()
        })();
        crate::counters::sqlite_commit(
            crate::counters::Counter::SqliteGroupDest,
            started,
            &outcome,
        );
        sql(outcome)
    }
    /// Record re-spellings about to be made in `db` (state `intent`), in
    /// one commit, before the destination's transaction: a crash after it
    /// leaves rows `--undo` can put back.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put_cwd_intents(&mut self, db: &Path, rows: &[(String, String, String)]) -> Result<()> {
        if !self.v3 || self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        let pass = self.pass;
        let outcome = (|| {
            let transaction = self.conn.transaction()?;
            for (id, old, new) in rows {
                // The old value is the one the row holds now: a row this
                // ledger re-spelled before and the destination spelled
                // again since is put back to the destination's latest
                // spelling, never to an older one that may name another
                // directory (M2 round 2 review). It is in flight again
                // (`intent`), so a crash before it is marked stays
                // undoable.
                transaction.execute(
                    "INSERT INTO cwd_update(id, db, old_cwd, new_cwd, state, pass)
                     VALUES (?1, ?2, ?3, ?4, 'intent', ?5)
                     ON CONFLICT(id) DO UPDATE SET
                        old_cwd = excluded.old_cwd,
                        db = excluded.db, new_cwd = excluded.new_cwd,
                        state = 'intent', pass = excluded.pass",
                    params![id, blob(db), old, new, pass],
                )?;
            }
            transaction.commit()
        })();
        sql(outcome)
    }

    /// Set the state of re-spelling rows, in one commit.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn mark_cwd(&mut self, ids: &[(String, &str)]) -> Result<()> {
        if !self.v3 || self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        let pass = self.pass;
        let outcome = (|| {
            let transaction = self.conn.transaction()?;
            for (id, state) in ids {
                transaction.execute(
                    "UPDATE cwd_update SET state = ?2, pass = ?3 WHERE id = ?1",
                    params![id, state, pass],
                )?;
            }
            transaction.commit()
        })();
        sql(outcome)
    }

    /// The re-spelling row of thread `id`.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn cwd_update(&self, id: &str) -> Result<Option<CwdUpdate>> {
        if !self.v3 {
            return Ok(None);
        }
        sql(self
            .conn
            .query_row(
                "SELECT db, old_cwd, new_cwd, state FROM cwd_update WHERE id = ?1",
                params![id],
                |row| {
                    Ok(CwdUpdate {
                        id: id.to_owned(),
                        db: path_of(row.get(0)?),
                        old_cwd: row.get(1)?,
                        new_cwd: row.get(2)?,
                        state: row.get(3)?,
                    })
                },
            )
            .optional())
    }

    /// Every re-spelling row, by thread ID.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read.
    pub fn cwd_updates(&self) -> Result<Vec<CwdUpdate>> {
        if !self.v3 {
            return Ok(Vec::new());
        }
        let mut statement = sql(self
            .conn
            .prepare("SELECT id, db, old_cwd, new_cwd, state FROM cwd_update ORDER BY id"))?;
        let rows = sql(statement.query_map([], |row| {
            Ok(CwdUpdate {
                id: row.get(0)?,
                db: path_of(row.get(1)?),
                old_cwd: row.get(2)?,
                new_cwd: row.get(3)?,
                state: row.get(4)?,
            })
        }))?;
        sql(rows.collect::<rusqlite::Result<Vec<_>>>())
    }

    /// Write a regularization row (insert or replace), committed before
    /// this returns.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put_regular(&self, row: &Regular) -> Result<()> {
        if !self.v3 || self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        let identity = |identity: Identity| {
            [
                int(identity.dev),
                int(identity.ino),
                int(identity.size),
                identity.mtime,
            ]
        };
        let [k_dev, k_ino, k_size, k_mtime] = identity(row.link_identity);
        let [t_dev, t_ino, t_size, t_mtime] = identity(row.target_identity);
        let [r_dev, r_ino, r_size, r_mtime] = identity(row.regular);
        sql(self.conn.execute(
            "INSERT OR REPLACE INTO regularized(link, target, resolved,
                k_dev, k_ino, k_size, k_mtime, t_dev, t_ino, t_size, t_mtime,
                r_dev, r_ino, r_size, r_mtime, state, pass, digest, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18, ?19)",
            params![
                blob(&row.link),
                blob(&row.target),
                blob(&row.resolved),
                k_dev,
                k_ino,
                k_size,
                k_mtime,
                t_dev,
                t_ino,
                t_size,
                t_mtime,
                r_dev,
                r_ino,
                r_size,
                r_mtime,
                row.state.word(),
                self.pass,
                row.digest.as_ref().map(<[u8; 32]>::as_slice),
                row.replace.then_some(REPLACE_KIND),
            ],
        ))
        .map(drop)
    }

    /// Mark a replacement `done` and write the placement row that names
    /// the landed file at its path, in one commit (review B1): the row of
    /// another landing that kept the link (`occupied`) is replaced by it.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit; the refusal of a
    /// read-only or version 2 ledger.
    pub fn put_replace_done(&self, regular: &Regular, placement: &Placement) -> Result<()> {
        if !self.v3 || self.read_only || !regular.replace {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute_batch("BEGIN IMMEDIATE"))?;
        let outcome = self.put_regular(regular).and_then(|()| self.put(placement));
        match outcome {
            Ok(()) => sql(self.conn.execute_batch("COMMIT")),
            Err(refusal) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(refusal)
            }
        }
    }

    /// Mark a replacement `done` that a crash left in flight, dropping in
    /// the same commit any placement row of its path that is not this
    /// ledger's own (`ours`, `adopted`, `continued`): the path now holds
    /// the landed file, decided afresh by the pass (review B1).
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn settle_replace_done(&self, regular: &Regular) -> Result<()> {
        if !self.v3 || self.read_only || !regular.replace {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute_batch("BEGIN IMMEDIATE"))?;
        let outcome = self.put_regular(regular).and_then(|()| {
            sql(self.conn.execute(
                "DELETE FROM placement WHERE dest = ?1
                 AND state NOT IN ('ours', 'adopted', 'continued')",
                params![blob(&regular.link)],
            ))
            .map(drop)
        });
        match outcome {
            Ok(()) => sql(self.conn.execute_batch("COMMIT")),
            Err(refusal) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(refusal)
            }
        }
    }

    /// Delete the placement row of `dest` (a replacement that never took
    /// place, or one `--undo` reversed).
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn delete_placement(&self, dest: &Path) -> Result<()> {
        if self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self
            .conn
            .execute("DELETE FROM placement WHERE dest = ?1", params![blob(dest)]))
        .map(drop)
    }

    fn regular_rows(&self, filter: Option<&Path>) -> Result<Vec<Regular>> {
        if !self.v3 {
            return Ok(Vec::new());
        }
        // A ledger this build has not opened for writing yet has no `kind`
        // column: every row of it is a regularization.
        let kind: i64 = sql(self.conn.query_row(
            "SELECT count(*) FROM pragma_table_info('regularized') WHERE name = 'kind'",
            [],
            |row| row.get(0),
        ))?;
        let mut statement = sql(self.conn.prepare(&format!(
            "SELECT link, target, resolved, k_dev, k_ino, k_size, k_mtime,
                    t_dev, t_ino, t_size, t_mtime, r_dev, r_ino, r_size, r_mtime, state, digest,
                    {}
             FROM regularized WHERE ?1 IS NULL OR link = ?1 ORDER BY link",
            if kind > 0 { "kind" } else { "NULL" }
        )))?;
        let identity = |row: &rusqlite::Row<'_>, at: usize| -> rusqlite::Result<Identity> {
            Ok(Identity {
                dev: uint(row.get(at)?),
                ino: uint(row.get(at + 1)?),
                size: uint(row.get(at + 2)?),
                mtime: row.get(at + 3)?,
            })
        };
        let rows = sql(statement.query_map(params![filter.map(blob)], |row| {
            Ok((
                path_of(row.get(0)?),
                path_of(row.get(1)?),
                path_of(row.get(2)?),
                identity(row, 3)?,
                identity(row, 7)?,
                identity(row, 11)?,
                row.get::<_, String>(15)?,
                row.get::<_, Option<Vec<u8>>>(16)?,
                row.get::<_, Option<String>>(17)?,
            ))
        }))?;
        let mut out = Vec::new();
        for row in rows {
            let (
                link,
                target,
                resolved,
                link_identity,
                target_identity,
                regular,
                state,
                hash,
                kind,
            ) = sql(row)?;
            out.push(Regular {
                link,
                target,
                resolved,
                link_identity,
                target_identity,
                regular,
                digest: digest(hash),
                state: RegularState::parse(&state)?,
                replace: match kind.as_deref() {
                    None => false,
                    Some(REPLACE_KIND) => true,
                    Some(_) => return Err(BulkloadRefusal::SchemaMismatch),
                },
            });
        }
        Ok(out)
    }

    /// The regularization row of the transcript at `link`.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read; `SCHEMA_MISMATCH` on a row
    /// this build cannot read.
    pub fn regular(&self, link: &Path) -> Result<Option<Regular>> {
        Ok(self.regular_rows(Some(link))?.into_iter().next())
    }

    /// Remove the regularization row of `link` (a regularization that put
    /// the symlink back: a later pass decides again).
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn delete_regular(&self, link: &Path) -> Result<()> {
        if !self.v3 || self.read_only {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        sql(self.conn.execute(
            "DELETE FROM regularized WHERE link = ?1",
            params![blob(link)],
        ))
        .map(drop)
    }

    /// Every regularization row.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed read; `SCHEMA_MISMATCH` on a row
    /// this build cannot read.
    pub fn regulars(&self) -> Result<Vec<Regular>> {
        self.regular_rows(None)
    }
}

/// A token for this ledger's temporary names: twelve hex digits of the
/// BLAKE3 of the state path, the time and this process.
fn new_token(state: &Path, now_ns: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(blob(state));
    hasher.update(&now_ns.to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&crate::reseat::now_ns().to_le_bytes());
    let digest = hasher.finalize();
    digest
        .as_bytes()
        .iter()
        .take(6)
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}
