//! Private, durable transfer state: the source's digest-only capture ledger
//! and the destination's output records and chunk hints.
//!
//! Wire v5 keeps no byte pack on either side (R-N58). A source capture is a
//! row key and its chunk manifest (digests and sizes, `manifest_root`), never
//! its bytes; the destination re-reads chunks only from published outputs,
//! through hints it re-verifies on use.

use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::fd::AsFd as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use bulkload_proto::frame::{manifest_root, ChunkSpec};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::{BulkloadRefusal, Result, RowSchema};

#[cfg(test)]
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static PUBLISH_GROUPS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMITS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMIT_NS: AtomicU64 = AtomicU64::new(0);

/// How many hint rows one chunk lookup tries, newest first.
const HINTS_PER_DIGEST: usize = 4;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishFault {
    None,
    AfterManifestInsert,
    BeforeCommit,
}

#[cfg(test)]
std::thread_local! {
    static PUBLISH_FAULT: std::cell::Cell<PublishFault> = const {
        std::cell::Cell::new(PublishFault::None)
    };
}

#[cfg(test)]
fn inject_fault(point: PublishFault) -> Result<()> {
    if PUBLISH_FAULT.with(std::cell::Cell::get) == point {
        return Err(BulkloadRefusal::Io(None));
    }
    Ok(())
}

/// Destination store roots whose output group commits fail, as a full disk
/// would fail them (#100). Keyed by store root, so tests running in parallel
/// never see each other's faults; the committer runs on its own thread, so a
/// thread-local hook cannot reach it.
#[cfg(test)]
static FAIL_OUTPUT_COMMITS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Test hook: while `on`, every output group commit of the store at `root`
/// (canonical) fails with `ENOSPC` before `COMMIT`, and rolls back.
#[cfg(test)]
pub(crate) fn fail_output_commits(root: &Path, on: bool) {
    let mut roots = FAIL_OUTPUT_COMMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    roots.retain(|failing| failing != root);
    if on {
        roots.push(root.to_path_buf());
    }
}

#[cfg(test)]
fn output_commit_fault(root: &Path) -> Result<()> {
    let failing = FAIL_OUTPUT_COMMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|failing| failing == root);
    if failing {
        return Err(BulkloadRefusal::Io(Some(libc::ENOSPC)));
    }
    Ok(())
}

// A publication crash point is named per store: the source ledger committer
// and the destination output committer hit `publish.source.*` and
// `publish.destination.*` respectively, and the crash receipt records the
// store root that was being written.
#[cfg(feature = "fault-injection")]
macro_rules! publication_crash {
    ($publisher:expr, $source:ident, $destination:ident) => {{
        let publisher = &$publisher;
        let point = match publisher.side {
            PublisherSide::Source => $crate::fault::Point::$source,
            PublisherSide::Destination => $crate::fault::Point::$destination,
        };
        $crate::fault::hit_in(point, publisher.store.root());
    }};
}

#[cfg(not(feature = "fault-injection"))]
macro_rules! publication_crash {
    ($publisher:expr, $source:ident, $destination:ident) => {{}};
}

// A source ledger point is both a unit-test refusal hook (`PublishFault`, the
// thread-local error path) and a crash point (`publication_crash!`, which under
// the `fault-injection` feature can end the process there).
#[cfg(test)]
macro_rules! publication_fault {
    ($publisher:expr, $point:ident, $crash:ident) => {{
        fault_point_in!($crash, $publisher.store.root());
        inject_fault(PublishFault::$point)?;
    }};
}

#[cfg(not(test))]
macro_rules! publication_fault {
    ($publisher:expr, $point:ident, $crash:ident) => {{
        fault_point_in!($crash, $publisher.store.root());
    }};
}

/// Process-local publication instrumentation.
///
/// Durations sum committer time and must not be interpreted as wall-time
/// shares. Concurrent independent transfers in the same process also
/// contribute.
#[derive(Clone, Copy, Debug)]
pub struct ChunkTiming {
    /// Source ledger groups committed by this process.
    pub publish_groups: u64,
    /// Successful durable source ledger commits by this process.
    pub sqlite_commits: u64,
    /// Aggregate nanoseconds spent in successful ledger commits.
    pub sqlite_commit_ns: u64,
}

impl ChunkTiming {
    /// Snapshot counters without resetting other callers' observations.
    #[must_use]
    pub fn snapshot() -> Self {
        Self {
            publish_groups: PUBLISH_GROUPS.load(Ordering::Relaxed),
            sqlite_commits: SQLITE_COMMITS.load(Ordering::Relaxed),
            sqlite_commit_ns: SQLITE_COMMIT_NS.load(Ordering::Relaxed),
        }
    }

    /// Space-separated `key=value` pairs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "publish_groups={} sqlite_commits={} sqlite_commit_ns={}",
            self.publish_groups, self.sqlite_commits, self.sqlite_commit_ns,
        )
    }

    /// Difference from a prior snapshot after the observed operation has joined.
    #[must_use]
    pub const fn since(self, before: Self) -> Self {
        Self {
            publish_groups: self.publish_groups.saturating_sub(before.publish_groups),
            sqlite_commits: self.sqlite_commits.saturating_sub(before.sqlite_commits),
            sqlite_commit_ns: self
                .sqlite_commit_ns
                .saturating_sub(before.sqlite_commit_ns),
        }
    }
}

fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// A completed content capture: chunk digests and sizes in file order,
/// repetitions included, and their [`manifest_root`]. Never any bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// [`manifest_root`] of `chunks`.
    pub root: [u8; 32],
    /// Content-defined chunks in order.
    pub chunks: Vec<ChunkSpec>,
}

impl Manifest {
    /// A manifest over `chunks`, with its root computed.
    #[must_use]
    pub fn new(chunks: Vec<ChunkSpec>) -> Self {
        Self {
            root: manifest_root(&chunks),
            chunks,
        }
    }

    /// Whether `root` is the root of `chunks`. A ledger row written before
    /// wire v5 held a whole-file hash in this place and fails the check.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        manifest_root(&self.chunks) == self.root
    }

    /// The summed chunk sizes, or `None` on overflow.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        self.chunks
            .iter()
            .try_fold(0_u64, |total, chunk| total.checked_add(chunk.size))
    }
}

/// One capture for the source ledger committer. Only a capture that passed
/// its final stat check is ever submitted (R-N86).
pub(crate) struct LedgerItem {
    /// The entry the capture belongs to, for crash receipts.
    #[cfg_attr(
        not(feature = "fault-injection"),
        allow(dead_code, reason = "read only by fault-injection crash receipts")
    )]
    pub entry: u64,
    pub key: Vec<u8>,
    pub manifest: Manifest,
}

/// A published destination output, ready for its group commit.
pub(crate) struct OutputRecord {
    pub key: Vec<u8>,
    pub rel_path: Vec<u8>,
    pub identity: StatIdentity,
    /// The source seat was racy when captured (#86): its stat identity
    /// cannot vouch for these bytes, so no output row is kept under `key`
    /// and the next run asks for a manifest instead of reusing it. Its
    /// chunk hints are still recorded; they are re-verified on use.
    pub racy: bool,
    /// First occurrence of each distinct chunk in this output.
    pub hints: Vec<ChunkHint>,
}

/// Where a chunk lies in an output named by the enclosing record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChunkHint {
    pub digest: [u8; 32],
    pub offset: u64,
    pub size: u64,
}

/// A persisted [`ChunkHint`] with its output's relative path.
#[derive(Debug)]
pub(crate) struct ChunkHintRow {
    pub path: Vec<u8>,
    pub offset: u64,
    pub size: u64,
}

struct Exclusive(fs::File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        // Best effort: the descriptor closes right after, which also drops
        // the lock.
        let _ = crate::io::sys::flock_unlock(&self.0);
    }
}

/// Exclusive owner of a store's durable publication.
pub(crate) struct StorePublisher {
    store: Store,
    side: PublisherSide,
    _exclusive: Exclusive,
}

/// Which side of a transfer a publisher writes for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublisherSide {
    /// The serving side's capture ledger.
    Source,
    /// The receiving side's output store.
    Destination,
}

/// An unfinished directory this state created, keyed by its row.
///
/// Committed once the directory exists under a tagged temporary name and its
/// parent is synced, before it is renamed into place (R-N102), and deleted
/// once the final mode is durable. It names an inode, never just a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDirectory {
    /// Device of the created directory.
    pub dev: u64,
    /// Inode of the created directory.
    pub ino: u64,
    /// The final mode `finish_directories` applies.
    pub mode: u32,
}

/// The `settings` row that marks a store whose ledger and output rows were
/// all written by the racy-capture guard (#86, #125). A store created by this
/// engine carries it from its first commit. A store without it predates the
/// guard: any of its rows may vouch for bytes captured inside the timestamp
/// tick of a same-size rewrite, so none of them is trusted (see
/// [`Store::open`]).
const RACY_GUARD_SETTING: &str = "racy_guard";

/// The `settings` row that marks a store whose state root has been sealed
/// (#161): its entry in the parent directory and its database's entry were
/// durable before this row committed. [`Store::open`] seals any store
/// without it, then adds it.
const ROOT_SEALED_SETTING: &str = "root_sealed";

/// A private, source-bound transfer state directory.
pub struct Store {
    root: PathBuf,
    conn: rusqlite::Connection,
    /// The store's `captures` and `outputs` rows were all written under the
    /// racy-capture guard (#125). A read-only handle on a store nobody has
    /// upgraded yet reads its rows as misses.
    rows_trusted: bool,
}

impl Store {
    /// Open or create a private transfer store outside the carried roots.
    ///
    /// A store written before the racy-capture guard (#86) has no
    /// [`RACY_GUARD_SETTING`] row, and none of its ledger or output rows is
    /// proven non-racy (#125). Opening it for writing invalidates them all
    /// in the transaction that adds the marker: every `captures` and
    /// `outputs` row is deleted, and the count is added to
    /// `transfer_legacy_rows_invalidated`. Each such seat is then read from
    /// the source once more, a re-read R25 allows because its row could not
    /// prove the seat was not racy when it was recorded. Chunk hints are
    /// kept: they are re-verified on use, so the re-read costs no wire bytes
    /// for content the destination still holds.
    ///
    /// The state root is durable before this returns, so before Start hands
    /// the store's authority to the peer and before any record commits
    /// (#161, R25): its entry in the parent and its database's entry are
    /// sealed ([`crate::io::durable::seal_state_root`]) ahead of the
    /// transaction that adds [`ROOT_SEALED_SETTING`]. A store without that
    /// marker (new, created by an earlier run that died before its seal, or
    /// created before #161) is sealed again on open.
    ///
    /// # Errors
    /// Refuses symlinks, non-private directories, flush and database failures.
    pub fn open(root: &Path) -> Result<Self> {
        let (parent, directory) = private_dir(root)?;
        let name = c"transfer.sqlite";
        // Created through `io::sys`, so the R-N88 trace sees the entry the
        // root seal below makes durable.
        match crate::io::sys::create_excl_at(directory.as_fd(), name, 0o600) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let found = crate::io::sys::fstatat_nofollow(&directory, name)?;
                if !found.is_file() || found.mode & 0o077 != 0 {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
            }
            Err(error) => return Err(error.into()),
        }
        let conn =
            rusqlite::Connection::open(root.join("transfer.sqlite")).map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_mins(1))
            .map_err(sqlite_error)?;
        crate::io::durable::configure_sqlite(&conn)?;
        // #161: sealed before the transaction that records it, and outside
        // SQLite's write lock, which a traced seal must never be taken under
        // (D5). This first read opens the WAL, so the seal covers the
        // database and its WAL.
        let sealed = has_setting(&conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?;
        if !sealed {
            crate::io::durable::seal_state_root(&parent, &directory)?;
        }
        let mut random = [0_u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let before = conn.total_changes();
        let started = Instant::now();
        conn.execute_batch("BEGIN").map_err(sqlite_error)?;
        // `output_hints` keeps several outputs per digest, newest first by
        // rowid, so losing one output does not lose reuse of its chunks. A
        // pre-v5 store's `output_chunks`, `chunks` and `chunk_locations` are
        // left untouched and unread.
        let mut invalidated = 0_usize;
        let created = conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS captures (key BLOB PRIMARY KEY, manifest BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS outputs (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS directories (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS output_hints (digest BLOB NOT NULL, path BLOB NOT NULL, offset INTEGER NOT NULL, size INTEGER NOT NULL, PRIMARY KEY (digest, path));
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
            )
            .and_then(|()| {
                conn.execute(
                    "INSERT OR IGNORE INTO settings VALUES ('authority', ?1)",
                    [random.as_slice()],
                )
            })
            .and_then(|_| {
                // #125: a store without the marker predates the racy guard.
                // Its rows are deleted with the marker's insert, in one
                // transaction, so no crash can leave the marker beside an
                // unproven row. A fresh store has no rows to delete.
                if racy_guarded(&conn)? {
                    return Ok(());
                }
                invalidated = conn.execute("DELETE FROM captures", [])?;
                invalidated = invalidated.saturating_add(conn.execute("DELETE FROM outputs", [])?);
                conn.execute(
                    "INSERT INTO settings VALUES (?1, ?2)",
                    (RACY_GUARD_SETTING, b"#86".as_slice()),
                )
                .map(|_| ())
            })
            .and_then(|()| {
                // Only once the seal above has returned (#161). Another
                // opener may have recorded it meanwhile.
                if sealed {
                    return Ok(());
                }
                conn.execute(
                    "INSERT OR IGNORE INTO settings VALUES (?1, ?2)",
                    (ROOT_SEALED_SETTING, b"#161".as_slice()),
                )
                .map(|_| ())
            })
            .and_then(|()| conn.execute_batch("COMMIT"))
            .map_err(sqlite_error);
        if created.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else if conn.total_changes() != before {
            counters::sqlite_commit(Counter::SqliteSchema, started, &created);
        }
        created?;
        counters::add_len(Counter::TransferLegacyRowsInvalidated, invalidated);
        Ok(Self {
            root: fs::canonicalize(root)?,
            conn,
            rows_trusted: true,
        })
    }

    /// Open an initialized store without obtaining any write capability.
    pub(crate) fn open_reader(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let db = root.join("transfer.sqlite");
        let meta = fs::symlink_metadata(&db)?;
        if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let conn = rusqlite::Connection::open_with_flags(
            db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_mins(1))
            .map_err(sqlite_error)?;
        // A reader never upgrades a store: one without the racy-guard
        // marker serves no capture and matches no output (#125).
        let rows_trusted = racy_guarded(&conn).map_err(sqlite_error)?;
        Ok(Self {
            root,
            conn,
            rows_trusted,
        })
    }

    /// Acquire the nonblocking single-writer guard for `side`.
    pub(crate) fn into_publisher(self, side: PublisherSide) -> Result<StorePublisher> {
        StorePublisher::open(self, side)
    }

    /// Canonical state root, used to reject recursive self-capture.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A persistent source-store identifier prevents cross-host inode collisions.
    /// [`Store::open`] creates it with the schema.
    ///
    /// # Errors
    /// Refuses a missing or malformed persistent authority.
    pub fn authority(&self) -> Result<[u8; 32]> {
        let bytes: Vec<u8> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key='authority'",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        bytes
            .try_into()
            .map_err(|_| BulkloadRefusal::SchemaMismatch)
    }

    /// The ledger's manifest for a row key, without opening source content.
    /// A row whose root does not match its chunks (a pre-v5 whole-file hash),
    /// or that does not decode exactly, is not a capture.
    ///
    /// # Errors
    /// Refuses database errors.
    pub fn capture(&self, key: &[u8]) -> Result<Option<Manifest>> {
        if !self.rows_trusted {
            return Ok(None);
        }
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT manifest FROM captures WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        // A row with bytes after its manifest is corrupt, not a capture
        // (#87): it is a miss, and the seat is read again.
        Ok(bytes
            .and_then(|value| bulkload_proto::frame::decode_exact::<Manifest>(&value).ok())
            .filter(Manifest::is_consistent))
    }

    /// Record one `Event::Commit` for this store (R-N88): a commit returned
    /// and made `records` durable. Called only after success.
    #[cfg(feature = "io-trace")]
    fn trace_commit(&self, records: impl FnOnce() -> Vec<crate::io::trace::CommitRecord>) {
        crate::io::trace::record("sqlite commit", || {
            use std::os::unix::fs::MetadataExt as _;
            let meta = fs::metadata(&self.root)?;
            Ok(crate::io::trace::Event::Commit {
                store: crate::io::NodeId {
                    dev: meta.dev(),
                    ino: meta.ino(),
                },
                records: records(),
            })
        });
    }

    /// How many ledger captures and output rows the store holds.
    #[cfg(test)]
    pub(crate) fn row_counts(&self) -> Result<(u64, u64)> {
        let count = |table: &str| -> Result<u64> {
            self.conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(sqlite_error)
                .and_then(|count| u64::try_from(count).map_err(|_| BulkloadRefusal::SchemaMismatch))
        };
        Ok((count("captures")?, count("outputs")?))
    }

    /// Drop the racy-guard marker, as a store written before #86 lacks it.
    #[cfg(test)]
    pub(crate) fn forget_racy_guard(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [RACY_GUARD_SETTING])
            .map_err(sqlite_error)?;
        Ok(())
    }

    /// Commit one identity-checked capture on its own.
    ///
    /// # Errors
    /// Refuses serialization or database failures.
    pub fn record_capture(&self, key: &[u8], value: &Manifest) -> Result<()> {
        let encoded = postcard::to_stdvec(value)?;
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        let started = Instant::now();
        let recorded = self
            .conn
            .execute(
                "INSERT INTO captures VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                (key, encoded),
            )
            .map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteRecordCapture, started, &recorded);
        recorded?;
        #[cfg(feature = "io-trace")]
        self.trace_commit(|| vec![crate::io::trace::CommitRecord::Capture { key: key.to_vec() }]);
        Ok(())
    }

    /// Whether a current output is the same object recorded on successful apply.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn output_matches(&self, key: &[u8], identity: &StatIdentity) -> Result<bool> {
        if !self.rows_trusted {
            return Ok(false);
        }
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM outputs WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(found == Some(identity_bytes(identity)?))
    }

    /// Whether any output hint is recorded: whether a manifest could be
    /// filled from published outputs at all.
    ///
    /// # Errors
    /// Refuses database failures.
    pub(crate) fn has_output_hints(&self) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM output_hints)", [], |row| {
                row.get(0)
            })
            .map_err(sqlite_error)
    }

    /// Where earlier transfers wrote a chunk into destination outputs, newest
    /// first, at most [`HINTS_PER_DIGEST`]. Hints only: the caller re-reads
    /// and re-verifies the bytes before use, and tries the next on a miss.
    ///
    /// # Errors
    /// Refuses database failures and malformed rows.
    pub(crate) fn output_chunks(&self, digest: &[u8; 32]) -> Result<Vec<ChunkHintRow>> {
        let mut statement = self
            .conn
            .prepare_cached(
                "SELECT path, offset, size FROM output_hints WHERE digest = ?1
                 ORDER BY rowid DESC LIMIT ?2",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    digest.as_slice(),
                    i64::try_from(HINTS_PER_DIGEST).unwrap_or(1)
                ],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(sqlite_error)?;
        let mut found = Vec::new();
        for row in rows {
            let (path, offset, size) = row.map_err(sqlite_error)?;
            found.push(ChunkHintRow {
                path,
                offset: u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?,
                size: u64::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?,
            });
        }
        Ok(found)
    }

    /// Bind a pending directory to the inode this state created.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn record_directory_created(
        &self,
        key: &[u8],
        dev: u64,
        ino: u64,
        mode: u32,
    ) -> Result<()> {
        let identity = postcard::to_stdvec(&PendingDirectory { dev, ino, mode })?;
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        let started = Instant::now();
        let recorded = self
            .conn
            .execute(
                "INSERT INTO directories VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                (key, identity),
            )
            .map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteDirectoryPending, started, &recorded);
        recorded?;
        #[cfg(feature = "io-trace")]
        self.trace_commit(|| {
            vec![crate::io::trace::CommitRecord::DirectoryCreated {
                key: key.to_vec(),
                node: (dev, ino)
                    .ne(&(0, 0))
                    .then_some(crate::io::NodeId { dev, ino }),
                mode,
            }]
        });
        Ok(())
    }

    /// The unfinished-directory record for `key`, if any.
    ///
    /// # Errors
    /// Refuses database failures, and refuses a record this engine cannot
    /// decode exactly with [`BulkloadRefusal::SchemaMismatch`], so an
    /// unreadable record never grants ownership.
    pub fn directory_record(&self, key: &[u8]) -> Result<Option<PendingDirectory>> {
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM directories WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        found
            .map(|bytes| match postcard::take_from_bytes(&bytes) {
                Ok((record, [])) => Ok(record),
                _ => Err(BulkloadRefusal::SchemaMismatch),
            })
            .transpose()
    }

    /// Clear every pending record bound to `(dev, ino)`, once that inode is
    /// gone, so a recycled inode number can never inherit its ownership.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn clear_directories_bound_to(&self, dev: u64, ino: u64) -> Result<()> {
        let bound: Vec<Vec<u8>> = {
            let mut statement = self
                .conn
                .prepare("SELECT key, identity FROM directories")
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .map_err(sqlite_error)?;
            let mut bound = Vec::new();
            for row in rows {
                let (key, identity) = row.map_err(sqlite_error)?;
                if let Ok((record, [])) = postcard::take_from_bytes::<PendingDirectory>(&identity) {
                    if record.dev == dev && record.ino == ino {
                        bound.push(key);
                    }
                }
            }
            bound
        };
        for key in bound {
            self.complete_directory(&key)?;
        }
        Ok(())
    }

    /// Retire pending ownership after directory metadata is durable.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn complete_directory(&self, key: &[u8]) -> Result<()> {
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        let started = Instant::now();
        let completed = self
            .conn
            .execute("DELETE FROM directories WHERE key = ?1", [key])
            .map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteDirectoryComplete, started, &completed);
        completed?;
        #[cfg(feature = "io-trace")]
        self.trace_commit(|| {
            vec![crate::io::trace::CommitRecord::DirectoryComplete { key: key.to_vec() }]
        });
        Ok(())
    }
}

impl StorePublisher {
    fn open(store: Store, role: PublisherSide) -> Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store.root.join("writer.lock"))?;
        crate::io::sys::flock_exclusive(&lock)?;
        Ok(Self {
            store,
            side: role,
            _exclusive: Exclusive(lock),
        })
    }

    /// The store this publisher writes.
    pub(crate) const fn store(&self) -> &Store {
        &self.store
    }

    /// The group-commit counter for this publisher's side.
    const fn group_counter(&self) -> Counter {
        match self.side {
            PublisherSide::Source => Counter::SqliteGroupSource,
            PublisherSide::Destination => Counter::SqliteGroupDest,
        }
    }

    /// Commit one group of published outputs and their chunk hints in a single
    /// transaction. Callers seal every file and directory first.
    ///
    /// # Errors
    /// Refuses serialization or database failures; nothing is committed then.
    pub(crate) fn commit_outputs(&self, outputs: &[OutputRecord]) -> Result<()> {
        // The trace's serial lock is taken before SQLite's write lock, the
        // order every traced store write uses (#74 review, D5).
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        self.store
            .conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let staged = (|| -> Result<()> {
            for output in outputs {
                if output.racy {
                    // Never a reuse key (#86): drop any row an earlier
                    // capture left under it, too.
                    self.store
                        .conn
                        .execute("DELETE FROM outputs WHERE key = ?1", [&output.key])
                        .map_err(sqlite_error)?;
                } else {
                    self.store
                        .conn
                        .execute(
                            "INSERT INTO outputs VALUES (?1, ?2)
                             ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                            (&output.key, identity_bytes(&output.identity)?),
                        )
                        .map_err(sqlite_error)?;
                }
                for hint in &output.hints {
                    // REPLACE gives the row a new rowid, so the newest
                    // writer of a digest is tried first.
                    self.store
                        .conn
                        .execute(
                            "INSERT OR REPLACE INTO output_hints (digest, path, offset, size)
                             VALUES (?1, ?2, ?3, ?4)",
                            rusqlite::params![
                                hint.digest.as_slice(),
                                &output.rel_path,
                                i64::try_from(hint.offset)
                                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                                i64::try_from(hint.size)
                                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                            ],
                        )
                        .map_err(sqlite_error)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = staged {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        publication_crash!(
            self,
            PublishSourceBeforeCommit,
            PublishDestinationBeforeCommit
        );
        let started = Instant::now();
        #[cfg(test)]
        let committed = output_commit_fault(self.store.root()).and_then(|()| {
            self.store
                .conn
                .execute_batch("COMMIT")
                .map_err(sqlite_error)
        });
        #[cfg(not(test))]
        let committed = self
            .store
            .conn
            .execute_batch("COMMIT")
            .map_err(sqlite_error);
        counters::sqlite_commit(self.group_counter(), started, &committed);
        #[cfg(feature = "io-trace")]
        if committed.is_ok() {
            self.store.trace_commit(|| {
                outputs
                    .iter()
                    .map(|output| crate::io::trace::CommitRecord::Output {
                        rel_path: output.rel_path.clone(),
                    })
                    .collect()
            });
        }
        if committed.is_ok() {
            publication_crash!(
                self,
                PublishSourceAfterCommit,
                PublishDestinationAfterCommit
            );
        }
        if let Err(error) = committed {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        Ok(())
    }

    /// Commit completed captures to the ledger in one transaction.
    fn commit_captures(&self, captures: &[LedgerItem]) -> Result<()> {
        if captures.is_empty() {
            return Ok(());
        }
        // Trace lock before SQLite's write lock, as everywhere (D5).
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        self.store
            .conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let persisted = (|| -> Result<()> {
            for capture in captures {
                self.store
                    .conn
                    .execute(
                        "INSERT INTO captures VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                        (&capture.key, postcard::to_stdvec(&capture.manifest)?),
                    )
                    .map_err(sqlite_error)?;
            }
            publication_fault!(self, AfterManifestInsert, PublishSourceAfterManifestInsert);
            Ok(())
        })();
        if let Err(error) = persisted {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        publication_crash!(
            self,
            PublishSourceBeforeCommit,
            PublishDestinationBeforeCommit
        );
        let commit_started = Instant::now();
        #[cfg(test)]
        let committed = inject_fault(PublishFault::BeforeCommit).and_then(|()| {
            self.store
                .conn
                .execute_batch("COMMIT")
                .map_err(sqlite_error)
        });
        #[cfg(not(test))]
        let committed = self
            .store
            .conn
            .execute_batch("COMMIT")
            .map_err(sqlite_error);
        if committed.is_ok() {
            // Successful commits only, matching `counters::sqlite_commit`.
            SQLITE_COMMITS.fetch_add(1, Ordering::Relaxed);
            SQLITE_COMMIT_NS.fetch_add(nanos(commit_started), Ordering::Relaxed);
        }
        counters::sqlite_commit(self.group_counter(), commit_started, &committed);
        #[cfg(feature = "io-trace")]
        if committed.is_ok() {
            self.store.trace_commit(|| {
                captures
                    .iter()
                    .map(|capture| crate::io::trace::CommitRecord::Capture {
                        key: capture.key.clone(),
                    })
                    .collect()
            });
        }
        if let Err(error) = committed {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        publication_crash!(
            self,
            PublishSourceAfterCommit,
            PublishDestinationAfterCommit
        );
        Ok(())
    }
}

/// Group-commit sink for a source store's digest-only ledger (R-N58). A
/// group is a set of completed captures committed in one transaction; there
/// are no chunk bytes to write or seal first.
pub(crate) struct LedgerSink {
    publisher: StorePublisher,
    failed: Option<BulkloadRefusal>,
}

impl LedgerSink {
    pub(crate) const fn new(publisher: StorePublisher) -> Self {
        Self {
            publisher,
            failed: None,
        }
    }

    /// Commit one group of captures.
    pub(crate) fn publish(&self, items: &[LedgerItem]) -> Result<()> {
        PUBLISH_GROUPS.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "fault-injection")]
        let _note = {
            let mut ids: Vec<usize> = items
                .iter()
                .map(|item| usize::try_from(item.entry).unwrap_or(usize::MAX))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            crate::fault::note_group(&ids, 0)
        };
        for item in items {
            if !item.manifest.is_consistent()
                || item
                    .manifest
                    .chunks
                    .iter()
                    .any(|chunk| chunk.size > u64::from(crate::hash::CDC_MAX_BYTES))
            {
                return Err(BulkloadRefusal::DigestMismatch);
            }
        }
        self.publisher.commit_captures(items)
    }
}

impl crate::io::durable::GroupSink for LedgerSink {
    const SIDE: crate::io::durable::GroupSide = crate::io::durable::GroupSide::Source;
    type Item = LedgerItem;
    type Report = Result<()>;

    fn weight(_: &LedgerItem) -> (u64, u64) {
        (1, 0)
    }

    fn commit(&mut self, items: Vec<LedgerItem>) {
        if self.failed.is_none() {
            if let Err(refusal) = self.publish(&items) {
                self.failed = Some(refusal);
            }
        }
    }

    fn failure(&self) -> Option<BulkloadRefusal> {
        self.failed.clone()
    }

    fn finish(self) -> Result<()> {
        self.failed.map_or(Ok(()), Err)
    }
}

/// Bind a row to its source root identity and destination namespace.
///
/// # Errors
/// Refuses serialization failure.
pub fn row_key(authority: &[u8], row: &RowSchema) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(&(authority, row))?)
}

/// Whether the store carries the [`RACY_GUARD_SETTING`] marker (#125).
fn racy_guarded(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key = ?1)",
        [RACY_GUARD_SETTING],
        |row| row.get(0),
    )
}

fn identity_bytes(identity: &StatIdentity) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(&(
        identity.dev,
        identity.ino,
        identity.size,
        identity.mtime_ns,
        identity.ctime_ns,
    ))?)
}

/// Whether the store has a `settings` table holding `key`. A database
/// [`Store::open`] has just created has no tables yet.
fn has_setting(conn: &rusqlite::Connection, key: &str) -> rusqlite::Result<bool> {
    let table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings')",
        [],
        |row| row.get(0),
    )?;
    if !table {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key = ?1)",
        [key],
        |row| row.get(0),
    )
}

/// Open the private state root `path`, creating it (0700) when absent, and
/// the directory that holds its entry, both kept open for the seal (#161).
///
/// A symlink, a non-directory, or a directory with any group or other bit at
/// `path` is refused. The parent is opened through the operator's path,
/// symlinks and all; the root itself never through a symlink. The root is
/// made with `io::sys::mkdirat`, so the R-N88 trace sees the entry.
fn private_dir(path: &Path) -> Result<(fs::File, fs::File)> {
    use std::os::unix::ffi::OsStrExt as _;
    // `.`, `..` and `x/..` name no leaf of their own: resolve them first.
    let path = if path.file_name().is_some() {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Owned(fs::canonicalize(path)?)
    };
    let leaf = path.file_name().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let parent = fs::File::open(parent)?;
    let name = crate::io::c_name(leaf.as_bytes())?;
    match crate::io::sys::mkdirat(&parent, &name, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let root = crate::io::sys::open_dir_at(&parent, &name).map_err(|error| {
        if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
            BulkloadRefusal::PathEscapesRoot
        } else {
            error.into()
        }
    })?;
    let root = fs::File::from(root);
    let found = crate::io::sys::fstat(&root)?;
    if !found.is_dir() || found.mode & 0o077 != 0 {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok((parent, root))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the `map_err` callback shape every store call uses"
)]
fn sqlite_error(error: rusqlite::Error) -> BulkloadRefusal {
    // A full disk is reported as the `ENOSPC` it is, so a failed group
    // commit names its cause (#100); anything else is the store's fault.
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        return BulkloadRefusal::Io(Some(libc::ENOSPC));
    }
    BulkloadRefusal::SqliteIntegrityCheckFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "tcfs-transfer-store-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(data: &[u8]) -> Manifest {
        let digest = crate::hash::hash_bytes(data);
        let chunk = ChunkSpec {
            digest,
            size: data.len() as u64,
        };
        Manifest::new(vec![chunk.clone(), chunk])
    }

    fn item(key: &[u8], data: &[u8]) -> LedgerItem {
        LedgerItem {
            entry: 7,
            key: key.to_vec(),
            manifest: manifest(data),
        }
    }

    fn ledger_sink(state: &Path) -> Result<LedgerSink> {
        Ok(LedgerSink::new(
            Store::open(state)?.into_publisher(PublisherSide::Source)?,
        ))
    }

    #[test]
    fn stores_commit_through_wal_with_full_flushes() -> Result<()> {
        let root = TestRoot::new()?;
        let store = Store::open(&root.0.join("state"))?;
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(sqlite_error)?;
        let synchronous: i64 = store
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .map_err(sqlite_error)?;
        let fullfsync: i64 = store
            .conn
            .query_row("PRAGMA fullfsync", [], |row| row.get(0))
            .map_err(sqlite_error)?;
        assert_eq!(mode, "wal");
        // 2 is FULL.
        assert_eq!(synchronous, 2);
        assert_eq!(fullfsync, 1);
        assert_eq!(
            store.authority()?,
            Store::open(&root.0.join("state"))?.authority()?
        );
        Ok(())
    }

    /// R-N58: the ledger holds digests and sizes only. A fresh store has no
    /// byte pack, no chunk directory and no chunk tables.
    #[test]
    fn a_store_holds_no_chunk_bytes() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let sink = ledger_sink(&state)?;
        sink.publish(&[item(b"capture", b"ledger content")])?;
        drop(sink);
        assert!(fs::symlink_metadata(state.join("chunks.pack")).is_err());
        assert!(fs::symlink_metadata(state.join("chunks")).is_err());
        let store = Store::open(&state)?;
        let tables: Vec<String> = {
            let mut statement = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .map_err(sqlite_error)?;
            let names = statement
                .query_map([], |row| row.get(0))
                .map_err(sqlite_error)?
                .collect::<rusqlite::Result<Vec<String>>>()
                .map_err(sqlite_error)?;
            names
        };
        assert_eq!(
            tables,
            [
                "captures",
                "directories",
                "output_hints",
                "outputs",
                "settings"
            ]
        );
        Ok(())
    }

    /// #161: a state root is a private directory, never reached through a
    /// symlink, and so is its database a private file.
    #[test]
    fn a_state_root_must_be_a_private_directory() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestRoot::new()?;
        let private = root.0.join("private");
        fs::create_dir(&private)?;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))?;
        std::os::unix::fs::symlink(&private, root.0.join("link"))?;
        fs::write(root.0.join("file"), b"")?;
        let shared = root.0.join("shared");
        fs::create_dir(&shared)?;
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o750))?;
        for refused in ["link", "file", "shared"] {
            assert!(
                matches!(
                    Store::open(&root.0.join(refused)),
                    Err(BulkloadRefusal::PathEscapesRoot)
                ),
                "{refused}"
            );
        }
        let state = root.0.join("state");
        drop(Store::open(&state)?);
        fs::set_permissions(
            state.join("transfer.sqlite"),
            fs::Permissions::from_mode(0o644),
        )?;
        assert!(matches!(
            Store::open(&state),
            Err(BulkloadRefusal::PathEscapesRoot)
        ));
        Ok(())
    }

    /// #161: a store records its root seal, and a store without the record
    /// (one from before #161, or whose creator died before its seal) gets it
    /// back on its next open.
    #[test]
    fn a_store_records_its_root_seal() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        assert!(has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?);
        store
            .conn
            .execute("DELETE FROM settings WHERE key = ?1", [ROOT_SEALED_SETTING])
            .map_err(sqlite_error)?;
        assert!(!has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?);
        drop(store);
        let store = Store::open(&state)?;
        assert!(has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?);
        Ok(())
    }

    #[test]
    fn ledger_preserves_manifest_order_and_root() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let first = crate::hash::hash_bytes(b"first");
        let second = crate::hash::hash_bytes(b"second");
        let chunks = vec![
            ChunkSpec {
                digest: first,
                size: 5,
            },
            ChunkSpec {
                digest: first,
                size: 5,
            },
            ChunkSpec {
                digest: second,
                size: 6,
            },
        ];
        let manifest = Manifest::new(chunks);
        ledger_sink(&state)?.publish(&[LedgerItem {
            entry: 1,
            key: b"capture".to_vec(),
            manifest: manifest.clone(),
        }])?;
        let captured = Store::open(&state)?
            .capture(b"capture")?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        assert_eq!(captured, manifest);
        assert_eq!(captured.size(), Some(16));
        Ok(())
    }

    /// A ledger row whose root is not its chunks' root (a pre-v5 whole-file
    /// hash, or a corrupt row) is never served as a capture, and an
    /// inconsistent manifest is never committed.
    #[test]
    fn an_inconsistent_manifest_is_neither_served_nor_committed() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let mut outdated = manifest(b"stale");
        outdated.root = crate::hash::hash_bytes(b"whole file");
        Store::open(&state)?.record_capture(b"stale", &outdated)?;
        assert!(Store::open(&state)?.capture(b"stale")?.is_none());
        let sink = ledger_sink(&state)?;
        assert_eq!(
            sink.publish(&[LedgerItem {
                entry: 0,
                key: b"bad".to_vec(),
                manifest: outdated,
            }]),
            Err(BulkloadRefusal::DigestMismatch)
        );
        drop(sink);
        assert!(Store::open(&state)?.capture(b"bad")?.is_none());
        Ok(())
    }

    /// A failed group stops the committer's callers at once: `sync` and
    /// every later `submit` return the sink's failure.
    #[test]
    fn a_failed_group_is_returned_by_sync_and_submit() -> Result<()> {
        use crate::io::durable::Committer;
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let mut bad = item(b"bad", b"bad");
        bad.manifest.root = [0; 32];
        let committer = Committer::spawn(ledger_sink(&state)?)?;
        committer.submit(bad)?;
        assert_eq!(committer.sync(), Err(BulkloadRefusal::DigestMismatch));
        assert_eq!(
            committer.submit(item(b"later", b"later")),
            Err(BulkloadRefusal::DigestMismatch)
        );
        assert_eq!(committer.finish()?, Err(BulkloadRefusal::DigestMismatch));
        Ok(())
    }

    #[test]
    fn publisher_is_exclusive() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Source)?;
        assert!(Store::open(&state)?
            .into_publisher(PublisherSide::Source)
            .is_err());
        drop(publisher);
        assert!(Store::open(&state)?
            .into_publisher(PublisherSide::Source)
            .is_ok());
        Ok(())
    }

    #[test]
    fn interrupted_publication_hides_manifest_and_retries_cleanly() -> Result<()> {
        for fault in [
            PublishFault::AfterManifestInsert,
            PublishFault::BeforeCommit,
        ] {
            let root = TestRoot::new()?;
            let state = root.0.join("state");
            let sink = ledger_sink(&state)?;
            PUBLISH_FAULT.with(|active| active.set(fault));
            assert!(matches!(
                sink.publish(&[item(b"capture", b"fault recovery content")]),
                Err(BulkloadRefusal::Io(None))
            ));
            PUBLISH_FAULT.with(|active| active.set(PublishFault::None));
            assert!(Store::open(&state)?.capture(b"capture")?.is_none());
            drop(sink);

            let sink = ledger_sink(&state)?;
            sink.publish(&[item(b"capture", b"fault recovery content")])?;
            drop(sink);
            assert_eq!(
                Store::open(&state)?.capture(b"capture")?,
                Some(manifest(b"fault recovery content"))
            );
        }
        Ok(())
    }

    #[test]
    fn output_records_and_chunk_hints_commit_together() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        let file = root.0.join("output");
        fs::write(&file, b"output")?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file)?);
        let hint = ChunkHint {
            digest: [3; 32],
            offset: 5,
            size: 7,
        };
        publisher.commit_outputs(&[OutputRecord {
            key: b"key".to_vec(),
            rel_path: b"nested/output".to_vec(),
            identity,
            racy: false,
            hints: vec![hint],
        }])?;
        drop(publisher);
        let reopened = Store::open(&state)?;
        assert!(reopened.output_matches(b"key", &identity)?);
        assert!(reopened.has_output_hints()?);
        let found = reopened.output_chunks(&[3; 32])?;
        assert_eq!(found.len(), 1);
        let found = found.first().ok_or(BulkloadRefusal::SealedObjectMissing)?;
        assert_eq!(
            (found.path.as_slice(), found.offset, found.size),
            (b"nested/output".as_slice(), 5, 7)
        );
        assert!(reopened.output_chunks(&[4; 32])?.is_empty());
        Ok(())
    }

    /// #86: a racy output keeps no row under its key, and drops one an
    /// earlier capture left there, so it is never a `Reuse`; its chunk hints
    /// are still recorded.
    #[test]
    fn a_racy_output_is_never_a_reuse_key() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        let file = root.0.join("output");
        fs::write(&file, b"output")?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file)?);
        let record = |racy: bool| OutputRecord {
            key: b"key".to_vec(),
            rel_path: b"output".to_vec(),
            identity,
            racy,
            hints: vec![ChunkHint {
                digest: [5; 32],
                offset: 0,
                size: 6,
            }],
        };
        publisher.commit_outputs(&[record(false)])?;
        assert!(publisher.store().output_matches(b"key", &identity)?);
        publisher.commit_outputs(&[record(true)])?;
        assert!(!publisher.store().output_matches(b"key", &identity)?);
        assert_eq!(publisher.store().row_counts()?, (0, 0));
        assert_eq!(publisher.store().output_chunks(&[5; 32])?.len(), 1);
        Ok(())
    }

    /// #87: a ledger row with bytes after its manifest is a miss, never a
    /// capture; the exact row is served.
    #[test]
    fn a_ledger_row_with_trailing_bytes_is_a_miss() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        let exact = manifest(b"exact");
        store.record_capture(b"exact", &exact)?;
        let mut padded = postcard::to_stdvec(&exact)?;
        padded.push(0);
        store
            .conn
            .execute(
                "INSERT INTO captures VALUES (?1, ?2)",
                (b"padded".as_slice(), padded),
            )
            .map_err(sqlite_error)?;
        assert_eq!(store.capture(b"exact")?, Some(exact));
        assert_eq!(store.capture(b"padded")?, None);
        Ok(())
    }

    /// #125: a store written before the racy guard (#86) has no marker, so
    /// none of its ledger or output rows is proven non-racy. A reader serves
    /// none of them; the first writable open deletes them all with the
    /// marker's insert and counts them, and keeps the chunk hints. Rows
    /// written after the upgrade are trusted and survive later opens.
    #[test]
    fn a_store_from_before_the_racy_guard_trusts_none_of_its_rows() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let file = root.0.join("output");
        fs::write(&file, b"output")?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file)?);
        let record = |key: &[u8]| OutputRecord {
            key: key.to_vec(),
            rel_path: b"output".to_vec(),
            identity,
            racy: false,
            hints: vec![ChunkHint {
                digest: [7; 32],
                offset: 0,
                size: 6,
            }],
        };
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        publisher.commit_outputs(&[record(b"old")])?;
        publisher
            .store()
            .record_capture(b"old", &manifest(b"old"))?;
        publisher.store().forget_racy_guard()?;
        drop(publisher);

        let reader = Store::open_reader(&state)?;
        assert_eq!(reader.capture(b"old")?, None);
        assert!(!reader.output_matches(b"old", &identity)?);
        drop(reader);

        let counted =
            || crate::counters::Counters::snapshot().get(Counter::TransferLegacyRowsInvalidated);
        let before = counted();
        let upgraded = Store::open(&state)?;
        assert!(counted() >= before + 2, "both rows are counted");
        assert_eq!(upgraded.row_counts()?, (0, 0));
        assert!(!upgraded.output_matches(b"old", &identity)?);
        assert_eq!(upgraded.capture(b"old")?, None);
        assert_eq!(upgraded.output_chunks(&[7; 32])?.len(), 1, "hints are kept");
        drop(upgraded);

        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        publisher.commit_outputs(&[record(b"new")])?;
        publisher
            .store()
            .record_capture(b"new", &manifest(b"new"))?;
        drop(publisher);
        let reopened = Store::open(&state)?;
        assert_eq!(reopened.row_counts()?, (1, 1));
        assert!(reopened.output_matches(b"new", &identity)?);
        assert_eq!(reopened.capture(b"new")?, Some(manifest(b"new")));
        assert!(Store::open_reader(&state)?.output_matches(b"new", &identity)?);
        Ok(())
    }

    /// Hint ordering (#59 review): several outputs holding one digest are all
    /// kept, newest first, and re-recording an output moves it to the front.
    #[test]
    fn hints_keep_every_holder_newest_first() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        let file = root.0.join("output");
        fs::write(&file, b"output")?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file)?);
        let record = |path: &[u8], offset: u64| OutputRecord {
            key: path.to_vec(),
            rel_path: path.to_vec(),
            identity,
            racy: false,
            hints: vec![ChunkHint {
                digest: [9; 32],
                offset,
                size: 3,
            }],
        };
        publisher.commit_outputs(&[record(b"a", 0), record(b"b", 10)])?;
        publisher.commit_outputs(&[record(b"c", 20)])?;
        let paths = |found: Vec<ChunkHintRow>| -> Vec<Vec<u8>> {
            found.into_iter().map(|row| row.path).collect()
        };
        assert_eq!(
            paths(publisher.store().output_chunks(&[9; 32])?),
            [b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]
        );
        publisher.commit_outputs(&[record(b"a", 30)])?;
        let found = publisher.store().output_chunks(&[9; 32])?;
        assert_eq!(
            found.first().map(|row| (row.path.clone(), row.offset)),
            Some((b"a".to_vec(), 30))
        );
        assert_eq!(found.len(), 3);
        Ok(())
    }
}
