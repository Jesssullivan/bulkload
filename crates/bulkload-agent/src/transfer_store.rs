//! Private, durable transfer state: the source's digest-only capture ledger
//! and the destination's output records and chunk hints.
//!
//! Wire v5 keeps no byte pack on either side (R-N58). A source capture is a
//! row key and its chunk manifest (digests and sizes, `manifest_root`), never
//! its bytes; the destination re-reads chunks only from published outputs,
//! through hints it re-verifies on use.

use crate::refuse::RefuseAt as _;
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

/// Destination store roots whose output group commits alone fail: a
/// superseding publish's intent still commits, so its exchange happens and
/// the commit of the new output's row is what fails.
#[cfg(test)]
static FAIL_GROUP_COMMITS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Test hook: while `on`, every `commit_outputs` of the store at `root`
/// (canonical) fails with `ENOSPC` before `COMMIT`, and rolls back;
/// `begin_supersedes` is left alone (compare [`fail_output_commits`]).
#[cfg(test)]
pub(crate) fn fail_group_commits(root: &Path, on: bool) {
    let mut roots = FAIL_GROUP_COMMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    roots.retain(|failing| failing != root);
    if on {
        roots.push(root.to_path_buf());
    }
}

#[cfg(test)]
fn group_commit_fault(root: &Path) -> Result<()> {
    let failing = FAIL_GROUP_COMMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|failing| failing == root);
    if failing {
        return Err(BulkloadRefusal::Io(Some(libc::ENOSPC)));
    }
    output_commit_fault(root)
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

/// One record for the source ledger committer: a completed capture, or a
/// seat's refusal to remember (#186).
pub(crate) struct LedgerItem {
    /// The entry the record belongs to, for crash receipts.
    #[cfg_attr(
        not(feature = "fault-injection"),
        allow(dead_code, reason = "read only by fault-injection crash receipts")
    )]
    pub entry: u64,
    pub key: Vec<u8>,
    pub record: LedgerRecord,
}

/// What a [`LedgerItem`] records under its row key.
pub(crate) enum LedgerRecord {
    /// A capture's manifest. Only a capture that passed its final stat check
    /// is ever submitted (R-N86).
    Capture(Manifest),
    /// The seat was refused for what its header holds (#186).
    Refused(RefusedSeat),
}

/// Why the source refused a seat for its content (#186, R25).
///
/// It is remembered under the seat's row key (its path and stat identity),
/// so that a rerun refuses the seat again without opening the file. Only a
/// refusal that depends on nothing but the seat's bytes is remembered, and
/// only when the seat was not racy when it was sniffed: its stat identity
/// then vouches for those bytes, exactly as it does for a capture (#86).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusedSeat {
    /// The file starts with a `SQLite` database or WAL magic: provider state,
    /// which only the `SQLite` backup path carries.
    SqliteHeader,
}

impl RefusedSeat {
    /// The stored form. Never reuse a retired value.
    const fn tag(self) -> i64 {
        match self {
            Self::SqliteHeader => 1,
        }
    }

    const fn from_tag(tag: i64) -> Option<Self> {
        match tag {
            1 => Some(Self::SqliteHeader),
            _ => None,
        }
    }

    /// The refusal this record stands for, with the code the sniff gave.
    #[must_use]
    pub const fn refusal(self) -> BulkloadRefusal {
        match self {
            Self::SqliteHeader => BulkloadRefusal::SqliteStateChanged,
        }
    }
}

/// Why the destination refused a seat's entry for what its path holds
/// (#187 review, R25).
///
/// It is remembered in the destination store under the entry's row key (the
/// seat's path and stat identity) together with the stat identity of the
/// file found at the path. While both are unchanged the answer cannot
/// change, so a rerun refuses the entry when it is offered, with the same
/// code, and the source reads nothing for it. It is remembered only when
/// the capture was not racy (#86) and the file at the path was settled: the
/// same before and after it was read, and not stamped within
/// [`crate::transfer::RACY_GRANULARITY_NS`] of that read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusedOutput {
    /// The path holds a file this store does not own, with other bytes
    /// than the seat's.
    Occupied,
    /// The path holds this store's own output with other bytes than the
    /// seat's, on a file system with no atomic exchange.
    ExchangeUnsupported,
}

impl RefusedOutput {
    /// The stored form. Never reuse a retired value.
    const fn tag(self) -> i64 {
        match self {
            Self::Occupied => 1,
            Self::ExchangeUnsupported => 2,
        }
    }

    const fn from_tag(tag: i64) -> Option<Self> {
        match tag {
            1 => Some(Self::Occupied),
            2 => Some(Self::ExchangeUnsupported),
            _ => None,
        }
    }

    /// The refusal this record stands for.
    #[must_use]
    pub const fn refusal(self) -> BulkloadRefusal {
        match self {
            Self::Occupied => BulkloadRefusal::DestinationOccupied,
            Self::ExchangeUnsupported => BulkloadRefusal::DestinationExchangeUnsupported,
        }
    }
}

/// A published destination output, ready for its group commit.
pub(crate) struct OutputRecord {
    pub key: Vec<u8>,
    pub rel_path: Vec<u8>,
    pub identity: StatIdentity,
    /// The source seat was racy when captured (#86): its stat identity
    /// cannot vouch for these bytes, so no output row is kept under `key`
    /// and the next run asks for a manifest instead of reusing it. Its
    /// chunk hints are still recorded; they are re-verified on use. The
    /// output still gets an ownership row ([`owner_key`]): this store
    /// published it, so a later change of its seat supersedes it.
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

/// An output row as stored: its row key and its encoded stat identity.
pub(crate) type OutputRow = (Vec<u8>, Vec<u8>);

/// A superseding publish in flight (WP0(d), OI-1003-Q18, #187): this store
/// is about to exchange the staged file `temp` with its own output `leaf`,
/// both directly inside the destination directory `dir`.
///
/// The record commits before the exchange, in the transaction that moves the
/// output's rows out of `outputs` and into it, so from then until it is
/// settled no row vouches for the path, whichever file a power loss leaves
/// there, and the record alone says what each of the two names may hold:
///
/// - `staged`, the new file, at `temp` (the exchange did not take effect) or
///   at `leaf` (it did);
/// - `owned`, the output this store's rows named, at `leaf` or, displaced,
///   at `temp`;
/// - anything else at `temp` is a file this store does not own, displaced
///   by the exchange: it is exchanged back, never removed.
///
/// Settling deletes the record: with the new output's row, in its group's
/// commit; or, when the exchange did not happen and the output is still
/// exactly `owned`, with `rows` put back, so the old output has its old row
/// again. The next session's sweep settles what a crash left
/// (`materialize::Destination::sweep`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SupersedeIntent {
    /// The destination-relative directory holding both names; empty for the
    /// destination root.
    pub dir: Vec<u8>,
    /// The staged file's temporary leaf name.
    pub temp: Vec<u8>,
    /// The output's leaf name.
    pub leaf: Vec<u8>,
    /// Device and inode of the staged file.
    pub staged: (u64, u64),
    /// Size and mtime of the staged file, sealed, before the exchange. The
    /// exchange moves neither, so a file at the leaf with `staged`'s inode
    /// and this stamp is the file this store staged, not written since
    /// (its ctime the exchange itself moved).
    pub stamp: (u64, i128),
    /// The identity this store's row recorded for the output it replaces.
    pub owned: StatIdentity,
    /// The output rows moved out of `outputs` when this record committed.
    pub rows: Vec<OutputRow>,
}

impl SupersedeIntent {
    /// The output's destination-relative path.
    #[cfg_attr(
        not(feature = "io-trace"),
        allow(dead_code, reason = "read only by the R-N88 trace's commit records")
    )]
    #[must_use]
    pub fn rel_path(&self) -> Vec<u8> {
        join_rel(&self.dir, &self.leaf)
    }

    /// The temporary's destination-relative path.
    #[must_use]
    pub fn temp_path(&self) -> Vec<u8> {
        join_rel(&self.dir, &self.temp)
    }
}

fn join_rel(dir: &[u8], leaf: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(leaf);
    path
}

/// How a superseding publish ended, for the commit that settles its record.
pub(crate) struct SupersedeSettle<'a> {
    pub intent: &'a SupersedeIntent,
    /// The exchange did not happen and the output is still this store's
    /// own: its rows go back into `outputs`.
    pub restore: bool,
    /// The exchange happened but the new file's row did not commit with it
    /// (a crash, or a failed group commit): the leaf holds the file this
    /// store staged, with this identity now. It gets an ownership row
    /// ([`owner_key`]), so it is still this store's own: adopted if its
    /// seat is unchanged, superseded if the seat changed again.
    pub owned: Option<StatIdentity>,
}

impl<'a> SupersedeSettle<'a> {
    /// A publish settled by its own group: its record goes, and with
    /// `restore` the old output's rows come back.
    pub(crate) const fn of(intent: &'a SupersedeIntent, restore: bool) -> Self {
        Self {
            intent,
            restore,
            owned: None,
        }
    }
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
    /// How this publisher's ledger row commits reach disk (WP0(g)). `Full`
    /// until [`StorePublisher::relax_ledger_rows`] relaxes a source
    /// publisher; a destination publisher is never relaxed.
    ledger_sync: crate::io::durable::LedgerSync,
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
    /// created before #161) is sealed again on open. A sealed store reopens
    /// without opening the root's parent at all.
    ///
    /// # Errors
    /// Refuses symlinks, non-private directories, flush and database failures.
    /// A root that still needs its seal under a parent the agent may search
    /// but not read refuses with `IO` (`EACCES`): its entry cannot be made
    /// durable, so no record may commit in it.
    pub fn open(root: &Path) -> Result<Self> {
        let mut state = StateRoot::open(root)?;
        let name = c"transfer.sqlite";
        // Created through `io::sys`, so the R-N88 trace sees the entry the
        // root seal below makes durable.
        match crate::io::sys::create_excl_at(state.root.as_fd(), name, 0o600) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let found = crate::io::sys::fstatat_nofollow(&state.root, name)
                    .refuse_at("transfer_store::open")?;
                if !found.is_file() || found.mode & 0o077 != 0 {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
            }
            Err(error) => return Err(crate::refuse::io(&error, "transfer_store::open")),
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
            state.seal()?;
        }
        let mut random = [0_u8; 32];
        fs::File::open("/dev/urandom")
            .refuse_at("transfer_store::open")?
            .read_exact(&mut random)
            .refuse_at("transfer_store::open")?;
        let before = conn.total_changes();
        let started = Instant::now();
        // The trace's serial lock before SQLite's write lock (D5): the
        // marker's commit is traced below, in the order it returned.
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
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
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS refused_seats (key BLOB PRIMARY KEY, kind INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS supersedes (dir BLOB NOT NULL, temp BLOB NOT NULL, intent BLOB NOT NULL, PRIMARY KEY (dir, temp));
                CREATE TABLE IF NOT EXISTS refused_outputs (key BLOB PRIMARY KEY, kind INTEGER NOT NULL, identity BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS owned_outputs (key BLOB PRIMARY KEY, identity BLOB NOT NULL);",
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
                // A remembered refusal (#186) is trusted on the same terms.
                // The table is younger than the guard, so this deletes
                // nothing a guarded engine wrote; it is not counted.
                conn.execute("DELETE FROM refused_seats", [])?;
                conn.execute("DELETE FROM refused_outputs", [])?;
                conn.execute("DELETE FROM owned_outputs", [])?;
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
            .and_then(|()| {
                conn.execute_batch("COMMIT")?;
                // R-N88: the marker's commit is traced the moment it returns,
                // so the trace orders it against the seal as it happened.
                #[cfg(feature = "io-trace")]
                {
                    if !sealed {
                        trace_root_sealed(&state.root);
                    }
                }
                Ok(())
            })
            .map_err(sqlite_error);
        if created.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else if conn.total_changes() != before {
            counters::sqlite_commit(Counter::SqliteSchema, started, &created);
        }
        created?;
        counters::add_len(Counter::TransferLegacyRowsInvalidated, invalidated);
        Ok(Self {
            root: fs::canonicalize(root).refuse_at("transfer_store::open")?,
            conn,
            rows_trusted: true,
        })
    }

    /// Open an initialized store without obtaining any write capability.
    pub(crate) fn open_reader(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).refuse_at("transfer_store::open_reader")?;
        let db = root.join("transfer.sqlite");
        let meta = fs::symlink_metadata(&db).refuse_at("transfer_store::open_reader")?;
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

    /// The remembered refusal of the seat under a row key, if any (#186):
    /// the source answers it without opening the file. A row this engine
    /// does not know (a later engine's kind) is a miss, and the seat is
    /// sniffed again.
    ///
    /// # Errors
    /// Refuses database errors.
    pub fn refused_seat(&self, key: &[u8]) -> Result<Option<RefusedSeat>> {
        if !self.rows_trusted {
            return Ok(None);
        }
        let kind: Option<i64> = self
            .conn
            .query_row(
                "SELECT kind FROM refused_seats WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(kind.and_then(RefusedSeat::from_tag))
    }

    /// The destination's remembered refusal of the entry under a row key,
    /// if the file at its path still has the identity it was refused with
    /// (see [`RefusedOutput`]). A row this engine does not know is a miss.
    ///
    /// # Errors
    /// Refuses database errors.
    pub fn refused_output(
        &self,
        key: &[u8],
        identity: &StatIdentity,
    ) -> Result<Option<RefusedOutput>> {
        if !self.rows_trusted {
            return Ok(None);
        }
        let found: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT kind, identity FROM refused_outputs WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let current = identity_bytes(identity)?;
        Ok(found
            .filter(|(_, held)| *held == current)
            .and_then(|(kind, _)| RefusedOutput::from_tag(kind)))
    }

    /// Remember the destination's refusal of the entry under a row key,
    /// bound to the identity of the file at its path (see
    /// [`RefusedOutput`]). The record claims nothing about any file's
    /// bytes being durable; losing it costs one more source read.
    ///
    /// # Errors
    /// Refuses serialization or database failures.
    pub fn remember_refused_output(
        &self,
        key: &[u8],
        identity: &StatIdentity,
        refused: RefusedOutput,
    ) -> Result<()> {
        let identity = identity_bytes(identity)?;
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        let started = Instant::now();
        let recorded = self
            .conn
            .execute(
                "INSERT INTO refused_outputs VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET kind=excluded.kind, identity=excluded.identity",
                (key, refused.tag(), identity),
            )
            .map_err(sqlite_error);
        counters::sqlite_commit(Counter::SqliteRefusedOutput, started, &recorded);
        recorded?;
        Ok(())
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

    /// How many rows `table` holds.
    #[cfg(test)]
    pub(crate) fn conn_count(&self, table: &str) -> Result<u64> {
        self.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sqlite_error)
            .and_then(|count| u64::try_from(count).map_err(|_| BulkloadRefusal::SchemaMismatch))
    }

    /// This connection's `PRAGMA synchronous`: 2 is FULL, 1 is NORMAL.
    #[cfg(test)]
    pub(crate) fn synchronous(&self) -> Result<i64> {
        self.conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .map_err(sqlite_error)
    }

    /// How many refusals the store remembers (#186).
    #[cfg(test)]
    pub(crate) fn refused_seats(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM refused_seats", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sqlite_error)
            .and_then(|count| u64::try_from(count).map_err(|_| BulkloadRefusal::SchemaMismatch))
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
        let encoded = postcard::to_stdvec(value).refuse_at("transfer_store::record_capture")?;
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

    /// The output rows this store holds for one destination path under
    /// `authority`, whatever source stat identity each was recorded from
    /// (WP0(d), #187): the row keys of a path share the prefix
    /// [`path_prefix`], so this is one range read of the primary key. The
    /// path's ownership row ([`owner_key`]) comes last, under the bare
    /// prefix.
    ///
    /// A file at that path is this store's own, untouched since, exactly
    /// when its current identity equals one of these rows' identities.
    ///
    /// # Errors
    /// Refuses database failures.
    pub(crate) fn output_rows(&self, authority: &[u8], rel_path: &[u8]) -> Result<Vec<OutputRow>> {
        if !self.rows_trusted {
            return Ok(Vec::new());
        }
        let prefix = path_prefix(authority, rel_path)?;
        let Some(end) = prefix_end(&prefix) else {
            return Ok(Vec::new());
        };
        let mut statement = self
            .conn
            .prepare_cached("SELECT key, identity FROM outputs WHERE key >= ?1 AND key < ?2")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map((&prefix, &end), |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(sqlite_error)?;
        let mut found = Vec::new();
        for row in rows {
            found.push(row.map_err(sqlite_error)?);
        }
        // The path's ownership row, if any, under the bare prefix.
        let owned: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM owned_outputs WHERE key = ?1",
                [&prefix],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        found.extend(owned.map(|identity| (prefix, identity)));
        Ok(found)
    }

    /// The superseding publishes recorded for names directly inside the
    /// destination directory `dir` and not yet settled (WP0(d), #187). A
    /// record this engine cannot decode exactly is refused, never skipped:
    /// skipping it would let the sweep treat a displaced file as a
    /// temporary.
    ///
    /// # Errors
    /// Refuses database failures and undecodable records
    /// ([`BulkloadRefusal::SchemaMismatch`]).
    pub(crate) fn supersede_intents(&self, dir: &[u8]) -> Result<Vec<SupersedeIntent>> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT intent FROM supersedes WHERE dir = ?1 ORDER BY temp")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map([dir], |row| row.get::<_, Vec<u8>>(0))
            .map_err(sqlite_error)?;
        let mut found = Vec::new();
        for row in rows {
            match postcard::take_from_bytes::<SupersedeIntent>(&row.map_err(sqlite_error)?) {
                Ok((intent, [])) if intent.dir == dir => found.push(intent),
                _ => return Err(BulkloadRefusal::SchemaMismatch),
            }
        }
        Ok(found)
    }

    /// Settle one superseding publish a crash left recorded, in a
    /// transaction of its own: with `restore`, its rows go back into
    /// `outputs` (the output is still this store's own, untouched); with
    /// `owned`, the staged file at the leaf gets an ownership row (see
    /// [`SupersedeSettle::owned`]); either way the record is deleted.
    ///
    /// # Errors
    /// Refuses database failures; nothing is settled then.
    pub(crate) fn settle_supersede(
        &self,
        intent: &SupersedeIntent,
        restore: bool,
        owned: Option<StatIdentity>,
    ) -> Result<()> {
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        let started = Instant::now();
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let settled = settle_in(
            &self.conn,
            &SupersedeSettle {
                intent,
                restore,
                owned,
            },
        )
        .and_then(|()| self.conn.execute_batch("COMMIT").map_err(sqlite_error));
        if settled.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        counters::sqlite_commit(Counter::SqliteSupersede, started, &settled);
        settled?;
        #[cfg(feature = "io-trace")]
        self.trace_commit(|| {
            vec![crate::io::trace::CommitRecord::SupersedeSettled {
                rel_path: intent.rel_path(),
                restored: restore,
            }]
        });
        Ok(())
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
        let identity = postcard::to_stdvec(&PendingDirectory { dev, ino, mode })
            .refuse_at("transfer_store::record_directory_created")?;
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
            .open(store.root.join("writer.lock"))
            .refuse_at("transfer_store::open")?;
        crate::io::sys::flock_exclusive(&lock).refuse_at("transfer_store::open")?;
        Ok(Self {
            store,
            side: role,
            ledger_sync: crate::io::durable::LedgerSync::Full,
            _exclusive: Exclusive(lock),
        })
    }

    /// Relax this SOURCE publisher's ledger row commits (WP0(g),
    /// OI-1003-Q37): from here on its connection commits with
    /// `synchronous=NORMAL`, `fullfsync=OFF`
    /// ([`crate::io::durable::relax_ledger_rows`]).
    ///
    /// The publisher's store is already open, so the commit that created
    /// it (schema and authority) returned under `synchronous=FULL` and its
    /// root is sealed (#161): the only commits left on this connection are
    /// ledger rows ([`StorePublisher::commit_captures`]).
    ///
    /// # Errors
    /// Refuses `PROTOCOL_STATE_VIOLATION` for a destination publisher,
    /// whose commits carry R25 and are never relaxed, and any `SQLite`
    /// refusal of the settings.
    fn relax_ledger_rows(&mut self) -> Result<()> {
        if self.side != PublisherSide::Source {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        crate::io::durable::relax_ledger_rows(&self.store.conn)?;
        self.ledger_sync = crate::io::durable::LedgerSync::Relaxed;
        Ok(())
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
    /// The same transaction settles the group's superseding publishes
    /// (`settled`, WP0(d), #187): each one's record is deleted, with the new
    /// output's row among `outputs` when it replaced the old one, or with
    /// the old output's rows put back when the exchange did not happen. So a
    /// replaced output's old rows are gone, and its new row present, in one
    /// commit.
    ///
    /// # Errors
    /// Refuses serialization or database failures; nothing is committed then.
    pub(crate) fn commit_outputs(
        &self,
        outputs: &[OutputRecord],
        settled: &[SupersedeSettle<'_>],
    ) -> Result<()> {
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
                row_in(&self.store.conn, output)?;
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
            for settle in settled {
                settle_in(&self.store.conn, settle)?;
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
        let committed = group_commit_fault(self.store.root()).and_then(|()| {
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
                    .chain(settled.iter().map(|settle| {
                        crate::io::trace::CommitRecord::SupersedeSettled {
                            rel_path: settle.intent.rel_path(),
                            restored: settle.restore,
                        }
                    }))
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

    /// Record a group's superseding publishes before any of their exchanges
    /// (WP0(d), #187), in one transaction: each output's rows leave
    /// `outputs` and its [`SupersedeIntent`] is written. From here until the
    /// record is settled no row vouches for the path, so no crash state
    /// holds a row beside a file it does not describe.
    ///
    /// # Errors
    /// Refuses serialization or database failures; nothing is recorded then.
    pub(crate) fn begin_supersedes(&self, intents: &[SupersedeIntent]) -> Result<()> {
        if intents.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "io-trace")]
        let _serial = crate::io::trace::serialize();
        self.store
            .conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let started = Instant::now();
        let recorded = (|| -> Result<()> {
            for intent in intents {
                for (key, _) in &intent.rows {
                    // A reuse row or the path's ownership row: either way
                    // it leaves with the intent.
                    for table in ["outputs", "owned_outputs"] {
                        self.store
                            .conn
                            .execute(&format!("DELETE FROM {table} WHERE key = ?1"), [key])
                            .map_err(sqlite_error)?;
                    }
                }
                self.store
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO supersedes (dir, temp, intent) VALUES (?1, ?2, ?3)",
                        (
                            &intent.dir,
                            &intent.temp,
                            postcard::to_stdvec(intent)
                                .refuse_at("transfer_store::begin_supersedes")?,
                        ),
                    )
                    .map_err(sqlite_error)?;
            }
            #[cfg(test)]
            output_commit_fault(self.store.root())?;
            self.store
                .conn
                .execute_batch("COMMIT")
                .map_err(sqlite_error)
        })();
        if recorded.is_err() {
            let _ = self.store.conn.execute_batch("ROLLBACK");
        }
        counters::sqlite_commit(Counter::SqliteSupersede, started, &recorded);
        recorded?;
        #[cfg(feature = "io-trace")]
        self.store.trace_commit(|| {
            intents
                .iter()
                .map(|intent| crate::io::trace::CommitRecord::SupersedeBegun {
                    rel_path: intent.rel_path(),
                    temp_path: intent.temp_path(),
                    staged: crate::io::NodeId {
                        dev: intent.staged.0,
                        ino: intent.staged.1,
                    },
                    owned: crate::io::NodeId {
                        dev: intent.owned.dev,
                        ino: intent.owned.ino,
                    },
                })
                .collect()
        });
        Ok(())
    }

    /// Commit completed captures, and refusals to remember (#186), to the
    /// ledger in one transaction.
    ///
    /// The only commit WP0(g) relaxes: on a relaxed source publisher
    /// ([`StorePublisher::relax_ledger_rows`]) the `COMMIT` below appends
    /// to the WAL and returns without syncing it.
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
                match &capture.record {
                    LedgerRecord::Capture(manifest) => self.store.conn.execute(
                        "INSERT INTO captures VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                        (
                            &capture.key,
                            postcard::to_stdvec(manifest)
                                .refuse_at("transfer_store::commit_captures")?,
                        ),
                    ),
                    LedgerRecord::Refused(refused) => self.store.conn.execute(
                        "INSERT INTO refused_seats VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET kind=excluded.kind",
                        (&capture.key, refused.tag()),
                    ),
                }
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
        if counters::sqlite_commit(self.group_counter(), commit_started, &committed)
            && self.ledger_sync == crate::io::durable::LedgerSync::Relaxed
        {
            // WP0(g): this commit returned without a sync of the WAL.
            counters::bump(Counter::SourceLedgerRelaxedCommits);
        }
        #[cfg(feature = "io-trace")]
        if committed.is_ok() {
            self.store.trace_commit(|| {
                captures
                    .iter()
                    .map(|capture| match capture.record {
                        LedgerRecord::Capture(_) => crate::io::trace::CommitRecord::Capture {
                            key: capture.key.clone(),
                        },
                        LedgerRecord::Refused(_) => crate::io::trace::CommitRecord::RefusedSeat {
                            key: capture.key.clone(),
                        },
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
/// group is a set of completed captures and remembered refusals (#186)
/// committed in one transaction; there are no chunk bytes to write or seal
/// first.
///
/// WP0(g) (OI-1003-Q20, Q37): under [`LedgerSync::Relaxed`] the group's
/// commit is not synced, and a group whose commit fails is counted and
/// dropped, not fatal (#163): the ledger is a cache, the destination's
/// rows carry R25, and each dropped row costs at most one more read of its
/// seat. A manifest that fails its own consistency check is not a failed
/// commit: it is refused `DIGEST_MISMATCH` and stops the session in both
/// modes.
///
/// [`LedgerSync::Relaxed`]: crate::io::durable::LedgerSync::Relaxed
pub(crate) struct LedgerSink {
    publisher: StorePublisher,
    failed: Option<BulkloadRefusal>,
}

impl LedgerSink {
    /// A sink for `publisher` whose row commits run in `sync` (`serve`
    /// passes the process-wide mode, [`crate::io::durable::ledger_sync`]).
    ///
    /// # Errors
    /// Refuses a relaxed sink on a destination publisher, and any `SQLite`
    /// refusal of the relaxed settings.
    pub(crate) fn with_sync(
        mut publisher: StorePublisher,
        sync: crate::io::durable::LedgerSync,
    ) -> Result<Self> {
        if sync == crate::io::durable::LedgerSync::Relaxed {
            publisher.relax_ledger_rows()?;
        }
        Ok(Self {
            publisher,
            failed: None,
        })
    }

    /// Commit one group of captures.
    ///
    /// # Errors
    /// Refuses an inconsistent manifest. A failed commit is returned under
    /// [`LedgerSync::Full`], and counted under `Relaxed`.
    ///
    /// [`LedgerSync::Full`]: crate::io::durable::LedgerSync::Full
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
            let LedgerRecord::Capture(manifest) = &item.record else {
                continue;
            };
            if !manifest.is_consistent()
                || manifest
                    .chunks
                    .iter()
                    .any(|chunk| chunk.size > u64::from(crate::hash::CDC_MAX_BYTES))
            {
                return Err(BulkloadRefusal::DigestMismatch);
            }
        }
        match self.publisher.commit_captures(items) {
            Err(_) if self.publisher.ledger_sync == crate::io::durable::LedgerSync::Relaxed => {
                // #163: counted, never fatal. The rows are not in the
                // ledger (the transaction rolled back), so a later run
                // misses them and reads their seats at most once more.
                counters::bump(Counter::SourceLedgerCommitFailed);
                counters::add_len(Counter::SourceLedgerRowsDropped, items.len());
                Ok(())
            }
            committed => committed,
        }
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
    postcard::to_stdvec(&(authority, row)).refuse_at("transfer_store::row_key")
}

/// The prefix every row key of one destination path shares under
/// `authority`: a row key is the postcard encoding of `(authority, row)`,
/// and a row's first field is its relative path, so the key starts with the
/// encoding of `(authority, rel_path)` (WP0(d), #187;
/// `tests::a_paths_rows_share_one_key_prefix` holds this to [`row_key`]).
///
/// # Errors
/// Refuses serialization failure.
pub(crate) fn path_prefix(authority: &[u8], rel_path: &[u8]) -> Result<Vec<u8>> {
    postcard::to_stdvec(&(authority, rel_path)).refuse_at("transfer_store::path_prefix")
}

/// The key of a path's ownership row, from any row key of that path: the
/// path's bare prefix ([`path_prefix`]). `None` for bytes that are not a row
/// key.
///
/// An ownership row is an `owned_outputs` row under that key. It records
/// the identity of a file this store published at the path, and nothing
/// about any source seat. It lives apart from `outputs`, whose every row is
/// a reuse key, so it is never one ([`Store::output_matches`]); the path's
/// read ([`Store::output_rows`]) returns it beside the reuse rows, told
/// apart by its key: a row key is always longer than its prefix (a row has
/// more fields than its path). It is written where an output has no reuse
/// row: published from a racy capture (#86), or exchanged into place by a
/// superseding publish whose row did not commit (#187).
pub(crate) fn owner_key(row_key: &[u8]) -> Option<&[u8]> {
    let (_, rest) = postcard::take_from_bytes::<&[u8]>(row_key).ok()?;
    let (_, rest) = postcard::take_from_bytes::<&[u8]>(rest).ok()?;
    row_key.get(..row_key.len().checked_sub(rest.len())?)
}

/// Write an output's row inside the caller's transaction: its reuse row
/// under its key, or, for a racy capture's output, its path's ownership row
/// alone.
fn row_in(conn: &rusqlite::Connection, output: &OutputRecord) -> Result<()> {
    if output.racy {
        // Never a reuse key (#86): drop any row an earlier capture left
        // under it, too.
        conn.execute("DELETE FROM outputs WHERE key = ?1", [&output.key])
            .map_err(sqlite_error)?;
        // Still this store's own output (#187): an ownership row names no
        // seat, so it is never a reuse key.
        if let Some(owner) = owner_key(&output.key) {
            own_in(conn, owner, &output.identity)?;
        }
        return Ok(());
    }
    conn.execute(
        "INSERT INTO outputs VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
        (&output.key, identity_bytes(&output.identity)?),
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// Write a path's ownership row inside the caller's transaction.
fn own_in(conn: &rusqlite::Connection, owner: &[u8], identity: &StatIdentity) -> Result<()> {
    conn.execute(
        "INSERT INTO owned_outputs VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
        (owner, identity_bytes(identity)?),
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// The least byte string greater than every string that starts with
/// `prefix`; `None` when there is none (a prefix of only `0xff` bytes).
fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if let Some(next) = last.checked_add(1) {
            end.push(next);
            return Some(end);
        }
    }
    None
}

/// Settle one superseding publish inside the caller's transaction.
fn settle_in(conn: &rusqlite::Connection, settle: &SupersedeSettle<'_>) -> Result<()> {
    if settle.restore {
        for (key, identity) in &settle.intent.rows {
            // A row a later group already wrote under the key wins. A key
            // that is its own bare prefix is the path's ownership row.
            let table = if owner_key(key) == Some(key.as_slice()) {
                "owned_outputs"
            } else {
                "outputs"
            };
            conn.execute(
                &format!("INSERT OR IGNORE INTO {table} VALUES (?1, ?2)"),
                (key, identity),
            )
            .map_err(sqlite_error)?;
        }
    }
    if let Some(identity) = &settle.owned {
        if let Some(owner) = settle
            .intent
            .rows
            .first()
            .and_then(|(key, _)| owner_key(key))
        {
            own_in(conn, owner, identity)?;
        }
    }
    conn.execute(
        "DELETE FROM supersedes WHERE dir = ?1 AND temp = ?2",
        (&settle.intent.dir, &settle.intent.temp),
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// Whether the store carries the [`RACY_GUARD_SETTING`] marker (#125).
fn racy_guarded(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key = ?1)",
        [RACY_GUARD_SETTING],
        |row| row.get(0),
    )
}

pub(crate) fn identity_bytes(identity: &StatIdentity) -> Result<Vec<u8>> {
    postcard::to_stdvec(&(
        identity.dev,
        identity.ino,
        identity.size,
        identity.mtime_ns,
        identity.ctime_ns,
    ))
    .refuse_at("transfer_store::identity_bytes")
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

/// Record the `Event::Commit` of the transaction that added
/// [`ROOT_SEALED_SETTING`] (R-N88, #161). The checker requires the state root
/// and its database from this commit on, so a marker that commits before its
/// seal fails the store proofs.
#[cfg(feature = "io-trace")]
fn trace_root_sealed(root: &fs::File) {
    crate::io::trace::record("sqlite commit", || {
        Ok(crate::io::trace::Event::Commit {
            store: crate::io::sys::fstat(root)?.node,
            records: vec![crate::io::trace::CommitRecord::RootSealed],
        })
    });
}

/// A private state root, open, and what sealing its entry takes (#161).
struct StateRoot {
    /// The root, never reached through a symlink at its own name.
    root: fs::File,
    /// The directory holding the root's entry. It is opened up front only
    /// when the root is made here, and otherwise only for a seal
    /// ([`StateRoot::seal`]): a sealed store under a parent the agent may
    /// search but not read still opens, as it did before #161.
    parent: Option<fs::File>,
    /// Where that parent is, and the root's name in it.
    parent_path: PathBuf,
    name: std::ffi::CString,
}

impl StateRoot {
    /// Open the private state root `path`, creating it (0700) when absent.
    ///
    /// A symlink, a non-directory, or a directory with any group or other
    /// bit at `path` is refused. A path that names its leaf only through
    /// `.`, `..` or a trailing `/` or `/.` is resolved first
    /// ([`resolve_leaf`]). Earlier components are the operator's, symlinks
    /// and all. The root is made with `io::sys::mkdirat`, so the R-N88 trace
    /// sees the entry.
    fn open(path: &Path) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = resolve_leaf(path)?;
        let leaf = path.file_name().ok_or(BulkloadRefusal::PathEscapesRoot)?;
        let parent_path = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let name = crate::io::c_name(leaf.as_bytes()).refuse_at("transfer_store::open")?;
        let escapes = |error: std::io::Error| {
            if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                BulkloadRefusal::PathEscapesRoot
            } else {
                crate::refuse::io(&error, "transfer_store::open")
            }
        };
        // An existing root needs only search permission on its parent.
        let (root, parent) = match crate::io::sys::open_dir_path_nofollow(&parent_path.join(leaf)) {
            Ok(root) => (root, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = fs::File::open(parent_path).refuse_at("transfer_store::open")?;
                match crate::io::sys::mkdirat(&parent, &name, 0o700) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(crate::refuse::io(&error, "transfer_store::open")),
                }
                let root = crate::io::sys::open_dir_at(&parent, &name).map_err(escapes)?;
                (root, Some(parent))
            }
            Err(error) => return Err(escapes(error)),
        };
        let root = fs::File::from(root);
        let found = crate::io::sys::fstat(&root).refuse_at("transfer_store::open")?;
        if !found.is_dir() || found.mode & 0o077 != 0 {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        Ok(Self {
            root,
            parent,
            parent_path: parent_path.to_path_buf(),
            name,
        })
    }

    /// Seal the root's entry in its parent and the entries it holds
    /// ([`crate::io::durable::seal_state_root`]).
    ///
    /// # Errors
    /// The flush failure, or [`StateRoot::open_parent`]'s refusal.
    fn seal(&mut self) -> Result<()> {
        let parent = match self.parent.take() {
            Some(parent) => parent,
            None => self.open_parent()?,
        };
        let sealed = crate::io::durable::seal_state_root(&parent, &self.root);
        self.parent = Some(parent);
        sealed.refuse_at("transfer_store::seal")
    }

    /// The directory holding the root's entry, opened for its seal. It must
    /// still hold this root under its name, or the seal would make some other
    /// entry durable.
    ///
    /// # Errors
    /// `IO` (`EACCES`) for a parent the agent may search but not read: the
    /// root's entry there cannot be sealed. `PATH_ESCAPES_ROOT` when the
    /// parent's entry is no longer this root.
    fn open_parent(&self) -> Result<fs::File> {
        let parent = fs::File::open(&self.parent_path).refuse_at("transfer_store::open_parent")?;
        let entry = crate::io::sys::fstatat_nofollow(&parent, &self.name)
            .refuse_at("transfer_store::open_parent")?;
        let root = crate::io::sys::fstat(&self.root).refuse_at("transfer_store::open_parent")?;
        if entry.node != root.node {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        Ok(parent)
    }
}

/// `path` with a leaf of its own, as `lstat` resolves it.
///
/// `.`, `..` and `x/..` name no leaf, and a trailing `/` or `/.` follows a
/// symlink at the leaf (`lstat("link/")` reports the directory it points
/// to, which the store accepted before #161). Such a path is canonicalized,
/// so the root opened and sealed is the directory it resolves to. One that
/// does not exist yet keeps its leaf as written, and is created there.
fn resolve_leaf(path: &Path) -> Result<std::borrow::Cow<'_, Path>> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = path.as_os_str().as_bytes();
    let trailing = bytes.ends_with(b"/") || bytes.ends_with(b"/.");
    if path.file_name().is_some() && !trailing {
        return Ok(std::borrow::Cow::Borrowed(path));
    }
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(std::borrow::Cow::Owned(resolved)),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && path.file_name().is_some() =>
        {
            Ok(std::borrow::Cow::Borrowed(path))
        }
        Err(error) => Err(crate::refuse::io(&error, "transfer_store::resolve_leaf")),
    }
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
            fs::create_dir(&path).refuse_at("transfer_store::tests::new")?;
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
            record: LedgerRecord::Capture(manifest(data)),
        }
    }

    /// The default ledger: relaxed row commits (WP0(g)).
    fn ledger_sink(state: &Path) -> Result<LedgerSink> {
        assert_eq!(
            crate::io::durable::ledger_sync(),
            crate::io::durable::LedgerSync::Relaxed,
            "the default mode (OI-1003-Q37)"
        );
        LedgerSink::with_sync(
            Store::open(state)?.into_publisher(PublisherSide::Source)?,
            crate::io::durable::LedgerSync::Relaxed,
        )
    }

    fn full_ledger_sink(state: &Path) -> Result<LedgerSink> {
        LedgerSink::with_sync(
            Store::open(state)?.into_publisher(PublisherSide::Source)?,
            crate::io::durable::LedgerSync::Full,
        )
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
                "owned_outputs",
                "refused_outputs",
                "refused_seats",
                "settings",
                "supersedes"
            ]
        );
        Ok(())
    }

    /// #186: a refusal is remembered under its row key, read back as the
    /// same refusal, replaced in place, and missed under any other key. A
    /// kind this engine does not know is a miss, and so is every row of a
    /// store without the racy-guard marker.
    #[test]
    fn a_refused_seat_is_remembered_under_its_row_key() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let sink = ledger_sink(&state)?;
        sink.publish(&[
            LedgerItem {
                entry: 3,
                key: b"refused".to_vec(),
                record: LedgerRecord::Refused(RefusedSeat::SqliteHeader),
            },
            item(b"capture", b"carried"),
        ])?;
        sink.publish(&[LedgerItem {
            entry: 4,
            key: b"refused".to_vec(),
            record: LedgerRecord::Refused(RefusedSeat::SqliteHeader),
        }])?;
        drop(sink);
        let store = Store::open(&state)?;
        assert_eq!(
            store.refused_seat(b"refused")?,
            Some(RefusedSeat::SqliteHeader)
        );
        assert_eq!(
            store.refused_seat(b"refused")?.map(RefusedSeat::refusal),
            Some(BulkloadRefusal::SqliteStateChanged)
        );
        assert_eq!(store.refused_seat(b"capture")?, None);
        assert_eq!(store.refused_seat(b"other")?, None);
        assert_eq!(store.capture(b"refused")?, None);
        assert_eq!(
            Store::open_reader(&state)?.refused_seat(b"refused")?,
            Some(RefusedSeat::SqliteHeader)
        );
        store
            .conn
            .execute("UPDATE refused_seats SET kind = 99", [])
            .map_err(sqlite_error)?;
        assert_eq!(store.refused_seat(b"refused")?, None, "an unknown kind");
        store
            .conn
            .execute("UPDATE refused_seats SET kind = 1", [])
            .map_err(sqlite_error)?;
        store.forget_racy_guard()?;
        assert_eq!(Store::open_reader(&state)?.refused_seat(b"refused")?, None);
        drop(store);
        assert_eq!(Store::open(&state)?.refused_seat(b"refused")?, None);
        Ok(())
    }

    fn seat_row(rel_path: &[u8], ino: u64) -> RowSchema {
        RowSchema {
            rel_path: rel_path.to_vec(),
            kind: bulkload_proto::FileKind::Regular,
            dev: 1,
            ino,
            size: 9,
            mtime_ns: 1_000,
            ctime_ns: 2_000,
            mode: 0o100_644,
            nlink: 1,
            link_target: None,
            blake3: None,
        }
    }

    /// WP0(d), #187: every row key of one destination path starts with
    /// [`path_prefix`], whatever the seat's stat identity, and no key of
    /// another path or authority does. `output_rows` depends on it.
    #[test]
    fn a_paths_rows_share_one_key_prefix() -> Result<()> {
        let prefix = path_prefix(b"authority", b"dir/seat")?;
        for ino in [1, 2, u64::MAX] {
            assert!(row_key(b"authority", &seat_row(b"dir/seat", ino))?.starts_with(&prefix));
        }
        for (authority, rel_path) in [
            (b"authority".as_slice(), b"dir/seat2".as_slice()),
            (b"authority", b"dir/sea"),
            (b"authority", b"dir"),
            (b"authority", b"dir/seat/below"),
            (b"authorit", b"ydir/seat"),
            (b"other", b"dir/seat"),
        ] {
            assert!(
                !row_key(authority, &seat_row(rel_path, 1))?.starts_with(&prefix),
                "{authority:?} {rel_path:?}"
            );
        }
        assert_eq!(prefix_end(&[1, 2, 3]), Some(vec![1, 2, 4]));
        assert_eq!(prefix_end(&[1, 0xff, 0xff]), Some(vec![2]));
        assert_eq!(prefix_end(&[0xff, 0xff]), None);
        assert_eq!(prefix_end(&[]), None);
        Ok(())
    }

    /// WP0(d), #187: a superseding publish's intent takes its output's rows
    /// out of the store in the commit that records it, and is settled
    /// either by putting them back or by the new output's row, in one
    /// commit. A path's rows are read without its neighbours'.
    #[test]
    fn a_superseding_publish_moves_rows_out_and_is_settled_in_one_commit() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let identity = |ino| StatIdentity {
            dev: 1,
            ino,
            size: 9,
            mtime_ns: 1_000,
            ctime_ns: 2_000,
        };
        let record = |rel_path: &[u8], ino| -> Result<OutputRecord> {
            Ok(OutputRecord {
                key: row_key(b"authority", &seat_row(rel_path, ino))?,
                rel_path: rel_path.to_vec(),
                identity: identity(ino),
                racy: false,
                hints: Vec::new(),
            })
        };
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        publisher.commit_outputs(
            &[
                record(b"dir/seat", 1)?,
                record(b"dir/seat", 2)?,
                record(b"dir/seat2", 3)?,
                record(b"dir", 4)?,
            ],
            &[],
        )?;
        let store = Store::open(&state)?;
        let rows = store.output_rows(b"authority", b"dir/seat")?;
        assert_eq!(rows.len(), 2, "both rows of the path, and no neighbour's");
        assert!(store.output_rows(b"other", b"dir/seat")?.is_empty());
        assert_eq!(store.output_rows(b"authority", b"dir/seat2")?.len(), 1);

        let intent = SupersedeIntent {
            dir: b"dir".to_vec(),
            temp: b".bulkload-0123456789abcdef-1-1".to_vec(),
            leaf: b"seat".to_vec(),
            staged: (1, 9),
            stamp: (9, 1_000),
            owned: identity(2),
            rows: rows.clone(),
        };
        assert_eq!(intent.rel_path(), b"dir/seat");
        assert_eq!(intent.temp_path(), b"dir/.bulkload-0123456789abcdef-1-1");
        publisher.begin_supersedes(std::slice::from_ref(&intent))?;
        assert!(store.output_rows(b"authority", b"dir/seat")?.is_empty());
        for (key, _) in &rows {
            assert!(!store.output_matches(key, &identity(1))?);
            assert!(!store.output_matches(key, &identity(2))?);
        }
        assert_eq!(store.output_rows(b"authority", b"dir/seat2")?.len(), 1);
        assert_eq!(
            store.supersede_intents(b"dir")?,
            std::slice::from_ref(&intent)
        );
        assert!(store.supersede_intents(b"")?.is_empty());
        assert!(store.supersede_intents(b"dir/seat")?.is_empty());

        // The exchange did not happen: the old output has its old rows again.
        store.settle_supersede(&intent, true, None)?;
        assert_eq!(store.output_rows(b"authority", b"dir/seat")?, rows);
        assert!(store.supersede_intents(b"dir")?.is_empty());

        // It happened: the new row and the settled intent are one commit.
        publisher.begin_supersedes(std::slice::from_ref(&intent))?;
        publisher.commit_outputs(
            &[record(b"dir/seat", 9)?],
            &[SupersedeSettle {
                intent: &intent,
                restore: false,
                owned: None,
            }],
        )?;
        let after: Vec<Vec<u8>> = store
            .output_rows(b"authority", b"dir/seat")?
            .into_iter()
            .map(|(_, identity)| identity)
            .collect();
        assert_eq!(
            after,
            [identity_bytes(&identity(9))?],
            "the old rows are dropped"
        );
        assert!(store.supersede_intents(b"dir")?.is_empty());

        // A record this engine cannot decode is refused, never skipped.
        publisher.begin_supersedes(std::slice::from_ref(&intent))?;
        store
            .conn
            .execute("UPDATE supersedes SET intent = x'00'", [])
            .map_err(sqlite_error)?;
        assert_eq!(
            store.supersede_intents(b"dir"),
            Err(BulkloadRefusal::SchemaMismatch)
        );
        Ok(())
    }

    /// #187 review: a superseding publish whose exchange happened and whose
    /// new row never committed (a crash) is settled by the sweep with an
    /// ownership row for the staged file at the leaf. The path's range read
    /// finds that row; it is never a reuse key.
    #[test]
    fn an_unrecorded_supersede_is_settled_with_an_ownership_row() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let identity = |ino| StatIdentity {
            dev: 1,
            ino,
            size: 9,
            mtime_ns: 1_000,
            ctime_ns: 2_000,
        };
        let record = |rel_path: &[u8], ino| -> Result<OutputRecord> {
            Ok(OutputRecord {
                key: row_key(b"authority", &seat_row(rel_path, ino))?,
                rel_path: rel_path.to_vec(),
                identity: identity(ino),
                racy: false,
                hints: Vec::new(),
            })
        };
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        publisher.commit_outputs(&[record(b"dir/seat", 1)?, record(b"dir/seat2", 3)?], &[])?;
        let store = Store::open(&state)?;
        let intent = SupersedeIntent {
            dir: b"dir".to_vec(),
            temp: b".bulkload-0123456789abcdef-1-1".to_vec(),
            leaf: b"seat".to_vec(),
            staged: (1, 7),
            stamp: (9, 1_000),
            owned: identity(1),
            rows: store.output_rows(b"authority", b"dir/seat")?,
        };
        publisher.begin_supersedes(std::slice::from_ref(&intent))?;
        store.settle_supersede(&intent, false, Some(identity(7)))?;
        let owner = path_prefix(b"authority", b"dir/seat")?;
        assert_eq!(
            store.output_rows(b"authority", b"dir/seat")?,
            [(owner.clone(), identity_bytes(&identity(7))?)],
            "the old rows are dropped; the ownership row stands"
        );
        for ino in [1, 2, 7, 9] {
            let key = row_key(b"authority", &seat_row(b"dir/seat", ino))?;
            assert_eq!(owner_key(&key), Some(owner.as_slice()));
            assert!(
                !store.output_matches(&key, &identity(7))?,
                "never a reuse key"
            );
        }
        assert_eq!(store.output_rows(b"authority", b"dir/seat2")?.len(), 1);
        assert!(store.supersede_intents(b"dir")?.is_empty());
        assert_eq!(owner_key(b"key"), None);
        assert_eq!(owner_key(&[]), None);
        Ok(())
    }

    /// #161: a state root is a private directory, never reached through a
    /// symlink, and so is its database a private file.
    #[test]
    fn a_state_root_must_be_a_private_directory() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestRoot::new()?;
        let private = root.0.join("private");
        fs::create_dir(&private)
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
        std::os::unix::fs::symlink(&private, root.0.join("link"))
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
        fs::write(root.0.join("file"), b"")
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
        let shared = root.0.join("shared");
        fs::create_dir(&shared)
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o750))
            .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
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
        )
        .refuse_at("transfer_store::tests::a_state_root_must_be_a_private_directory")?;
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

    /// Whether the database under `state` holds the root-seal marker, read
    /// on a connection of its own.
    fn marker_at(state: &Path) -> Result<bool> {
        let conn =
            rusqlite::Connection::open(state.join("transfer.sqlite")).map_err(sqlite_error)?;
        has_setting(&conn, ROOT_SEALED_SETTING).map_err(sqlite_error)
    }

    /// #161: the marker commits only once the seal has returned. An open
    /// whose seal fails refuses and leaves no marker behind, so the next open
    /// seals the root and only then records it.
    #[test]
    fn a_store_whose_seal_fails_records_no_seal() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        crate::io::durable::fail_dir_seals(true);
        let failed = Store::open(&state).map(drop);
        crate::io::durable::fail_dir_seals(false);
        assert_eq!(failed, Err(BulkloadRefusal::Io(Some(libc::EIO))));
        assert!(!marker_at(&state)?, "a marker without its seal");
        let store = Store::open(&state)?;
        assert!(has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?);
        Ok(())
    }

    /// A sealed store reopens under a parent the agent may search but not
    /// list (0311), as before #161: only a seal opens the parent. A root that
    /// still needs its seal there refuses with `IO` (`EACCES`) and records no
    /// marker; once the parent is readable, the next open seals it.
    #[test]
    fn a_sealed_store_opens_under_a_parent_it_cannot_list() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestRoot::new()?;
        let parent = root.0.join("parent");
        fs::create_dir(&parent).refuse_at(
            "transfer_store::tests::a_sealed_store_opens_under_a_parent_it_cannot_list",
        )?;
        let sealed = parent.join("sealed");
        drop(Store::open(&sealed)?);
        let unsealed = parent.join("unsealed");
        drop(Store::open(&unsealed)?);
        rusqlite::Connection::open(unsealed.join("transfer.sqlite"))
            .and_then(|conn| {
                conn.execute("DELETE FROM settings WHERE key = ?1", [ROOT_SEALED_SETTING])
            })
            .map_err(sqlite_error)?;
        let fresh = parent.join("fresh");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o311)).refuse_at(
            "transfer_store::tests::a_sealed_store_opens_under_a_parent_it_cannot_list",
        )?;
        let reopened = Store::open(&sealed).map(drop);
        let needs_seal = Store::open(&unsealed).map(drop);
        let created = Store::open(&fresh).map(drop);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).refuse_at(
            "transfer_store::tests::a_sealed_store_opens_under_a_parent_it_cannot_list",
        )?;
        assert_eq!(reopened, Ok(()));
        // Permission bits do not bind the superuser.
        if crate::io::sys::effective_uid() != 0 {
            assert_eq!(needs_seal, Err(BulkloadRefusal::Io(Some(libc::EACCES))));
            assert!(!marker_at(&unsealed)?, "a marker without its seal");
            assert_eq!(created, Err(BulkloadRefusal::Io(Some(libc::EACCES))));
            assert!(!fresh.exists(), "a root made where it cannot be sealed");
        }
        let store = Store::open(&unsealed)?;
        assert!(has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?);
        Ok(())
    }

    /// A state root named with a trailing `/` or `/.` resolves as `lstat`
    /// resolves it, through a symlink at its leaf, as before #161; the store
    /// then lives in, and seals, the directory the link names. The bare link
    /// stays refused (above), and so does a link to a shared directory.
    #[test]
    fn a_state_root_named_with_a_trailing_slash_follows_its_leaf() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestRoot::new()?;
        let private = root.0.join("private");
        fs::create_dir(&private).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        std::os::unix::fs::symlink(&private, root.0.join("link")).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        let canonical = fs::canonicalize(&private).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        for spelling in ["link/", "link/.", "link//", "link/./"] {
            let store = Store::open(&root.0.join(spelling))?;
            assert_eq!(store.root(), canonical.as_path(), "{spelling}");
            assert!(
                has_setting(&store.conn, ROOT_SEALED_SETTING).map_err(sqlite_error)?,
                "{spelling}"
            );
        }
        assert!(fs::symlink_metadata(private.join("transfer.sqlite"))
            .refuse_at(
                "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf"
            )?
            .is_file());
        assert!(fs::symlink_metadata(root.0.join("link"))
            .refuse_at(
                "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf"
            )?
            .is_symlink());
        // A fresh name with a trailing slash is made under that name.
        let fresh = Store::open(&root.0.join("fresh/"))?;
        assert_eq!(
            fresh.root(),
            fs::canonicalize(root.0.join("fresh")).refuse_at("transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf")?.as_path()
        );
        let shared = root.0.join("shared");
        fs::create_dir(&shared).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o750)).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        std::os::unix::fs::symlink(&shared, root.0.join("shared-link")).refuse_at(
            "transfer_store::tests::a_state_root_named_with_a_trailing_slash_follows_its_leaf",
        )?;
        for refused in ["shared-link/", "shared-link/."] {
            assert!(
                matches!(
                    Store::open(&root.0.join(refused)),
                    Err(BulkloadRefusal::PathEscapesRoot)
                ),
                "{refused}"
            );
        }
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
            record: LedgerRecord::Capture(manifest.clone()),
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
                record: LedgerRecord::Capture(outdated),
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
        if let LedgerRecord::Capture(manifest) = &mut bad.record {
            manifest.root = [0; 32];
        }
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

    fn pragma(publisher: &StorePublisher, name: &str) -> Result<i64> {
        publisher
            .store
            .conn
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(sqlite_error)
    }

    /// WP0(g) (OI-1003-Q37): only the source publisher's row commits are
    /// relaxed. The connection that creates a store, and with it the
    /// authority, commits `synchronous=FULL`, `fullfsync=ON`; so does every
    /// destination publisher, which refuses to be relaxed; and a checkpoint
    /// stays a full barrier on the relaxed connection.
    #[test]
    fn only_a_source_ledgers_row_commits_are_relaxed() -> Result<()> {
        use crate::io::durable::LedgerSync;
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        // The creating connection: the authority's commit is FULL.
        let created = Store::open(&state)?;
        assert_eq!(created.synchronous()?, 2);
        let authority = created.authority()?;
        drop(created);

        let relaxed = ledger_sink(&state)?;
        assert_eq!(relaxed.publisher.ledger_sync, LedgerSync::Relaxed);
        assert_eq!(pragma(&relaxed.publisher, "synchronous")?, 1);
        assert_eq!(pragma(&relaxed.publisher, "fullfsync")?, 0);
        assert_eq!(pragma(&relaxed.publisher, "checkpoint_fullfsync")?, 1);
        let before = counters::Counters::snapshot();
        relaxed.publish(&[item(b"relaxed", b"relaxed row")])?;
        let after = counters::Counters::snapshot().since(before);
        assert!(after.get(Counter::SourceLedgerRelaxedCommits) >= 1);
        drop(relaxed);
        // Another connection reads the relaxed row, and the authority is
        // the one the FULL commit made.
        let reopened = Store::open(&state)?;
        assert_eq!(reopened.synchronous()?, 2);
        assert_eq!(reopened.authority()?, authority);
        assert_eq!(
            reopened.capture(b"relaxed")?,
            Some(manifest(b"relaxed row"))
        );
        drop(reopened);

        let full = full_ledger_sink(&state)?;
        assert_eq!(full.publisher.ledger_sync, LedgerSync::Full);
        assert_eq!(pragma(&full.publisher, "synchronous")?, 2);
        assert_eq!(pragma(&full.publisher, "fullfsync")?, 1);
        drop(full);

        // A destination publisher is never relaxed, whatever the mode.
        let destination = root.0.join("destination-state");
        let mut publisher =
            Store::open(&destination)?.into_publisher(PublisherSide::Destination)?;
        assert_eq!(
            publisher.relax_ledger_rows(),
            Err(BulkloadRefusal::ProtocolStateViolation)
        );
        assert_eq!(publisher.ledger_sync, LedgerSync::Full);
        assert_eq!(pragma(&publisher, "synchronous")?, 2);
        assert_eq!(pragma(&publisher, "fullfsync")?, 1);
        assert!(matches!(
            LedgerSink::with_sync(publisher, LedgerSync::Relaxed),
            Err(BulkloadRefusal::ProtocolStateViolation)
        ));
        Ok(())
    }

    /// #163 (WP0(g)): a relaxed ledger's failed row commit is counted and
    /// dropped; the sink does not fail, later groups commit, and the
    /// dropped row is simply absent (a miss, one more read of its seat).
    /// Under `LedgerSync::Full` the same failure is sticky, as before.
    #[test]
    fn a_failed_relaxed_ledger_commit_is_counted_not_fatal() -> Result<()> {
        use crate::io::durable::GroupSink as _;
        for fault in [
            PublishFault::AfterManifestInsert,
            PublishFault::BeforeCommit,
        ] {
            let root = TestRoot::new()?;
            let state = root.0.join("state");
            let mut sink = ledger_sink(&state)?;
            let before = counters::Counters::snapshot();
            PUBLISH_FAULT.with(|active| active.set(fault));
            sink.commit(vec![
                item(b"dropped-1", b"dropped one"),
                item(b"dropped-2", b"dropped two"),
            ]);
            PUBLISH_FAULT.with(|active| active.set(PublishFault::None));
            let after = counters::Counters::snapshot().since(before);
            assert_eq!(sink.failure(), None, "counted, not fatal");
            assert!(after.get(Counter::SourceLedgerCommitFailed) >= 1);
            assert!(after.get(Counter::SourceLedgerRowsDropped) >= 2);
            // The next group commits on the same sink.
            sink.commit(vec![item(b"later", b"later row")]);
            assert_eq!(sink.failure(), None);
            assert_eq!(sink.finish(), Ok(()));
            let store = Store::open(&state)?;
            assert!(store.capture(b"dropped-1")?.is_none());
            assert!(store.capture(b"dropped-2")?.is_none());
            assert_eq!(store.capture(b"later")?, Some(manifest(b"later row")));
            drop(store);

            let mut strict = full_ledger_sink(&state)?;
            PUBLISH_FAULT.with(|active| active.set(fault));
            strict.commit(vec![item(b"strict", b"strict row")]);
            PUBLISH_FAULT.with(|active| active.set(PublishFault::None));
            assert_eq!(strict.failure(), Some(BulkloadRefusal::Io(None)));
            // Sticky: the later group is dropped and the sink reports it.
            strict.commit(vec![item(b"strict-later", b"strict later")]);
            assert_eq!(strict.finish(), Err(BulkloadRefusal::Io(None)));
            assert!(Store::open(&state)?.capture(b"strict-later")?.is_none());
        }
        Ok(())
    }

    /// A manifest that fails its own consistency check is not a failed
    /// commit: it stops a relaxed ledger too.
    #[test]
    fn an_inconsistent_manifest_stops_a_relaxed_ledger() -> Result<()> {
        use crate::io::durable::GroupSink as _;
        let root = TestRoot::new()?;
        let mut sink = ledger_sink(&root.0.join("state"))?;
        assert_eq!(
            sink.publisher.ledger_sync,
            crate::io::durable::LedgerSync::Relaxed
        );
        let mut bad = item(b"bad", b"bad");
        if let LedgerRecord::Capture(manifest) = &mut bad.record {
            manifest.root = [0; 32];
        }
        sink.commit(vec![bad]);
        assert_eq!(sink.failure(), Some(BulkloadRefusal::DigestMismatch));
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
            // The strict ledger returns the failure; the relaxed one counts
            // it (`a_failed_relaxed_ledger_commit_is_counted_not_fatal`).
            let sink = full_ledger_sink(&state)?;
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
        fs::write(&file, b"output")
            .refuse_at("transfer_store::tests::output_records_and_chunk_hints_commit_together")?;
        let identity =
            StatIdentity::from_metadata(&fs::metadata(&file).refuse_at(
                "transfer_store::tests::output_records_and_chunk_hints_commit_together",
            )?);
        let hint = ChunkHint {
            digest: [3; 32],
            offset: 5,
            size: 7,
        };
        publisher.commit_outputs(
            &[OutputRecord {
                key: b"key".to_vec(),
                rel_path: b"nested/output".to_vec(),
                identity,
                racy: false,
                hints: vec![hint],
            }],
            &[],
        )?;
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
        fs::write(&file, b"output")
            .refuse_at("transfer_store::tests::a_racy_output_is_never_a_reuse_key")?;
        let identity = StatIdentity::from_metadata(
            &fs::metadata(&file)
                .refuse_at("transfer_store::tests::a_racy_output_is_never_a_reuse_key")?,
        );
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
        publisher.commit_outputs(&[record(false)], &[])?;
        assert!(publisher.store().output_matches(b"key", &identity)?);
        publisher.commit_outputs(&[record(true)], &[])?;
        assert!(!publisher.store().output_matches(b"key", &identity)?);
        assert_eq!(publisher.store().row_counts()?, (0, 0));
        assert_eq!(publisher.store().output_chunks(&[5; 32])?.len(), 1);
        Ok(())
    }

    /// #187 review: an output published from a racy capture keeps no reuse
    /// row, but it gets an ownership row under its path's bare prefix, so
    /// it is this store's own when its seat changes again.
    #[test]
    fn a_racy_output_has_an_ownership_row_and_no_reuse_row() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let publisher = Store::open(&state)?.into_publisher(PublisherSide::Destination)?;
        let identity = StatIdentity {
            dev: 1,
            ino: 5,
            size: 9,
            mtime_ns: 1_000,
            ctime_ns: 2_000,
        };
        let key = row_key(b"authority", &seat_row(b"dir/seat", 5))?;
        let record = |racy: bool| OutputRecord {
            key: key.clone(),
            rel_path: b"dir/seat".to_vec(),
            identity,
            racy,
            hints: Vec::new(),
        };
        publisher.commit_outputs(&[record(true)], &[])?;
        let store = publisher.store();
        assert!(!store.output_matches(&key, &identity)?);
        assert_eq!(
            store.output_rows(b"authority", b"dir/seat")?,
            [(
                path_prefix(b"authority", b"dir/seat")?,
                identity_bytes(&identity)?
            )]
        );
        // A settled capture of the same output adds its reuse row.
        publisher.commit_outputs(&[record(false)], &[])?;
        assert!(store.output_matches(&key, &identity)?);
        assert_eq!(store.output_rows(b"authority", b"dir/seat")?.len(), 2);
        Ok(())
    }

    /// #187 review: the destination's refusal of an entry is answered from
    /// its record only while the file at the path keeps the identity it was
    /// refused with; a store that trusts none of its rows trusts none of
    /// these.
    #[test]
    fn a_refused_output_is_remembered_under_its_row_key_and_file_identity() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        let identity = |ctime_ns| StatIdentity {
            dev: 1,
            ino: 5,
            size: 9,
            mtime_ns: 1_000,
            ctime_ns,
        };
        assert_eq!(store.refused_output(b"seat", &identity(1))?, None);
        let before = counters::Counters::snapshot();
        store.remember_refused_output(b"seat", &identity(1), RefusedOutput::Occupied)?;
        assert_eq!(
            counters::Counters::snapshot()
                .since(before)
                .get(Counter::SqliteRefusedOutput),
            1
        );
        assert_eq!(
            store.refused_output(b"seat", &identity(1))?,
            Some(RefusedOutput::Occupied)
        );
        assert_eq!(store.refused_output(b"seat", &identity(2))?, None);
        assert_eq!(store.refused_output(b"other", &identity(1))?, None);
        store.remember_refused_output(b"seat", &identity(2), RefusedOutput::ExchangeUnsupported)?;
        assert_eq!(store.refused_output(b"seat", &identity(1))?, None);
        assert_eq!(
            store.refused_output(b"seat", &identity(2))?,
            Some(RefusedOutput::ExchangeUnsupported)
        );
        assert_eq!(
            RefusedOutput::Occupied.refusal(),
            BulkloadRefusal::DestinationOccupied
        );
        assert_eq!(
            RefusedOutput::ExchangeUnsupported.refusal(),
            BulkloadRefusal::DestinationExchangeUnsupported
        );
        // A kind this engine does not know is a miss.
        store
            .conn
            .execute("UPDATE refused_outputs SET kind = 99", [])
            .map_err(sqlite_error)?;
        assert_eq!(store.refused_output(b"seat", &identity(2))?, None);
        store
            .conn
            .execute("UPDATE refused_outputs SET kind = 1", [])
            .map_err(sqlite_error)?;
        store.forget_racy_guard()?;
        assert_eq!(
            Store::open_reader(&state)?.refused_output(b"seat", &identity(2))?,
            None
        );
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
        let mut padded = postcard::to_stdvec(&exact)
            .refuse_at("transfer_store::tests::a_ledger_row_with_trailing_bytes_is_a_miss")?;
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
        fs::write(&file, b"output").refuse_at(
            "transfer_store::tests::a_store_from_before_the_racy_guard_trusts_none_of_its_rows",
        )?;
        let identity = StatIdentity::from_metadata(&fs::metadata(&file).refuse_at(
            "transfer_store::tests::a_store_from_before_the_racy_guard_trusts_none_of_its_rows",
        )?);
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
        publisher.commit_outputs(&[record(b"old")], &[])?;
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
        publisher.commit_outputs(&[record(b"new")], &[])?;
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
        fs::write(&file, b"output")
            .refuse_at("transfer_store::tests::hints_keep_every_holder_newest_first")?;
        let identity = StatIdentity::from_metadata(
            &fs::metadata(&file)
                .refuse_at("transfer_store::tests::hints_keep_every_holder_newest_first")?,
        );
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
        publisher.commit_outputs(&[record(b"a", 0), record(b"b", 10)], &[])?;
        publisher.commit_outputs(&[record(b"c", 20)], &[])?;
        let paths = |found: Vec<ChunkHintRow>| -> Vec<Vec<u8>> {
            found.into_iter().map(|row| row.path).collect()
        };
        assert_eq!(
            paths(publisher.store().output_chunks(&[9; 32])?),
            [b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]
        );
        publisher.commit_outputs(&[record(b"a", 30)], &[])?;
        let found = publisher.store().output_chunks(&[9; 32])?;
        assert_eq!(
            found.first().map(|row| (row.path.clone(), row.offset)),
            Some((b"a".to_vec(), 30))
        );
        assert_eq!(found.len(), 3);
        Ok(())
    }
}
