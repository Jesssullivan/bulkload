//! The re-seat ledger (R0, TIN-6587): one `SQLite` store in the run's
//! private state, `STATE/reseat.sqlite`, bulkload's own (WAL,
//! `synchronous=FULL`, `io::durable::configure_sqlite`).
//!
//! - `placement`: one row per destination file a pass placed, adopted or
//!   kept, keyed by its destination path. It holds the landed file's
//!   identity at the last pass (so an unchanged landed file is never read
//!   again, S3), the destination file's identity after bulkload's last
//!   write (so a destination resume is seen without reading it), the bytes
//!   placed and two window digests over them (so a grown transcript is
//!   proven to extend them by reading only the windows, R25), and an
//!   intent while an extension or a supersede is in flight.
//! - `session`: one row per session or thread: its recorded working
//!   directory, its lane, its destination key and its plan slug.
//! - `thread_row`: one row per Codex thread row this ledger registered or
//!   adopted in a destination index.
//! - `pass`: one row per pass.
//!
//! A row is committed only after the bytes it describes are durable at the
//! destination (the engine rule).

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use crate::{BulkloadRefusal, Result};

/// The ledger file inside STATE.
pub const FILE: &str = "reseat.sqlite";

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
    head BLOB, tail BLOB, digest BLOB,
    intent TEXT, intent_size INTEGER, intent_ino INTEGER,
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
CREATE TABLE IF NOT EXISTS pass(
    pass INTEGER PRIMARY KEY, started_ns INTEGER NOT NULL, mode TEXT NOT NULL
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
    /// Appending landed bytes to the destination file from `placed` up to
    /// this size.
    Extend(u64),
    /// Exchanging in a staged file with this inode.
    Supersede(u64),
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
    pub head: Option<[u8; 32]>,
    pub tail: Option<[u8; 32]>,
    pub digest: Option<[u8; 32]>,
    pub intent: Option<Intent>,
}

/// One session row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub tool: String,
    pub id: String,
    pub landing: PathBuf,
    pub source_cwd: Option<String>,
    pub dest_cwd: Option<PathBuf>,
    pub dest_key: Option<String>,
    pub slug: Option<String>,
    pub outcome: String,
}

/// The open ledger.
pub struct Ledger {
    conn: Connection,
    pass: i64,
    read_only: bool,
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
    /// store cannot be opened or configured.
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
        sql(conn.execute_batch(SCHEMA))?;
        sql(conn.execute(
            "INSERT INTO pass(started_ns, mode) VALUES (?1, ?2)",
            params![now_ns, mode],
        ))?;
        let pass = conn.last_insert_rowid();
        Ok(Self {
            conn,
            pass,
            read_only: false,
        })
    }

    /// Open an existing ledger read-only (`--check`), or `None` when there
    /// is none.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` when it exists and cannot be read.
    pub fn open_read_only(state: &Path) -> Result<Option<Self>> {
        let path = state.join(FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let conn = sql(Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        Ok(Some(Self {
            conn,
            pass: 0,
            read_only: true,
        }))
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
                        d_dev, d_ino, d_size, d_mtime, placed, head, tail, digest,
                        intent, intent_size, intent_ino
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
                        (
                            row.get::<_, Option<Vec<u8>>>(13)?,
                            row.get::<_, Option<Vec<u8>>>(14)?,
                            row.get::<_, Option<Vec<u8>>>(15)?,
                        ),
                        (
                            row.get::<_, Option<String>>(16)?,
                            row.get::<_, Option<i64>>(17)?,
                            row.get::<_, Option<i64>>(18)?,
                        ),
                    ))
                },
            )
            .optional())?;
        let Some((landing, kind, session, state, numbers, digests, intent)) = row else {
            return Ok(None);
        };
        let [l_dev, l_ino, l_size, l_mtime, d_dev, d_ino, d_size, d_mtime, placed] = numbers;
        let intent = match intent {
            (Some(word), Some(size), _) if word == "extend" => Some(Intent::Extend(uint(size))),
            (Some(word), _, Some(ino)) if word == "supersede" => Some(Intent::Supersede(uint(ino))),
            (None, _, _) => None,
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
            head: digest(digests.0),
            tail: digest(digests.1),
            digest: digest(digests.2),
            intent,
        }))
    }

    /// Write `row` (insert or replace), committed before this returns.
    ///
    /// # Errors
    /// `SQLITE_STATE_CHANGED` on a failed commit.
    pub fn put(&self, row: &Placement) -> Result<()> {
        let (intent, size, ino) = match row.intent {
            None => (None, None, None),
            Some(Intent::Extend(size)) => (Some("extend"), Some(int(size)), None),
            Some(Intent::Supersede(ino)) => (Some("supersede"), None, Some(int(ino))),
        };
        let started = std::time::Instant::now();
        let outcome = self.conn.execute(
            "INSERT OR REPLACE INTO placement(dest, landing, kind, session, state,
                l_dev, l_ino, l_size, l_mtime, d_dev, d_ino, d_size, d_mtime, placed,
                head, tail, digest, intent, intent_size, intent_ino, pass)
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
                row.head.as_ref().map(<[u8; 32]>::as_slice),
                row.tail.as_ref().map(<[u8; 32]>::as_slice),
                row.digest.as_ref().map(<[u8; 32]>::as_slice),
                intent,
                size,
                ino,
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
                "SELECT landing, source_cwd, dest_cwd, dest_key, slug, outcome
                 FROM session WHERE tool = ?1 AND id = ?2",
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
                dest_key, slug, outcome, pass)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
}
