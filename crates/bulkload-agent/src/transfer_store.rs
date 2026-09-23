//! Private, durable chunk storage and completion records for native transfers.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bulkload_proto::frame::ChunkSpec;
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::{BulkloadRefusal, Result, RowSchema};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static PUT_CALLS: AtomicU64 = AtomicU64::new(0);
static PUT_NS: AtomicU64 = AtomicU64::new(0);
static FILE_SYNCS: AtomicU64 = AtomicU64::new(0);
static FILE_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static DIR_SYNCS: AtomicU64 = AtomicU64::new(0);
static DIR_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static PUBLISH_GROUPS: AtomicU64 = AtomicU64::new(0);
static PACK_APPEND_NS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMITS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMIT_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishFault {
    None,
    AfterAppend,
    AfterSync,
    AfterLocationInsert,
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

#[cfg(test)]
macro_rules! publication_fault {
    ($point:ident) => {{
        inject_fault(PublishFault::$point)?;
    }};
}

#[cfg(not(test))]
macro_rules! publication_fault {
    ($point:ident) => {{}};
}

/// Maximum buffered chunks per producer (at most 64 MiB of chunk payload).
pub(crate) const PERSIST_BATCH: usize = 256;
#[cfg(test)]
const PERSIST_WORKERS: usize = 2;

/// Process-local chunk persistence instrumentation.
///
/// Durations sum worker time,
/// including failed operations, and must not be interpreted as wall-time shares.
/// Concurrent independent transfers in the same process also contribute.
#[derive(Clone, Copy, Debug)]
pub struct ChunkTiming {
    /// Calls to `put_chunk`, including already-present content.
    pub put_calls: u64,
    /// Aggregate worker nanoseconds inside `put_chunk`.
    pub put_ns: u64,
    /// Attempted pack seals (a barrier per group, or a full flush under strict).
    pub file_syncs: u64,
    /// Aggregate pack-seal worker nanoseconds.
    pub file_sync_ns: u64,
    /// Attempted chunk-directory syncs.
    pub dir_syncs: u64,
    /// Aggregate chunk-directory sync worker nanoseconds.
    pub dir_sync_ns: u64,
    /// Source pack groups committed by this process.
    pub publish_groups: u64,
    /// Aggregate nanoseconds spent appending payload bytes to the pack.
    pub pack_append_ns: u64,
    /// Successful durable `SQLite` publication commits by this process.
    pub sqlite_commits: u64,
    /// Aggregate nanoseconds spent in successful publication commits.
    pub sqlite_commit_ns: u64,
}

impl ChunkTiming {
    /// Snapshot counters without resetting other callers' observations.
    #[must_use]
    pub fn snapshot() -> Self {
        Self {
            put_calls: PUT_CALLS.load(Ordering::Relaxed),
            put_ns: PUT_NS.load(Ordering::Relaxed),
            file_syncs: FILE_SYNCS.load(Ordering::Relaxed),
            file_sync_ns: FILE_SYNC_NS.load(Ordering::Relaxed),
            dir_syncs: DIR_SYNCS.load(Ordering::Relaxed),
            dir_sync_ns: DIR_SYNC_NS.load(Ordering::Relaxed),
            publish_groups: PUBLISH_GROUPS.load(Ordering::Relaxed),
            pack_append_ns: PACK_APPEND_NS.load(Ordering::Relaxed),
            sqlite_commits: SQLITE_COMMITS.load(Ordering::Relaxed),
            sqlite_commit_ns: SQLITE_COMMIT_NS.load(Ordering::Relaxed),
        }
    }

    /// Space-separated `key=value` pairs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "publish_groups={} pack_append_ns={} file_syncs={} file_sync_ns={} sqlite_commits={} sqlite_commit_ns={} legacy_put_calls={} legacy_put_ns={} legacy_dir_syncs={} legacy_dir_sync_ns={}",
            self.publish_groups,
            self.pack_append_ns,
            self.file_syncs,
            self.file_sync_ns,
            self.sqlite_commits,
            self.sqlite_commit_ns,
            self.put_calls,
            self.put_ns,
            self.dir_syncs,
            self.dir_sync_ns,
        )
    }

    /// Difference from a prior snapshot after the observed operation has joined.
    #[must_use]
    pub const fn since(self, before: Self) -> Self {
        Self {
            put_calls: self.put_calls.saturating_sub(before.put_calls),
            put_ns: self.put_ns.saturating_sub(before.put_ns),
            file_syncs: self.file_syncs.saturating_sub(before.file_syncs),
            file_sync_ns: self.file_sync_ns.saturating_sub(before.file_sync_ns),
            dir_syncs: self.dir_syncs.saturating_sub(before.dir_syncs),
            dir_sync_ns: self.dir_sync_ns.saturating_sub(before.dir_sync_ns),
            publish_groups: self.publish_groups.saturating_sub(before.publish_groups),
            pack_append_ns: self.pack_append_ns.saturating_sub(before.pack_append_ns),
            sqlite_commits: self.sqlite_commits.saturating_sub(before.sqlite_commits),
            sqlite_commit_ns: self
                .sqlite_commit_ns
                .saturating_sub(before.sqlite_commit_ns),
        }
    }
}

struct PutTimer(Instant);

impl Drop for PutTimer {
    fn drop(&mut self) {
        PUT_NS.fetch_add(nanos(self.0), Ordering::Relaxed);
    }
}

fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn timed_sync(
    file: &fs::File,
    count: &AtomicU64,
    time: &AtomicU64,
    directory: bool,
) -> std::io::Result<()> {
    count.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let result = if directory {
        counters::sync_dir(file)
    } else {
        counters::sync_full(file)
    };
    time.fetch_add(nanos(started), Ordering::Relaxed);
    result
}

/// Completed content capture; chunks retain file order, including repetitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Digest of the complete file.
    pub digest: [u8; 32],
    /// Content-defined chunks in order.
    pub chunks: Vec<ChunkSpec>,
}

/// Chunk bytes shared between the sender and the pack committer.
pub(crate) type ChunkData = Arc<Vec<u8>>;

/// Bounded capture output, consumed by the source's sending thread.
pub(crate) enum PreparedEvent {
    Chunks {
        capture_id: usize,
        chunks: Vec<([u8; 32], ChunkData)>,
    },
    Complete {
        capture_id: usize,
        bytes_read: u64,
        key: Vec<u8>,
        manifest: Manifest,
        /// Served from an earlier committed capture; nothing new to record.
        reused: bool,
    },
    Refused {
        capture_id: usize,
        bytes_read: u64,
        refusal: BulkloadRefusal,
    },
}

/// Work for a source store's pack committer.
pub(crate) enum PackItem {
    /// Chunks of an unfinished capture: appended to the pack at once, but
    /// indexed only when that capture completes.
    Chunks {
        capture_id: usize,
        chunks: Vec<([u8; 32], ChunkData)>,
    },
    /// A capture that passed its final stat check: its chunk locations and
    /// its record commit together, after the pack is sealed.
    Capture { key: Vec<u8>, manifest: Manifest },
    /// A capture that was refused: none of its chunks is indexed for it.
    Refused { capture_id: usize },
}

/// A chunk's place in the pack.
#[derive(Clone, Copy, Debug)]
struct Location {
    digest: [u8; 32],
    offset: u64,
    size: usize,
}

/// A published destination output, ready for its group commit.
pub(crate) struct OutputRecord {
    pub key: Vec<u8>,
    pub rel_path: Vec<u8>,
    pub identity: StatIdentity,
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
        // SAFETY: this guard owns a live descriptor for the acquired flock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Exclusive owner of pack offsets and durable capture publication.
pub(crate) struct StorePublisher {
    store: Store,
    pack: fs::File,
    pack_end: u64,
    /// Digests known to be held: appended this session or found indexed.
    known: HashSet<[u8; 32]>,
    _exclusive: Exclusive,
}

/// Which protocol half owns a store, for counter attribution only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Source,
    Destination,
    Unattributed,
}

/// A private, source-bound transfer state directory.
pub struct Store {
    root: PathBuf,
    conn: rusqlite::Connection,
    side: Side,
}

impl Store {
    /// Open or create a private transfer store outside the carried roots.
    ///
    /// # Errors
    /// Refuses symlinks, non-private directories and database failures.
    pub fn open(root: &Path) -> Result<Self> {
        private_dir(root)?;
        private_dir(&root.join("chunks"))?;
        let pack = root.join("chunks.pack");
        if let Ok(meta) = fs::symlink_metadata(&pack) {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&pack)?;
        }
        let db = root.join("transfer.sqlite");
        if let Ok(meta) = fs::symlink_metadata(&db) {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db)?;
        }
        let conn = rusqlite::Connection::open(db).map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_mins(1))
            .map_err(sqlite_error)?;
        crate::io::durable::configure_sqlite(&conn)?;
        let mut random = [0_u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let before = conn.total_changes();
        let started = Instant::now();
        conn.execute_batch("BEGIN").map_err(sqlite_error)?;
        let created = conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS captures (key BLOB PRIMARY KEY, manifest BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS outputs (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS directories (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS chunks (digest BLOB PRIMARY KEY, payload BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS chunk_locations (digest BLOB PRIMARY KEY, offset INTEGER NOT NULL, size INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS output_chunks (digest BLOB PRIMARY KEY, path BLOB NOT NULL, offset INTEGER NOT NULL, size INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
            )
            .and_then(|()| {
                conn.execute(
                    "INSERT OR IGNORE INTO settings VALUES ('authority', ?1)",
                    [random.as_slice()],
                )
            })
            .and_then(|_| conn.execute_batch("COMMIT"))
            .map_err(sqlite_error);
        if created.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else if conn.total_changes() != before {
            counters::sqlite_commit(Counter::SqliteSchema, started, &created);
        }
        created?;
        Ok(Self {
            root: fs::canonicalize(root)?,
            conn,
            side: Side::Unattributed,
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
        Ok(Self {
            root,
            conn,
            side: Side::Unattributed,
        })
    }

    /// Attribute this store's pack writes and publication commits to `side`.
    #[must_use]
    pub(crate) const fn with_side(mut self, side: Side) -> Self {
        self.side = side;
        self
    }

    /// Acquire the nonblocking single-writer guard and reconcile the pack tail.
    pub(crate) fn into_publisher(self) -> Result<StorePublisher> {
        StorePublisher::open(self)
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

    /// Retrieve a capture without reopening source content.
    ///
    /// # Errors
    /// Refuses malformed records or database errors.
    pub fn capture(&self, key: &[u8]) -> Result<Option<Manifest>> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT manifest FROM captures WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        bytes
            .map(|value| postcard::from_bytes(&value).map_err(Into::into))
            .transpose()
    }

    /// Commit only a completed identity-checked capture.
    ///
    /// # Errors
    /// Refuses serialization or database failures.
    pub fn record_capture(&self, key: &[u8], value: &Manifest) -> Result<()> {
        let encoded = postcard::to_stdvec(value)?;
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
        Ok(())
    }

    /// Whether a current output is the same object recorded on successful apply.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn output_matches(&self, key: &[u8], identity: &StatIdentity) -> Result<bool> {
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

    /// Commit one group of published outputs and their chunk hints in a single
    /// transaction. Callers seal every file and directory first.
    ///
    /// # Errors
    /// Refuses serialization or database failures; nothing is committed then.
    pub(crate) fn commit_outputs(&self, outputs: &[OutputRecord]) -> Result<()> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let staged = (|| -> Result<()> {
            for output in outputs {
                self.conn
                    .execute(
                        "INSERT INTO outputs VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                        (&output.key, identity_bytes(&output.identity)?),
                    )
                    .map_err(sqlite_error)?;
                for hint in &output.hints {
                    self.conn
                        .execute(
                            "INSERT INTO output_chunks (digest, path, offset, size)
                             VALUES (?1, ?2, ?3, ?4)
                             ON CONFLICT(digest) DO UPDATE SET
                                 path=excluded.path, offset=excluded.offset, size=excluded.size",
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
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        let started = Instant::now();
        let committed = self.conn.execute_batch("COMMIT").map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteGroupDest, started, &committed);
        if let Err(error) = committed {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        Ok(())
    }

    /// Where an earlier transfer wrote a chunk into a destination output. A
    /// hint only: the caller re-reads and re-verifies the bytes before use.
    ///
    /// # Errors
    /// Refuses database failures and malformed rows.
    pub(crate) fn output_chunk(&self, digest: &[u8; 32]) -> Result<Option<ChunkHintRow>> {
        let found: Option<(Vec<u8>, i64, i64)> = self
            .conn
            .query_row(
                "SELECT path, offset, size FROM output_chunks WHERE digest = ?1",
                [digest.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        found
            .map(|(path, offset, size)| {
                Ok(ChunkHintRow {
                    path,
                    offset: u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?,
                    size: u64::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?,
                })
            })
            .transpose()
    }

    /// Remember an unfinished directory by inode and its intended mode.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn pending_directory(
        &self,
        key: &[u8],
        dev: u64,
        ino: u64,
        mode: u32,
        record: bool,
    ) -> Result<bool> {
        let identity = postcard::to_stdvec(&(dev, ino, mode))?;
        if record {
            let started = Instant::now();
            let recorded = self
                .conn
                .execute(
                    "INSERT INTO directories VALUES (?1, ?2)
                ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                    (key, &identity),
                )
                .map_err(sqlite_error);
            counters::sqlite_commit(Counter::SqliteDirectoryPending, started, &recorded);
            recorded?;
            return Ok(true);
        }
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM directories WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(found == Some(identity))
    }

    /// Retire pending ownership after directory metadata is durable.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn complete_directory(&self, key: &[u8]) -> Result<()> {
        let started = Instant::now();
        let completed = self
            .conn
            .execute("DELETE FROM directories WHERE key = ?1", [key])
            .map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteDirectoryComplete, started, &completed);
        completed?;
        Ok(())
    }

    /// Read and authenticate a stored chunk. Missing or corrupt chunks are misses.
    ///
    /// # Errors
    /// Refuses unexpected filesystem failures.
    pub fn chunk(&self, digest: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        self.chunk_for(digest, Counter::OtherChunkRead)
    }

    /// [`Store::chunk`], counting bytes read under the caller's `stage`.
    ///
    /// # Errors
    /// Refuses unexpected filesystem failures.
    pub fn chunk_for(&self, digest: &[u8; 32], stage: Counter) -> Result<Option<Vec<u8>>> {
        let stored: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT payload FROM chunks WHERE digest = ?1",
                [digest.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(data) = stored {
            counters::add_len(stage, data.len());
            if data.len() > crate::hash::CDC_MAX_BYTES as usize
                || counters::hash(self.verify_purpose(), &data) != *digest
            {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            return Ok(Some(data));
        }
        let location: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT offset, size FROM chunk_locations WHERE digest = ?1",
                [digest.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some((offset, size)) = location {
            let offset = u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?;
            let size = usize::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?;
            if size > crate::hash::CDC_MAX_BYTES as usize {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            let mut pack = crate::hash::open_nofollow(&self.root.join("chunks.pack"))?;
            pack.seek(std::io::SeekFrom::Start(offset))?;
            let mut data = vec![0_u8; size];
            pack.read_exact(&mut data)?;
            counters::add_len(stage, data.len());
            if counters::hash(self.verify_purpose(), &data) != *digest {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            return Ok(Some(data));
        }
        Self::chunk_at(&self.root, digest, stage)
    }

    const fn verify_purpose(&self) -> Counter {
        match self.side {
            Side::Destination => Counter::HashDestReuse,
            Side::Source | Side::Unattributed => Counter::HashStoreReadVerify,
        }
    }

    fn chunk_at(root: &Path, digest: &[u8; 32], stage: Counter) -> Result<Option<Vec<u8>>> {
        let path = root.join("chunks").join(hex(digest));
        let file = match crate::hash::open_nofollow(&path) {
            Ok(file) => file,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => return Ok(None),
            Err(error) => return Err(error),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > u64::from(crate::hash::CDC_MAX_BYTES) {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        let mut data = Vec::new();
        file.take(u64::from(crate::hash::CDC_MAX_BYTES) + 1)
            .read_to_end(&mut data)?;
        counters::add_len(stage, data.len());
        if counters::hash(Counter::HashStoreReadVerify, &data) != *digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        Ok(Some(data))
    }

    /// Publish an authenticated chunk atomically without replacing existing data.
    ///
    /// # Errors
    /// Refuses a digest mismatch or failed durable publication.
    pub fn put_chunk(&self, digest: &[u8; 32], data: &[u8]) -> Result<()> {
        Self::put_chunk_at(&self.root, digest, data, true)
    }

    fn put_chunk_at(
        root: &Path,
        digest: &[u8; 32],
        data: &[u8],
        sync_directory: bool,
    ) -> Result<()> {
        PUT_CALLS.fetch_add(1, Ordering::Relaxed);
        let _timer = PutTimer(Instant::now());
        if data.len() > crate::hash::CDC_MAX_BYTES as usize
            || counters::hash(Counter::HashLegacyPut, data) != *digest
        {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        if Self::chunk_at(root, digest, Counter::OtherChunkRead)?.is_some() {
            // A concurrent publisher may have linked the already-synced inode
            // but not yet synced its directory. Fence that link before reuse.
            if sync_directory {
                timed_sync(
                    &fs::File::open(root.join("chunks"))?,
                    &DIR_SYNCS,
                    &DIR_SYNC_NS,
                    true,
                )?;
            }
            return Ok(());
        }
        let chunks = root.join("chunks");
        let staging = chunks.join(format!(
            ".part-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let result = (|| -> Result<()> {
            file.write_all(data)?;
            counters::add_len(Counter::LegacyChunkWrite, data.len());
            timed_sync(&file, &FILE_SYNCS, &FILE_SYNC_NS, false)?;
            let target = chunks.join(hex(digest));
            match fs::hard_link(&staging, &target) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Self::chunk_at(root, digest, Counter::OtherChunkRead)?
                        .ok_or(BulkloadRefusal::DigestMismatch)?;
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })();
        fs::remove_file(staging)?;
        if sync_directory {
            timed_sync(&fs::File::open(chunks)?, &DIR_SYNCS, &DIR_SYNC_NS, true)?;
        }
        result
    }
}

impl StorePublisher {
    fn open(store: Store) -> Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store.root.join("writer.lock"))?;
        // SAFETY: the owned descriptor remains open for the guard lifetime.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let exclusive = Exclusive(lock);
        let pack = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store.root.join("chunks.pack"))?;
        let pack_len = pack.metadata()?.len();
        let mut committed_end = 0_u64;
        {
            let mut statement = store
                .conn
                .prepare("SELECT offset, size FROM chunk_locations ORDER BY offset, size")
                .map_err(sqlite_error)?;
            let ranges = statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .map_err(sqlite_error)?;
            for range in ranges {
                let (offset, size) = range.map_err(sqlite_error)?;
                let offset = u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?;
                let size = u64::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?;
                let end = offset
                    .checked_add(size)
                    .ok_or(BulkloadRefusal::DigestMismatch)?;
                if size > u64::from(crate::hash::CDC_MAX_BYTES)
                    || offset < committed_end
                    || end > pack_len
                {
                    return Err(BulkloadRefusal::DigestMismatch);
                }
                committed_end = end;
            }
        }
        if pack_len > committed_end {
            pack.set_len(committed_end)?;
            timed_sync(&pack, &FILE_SYNCS, &FILE_SYNC_NS, false)?;
        }
        Ok(Self {
            store,
            pack,
            pack_end: committed_end,
            known: HashSet::new(),
            _exclusive: exclusive,
        })
    }

    /// The store this publisher writes.
    pub(crate) const fn store(&self) -> &Store {
        &self.store
    }

    /// Whether the pack, the index or the legacy chunk directory holds `digest`,
    /// counting chunks appended earlier in this session.
    fn contains(&mut self, digest: &[u8; 32]) -> Result<bool> {
        if self.known.contains(digest) {
            return Ok(true);
        }
        let indexed: bool = self
            .store
            .conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM chunks WHERE digest = ?1
                    UNION ALL
                    SELECT 1 FROM chunk_locations WHERE digest = ?1
                )",
                [digest.as_slice()],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        let present = indexed
            || Store::chunk_at(&self.store.root, digest, Counter::OtherChunkRead)?.is_some();
        if present {
            self.known.insert(*digest);
        }
        Ok(present)
    }

    /// Stop treating an appended but never-indexed chunk as held.
    fn forget(&mut self, digest: &[u8; 32]) {
        self.known.remove(digest);
    }

    /// Append one chunk unless the store already holds it. The bytes were
    /// hashed by this process when they were captured, so they are not hashed
    /// again here.
    fn append(&mut self, digest: &[u8; 32], data: &[u8]) -> Result<Option<Location>> {
        if data.len() > crate::hash::CDC_MAX_BYTES as usize {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        if self.contains(digest)? {
            return Ok(None);
        }
        let offset = self.pack_end;
        self.pack.write_all_at(data, offset)?;
        self.pack_end = offset
            .checked_add(data.len() as u64)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        self.known.insert(*digest);
        counters::add_len(
            match self.store.side {
                Side::Source => Counter::SourcePackWrite,
                Side::Destination => Counter::DestPackWrite,
                Side::Unattributed => Counter::OtherPackWrite,
            },
            data.len(),
        );
        Ok(Some(Location {
            digest: *digest,
            offset,
            size: data.len(),
        }))
    }

    /// Seal every appended byte ahead of the records that will point at it.
    fn seal(&self) -> Result<()> {
        FILE_SYNCS.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let sealed = crate::io::durable::seal_file(&self.pack);
        FILE_SYNC_NS.fetch_add(nanos(started), Ordering::Relaxed);
        Ok(sealed?)
    }

    /// Expose sealed chunk locations and completed captures in one transaction.
    fn commit(&self, locations: &[Location], captures: &[(Vec<u8>, Manifest)]) -> Result<()> {
        if locations.is_empty() && captures.is_empty() {
            return Ok(());
        }
        self.store
            .conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let persisted = (|| -> Result<()> {
            for location in locations {
                self.store
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO chunk_locations (digest, offset, size)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![
                            location.digest.as_slice(),
                            i64::try_from(location.offset)
                                .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                            i64::try_from(location.size)
                                .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                        ],
                    )
                    .map_err(sqlite_error)?;
            }
            publication_fault!(AfterLocationInsert);
            for (key, manifest) in captures {
                self.store
                    .conn
                    .execute(
                        "INSERT INTO captures VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                        (key, postcard::to_stdvec(manifest)?),
                    )
                    .map_err(sqlite_error)?;
            }
            publication_fault!(AfterManifestInsert);
            Ok(())
        })();
        if let Err(error) = persisted {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
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
        counters::sqlite_commit(Counter::SqliteGroupSource, commit_started, &committed);
        if let Err(error) = committed {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        Ok(())
    }
}

/// Group-commit sink for a source store: captured chunks are appended to the
/// pack as they arrive, and each group seals the pack before committing the
/// chunk locations and the captures that reference them.
pub(crate) struct PackSink {
    publisher: StorePublisher,
    /// Appended but unindexed chunks, by digest, with the capture that
    /// appended them. A location is indexed only with a completed capture.
    pending: HashMap<[u8; 32], (usize, Location)>,
    failed: Option<BulkloadRefusal>,
}

impl PackSink {
    pub(crate) fn new(publisher: StorePublisher) -> Self {
        Self {
            publisher,
            pending: HashMap::new(),
            failed: None,
        }
    }

    /// Append, seal, then commit one group. A capture commits only once every
    /// chunk it names is in the pack, and no chunk location is indexed before
    /// a capture that names it has passed its final stat check. Pack bytes of
    /// a refused capture stay in the pack, unindexed.
    pub(crate) fn publish(&mut self, items: Vec<PackItem>) -> Result<()> {
        PUBLISH_GROUPS.fetch_add(1, Ordering::Relaxed);
        let append_started = Instant::now();
        let mut locations = Vec::new();
        let mut captures = Vec::new();
        for item in items {
            match item {
                PackItem::Chunks { capture_id, chunks } => {
                    for (digest, data) in chunks {
                        if let Some(location) = self.publisher.append(&digest, &data)? {
                            self.pending.insert(digest, (capture_id, location));
                        }
                    }
                }
                PackItem::Capture { key, manifest } => {
                    // Index every chunk this capture names that is still
                    // pending, whichever capture appended it first.
                    for chunk in &manifest.chunks {
                        if let Some((_, location)) = self.pending.remove(&chunk.digest) {
                            locations.push(location);
                        }
                    }
                    captures.push((key, manifest));
                }
                PackItem::Refused { capture_id } => {
                    let dropped: Vec<_> = self
                        .pending
                        .iter()
                        .filter(|(_, (owner, _))| *owner == capture_id)
                        .map(|(digest, _)| *digest)
                        .collect();
                    for digest in dropped {
                        self.pending.remove(&digest);
                        self.publisher.forget(&digest);
                    }
                }
            }
        }
        PACK_APPEND_NS.fetch_add(nanos(append_started), Ordering::Relaxed);
        publication_fault!(AfterAppend);
        if !locations.is_empty() {
            self.publisher.seal()?;
        }
        publication_fault!(AfterSync);
        for (_, manifest) in &captures {
            for chunk in &manifest.chunks {
                if chunk.size > u64::from(crate::hash::CDC_MAX_BYTES) {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                if !self.publisher.contains(&chunk.digest)? {
                    return Err(BulkloadRefusal::SealedObjectMissing);
                }
            }
        }
        self.publisher.commit(&locations, &captures)
    }
}

impl crate::io::durable::GroupSink for PackSink {
    type Item = PackItem;
    type Report = Result<()>;

    fn weight(item: &PackItem) -> (u64, u64) {
        match item {
            PackItem::Chunks { chunks, .. } => (
                0,
                chunks.iter().fold(0_u64, |total, (_, data)| {
                    total.saturating_add(data.len() as u64)
                }),
            ),
            PackItem::Capture { .. } => (1, 0),
            PackItem::Refused { .. } => (0, 0),
        }
    }

    fn commit(&mut self, items: Vec<PackItem>) {
        if self.failed.is_none() {
            if let Err(refusal) = self.publish(items) {
                self.failed = Some(refusal);
            }
        }
    }

    fn finish(self) -> Result<()> {
        self.failed.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
fn persist_batch_with<F>(chunks: &[([u8; 32], Vec<u8>)], persist: &F) -> Result<()>
where
    F: Fn(&[u8; 32], &[u8]) -> Result<()> + Sync,
{
    if chunks.len() > PERSIST_BATCH {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if chunks
        .iter()
        .any(|(_, data)| data.len() > crate::hash::CDC_MAX_BYTES as usize)
    {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    if chunks.is_empty() {
        return Ok(());
    }
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for group in chunks.chunks(chunks.len().div_ceil(PERSIST_WORKERS)) {
            workers.push(std::thread::Builder::new().spawn_scoped(scope, move || {
                for (digest, data) in group {
                    persist(digest, data)?;
                }
                Ok(())
            })?);
        }
        let mut outcome = Ok(());
        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| BulkloadRefusal::Io(None))
                .and_then(|result| result);
            // Join even after an earlier refusal; no background writes escape.
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome
    })
}

/// Bind a row to its source root identity and destination namespace.
///
/// # Errors
/// Refuses serialization failure.
pub fn row_key(authority: &[u8], row: &RowSchema) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(&(authority, row))?)
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

fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && meta.permissions().mode().trailing_zeros() >= 6 => Ok(()),
        Ok(_) => Err(BulkloadRefusal::PathEscapesRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut value, byte| {
        let _ = write!(value, "{byte:02x}");
        value
    })
}

fn sqlite_error(_: rusqlite::Error) -> BulkloadRefusal {
    BulkloadRefusal::SqliteIntegrityCheckFailed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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

    fn publication(data: &[u8]) -> (Vec<PackItem>, Manifest) {
        let digest = crate::hash::hash_bytes(data);
        let manifest = Manifest {
            digest,
            chunks: vec![
                ChunkSpec {
                    digest,
                    size: data.len() as u64,
                },
                ChunkSpec {
                    digest,
                    size: data.len() as u64,
                },
            ],
        };
        let shared = Arc::new(data.to_vec());
        (
            vec![
                PackItem::Chunks {
                    capture_id: 7,
                    chunks: vec![(digest, Arc::clone(&shared)), (digest, shared)],
                },
                PackItem::Capture {
                    key: b"capture".to_vec(),
                    manifest: manifest.clone(),
                },
            ],
            manifest,
        )
    }

    fn pack_sink(state: &Path) -> Result<PackSink> {
        Ok(PackSink::new(Store::open(state)?.into_publisher()?))
    }

    #[test]
    fn batch_joins_success_and_refusal_before_returning() -> Result<()> {
        let chunks = vec![([0; 32], vec![0]), ([1; 32], vec![1])];
        let finished = AtomicUsize::new(0);
        persist_batch_with(&chunks, &|_, _| {
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        assert_eq!(finished.load(Ordering::SeqCst), 2);
        finished.store(0, Ordering::SeqCst);
        let failed = persist_batch_with(&chunks, &|digest, _| {
            if digest.first() == Some(&0) {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(failed, Err(BulkloadRefusal::DigestMismatch));
        // The second worker finishes even if the first worker failed first.
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        Ok(())
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

    #[test]
    fn group_deduplicates_chunks_and_preserves_manifest_order() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let first = Arc::new(b"first persisted chunk".to_vec());
        let second = Arc::new(b"second persisted chunk".to_vec());
        let first_digest = crate::hash::hash_bytes(&first);
        let second_digest = crate::hash::hash_bytes(&second);
        let manifest = Manifest {
            digest: crate::hash::hash_bytes(b"capture"),
            chunks: vec![
                ChunkSpec {
                    digest: first_digest,
                    size: first.len() as u64,
                },
                ChunkSpec {
                    digest: first_digest,
                    size: first.len() as u64,
                },
                ChunkSpec {
                    digest: second_digest,
                    size: second.len() as u64,
                },
            ],
        };
        let mut sink = pack_sink(&state)?;
        sink.publish(vec![
            PackItem::Chunks {
                capture_id: 1,
                chunks: vec![
                    (first_digest, Arc::clone(&first)),
                    (first_digest, Arc::clone(&first)),
                ],
            },
            PackItem::Chunks {
                capture_id: 1,
                chunks: vec![
                    (first_digest, Arc::clone(&first)),
                    (second_digest, Arc::clone(&second)),
                ],
            },
            PackItem::Capture {
                key: b"capture".to_vec(),
                manifest,
            },
        ])?;
        drop(sink);

        let reopened = Store::open(&state)?;
        let captured = reopened
            .capture(b"capture")?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        assert_eq!(captured.chunks.len(), 3);
        assert_eq!(
            captured.chunks.first().map(|chunk| chunk.digest),
            Some(first_digest)
        );
        assert_eq!(
            captured.chunks.get(1).map(|chunk| chunk.digest),
            Some(first_digest)
        );
        assert_eq!(
            captured.chunks.get(2).map(|chunk| chunk.digest),
            Some(second_digest)
        );
        assert_eq!(reopened.chunk(&first_digest)?.as_ref(), Some(&*first));
        assert_eq!(reopened.chunk(&second_digest)?.as_ref(), Some(&*second));
        assert_eq!(
            fs::metadata(reopened.root.join("chunks.pack"))?.len(),
            (first.len() + second.len()) as u64
        );
        Ok(())
    }

    #[test]
    fn a_capture_never_commits_ahead_of_its_chunks() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let (_, manifest) = publication(b"never appended");
        let mut sink = pack_sink(&state)?;
        assert_eq!(
            sink.publish(vec![PackItem::Capture {
                key: b"capture".to_vec(),
                manifest,
            }]),
            Err(BulkloadRefusal::SealedObjectMissing)
        );
        drop(sink);
        assert!(Store::open(&state)?.capture(b"capture")?.is_none());
        Ok(())
    }

    #[test]
    fn a_refused_capture_indexes_none_of_its_chunks() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let (items, _) = publication(b"refused before its final stat check");
        let digest = crate::hash::hash_bytes(b"refused before its final stat check");
        let mut sink = pack_sink(&state)?;
        let mut items = items.into_iter();
        let chunks = items.next().ok_or(BulkloadRefusal::SealedObjectMissing)?;
        // The chunks commit in a group of their own, then the capture is refused.
        sink.publish(vec![chunks])?;
        sink.publish(vec![PackItem::Refused { capture_id: 7 }])?;
        drop(sink);
        let store = Store::open(&state)?;
        assert!(store.capture(b"capture")?.is_none());
        assert!(store.chunk(&digest)?.is_none());
        // A later capture of the same bytes appends and indexes them afresh.
        let (retry, _) = publication(b"refused before its final stat check");
        let mut sink = pack_sink(&state)?;
        sink.publish(retry)?;
        drop(sink);
        assert!(Store::open(&state)?.chunk(&digest)?.is_some());
        Ok(())
    }

    #[test]
    fn publisher_is_exclusive_and_reconciles_unindexed_tail() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher()?;
        assert!(Store::open(&state)?.into_publisher().is_err());
        drop(publisher);

        let mut pack = OpenOptions::new()
            .append(true)
            .open(state.join("chunks.pack"))?;
        pack.write_all(b"unindexed tail")?;
        pack.sync_all()?;
        drop(pack);
        let reconciled = Store::open(&state)?.into_publisher()?;
        assert_eq!(fs::metadata(state.join("chunks.pack"))?.len(), 0);
        drop(reconciled);
        Ok(())
    }

    #[test]
    fn publisher_refuses_indexed_ranges_beyond_pack() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let (items, _) = publication(b"indexed content");
        let mut sink = pack_sink(&state)?;
        sink.publish(items)?;
        drop(sink);
        OpenOptions::new()
            .write(true)
            .open(state.join("chunks.pack"))?
            .set_len(0)?;
        assert!(matches!(
            Store::open(&state)?.into_publisher(),
            Err(BulkloadRefusal::DigestMismatch)
        ));
        Ok(())
    }

    #[test]
    fn interrupted_publication_hides_manifest_and_retries_cleanly() -> Result<()> {
        for fault in [
            PublishFault::AfterAppend,
            PublishFault::AfterSync,
            PublishFault::AfterLocationInsert,
            PublishFault::AfterManifestInsert,
            PublishFault::BeforeCommit,
        ] {
            let root = TestRoot::new()?;
            let state = root.0.join("state");
            let (items, manifest) = publication(b"fault recovery content");
            let mut sink = pack_sink(&state)?;
            PUBLISH_FAULT.with(|active| active.set(fault));
            assert!(matches!(
                sink.publish(items),
                Err(BulkloadRefusal::Io(None))
            ));
            PUBLISH_FAULT.with(|active| active.set(PublishFault::None));
            assert!(Store::open(&state)?.capture(b"capture")?.is_none());
            drop(sink);

            let mut sink = pack_sink(&state)?;
            assert_eq!(fs::metadata(state.join("chunks.pack"))?.len(), 0);
            let (retry, _) = publication(b"fault recovery content");
            sink.publish(retry)?;
            drop(sink);
            assert_eq!(
                Store::open(&state)?
                    .capture(b"capture")?
                    .ok_or(BulkloadRefusal::SealedObjectMissing)?
                    .digest,
                manifest.digest
            );
        }
        Ok(())
    }

    #[test]
    fn output_records_and_chunk_hints_commit_together() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        let file = root.0.join("output");
        fs::write(&file, b"output")?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file)?);
        let hint = ChunkHint {
            digest: [3; 32],
            offset: 5,
            size: 7,
        };
        store.commit_outputs(&[OutputRecord {
            key: b"key".to_vec(),
            rel_path: b"nested/output".to_vec(),
            identity,
            hints: vec![hint],
        }])?;
        let reopened = Store::open(&state)?;
        assert!(reopened.output_matches(b"key", &identity)?);
        let found = reopened
            .output_chunk(&[3; 32])?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        assert_eq!(
            (found.path.as_slice(), found.offset, found.size),
            (b"nested/output".as_slice(), 5, 7)
        );
        assert!(reopened.output_chunk(&[4; 32])?.is_none());
        Ok(())
    }

    #[test]
    fn oversized_batch_refuses_before_any_write() {
        let chunks = vec![([0; 32], Vec::new()); PERSIST_BATCH + 1];
        let calls = AtomicUsize::new(0);
        let result = persist_batch_with(&chunks, &|_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(result, Err(BulkloadRefusal::BudgetExceeded));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let oversized = vec![([0; 32], vec![0; crate::hash::CDC_MAX_BYTES as usize + 1])];
        assert_eq!(
            persist_batch_with(&oversized, &|_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Err(BulkloadRefusal::DigestMismatch)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
