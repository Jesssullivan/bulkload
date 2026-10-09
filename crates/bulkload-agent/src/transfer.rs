//! Native resumable transfer over one full-duplex framed stream (wire v6).
//!
//! The source walks its root and offers every seat as a numbered
//! [`Control::Entry`], up to [`ENTRY_WINDOW`] undecided at once. The
//! destination answers each with a [`Control::Decide`]:
//!
//! - `Skip`/`Refuse`: nothing to send (a directory or symlink the destination
//!   made itself, or a row it refused);
//! - `Reuse`: the destination holds this exact source identity durably, so
//!   the source reads nothing (R25, R-N58);
//! - `Send`: the source reads the file once, chunks and hashes it, and streams
//!   every chunk as a binary `Data` frame (header and payload with `writev`),
//!   then [`Control::End`] with the `manifest_root`;
//! - `WantManifest`: the destination may fill chunks locally (an existing
//!   output to adopt, or chunks held by published outputs). The source sends
//!   [`Control::Manifest`] (from its digest-only ledger without reading, when
//!   the stat identity is recorded), the destination answers
//!   [`Control::NeedChunks`] by index, and the source sends only those.
//!
//! An existing output that no longer holds the seat's bytes is superseded
//! when it is this store's own, untouched since its row was written (WP0(d),
//! OI-1003-Q18, #187): the new file is staged beside it, filled from the old
//! output's own chunks and the chunks the source sends, and exchanged with
//! it in its group commit ([`crate::materialize`], "Superseding publish").
//! Any other existing file is refused `DESTINATION_OCCUPIED` and left as it
//! is.
//!
//! A seat the source refuses for its content (a `SQLite` header in its first
//! bytes) is sniffed once: the refusal is remembered in the source ledger
//! under the seat's row key, so a later run refuses the unchanged seat with
//! the same code without opening it, and the sniffed bytes are counted as
//! `source_sniff_bytes`, never as content (#186, R25).
//!
//! **`SQLite` snapshot seats (#218).** With `--sqlite=snapshot`
//! ([`SqliteMode::Snapshot`], chosen by the destination and carried in
//! `Open`), a seat whose sniff sees a `SQLite` database magic is not refused:
//! the source takes a backup-API snapshot of it into its private state
//! (`provider_sqlite::snapshot_for_carry`, under OI-1003-Q16's stepped lock
//! and Q36/Q72's counted writes; never as root, OI-1003-Q76, and never as
//! another user than the database's owner), announces it with
//! [`Control::SqliteSnapshot`] and streams the snapshot, never the live
//! file. The destination verifies it (`integrity_check`) before publishing
//! it in journal mode DELETE, never beside a sidecar of its own. A changed
//! store's new snapshot supersedes its older output only through the
//! superseding exchange (OI-1003-Q146): when this store published that
//! output and it is untouched since, and no `-wal`, `-journal` or `-shm`
//! sits beside it at Decide or at the last look before the exchange; any
//! other output is refused `DESTINATION_OCCUPIED`. The live `-wal`, `-shm` and
//! `-journal` are never carried: the walk reports each one beside a regular
//! base as [`Control::SqliteSidecar`], and its outcome is its base's. A
//! snapshot seat is keyed on its row and its `-wal`'s identity
//! ([`sqlite_key`]), so an unchanged store is `Reuse`d with nothing opened.
//! See `docs/agent-notes/2026-10-08-sqlite-carry-design.md`.
//!
//! Data frames are paced by [`Control::Credit`]: the source never has more
//! than the granted payload bytes in flight. The source keeps no byte pack;
//! its ledger records digests and sizes only, so a refused capture leaves
//! nothing behind (R-N86). The destination writes each output from
//! digest-verified chunks and publishes it through group commit
//! ([`crate::io::durable`]).
//!
//! The source walk is a stream (W4 PR 3): a walk thread yields seats as it
//! finds them, a directory before anything beneath it, and the sending
//! thread offers each as soon as the entry window has room, so the first
//! `Entry` leaves before the walk ends. The walk runs at most
//! [`WALK_AHEAD`] items ahead of the wire. Every source seat, walked or
//! read, is resolved component by component beneath one descriptor of the
//! source root (`openat` with `O_NOFOLLOW` at every component): an
//! intermediate directory swapped for a symlink is refused, never followed
//! out of the root. Content is read with `pread`, never mapped.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{IoSlice, Read, Write};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use bulkload_proto::frame::{
    data_prefix, manifest_root, wire_id, ChunkSpec, Control, DataHeader, Decision, Frame,
    SidecarId, SqliteMode, DATA_HEADER_BYTES, FRAME_HEADER_BYTES, MAX_DATA_PAYLOAD,
    MAX_FRAME_BYTES, TAG_DATA,
};
use bulkload_proto::{FileKind, PROTO_VERSION};

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::io::durable::{Committer, LedgerSync};
use crate::materialize::{
    owned_output, verify_existing, Destination, Displaced, OwnedOutput, PendingOutput, Publication,
    PublishSink, SharedDisplaced, StagedFile,
};
use crate::transfer_store::{
    row_key, sqlite_key, ChunkHint, LedgerItem, LedgerRecord, LedgerSink, Manifest, OutputRecord,
    PublisherSide, RefusedOutput, RefusedSeat, Store,
};
use crate::walk::{WalkItem, Walker};
use crate::{BulkloadRefusal, Result, RowSchema};

/// Capture threads on the source.
const CAPTURE_WORKERS: usize = 4;
/// Entries offered but not yet decided, at most.
const ENTRY_WINDOW: usize = 1024;
/// Walked items not yet written to the wire, at most: the streaming walk
/// runs this far ahead of the sending thread and then waits.
const WALK_AHEAD: usize = 4 * ENTRY_WINDOW;
/// Entries whose content is in flight on the source, at most. Each may hold
/// one staged file open on the destination.
const ACTIVE_ENTRIES: usize = CAPTURE_WORKERS * 4;
/// Staged files the destination accepts open at once; a source that opens
/// more is refused.
const MAX_OPEN_ENTRIES: usize = 64;
/// Data payload bytes the destination grants up front.
const CREDIT_WINDOW: u64 = 16 * 1024 * 1024;
/// The destination returns credit once this much payload has been written.
const CREDIT_RETURN: u64 = 1024 * 1024;
/// Chunk bytes the source may hold in memory between a fresh manifest and
/// the chunk requests that follow it; past this it reads them again.
const RETAIN_BYTES: u64 = 512 * 1024 * 1024;
// At most this many files keep an open descriptor for chunk reuse before
// their group commits; fewer when the descriptor budget is smaller.
const SESSION_FILES: u64 = 256;
// Outputs held open while one manifest is filled from committed hints.
const HINT_FILES: usize = 16;
// Manifests are bounded separately. The walk is streamed; the source keeps
// one small slot per offered entry and drops each row once it is retired.
const MAX_MANIFEST_CHUNKS: usize = 131_072;
/// Header bytes a capture reads before it can refuse a `SQLite` database or
/// WAL by its magic (#186).
const SNIFF_BYTES: u64 = 16;
/// Bytes of `SQLite` snapshot slots the source keeps at once, streamed or
/// kept for chunk requests (#218, design D9: the salvage bound's figure;
/// unruled). Every slot is reserved before its backup and holds its
/// reservation until it is removed (#218 review); a capture thread waits
/// up to [`crate::provider_sqlite::CARRY_WALL`] for room. A store whose
/// main file and `-wal` together exceed it, or that finds no room in that
/// time, is refused `BUDGET_EXCEEDED` (not remembered).
const SLOT_BYTES: u64 = 4 << 30;
/// The private directory, inside the source state, that holds snapshot
/// slots (#218). Emptied when `serve` starts: no record depends on a slot.
const SLOT_DIR: &str = "sqlite-snapshots";
/// The session's `SQLite` mode when the caller names none: `refuse` (design
/// D1), unless `--sqlite=` set it for the process.
static SQLITE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set the process's default [`SqliteMode`] for `copy` and `receive`
/// (`--sqlite=refuse|snapshot`).
pub fn set_sqlite_mode(mode: SqliteMode) {
    SQLITE_MODE.store(mode == SqliteMode::Snapshot, Ordering::Relaxed);
}

/// The process's default [`SqliteMode`]: `refuse` unless set.
#[must_use]
pub fn sqlite_mode() -> SqliteMode {
    if SQLITE_MODE.load(Ordering::Relaxed) {
        SqliteMode::Snapshot
    } else {
        SqliteMode::Refuse
    }
}

/// Parse `--sqlite=`'s value.
///
/// # Errors
/// Refuses anything but `refuse` or `snapshot` `FIELD_DOMAIN_VIOLATION`.
pub fn parse_sqlite_mode(value: &str) -> Result<SqliteMode> {
    match value {
        "refuse" => Ok(SqliteMode::Refuse),
        "snapshot" => Ok(SqliteMode::Snapshot),
        _ => Err(BulkloadRefusal::FieldDomainViolation),
    }
}

/// The printed name of a [`SqliteMode`], as `--sqlite=` takes it.
#[must_use]
pub const fn sqlite_mode_label(mode: SqliteMode) -> &'static str {
    match mode {
        SqliteMode::Refuse => "refuse",
        SqliteMode::Snapshot => "snapshot",
    }
}

/// The default S4 disposition ruled for a transfer refusal, if any, from
/// its code and the `SQLite` result code it carries
/// ([`BulkloadRefusal::sqlite_code`]).
///
/// It is printed beside the refusal: `abandon` for an integrity failure
/// (OI-1003-Q148: a store that fails its integrity check is not carried,
/// and is closed as abandoned unless the operator reviews it otherwise).
/// That is `SQLITE_INTEGRITY_CHECK_FAILED`, and `SQLITE_BACKUP_FAILED` whose
/// primary code is 11 (`SQLITE_CORRUPT`) or 26 (`SQLITE_NOTADB`): corruption
/// the backup step itself met, which never reaches `integrity_check` (the
/// same primary codes a snapshot refusal is remembered by, R10). Transfer
/// refusals are not yet rows of the closure ledger (WP3 PR 4); until they
/// are, this is the report's text only, and every other refusal has no
/// default.
#[must_use]
pub fn default_disposition(code: &str, sqlite_code: Option<i32>) -> Option<&'static str> {
    let integrity = code == BulkloadRefusal::SqliteIntegrityCheckFailed.code()
        || (code == BulkloadRefusal::SqliteBackupFailed(None).code()
            && sqlite_code.is_some_and(|sqlite| matches!(sqlite & 0xff, 11 | 26)));
    integrity.then_some("abandon")
}

static WALK_NS: AtomicU64 = AtomicU64::new(0);
static WALK_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static REUSE_CENSUS_NS: AtomicU64 = AtomicU64::new(0);
static CDC_HASH_NS: AtomicU64 = AtomicU64::new(0);
static QUEUE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static TRANSFER_NS: AtomicU64 = AtomicU64::new(0);
static MATERIALIZE_NS: AtomicU64 = AtomicU64::new(0);
// Hand-off timers (S1, OI-1003-Q115): where each side of the wire blocks.
static SEND_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static SEND_HANDLE_NS: AtomicU64 = AtomicU64::new(0);
static RECV_READ_NS: AtomicU64 = AtomicU64::new(0);
static RECV_SETTLE_NS: AtomicU64 = AtomicU64::new(0);
static RECV_VERIFY_NS: AtomicU64 = AtomicU64::new(0);
// The receiving side's timeline (S1, OI-1003-Q119): setup, stream, tail.
static RECV_SETUP_NS: AtomicU64 = AtomicU64::new(0);
static RECV_STREAM_NS: AtomicU64 = AtomicU64::new(0);
static RECV_TAIL_NS: AtomicU64 = AtomicU64::new(0);

/// Receiver accounting; any refusal means the requested carry is incomplete.
#[derive(Debug, Default)]
pub struct TransferStats {
    /// Newly materialized or independently verified regular files.
    pub completed: u64,
    /// Outputs skipped using a previously persisted completion.
    pub reused: u64,
    /// Content bytes received across the transport.
    pub bytes_received: u64,
    /// Actual source file bytes read while preparing missing captures.
    pub source_bytes_read: u64,
    /// Relative paths and refusal codes. Contents and credentials are never logged.
    pub refusals: Vec<(Vec<u8>, String)>,
    /// `SQLite`'s result code of a refusal in [`Self::refusals`] that
    /// carries one (`SQLITE_BACKUP_FAILED`), by relative path: what
    /// [`Self::default_disposition`] reads (OI-1003-Q148).
    pub refusal_sqlite_codes: BTreeMap<Vec<u8>, i32>,
    /// Orphaned temporaries of this destination store removed by name.
    pub temporaries_removed: u64,
    /// Temporary-grammar names left in place because the sweep could not
    /// prove this store created them, by destination-relative path.
    pub temporaries_left: Vec<Vec<u8>>,
    /// Source files in the tagged temporary-name grammar that the source walk
    /// recorded and did not carry, by source-relative path.
    pub source_engine_temporaries: Vec<Vec<u8>>,
    /// Directories created by the no-replace rename of a tagged temporary.
    pub directories_renamed: u64,
    /// Directories created by the plain `mkdirat` fallback, on a filesystem
    /// without a no-replace rename (R-N119), by relative path.
    pub directories_fallback: Vec<Vec<u8>>,
    /// Outputs a crash left durable with no row, adopted from their capture
    /// record without a source read (#169; `transfer_unrowed_adopted`).
    pub unrowed_adopted: u64,
    /// Existing outputs with no matching row that no capture record proved,
    /// each asked for by manifest as before (#169;
    /// `transfer_unrowed_unproven`).
    pub unrowed_unproven: u64,
    /// The session's `SQLite` mode (#218).
    pub sqlite_mode: SqliteMode,
    /// Database seats published from a source snapshot, or adopted or reused
    /// as one (#218), by relative path.
    pub sqlite_snapshots: Vec<Vec<u8>>,
    /// Live `-wal`, `-shm` and `-journal` files the source reported beside a
    /// base that was carried as a snapshot: covered by it, never carried
    /// (#218), by relative path. A sidecar whose base was not is a refusal.
    pub sqlite_sidecars_covered: Vec<Vec<u8>>,
}

impl TransferStats {
    /// Record a refusal of `rel_path` with `code` and the `SQLite` result
    /// code it carries, if any.
    fn refuse(&mut self, rel_path: Vec<u8>, code: String, sqlite_code: Option<i32>) {
        if let Some(sqlite_code) = sqlite_code {
            self.refusal_sqlite_codes
                .insert(rel_path.clone(), sqlite_code);
        }
        self.refusals.push((rel_path, code));
    }

    /// The default S4 disposition of the refusal of `rel_path` with `code`
    /// ([`default_disposition`], OI-1003-Q148).
    #[must_use]
    pub fn default_disposition(&self, rel_path: &[u8], code: &str) -> Option<&'static str> {
        default_disposition(code, self.refusal_sqlite_codes.get(rel_path).copied())
    }

    /// Refusals that are walk caps (#129): subtrees the source walk did not
    /// carry because they lie past its depth or path-length bound. Each is
    /// also in [`Self::refusals`], by path and code, so a capped subtree is
    /// never counted as carried.
    #[must_use]
    pub fn capped_subtrees(&self) -> u64 {
        self.refusals
            .iter()
            .filter(|(_, code)| crate::walk::is_cap_refusal(code))
            .count() as u64
    }
}

/// Cumulative process-scope phase counters; concurrent transfers may overlap.
#[derive(Clone, Copy, Debug)]
pub struct TransferTiming {
    /// Walk thread work: listing, stat and hand-over, excluding
    /// [`Self::walk_wait_ns`].
    pub walk_ns: u64,
    /// Walk thread time blocked on a walk-ahead slot, waiting for the wire
    /// (back-pressure, #112).
    pub walk_wait_ns: u64,
    pub reuse_census_ns: u64,
    /// Capture workers' chunking and hashing, including the credit wait
    /// they take inside it ([`Self::queue_wait_ns`]).
    pub cdc_hash_ns: u64,
    /// Capture workers blocked on credit: the destination has not yet
    /// returned the window's bytes.
    pub queue_wait_ns: u64,
    /// The destination's data frames: wire verify and placement.
    pub transfer_ns: u64,
    pub materialize_ns: u64,
    /// The source's sending thread blocked on its event queue, with nothing
    /// to send (S1 hand-offs, OI-1003-Q115).
    pub send_wait_ns: u64,
    /// The sending thread handling an event, mostly writing frames to the
    /// wire (blocked there when the wire is full).
    pub send_handle_ns: u64,
    /// The receiving thread in `read_frame`: blocked on the wire, or parsing.
    pub recv_read_ns: u64,
    /// The receiving thread answering committed outputs (`settle_held`).
    pub recv_settle_ns: u64,
    /// The receiving thread's wire verify, part of [`Self::transfer_ns`].
    pub recv_verify_ns: u64,
    /// `receive` before its frame loop: both stores and the destination
    /// opened, the committer started, the root swept, `Open` / `Start`
    /// exchanged (so the source's store opening too) (S1 timeline).
    pub recv_setup_ns: u64,
    /// `receive`'s frame loop, from the first frame to `SourceDone`.
    pub recv_stream_ns: u64,
    /// `receive` after its frame loop: the last group commits, directory
    /// records and the session's flushes (`finish_receive`).
    pub recv_tail_ns: u64,
}

impl TransferTiming {
    #[must_use]
    pub fn snapshot() -> Self {
        Self {
            walk_ns: WALK_NS.load(Ordering::Relaxed),
            walk_wait_ns: WALK_WAIT_NS.load(Ordering::Relaxed),
            reuse_census_ns: REUSE_CENSUS_NS.load(Ordering::Relaxed),
            cdc_hash_ns: CDC_HASH_NS.load(Ordering::Relaxed),
            queue_wait_ns: QUEUE_WAIT_NS.load(Ordering::Relaxed),
            transfer_ns: TRANSFER_NS.load(Ordering::Relaxed),
            materialize_ns: MATERIALIZE_NS.load(Ordering::Relaxed),
            send_wait_ns: SEND_WAIT_NS.load(Ordering::Relaxed),
            send_handle_ns: SEND_HANDLE_NS.load(Ordering::Relaxed),
            recv_read_ns: RECV_READ_NS.load(Ordering::Relaxed),
            recv_settle_ns: RECV_SETTLE_NS.load(Ordering::Relaxed),
            recv_verify_ns: RECV_VERIFY_NS.load(Ordering::Relaxed),
            recv_setup_ns: RECV_SETUP_NS.load(Ordering::Relaxed),
            recv_stream_ns: RECV_STREAM_NS.load(Ordering::Relaxed),
            recv_tail_ns: RECV_TAIL_NS.load(Ordering::Relaxed),
        }
    }

    /// Space-separated `key=value` pairs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "walk_ns={} walk_wait_ns={} reuse_census_ns={} cdc_hash_ns={} queue_wait_ns={} transfer_ns={} materialize_ns={} send_wait_ns={} send_handle_ns={} recv_read_ns={} recv_settle_ns={} recv_verify_ns={} recv_setup_ns={} recv_stream_ns={} recv_tail_ns={}",
            self.walk_ns,
            self.walk_wait_ns,
            self.reuse_census_ns,
            self.cdc_hash_ns,
            self.queue_wait_ns,
            self.transfer_ns,
            self.materialize_ns,
            self.send_wait_ns,
            self.send_handle_ns,
            self.recv_read_ns,
            self.recv_settle_ns,
            self.recv_verify_ns,
            self.recv_setup_ns,
            self.recv_stream_ns,
            self.recv_tail_ns,
        )
    }

    #[must_use]
    pub const fn since(self, before: Self) -> Self {
        Self {
            walk_ns: self.walk_ns.saturating_sub(before.walk_ns),
            walk_wait_ns: self.walk_wait_ns.saturating_sub(before.walk_wait_ns),
            reuse_census_ns: self.reuse_census_ns.saturating_sub(before.reuse_census_ns),
            cdc_hash_ns: self.cdc_hash_ns.saturating_sub(before.cdc_hash_ns),
            queue_wait_ns: self.queue_wait_ns.saturating_sub(before.queue_wait_ns),
            transfer_ns: self.transfer_ns.saturating_sub(before.transfer_ns),
            materialize_ns: self.materialize_ns.saturating_sub(before.materialize_ns),
            send_wait_ns: self.send_wait_ns.saturating_sub(before.send_wait_ns),
            send_handle_ns: self.send_handle_ns.saturating_sub(before.send_handle_ns),
            recv_read_ns: self.recv_read_ns.saturating_sub(before.recv_read_ns),
            recv_settle_ns: self.recv_settle_ns.saturating_sub(before.recv_settle_ns),
            recv_verify_ns: self.recv_verify_ns.saturating_sub(before.recv_verify_ns),
            recv_setup_ns: self.recv_setup_ns.saturating_sub(before.recv_setup_ns),
            recv_stream_ns: self.recv_stream_ns.saturating_sub(before.recv_stream_ns),
            recv_tail_ns: self.recv_tail_ns.saturating_sub(before.recv_tail_ns),
        }
    }
}

struct PhaseTimer(&'static AtomicU64, Instant);

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        self.0.fetch_add(elapsed_ns(self.1), Ordering::Relaxed);
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Whether either canonical path contains the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The canonical path a private state root has, or will have once a store
/// creates it: the root itself when it exists, else its canonical parent
/// joined with its name. Read-only, so an overlap with a root it must not
/// touch is refused before anything is created (WP1 PR 3, S2).
fn canonical_state(state: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(state) {
        Ok(_) => std::fs::canonicalize(state).refuse_at("transfer::canonical_state"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = state.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
            let parent = match state.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            Ok(std::fs::canonicalize(parent)
                .refuse_at("transfer::canonical_state")?
                .join(name))
        }
        Err(error) => Err(crate::refuse::io(&error, "transfer::canonical_state")),
    }
}

/// Run the same framed protocol locally over a bounded Unix stream pair,
/// in the process's [`sqlite_mode`].
///
/// # Errors
/// Refuses overlapping roots, malformed data and transport or source failures.
pub fn copy(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
) -> Result<TransferStats> {
    copy_with(
        source,
        destination,
        source_state,
        destination_state,
        sqlite_mode(),
    )
}

/// [`copy`] in the given [`SqliteMode`] (#218). Its source half is
/// [`serve`], in this process, so every source rule `serve` applies holds
/// here too, the root refusal of `snapshot` mode included.
///
/// # Errors
/// As [`copy`].
pub fn copy_with(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
    sqlite: SqliteMode,
) -> Result<TransferStats> {
    let source_root = std::fs::canonicalize(source).refuse_at("transfer::copy")?;
    let destination_root = std::fs::canonicalize(destination).refuse_at("transfer::copy")?;
    if overlaps(&source_root, &destination_root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    // S2 (WP1 PR 3): both halves run at once, so neither state root may be
    // created inside the source, and each must stay apart from its own root,
    // all decided before either store exists.
    let source_state_root = canonical_state(source_state)?;
    let destination_state_root = canonical_state(destination_state)?;
    if overlaps(&source_state_root, &source_root)
        || overlaps(&destination_state_root, &source_root)
        || overlaps(&destination_state_root, &destination_root)
    {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let (mut sender, mut receiver) =
        std::os::unix::net::UnixStream::pair().refuse_at("transfer::copy")?;
    for stream in [&sender, &receiver] {
        tune_stream(stream);
    }
    // Test-only hang guard. It must exceed the slowest single socket stall in
    // the suite, which on a loaded host can be many seconds.
    #[cfg(test)]
    for stream in [&sender, &receiver] {
        stream
            .set_read_timeout(Some(std::time::Duration::from_mins(5)))
            .refuse_at("transfer::copy")?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_mins(5)))
            .refuse_at("transfer::copy")?;
    }
    std::thread::scope(|scope| -> Result<TransferStats> {
        let producer = std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                let input = sender.try_clone().refuse_at("transfer::copy")?;
                let served = serve(input, &mut sender);
                // The source's reader thread holds a clone of this end; shutting
                // it down ends that thread and tells the destination the source
                // is gone, whatever `serve` returned.
                let _ = sender.shutdown(std::net::Shutdown::Both);
                served
            })
            .refuse_at("transfer::copy")?;
        let result = {
            let mut output = receiver.try_clone().refuse_at("transfer::copy")?;
            receive_with(
                &mut receiver,
                &mut output,
                source,
                source_state,
                destination,
                destination_state,
                sqlite,
            )
        };
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        drop(receiver);
        producer.join().map_err(|_| BulkloadRefusal::WorkerLost)??;
        result
    })
}

/// Raise one transfer stream's kernel buffers, best effort. A stream that
/// cannot be tuned still carries the protocol, only slower.
pub fn tune_stream(stream: &impl std::os::fd::AsFd) {
    if matches!(crate::io::tune_transport(stream), Ok(true)) {
        counters::bump(Counter::TransportTuned);
    }
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// Data payload bytes the destination has granted and the source has not yet
/// spent. Capture threads take credit before they hand a chunk to the
/// sending thread, so chunk memory in flight is bounded by the window.
struct Credit {
    state: Mutex<(u64, bool)>,
    ready: Condvar,
}

impl Credit {
    const fn new() -> Self {
        Self {
            state: Mutex::new((0, false)),
            ready: Condvar::new(),
        }
    }

    /// Add credit the destination returned. Available credit never exceeds
    /// [`CREDIT_WINDOW`]: an honest destination only returns what the source
    /// spent, so a grant past the window is refused.
    fn grant(&self, bytes: u64) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let granted = state
                .0
                .checked_add(bytes)
                .filter(|granted| *granted <= CREDIT_WINDOW)
                .ok_or(BulkloadRefusal::BudgetExceeded)?;
            state.0 = granted;
        }
        self.ready.notify_all();
        Ok(())
    }

    /// Stop every waiter: the session is over.
    fn close(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.ready.notify_all();
    }

    fn acquire(&self, bytes: u64) -> Result<()> {
        let started = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if state.1 {
                return Err(BulkloadRefusal::WorkerLost);
            }
            if state.0 >= bytes {
                state.0 -= bytes;
                QUEUE_WAIT_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
                return Ok(());
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Chunk bytes held between a fresh manifest and its chunk requests, with
/// their reservation against [`RETAIN_BYTES`], released on drop. A `SQLite`
/// snapshot's manifest keeps its slot file instead, which holds its own
/// reservation against [`SLOT_BYTES`] (#218): the chunks are read from it
/// again.
struct Retained {
    chunks: Vec<Arc<Vec<u8>>>,
    slot: Option<SlotFile>,
    reserved: u64,
    budget: Arc<AtomicU64>,
}

impl Retained {
    /// Reserve `bytes` from `budget`, or `None` when it would overrun.
    fn reserve(budget: &Arc<AtomicU64>, bytes: u64) -> Option<Self> {
        budget
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                left.checked_sub(bytes)
            })
            .ok()?;
        Some(Self {
            chunks: Vec::new(),
            slot: None,
            reserved: bytes,
            budget: Arc::clone(budget),
        })
    }

    /// Keep a snapshot's slot file, which holds its own reservation against
    /// the slot budget (#218 review): nothing of `budget` is taken.
    fn slot(budget: &Arc<AtomicU64>, slot: SlotFile) -> Self {
        Self {
            chunks: Vec::new(),
            slot: Some(slot),
            reserved: 0,
            budget: Arc::clone(budget),
        }
    }
}

/// A `SQLite` snapshot the source took into its private state (#218): open
/// for reading, and removed when dropped, at the end of the entry's content
/// (its streamed `End`, or the chunk requests after its manifest), on a
/// refusal and when the session ends. A crash leaves it for the next
/// `serve`, which empties the slot directory: no record depends on a slot.
struct SlotFile {
    file: std::fs::File,
    path: PathBuf,
    /// Its share of the slot budget, given back once the file is removed.
    reserved: SlotReservation,
}

impl Drop for SlotFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The source's budget of snapshot slot bytes alive at once (#218 review,
/// [`SLOT_BYTES`]): every slot, streamed or kept for a manifest, is
/// reserved before its backup writes it and given back once it is removed,
/// so the slots of every capture thread together never pass it.
struct SlotBudget {
    total: u64,
    left: Mutex<u64>,
    freed: Condvar,
}

impl SlotBudget {
    const fn new(total: u64) -> Self {
        Self {
            total,
            left: Mutex::new(total),
            freed: Condvar::new(),
        }
    }

    /// Reserve `bytes`, waiting at most `wait` for other slots to be
    /// removed; `None` when they still do not fit, or never could.
    fn reserve(self: &Arc<Self>, bytes: u64, wait: std::time::Duration) -> Option<SlotReservation> {
        if bytes > self.total {
            return None;
        }
        let deadline = Instant::now() + wait;
        let mut left = self.left.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *left >= bytes {
                *left -= bytes;
                return Some(SlotReservation {
                    budget: Arc::clone(self),
                    bytes,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            left = self
                .freed
                .wait_timeout(left, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// One slot's share of the [`SlotBudget`], given back when dropped.
struct SlotReservation {
    budget: Arc<SlotBudget>,
    bytes: u64,
}

impl SlotReservation {
    /// Make this reservation exactly `bytes`: what is over is given back at
    /// once; more is taken only when it is free now. `false` when it is not.
    fn true_up(&mut self, bytes: u64) -> bool {
        let mut left = self
            .budget
            .left
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let fits = bytes <= self.bytes || *left >= bytes - self.bytes;
        if fits {
            // `left + self.bytes` is at most twice the budget, and at
            // least `bytes` when it fits.
            *left = (*left + self.bytes) - bytes;
        }
        drop(left);
        if !fits {
            return false;
        }
        let shrunk = bytes < self.bytes;
        self.bytes = bytes;
        if shrunk {
            self.budget.freed.notify_all();
        }
        true
    }
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        *self
            .budget
            .left
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += self.bytes;
        self.budget.freed.notify_all();
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        self.budget.fetch_add(self.reserved, Ordering::AcqRel);
    }
}

/// Work for a capture thread. `wal`: the `-wal` identity the walk offered
/// with the entry (#218), which keys its remembered snapshot refusals.
enum Job {
    /// Read, chunk and stream entry `entry`.
    Send {
        entry: u64,
        row: Arc<RowSchema>,
        wal: Option<SidecarId>,
    },
    /// Produce entry `entry`'s manifest, from the ledger when it can.
    Manifest {
        entry: u64,
        row: Arc<RowSchema>,
        wal: Option<SidecarId>,
    },
    /// Send the requested chunks of an offered manifest, then `End`.
    Serve {
        entry: u64,
        row: Arc<RowSchema>,
        manifest: Manifest,
        indices: Vec<u32>,
        retained: Option<Retained>,
        /// The row key to record once the destination holds the bytes.
        record: Option<Vec<u8>>,
        /// The manifest's capture was racy (#86).
        racy: bool,
    },
}

/// What the sending thread hears: destination frames from the reader
/// thread, and capture output from the capture threads.
enum Event {
    Decide {
        entry: u64,
        decision: Decision,
    },
    NeedChunks {
        entry: u64,
        indices: Vec<u32>,
    },
    Held {
        entry: u64,
        held: bool,
    },
    PeerFailed(BulkloadRefusal),
    /// One item of the streaming walk, its walk-ahead slot already taken.
    Walked(WalkItem),
    /// The streaming walk has yielded its last item.
    WalkEnded,
    /// One chunk, its credit already taken.
    Chunk {
        header: DataHeader,
        data: Arc<Vec<u8>>,
    },
    /// An entry's content is a `SQLite` snapshot (#218): sent before its
    /// first chunk or its manifest.
    Snapshot {
        entry: u64,
        size: u64,
        wal: Option<SidecarId>,
    },
    Manifest {
        entry: u64,
        /// The row key to record, for a manifest that is not yet in the ledger.
        record: Option<Vec<u8>>,
        manifest: Manifest,
        retained: Option<Retained>,
        /// The capture was racy: neither side records it (#86).
        racy: bool,
        bytes_read: u64,
    },
    End {
        entry: u64,
        /// The capture to record once the destination holds the bytes, for
        /// a capture not yet in the ledger.
        record: Option<(Vec<u8>, Manifest)>,
        root: [u8; 32],
        chunks: u32,
        size: u64,
        /// The capture was racy: neither side records it (#86).
        racy: bool,
        bytes_read: u64,
    },
    Refused {
        entry: u64,
        refusal: BulkloadRefusal,
        bytes_read: u64,
        /// The refusal to remember under this key (#186): the seat's bytes
        /// alone gave it, and the seat was not racy when it was read (its
        /// row key, or for a snapshot refusal its snapshot key, #218).
        remember: Option<(Vec<u8>, RefusedSeat)>,
    },
}

/// Where one entry stands on the source.
enum SourceEntry {
    Undecided,
    Queued,
    Working,
    Offered {
        manifest: Manifest,
        retained: Option<Retained>,
        record: Option<Vec<u8>>,
        racy: bool,
    },
    Done,
}

/// The source's retention budget.
#[cfg(not(test))]
const fn retain_budget(_root: &Path) -> u64 {
    RETAIN_BYTES
}

/// The source's retention budget; tests set a smaller one per source root.
#[cfg(test)]
fn retain_budget(root: &Path) -> u64 {
    RETAIN_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(override_root, _)| override_root == root)
        .map_or(RETAIN_BYTES, |(_, bytes)| *bytes)
}

/// Test-only retention budgets by canonical source root.
#[cfg(test)]
static RETAIN_OVERRIDE: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// At most this many salvaged temporaries outlive a session (#124,
/// OI-1002-Q33): those whose chunks a refused entry staged.
///
/// OI-1002-Q33 ruled that salvage is bounded, and OI-1003-Q24 (2026-10-03)
/// ratified these values: 1024 temporaries and 4 GiB per session.
const SALVAGE_KEEP_FILES: usize = 1024;
/// At most this many bytes of salvaged temporaries outlive a session.
const SALVAGE_KEEP_BYTES: u64 = 4 << 30;

/// How many salvaged temporaries, and how many bytes of them, may outlive a
/// session at the destination root `root`.
#[cfg(not(test))]
const fn salvage_bound(_root: &Path) -> (usize, u64) {
    (SALVAGE_KEEP_FILES, SALVAGE_KEEP_BYTES)
}

/// The salvage bound; tests set a smaller one per destination root.
#[cfg(test)]
fn salvage_bound(root: &Path) -> (usize, u64) {
    SALVAGE_BOUND_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(override_root, _)| override_root == root)
        .map_or((SALVAGE_KEEP_FILES, SALVAGE_KEEP_BYTES), |(_, bound)| {
            *bound
        })
}

/// Test-only salvage bounds by canonical destination root.
#[cfg(test)]
static SALVAGE_BOUND_OVERRIDE: Mutex<Vec<(PathBuf, (usize, u64))>> = Mutex::new(Vec::new());

/// Timestamp granularity the racy-capture guard allows for (#86): the Git
/// carry's own [`crate::git_carry::RACY_GRANULARITY_NS`], 2 s.
pub const RACY_GRANULARITY_NS: i128 = crate::git_carry::RACY_GRANULARITY_NS;

/// Wait until no seat beneath `root` is racy against the wall clock: until
/// the clock is past every seat's mtime and ctime by more than
/// [`RACY_GRANULARITY_NS`]. Returns the time waited.
///
/// For a corpus written just before it is copied (a benchmark fixture or a
/// test), so that its first copy records every capture and a warm run can
/// show 0 source reads. The copy itself never waits: a racy seat is sent
/// and read again by a later run (#86). The tree is walked without
/// following symlinks.
///
/// # Errors
/// Refuses an unreadable tree.
pub fn settle_racy_window(root: &Path) -> Result<std::time::Duration> {
    fn newest(directory: &Path, stamp: &mut i128) -> Result<()> {
        for entry in std::fs::read_dir(directory).refuse_at("transfer::newest")? {
            let entry = entry.refuse_at("transfer::newest")?;
            let meta = entry.metadata().refuse_at("transfer::newest")?;
            let identity = StatIdentity::from_metadata(&meta);
            *stamp = (*stamp).max(identity.mtime_ns).max(identity.ctime_ns);
            if meta.is_dir() {
                newest(&entry.path(), stamp)?;
            }
        }
        Ok(())
    }
    let started = Instant::now();
    let mut stamp = StatIdentity::from_metadata(
        &std::fs::symlink_metadata(root).refuse_at("transfer::newest")?,
    )
    .ctime_ns;
    newest(root, &mut stamp)?;
    let settled = stamp.saturating_add(RACY_GRANULARITY_NS);
    // A seat stamped far in the future would never settle: refuse rather
    // than wait for it.
    if settled.saturating_sub(crate::git_carry::pass_start_ns()) > 30 * RACY_GRANULARITY_NS {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    loop {
        let now = crate::git_carry::pass_start_ns();
        if now > settled {
            return Ok(started.elapsed());
        }
        let wait = u64::try_from(settled.saturating_sub(now))
            .unwrap_or(u64::MAX)
            .saturating_add(1_000_000);
        std::thread::sleep(std::time::Duration::from_nanos(wait));
    }
}

/// The clock a capture is judged racy against: the capturing host's wall
/// clock, in nanoseconds since the epoch (see the design's known limit on
/// filesystem clocks).
#[cfg(not(test))]
fn capture_clock(_root: &Path) -> i128 {
    crate::git_carry::pass_start_ns()
}

/// The clock a capture is judged racy against. Unit tests write their
/// fixtures just before they copy them, so every seat would be racy against
/// the real clock and no warm resume could be shown: by default they read a
/// clock one hour ahead. A test of the guard itself pins the clock for its
/// source root with [`set_capture_clock`].
#[cfg(test)]
fn capture_clock(root: &Path) -> i128 {
    CAPTURE_CLOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(pinned_root, _)| pinned_root == root)
        .map_or_else(
            || crate::git_carry::pass_start_ns() + 3_600_000_000_000,
            |(_, clock)| *clock,
        )
}

/// Pin (or, with `None`, unpin) the capture clock for a canonical source root.
#[cfg(test)]
fn set_capture_clock(root: &Path, clock: Option<i128>) {
    let mut pinned = CAPTURE_CLOCK.lock().unwrap_or_else(PoisonError::into_inner);
    pinned.retain(|(pinned_root, _)| pinned_root != root);
    if let Some(clock) = clock {
        pinned.push((root.to_path_buf(), clock));
    }
}

/// Test-only capture clocks by canonical source root.
#[cfg(test)]
static CAPTURE_CLOCK: Mutex<Vec<(PathBuf, i128)>> = Mutex::new(Vec::new());

/// A test hook run on a capture thread.
#[cfg(test)]
type CaptureHook = Arc<dyn Fn() + Send + Sync>;

/// Test-only hooks by canonical source root, run between a refused seat's
/// header sniff and the stat that decides whether its refusal may be
/// remembered (#186): where a writer can still move the seat.
#[cfg(test)]
static AFTER_SNIFF: Mutex<Vec<(PathBuf, CaptureHook)>> = Mutex::new(Vec::new());

/// Set (or, with `None`, clear) the after-sniff hook of a canonical source
/// root.
#[cfg(test)]
fn set_after_sniff(root: &Path, hook: Option<CaptureHook>) {
    let mut hooks = AFTER_SNIFF.lock().unwrap_or_else(PoisonError::into_inner);
    hooks.retain(|(hooked_root, _)| hooked_root != root);
    if let Some(hook) = hook {
        hooks.push((root.to_path_buf(), hook));
    }
}

#[cfg(test)]
fn after_sniff(root: &Path) {
    let hook = AFTER_SNIFF
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(hooked_root, _)| hooked_root == root)
        .map(|(_, hook)| Arc::clone(hook));
    if let Some(hook) = hook {
        hook();
    }
}

/// Test-only: who a `snapshot`-mode session's source half takes itself to
/// be, by canonical source root. Unit tests of what the source does once it
/// may read run as root in CI, so they assume an ordinary user; tests of the
/// refusals assume root, or another user than a database's owner. The
/// refusals themselves are proved with the real uid as well
/// (`tests/sqlite_carry.rs`), as the provider's `assume_unprivileged` seam
/// is.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum AssumeUid {
    /// Not root; the real effective uid otherwise.
    Unprivileged,
    /// Root.
    Root,
    /// This effective uid, not root.
    Euid(u32),
}

#[cfg(test)]
static ASSUMED_UID: Mutex<Vec<(PathBuf, AssumeUid)>> = Mutex::new(Vec::new());

/// Test-only: assume `uid` for `root`'s sessions (`None` stops).
#[cfg(test)]
fn assume_uid(root: &Path, uid: Option<AssumeUid>) {
    let mut roots = ASSUMED_UID.lock().unwrap_or_else(PoisonError::into_inner);
    roots.retain(|(known, _)| known != root);
    if let Some(uid) = uid {
        roots.push((root.to_path_buf(), uid));
    }
}

#[cfg(test)]
fn assumed_uid(root: &Path) -> Option<AssumeUid> {
    ASSUMED_UID
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(known, _)| known == root)
        .map(|(_, uid)| *uid)
}

/// Whether a `snapshot`-mode source would open its databases as root
/// (OI-1003-Q76): the effective uid is 0.
#[cfg(not(test))]
fn reads_as_root(_root: &Path) -> bool {
    crate::io::sys::effective_uid() == 0
}

/// Whether a `snapshot`-mode source would open its databases as root; unit
/// tests may assume otherwise for a root ([`assume_uid`]).
#[cfg(test)]
fn reads_as_root(root: &Path) -> bool {
    match assumed_uid(root) {
        Some(AssumeUid::Root) => true,
        Some(AssumeUid::Unprivileged | AssumeUid::Euid(_)) => false,
        None => crate::io::sys::effective_uid() == 0,
    }
}

/// The effective uid a database seat's owner is compared with (#218, R4).
#[cfg(not(test))]
fn source_euid(_root: &Path) -> u32 {
    crate::io::sys::effective_uid()
}

/// The effective uid a database seat's owner is compared with; unit tests
/// may assume another ([`assume_uid`]).
#[cfg(test)]
fn source_euid(root: &Path) -> u32 {
    match assumed_uid(root) {
        Some(AssumeUid::Euid(uid)) => uid,
        _ => crate::io::sys::effective_uid(),
    }
}

/// Test-only hooks by canonical source root, run once a snapshot's backup
/// and its post-stat are done and before its slot is chunked (#218, review
/// R12): where a writer's commit must not reach the carried bytes.
#[cfg(test)]
static AFTER_SNAPSHOT: Mutex<Vec<(PathBuf, CaptureHook)>> = Mutex::new(Vec::new());

/// Set (or, with `None`, clear) the after-snapshot hook of a canonical
/// source root.
#[cfg(test)]
fn set_after_snapshot(root: &Path, hook: Option<CaptureHook>) {
    let mut hooks = AFTER_SNAPSHOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    hooks.retain(|(hooked_root, _)| hooked_root != root);
    if let Some(hook) = hook {
        hooks.push((root.to_path_buf(), hook));
    }
}

#[cfg(test)]
fn after_snapshot(root: &Path) {
    let hook = AFTER_SNAPSHOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(hooked_root, _)| hooked_root == root)
        .map(|(_, hook)| Arc::clone(hook));
    if let Some(hook) = hook {
        hook();
    }
}

/// Test-only hooks by canonical source root, run inside a snapshot's backup
/// lock, after its pre-stats and before `SQLite` opens the seat by its path
/// (#218 review, R9): where a directory swapped for a symlink would be
/// followed.
#[cfg(test)]
static BEFORE_BACKUP: Mutex<Vec<(PathBuf, CaptureHook)>> = Mutex::new(Vec::new());

/// Set (or, with `None`, clear) the before-backup hook of a canonical source
/// root.
#[cfg(test)]
fn set_before_backup(root: &Path, hook: Option<CaptureHook>) {
    let mut hooks = BEFORE_BACKUP.lock().unwrap_or_else(PoisonError::into_inner);
    hooks.retain(|(hooked_root, _)| hooked_root != root);
    if let Some(hook) = hook {
        hooks.push((root.to_path_buf(), hook));
    }
}

#[cfg(test)]
fn before_backup(root: &Path) {
    let hook = BEFORE_BACKUP
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(hooked_root, _)| hooked_root == root)
        .map(|(_, hook)| Arc::clone(hook));
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: this thread holds its `serve`'s backup lock.
    static HOLDS_BACKUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only: sniffed database descriptors closed by a thread that did not
/// hold the backup lock (#218 review: such a close drops every POSIX lock
/// this process holds on the inode, a live backup's included).
#[cfg(test)]
static SNIFF_CLOSED_UNLOCKED: AtomicU64 = AtomicU64::new(0);

/// Test-only: marks this thread as holding the backup lock while alive.
#[cfg(test)]
struct BackupHeld;

#[cfg(test)]
impl BackupHeld {
    fn mark() -> Self {
        HOLDS_BACKUP.set(true);
        Self
    }
}

#[cfg(test)]
impl Drop for BackupHeld {
    fn drop(&mut self) {
        HOLDS_BACKUP.set(false);
    }
}

/// The retained-slot budget; tests set a smaller one per source root.
#[cfg(not(test))]
const fn slot_budget(_root: &Path) -> u64 {
    SLOT_BYTES
}

/// The retained-slot budget; tests set a smaller one per source root.
#[cfg(test)]
fn slot_budget(root: &Path) -> u64 {
    SLOT_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(override_root, _)| override_root == root)
        .map_or(SLOT_BYTES, |(_, bytes)| *bytes)
}

/// Test-only slot budgets by canonical source root.
#[cfg(test)]
static SLOT_OVERRIDE: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// What every capture thread shares.
struct SourceWork<'a> {
    /// The canonical source root, which keys test-only hooks and clocks.
    /// `SQLite` opens a snapshot seat by its path beneath it (#218, R9).
    root: &'a Path,
    /// The source root every read resolves beneath, component by component.
    root_fd: BorrowedFd<'a>,
    authority: &'a [u8],
    state: &'a Path,
    credit: &'a Credit,
    retain: Arc<AtomicU64>,
    /// How this session's ledger rows are committed (WP0(g)); it also says
    /// how a ledger read that fails is answered ([`ledger_read`]).
    ledger: LedgerSync,
    /// The session's `SQLite` mode (#218).
    sqlite: SqliteMode,
    /// The effective uid: a database seat owned by another is refused
    /// `SQLITE_SOURCE_NOT_OWNER` before `SQLite` opens it (#218, R4).
    euid: u32,
    /// The provider's own root refusal (OI-1003-Q76), passed through; a
    /// `snapshot`-mode session as root is refused at `Open` already.
    as_root: bool,
    /// One backup at a time per `serve` (#218, design D9).
    backup: &'a Mutex<()>,
    /// The private slot directory snapshots are written to (#218).
    slots: &'a Path,
    /// What is left of [`SLOT_BYTES`] for slots alive at once.
    slot_budget: Arc<SlotBudget>,
}

/// Walk-ahead slots: the walk thread takes one per item it hands over, and
/// the sending thread gives it back once the item is on the wire.
struct WalkGate {
    state: Mutex<(usize, bool)>,
    ready: Condvar,
    limit: usize,
}

impl WalkGate {
    const fn new() -> Self {
        Self::with_limit(WALK_AHEAD)
    }

    const fn with_limit(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, false)),
            ready: Condvar::new(),
            limit,
        }
    }

    /// Take one slot, waiting while `limit` ([`WALK_AHEAD`]) are out; `false`
    /// once the session is over. Time spent blocked is added to `waited_ns`;
    /// the clock is read only when the gate is full.
    fn take(&self, waited_ns: &mut u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut blocked = None;
        loop {
            if state.1 || state.0 < self.limit {
                if let Some(started) = blocked {
                    *waited_ns = waited_ns.saturating_add(elapsed_ns(started));
                }
                if state.1 {
                    return false;
                }
                state.0 += 1;
                return true;
            }
            blocked.get_or_insert_with(Instant::now);
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn give(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.0 = state.0.saturating_sub(1);
        }
        self.ready.notify_one();
    }

    /// Stop the walk thread: the session is over.
    fn close(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.ready.notify_all();
    }
}

/// One walk thread's time: `walk_ns` is walk work (listing, stat and
/// hand-over), `wait_ns` is time blocked on a walk-ahead slot. Their sum is
/// the walk thread's lifetime (#112).
#[derive(Clone, Copy, Debug)]
struct WalkTime {
    walk_ns: u64,
    wait_ns: u64,
}

/// The walk thread: hand every walked item to the sending thread, at most
/// [`WALK_AHEAD`] ahead of the wire, then say the walk has ended.
fn walk_source<I: IntoIterator<Item = WalkItem>>(
    walker: I,
    gate: &WalkGate,
    events: &Sender<Event>,
) -> WalkTime {
    let started = Instant::now();
    let mut wait_ns = 0;
    let finished = walker
        .into_iter()
        .all(|item| gate.take(&mut wait_ns) && events.send(Event::Walked(item)).is_ok());
    if finished {
        let _ = events.send(Event::WalkEnded);
    }
    let time = WalkTime {
        walk_ns: elapsed_ns(started).saturating_sub(wait_ns),
        wait_ns,
    };
    WALK_NS.fetch_add(time.walk_ns, Ordering::Relaxed);
    WALK_WAIT_NS.fetch_add(time.wait_ns, Ordering::Relaxed);
    time
}

/// Serve one transfer request from a caller-authenticated stdio transport.
///
/// A reader thread owns `input` for the whole session and may outlive this
/// call; the caller closes the transport after `serve` returns (a process
/// exit does).
///
/// # Errors
/// Refuses malformed frames, a peer on another wire, unsafe state roots,
/// source failures and broken I/O.
pub fn serve<R: Read + Send + 'static, W: Write>(mut input: R, output: &mut W) -> Result<()> {
    let (root, state, sqlite, as_root) = read_open(&mut input)?;
    // S2 (WP1 PR 3): refuse before `Store::open` creates the state root, so
    // an overlapping state never writes a byte inside the source.
    if overlaps(&canonical_state(&state)?, &root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let store = Store::open(&state)?;
    // Again on the store's own canonical root: a state swapped for a symlink
    // after the check above still refuses before any source read.
    if overlaps(store.root(), &root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    // Every source seat is walked and read beneath this one descriptor
    // (W4 PR 3); its identity is the root's in the authority.
    let root_fd = crate::io::sys::open_root(&root).refuse_at("transfer::serve")?;
    let meta = crate::io::sys::fstat(&root_fd).refuse_at("transfer::serve")?;
    let authority = postcard::to_stdvec(&(
        store.authority()?,
        root.as_os_str().as_bytes(),
        meta.node.dev,
        meta.node.ino,
    ))
    .refuse_at("transfer::serve")?;
    // WP0(g): the store exists, and its authority is durable, since the
    // open above returned; only this second connection's row commits are
    // relaxed (`LedgerSink::with_sync`, `--source-ledger-sync`).
    let ledger = crate::io::durable::ledger_sync();
    let committer = Committer::spawn(LedgerSink::with_sync(
        Store::open(&state)?.into_publisher(PublisherSide::Source)?,
        ledger,
    )?)?;
    write_control(
        output,
        &Control::Start {
            authority: authority.clone(),
        },
    )?;
    let walker = Walker::new(root_fd.as_fd(), true)?.with_sqlite(sqlite == SqliteMode::Snapshot);
    let slots = open_slots(store.root(), sqlite)?;
    let backup = Mutex::new(());
    let credit = Arc::new(Credit::new());
    let gate = WalkGate::new();
    let (events_sender, events) = std::sync::mpsc::channel();
    spawn_reader(input, events_sender.clone(), Arc::clone(&credit))?;
    let work = SourceWork {
        root: &root,
        root_fd: root_fd.as_fd(),
        authority: &authority,
        state: store.root(),
        credit: &credit,
        retain: Arc::new(AtomicU64::new(retain_budget(&root))),
        ledger,
        sqlite,
        euid: source_euid(&root),
        as_root,
        backup: &backup,
        slots: &slots,
        slot_budget: Arc::new(SlotBudget::new(slot_budget(&root))),
    };
    let (jobs, job_queue) = std::sync::mpsc::channel();
    let job_queue = Mutex::new(job_queue);
    let served = std::thread::scope(|scope| -> Result<(u64, u64)> {
        {
            let (events, gate) = (events_sender.clone(), &gate);
            std::thread::Builder::new()
                .name("bulkload-serve-walk".to_owned())
                .spawn_scoped(scope, move || {
                    walk_source(walker, gate, &events);
                })
                .refuse_at("transfer::serve")?;
        }
        for _ in 0..CAPTURE_WORKERS {
            let events = events_sender.clone();
            let (work, job_queue) = (&work, &job_queue);
            std::thread::Builder::new()
                .spawn_scoped(scope, move || capture_worker(work, job_queue, &events))
                .refuse_at("transfer::serve")?;
        }
        let sent = send_entries(output, &committer, &events, jobs, &gate);
        // Wake any capture thread waiting on credit, and the walk thread
        // waiting on a walk-ahead slot; their work is moot now.
        credit.close();
        gate.close();
        sent
    });
    drop(events_sender);
    let (source_bytes_read, entries) = served?;
    // Every capture of this transfer is committed, or the transfer fails,
    // before the receiver is told the source is done.
    committer.sync()?;
    fault_point!(ServeBeforeDone);
    write_control(
        output,
        &Control::SourceDone {
            entries,
            source_bytes_read,
        },
    )?;
    committer.finish()?
}

/// Read the session's `Open`: the canonical source root, the private state,
/// the `SQLite` mode, and whether a database would be opened as root.
///
/// The root is canonical: `SQLite` opens a snapshot seat by its path
/// beneath it, so no component of the root may be a symlink
/// `SQLITE_OPEN_NOFOLLOW` would refuse (#218, R9); the root descriptor is
/// opened from the same path. A `snapshot`-mode session as root is refused
/// here (OI-1003-Q76, #218): it would open databases WAL-aware, and as root
/// `SQLite` re-applies their owner to the `-wal` and `-shm` it opens. That
/// is before any store is created or anything beneath the root is opened,
/// and `copy` runs [`serve`] as its source half, so the one check covers
/// both verbs (R14).
fn read_open<R: Read>(input: &mut R) -> Result<(PathBuf, PathBuf, SqliteMode, bool)> {
    let Frame::Control(Control::Open {
        proto,
        wire_id: id,
        root,
        state,
        sqlite,
    }) = read_frame(input)?
    else {
        return Err(BulkloadRefusal::ProtocolStateViolation);
    };
    if proto != PROTO_VERSION || id != wire_id() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    let root = std::fs::canonicalize(path(root)).refuse_at("transfer::serve")?;
    let as_root = reads_as_root(&root);
    if sqlite == SqliteMode::Snapshot && as_root {
        return Err(BulkloadRefusal::SqliteSourceAsRoot);
    }
    Ok((root, path(state), sqlite, as_root))
}

/// The private directory snapshot slots are written to (#218), emptied of
/// whatever a crashed session left: no record depends on a slot. Created
/// (mode 0700, inside the private state root) only in `snapshot` mode; in
/// `refuse` mode an existing one is still emptied.
fn open_slots(state: &Path, sqlite: SqliteMode) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let slots = state.join(SLOT_DIR);
    if sqlite == SqliteMode::Snapshot {
        match std::fs::DirBuilder::new().mode(0o700).create(&slots) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(crate::refuse::io(&error, "transfer::open_slots")),
        }
    }
    match std::fs::read_dir(&slots) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.refuse_at("transfer::open_slots")?;
                std::fs::remove_file(entry.path()).refuse_at("transfer::open_slots")?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::refuse::io(&error, "transfer::open_slots")),
    }
    Ok(slots)
}

/// Read the destination's frames for the whole session: credit is granted
/// at once, decisions and chunk requests go to the sending thread.
fn spawn_reader<R: Read + Send + 'static>(
    mut input: R,
    events: Sender<Event>,
    credit: Arc<Credit>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("bulkload-serve-reader".to_owned())
        .spawn(move || {
            let failure = loop {
                let event = match read_frame(&mut input) {
                    Ok(Frame::Control(Control::Credit { bytes })) => {
                        if let Err(refusal) = credit.grant(bytes) {
                            break refusal;
                        }
                        continue;
                    }
                    Ok(Frame::Control(Control::Decide { entry, decision })) => {
                        Event::Decide { entry, decision }
                    }
                    Ok(Frame::Control(Control::Held { entry, held })) => {
                        Event::Held { entry, held }
                    }
                    Ok(Frame::Control(Control::NeedChunks { entry, indices })) => {
                        Event::NeedChunks { entry, indices }
                    }
                    Ok(_) => break BulkloadRefusal::ProtocolStateViolation,
                    Err(refusal) => break refusal,
                };
                if events.send(event).is_err() {
                    break BulkloadRefusal::WorkerLost;
                }
            };
            credit.close();
            let _ = events.send(Event::PeerFailed(failure));
        })
        .refuse_at("transfer::spawn_reader")?;
    Ok(())
}

/// The sending thread's view of the session.
struct Outbound<'a, W> {
    output: &'a mut W,
    committer: &'a Committer<LedgerSink>,
    jobs: Sender<Job>,
    gate: &'a WalkGate,
    /// Walked rows not yet offered, in walk order, each with its `-wal`'s
    /// identity in `snapshot` mode (#218).
    pending: VecDeque<(RowSchema, Option<SidecarId>)>,
    /// Whether the walk thread has yielded its last item.
    walked: bool,
    /// One slot per offered entry, by entry number.
    entries: Vec<SourceEntry>,
    /// Each offered entry's row, dropped once the entry is retired.
    rows: Vec<Option<Arc<RowSchema>>>,
    /// The `-wal` identity an undecided entry was offered with (#218); few
    /// entries have one.
    wals: HashMap<u64, SidecarId>,
    undecided: usize,
    walk_done: bool,
    queue: VecDeque<Job>,
    active: usize,
    bytes_read: u64,
    /// Entries whose `End` is sent and whose `Held` is awaited, with the
    /// capture to record once the destination holds the bytes.
    awaiting: HashMap<u64, Option<(Vec<u8>, Manifest)>>,
}

/// The sending thread: offer walked entries, act on decisions and chunk
/// requests, and write capture output as it arrives. Returns the source
/// bytes read and the entries offered.
fn send_entries<W: Write>(
    output: &mut W,
    committer: &Committer<LedgerSink>,
    events: &Receiver<Event>,
    jobs: Sender<Job>,
    gate: &WalkGate,
) -> Result<(u64, u64)> {
    let mut outbound = Outbound {
        output,
        committer,
        jobs,
        gate,
        pending: VecDeque::new(),
        walked: false,
        entries: Vec::new(),
        rows: Vec::new(),
        wals: HashMap::new(),
        undecided: 0,
        walk_done: false,
        queue: VecDeque::new(),
        active: 0,
        bytes_read: 0,
        awaiting: HashMap::new(),
    };
    loop {
        outbound.offer()?;
        if outbound.finished() {
            return Ok((outbound.bytes_read, outbound.entries.len() as u64));
        }
        let waited = Instant::now();
        let event = events.recv().map_err(|_| BulkloadRefusal::WorkerLost)?;
        SEND_WAIT_NS.fetch_add(elapsed_ns(waited), Ordering::Relaxed);
        let _handle_timer = PhaseTimer(&SEND_HANDLE_NS, Instant::now());
        outbound.handle(event)?;
    }
}

impl<W: Write> Outbound<'_, W> {
    /// Offer walked entries up to the window, close the walk once it has
    /// ended and every row is offered, and start queued jobs up to the
    /// active bound.
    fn offer(&mut self) -> Result<()> {
        while self.undecided < ENTRY_WINDOW {
            let Some((row, wal)) = self.pending.pop_front() else {
                break;
            };
            let entry = self.entries.len() as u64;
            write_control(
                self.output,
                &Control::Entry {
                    entry,
                    row: row.clone(),
                    wal,
                },
            )?;
            if let Some(wal) = wal {
                self.wals.insert(entry, wal);
            }
            self.gate.give();
            self.entries.push(SourceEntry::Undecided);
            self.rows.push(Some(Arc::new(row)));
            self.undecided += 1;
        }
        if self.walked && self.pending.is_empty() && !self.walk_done {
            write_control(
                self.output,
                &Control::WalkDone {
                    entries: self.entries.len() as u64,
                },
            )?;
            self.walk_done = true;
        }
        while self.active < ACTIVE_ENTRIES {
            let Some(job) = self.queue.pop_front() else {
                break;
            };
            self.jobs
                .send(job)
                .map_err(|_| BulkloadRefusal::WorkerLost)?;
            self.active += 1;
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.walk_done
            && self.undecided == 0
            && self.queue.is_empty()
            && self.active == 0
            && self.awaiting.is_empty()
    }

    fn slot(&mut self, entry: u64) -> Result<(&mut SourceEntry, &mut Option<Arc<RowSchema>>)> {
        let index = usize::try_from(entry).map_err(|_| BulkloadRefusal::ProtocolStateViolation)?;
        Ok((
            self.entries
                .get_mut(index)
                .ok_or(BulkloadRefusal::ProtocolStateViolation)?,
            self.rows
                .get_mut(index)
                .ok_or(BulkloadRefusal::ProtocolStateViolation)?,
        ))
    }

    /// Retire an entry whose job a capture thread held, returning its row.
    fn finish_entry(&mut self, entry: u64) -> Result<Arc<RowSchema>> {
        let (slot, row) = self.slot(entry)?;
        if !matches!(slot, SourceEntry::Queued | SourceEntry::Working) {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        *slot = SourceEntry::Done;
        let row = row.take().ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        self.active -= 1;
        Ok(row)
    }

    /// One walked item: a row waits for the window, the rest go out now.
    fn walked(&mut self, item: WalkItem) -> Result<()> {
        match item {
            WalkItem::Row(row) => {
                // Its walk-ahead slot is given back once it is offered.
                self.pending.push_back((row, None));
                return Ok(());
            }
            WalkItem::RowWithWal(row, wal) => {
                self.pending.push_back((row, Some(wal)));
                return Ok(());
            }
            // Recorded, never carried: the receiver binds it to its base's
            // outcome (#218, R2).
            WalkItem::SqliteSidecar { rel_path, database } => {
                write_control(self.output, &Control::SqliteSidecar { rel_path, database })?;
            }
            WalkItem::Refused(seat) => write_control(
                self.output,
                &Control::Refused {
                    entry: None,
                    rel_path: seat.rel_path,
                    code: seat.refusal.code().to_owned(),
                    sqlite_code: seat.refusal.sqlite_code(),
                },
            )?,
            // Recorded, never carried: the receiver reports each one (R-N79).
            WalkItem::Engine(rel_path) => {
                write_control(self.output, &Control::EngineTemporary { rel_path })?;
            }
        }
        self.gate.give();
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::PeerFailed(refusal) => return Err(refusal),
            Event::Walked(item) => self.walked(item)?,
            Event::WalkEnded => self.walked = true,
            Event::Decide { entry, decision } => self.decide(entry, &decision)?,
            Event::NeedChunks { entry, indices } => self.need_chunks(entry, indices)?,
            Event::Chunk { header, data } => write_data(self.output, &header, &data)?,
            Event::Snapshot { entry, size, wal } => self.snapshot(entry, size, wal)?,
            Event::Manifest {
                entry,
                record,
                manifest,
                retained,
                racy,
                bytes_read,
            } => {
                self.bytes_read = self.bytes_read.saturating_add(bytes_read);
                write_control(
                    self.output,
                    &Control::Manifest {
                        entry,
                        root: manifest.root,
                        chunks: manifest.chunks.clone(),
                    },
                )?;
                let (slot, _) = self.slot(entry)?;
                if !matches!(slot, SourceEntry::Queued | SourceEntry::Working) {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                *slot = SourceEntry::Offered {
                    manifest,
                    retained,
                    record,
                    racy,
                };
            }
            Event::Held { entry, held } => {
                let record = self
                    .awaiting
                    .remove(&entry)
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
                // A capture is recorded only once the destination holds its
                // bytes durably, so a committed capture never costs a source
                // read again (R25, OI-1001-Q15).
                if let (true, Some((key, manifest))) = (held, record) {
                    self.committer.submit(LedgerItem {
                        entry,
                        key,
                        record: LedgerRecord::Capture(manifest),
                    })?;
                }
            }
            Event::End {
                entry,
                record,
                root,
                chunks,
                size,
                racy,
                bytes_read,
            } => {
                self.bytes_read = self.bytes_read.saturating_add(bytes_read);
                self.finish_entry(entry)?;
                self.awaiting.insert(entry, record);
                write_control(
                    self.output,
                    &Control::End {
                        entry,
                        root,
                        chunks,
                        size,
                        racy,
                    },
                )?;
                fault_point!(ServeAfterContent);
            }
            Event::Refused {
                entry,
                refusal,
                bytes_read,
                remember,
            } => {
                self.bytes_read = self.bytes_read.saturating_add(bytes_read);
                let row = self.finish_entry(entry)?;
                // #186: a refusal the seat's bytes alone gave needs no
                // `Held`: it is recorded at once, so the next run refuses
                // the unchanged seat without opening it (R25).
                if let Some((key, refused)) = remember {
                    self.committer.submit(LedgerItem {
                        entry,
                        key,
                        record: LedgerRecord::Refused(refused),
                    })?;
                }
                write_control(
                    self.output,
                    &Control::Refused {
                        entry: Some(entry),
                        rel_path: row.rel_path.clone(),
                        code: refusal.code().to_owned(),
                        sqlite_code: refusal.sqlite_code(),
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Announce an entry's content as a snapshot (#218): only while its
    /// job is on a capture thread.
    fn snapshot(&mut self, entry: u64, size: u64, wal: Option<SidecarId>) -> Result<()> {
        let (slot, _) = self.slot(entry)?;
        if !matches!(slot, SourceEntry::Queued | SourceEntry::Working) {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        write_control(self.output, &Control::SqliteSnapshot { entry, size, wal })
    }

    fn decide(&mut self, entry: u64, decision: &Decision) -> Result<()> {
        let wal = self.wals.remove(&entry);
        let (slot, held) = self.slot(entry)?;
        if !matches!(slot, SourceEntry::Undecided) {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        let row = held
            .as_ref()
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        let regular = row.kind == FileKind::Regular;
        let (next, job) = match decision {
            Decision::Skip | Decision::Reuse | Decision::Refuse { .. } => {
                // Retired: nothing more is read or sent for it.
                *held = None;
                (SourceEntry::Done, None)
            }
            Decision::Send | Decision::WantManifest if !regular => {
                return Err(BulkloadRefusal::ProtocolStateViolation);
            }
            Decision::Send => (
                SourceEntry::Queued,
                Some(Job::Send {
                    entry,
                    row: Arc::clone(row),
                    wal,
                }),
            ),
            Decision::WantManifest => (
                SourceEntry::Queued,
                Some(Job::Manifest {
                    entry,
                    row: Arc::clone(row),
                    wal,
                }),
            ),
        };
        *slot = next;
        self.undecided -= 1;
        self.queue.extend(job);
        Ok(())
    }

    fn need_chunks(&mut self, entry: u64, indices: Vec<u32>) -> Result<()> {
        let (slot, held) = self.slot(entry)?;
        let row = Arc::clone(
            held.as_ref()
                .ok_or(BulkloadRefusal::ProtocolStateViolation)?,
        );
        let SourceEntry::Offered {
            manifest,
            retained,
            record,
            racy,
        } = std::mem::replace(slot, SourceEntry::Working)
        else {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        };
        let count = manifest.chunks.len();
        if indices.windows(2).any(|pair| pair.first() >= pair.get(1))
            || indices.iter().any(|index| *index as usize >= count)
        {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        self.jobs
            .send(Job::Serve {
                entry,
                row,
                manifest,
                indices,
                retained,
                record,
                racy,
            })
            .map_err(|_| BulkloadRefusal::WorkerLost)
    }
}

/// One capture thread: take jobs until the queue closes.
fn capture_worker(work: &SourceWork<'_>, jobs: &Mutex<Receiver<Job>>, events: &Sender<Event>) {
    let store = Store::open_reader(work.state);
    loop {
        let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = job else {
            break;
        };
        let event = match &store {
            Ok(store) => run_job(work, store, job, events),
            Err(refusal) => Event::Refused {
                entry: match job {
                    Job::Send { entry, .. }
                    | Job::Manifest { entry, .. }
                    | Job::Serve { entry, .. } => entry,
                },
                refusal: refusal.clone(),
                bytes_read: 0,
                remember: None,
            },
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

fn run_job(work: &SourceWork<'_>, store: &Store, job: Job, events: &Sender<Event>) -> Event {
    let mut bytes_read = 0;
    // What a capture's own bytes refused, to remember, and under which key
    // (#186; a snapshot's under its snapshot key, #218).
    let mut sniffed = None;
    let (entry, outcome) = match job {
        Job::Send { entry, row, wal } => {
            let outcome = remembered_refusal(work, store, &row, wal.as_ref()).and_then(|()| {
                send_capture(
                    work,
                    (entry, &row, wal.as_ref()),
                    false,
                    events,
                    &mut bytes_read,
                    &mut sniffed,
                )
            });
            (entry, outcome)
        }
        Job::Manifest { entry, row, wal } => {
            let outcome = remembered_refusal(work, store, &row, wal.as_ref()).and_then(|()| {
                match manifest_capture(
                    work,
                    store,
                    (entry, &row, wal.as_ref()),
                    events,
                    &mut bytes_read,
                    &mut sniffed,
                ) {
                    Ok(Offered::Offer((record, manifest, retained, racy))) => Ok(Event::Manifest {
                        entry,
                        record,
                        manifest,
                        retained,
                        racy,
                        bytes_read,
                    }),
                    // A snapshot seat: its manifest, or its streamed content
                    // when its slot could not be kept (#218).
                    Ok(Offered::Done(event)) => Ok(event),
                    // No ledger row and no room to keep the chunks: building
                    // a manifest first would read the seat twice (R25).
                    // Stream it instead; the destination takes data in place
                    // of a manifest. A database seat is still offered as a
                    // manifest: its slot, not memory, keeps its chunks, so
                    // the slot budget alone decides (#218 review, design
                    // 5.6).
                    Ok(Offered::NoRoom) => send_capture(
                        work,
                        (entry, &row, wal.as_ref()),
                        true,
                        events,
                        &mut bytes_read,
                        &mut sniffed,
                    ),
                    Err(refusal) => Err(refusal),
                }
            });
            (entry, outcome)
        }
        Job::Serve {
            entry,
            row,
            manifest,
            indices,
            retained,
            record,
            racy,
        } => (
            entry,
            serve_chunks(
                work,
                entry,
                &row,
                &manifest,
                &indices,
                retained.as_ref(),
                events,
                &mut bytes_read,
            )
            .map(|event| match event {
                Event::End {
                    entry,
                    root,
                    chunks,
                    size,
                    bytes_read,
                    ..
                } => Event::End {
                    entry,
                    record: record.map(|key| (key, manifest)),
                    root,
                    chunks,
                    size,
                    racy,
                    bytes_read,
                },
                other => other,
            }),
        ),
    };
    outcome.unwrap_or_else(|refusal| Event::Refused {
        entry,
        refusal,
        bytes_read,
        remember: sniffed,
    })
}

/// #186 (R25): a seat whose refusal the ledger remembers under this exact
/// row key (its path and stat identity) is refused again from that record,
/// with the same code, before the file is opened: 0 source bytes. A seat
/// whose identity moved has another key, and is sniffed again.
///
/// The records depend on the session's mode (#218, R8): `refuse` mode
/// honours the v5 header record and the WAL-header record; `snapshot` mode
/// honours the WAL-header record, ignores the v5 one (it does not say which
/// magic was seen: the seat is sniffed once more), and honours a snapshot
/// refusal under the seat's snapshot key, its row and the `-wal` identity
/// the walk offered.
fn remembered_refusal(
    work: &SourceWork<'_>,
    store: &Store,
    row: &RowSchema,
    wal: Option<&SidecarId>,
) -> Result<()> {
    let by_row = ledger_read(
        work.ledger,
        store.refused_seat(&row_key(work.authority, row)?),
    )?
    .filter(|refused| work.sqlite == SqliteMode::Refuse || *refused != RefusedSeat::SqliteHeader);
    let refused = match by_row {
        Some(refused) => Some(refused),
        None if work.sqlite == SqliteMode::Snapshot && row.kind == FileKind::Regular => {
            ledger_read(
                work.ledger,
                store.refused_seat(&sqlite_key(work.authority, row, wal)?),
            )?
        }
        None => None,
    };
    refused.map_or(Ok(()), |refused| {
        counters::bump(Counter::TransferRefusedSeatsRemembered);
        Err(refused.refusal())
    })
}

/// One offered seat: its entry, its row and the `-wal` identity the walk
/// offered beside it (#218).
type Offering<'s> = (u64, &'s RowSchema, Option<&'s SidecarId>);

/// Read, chunk and stream one file; its `End` carries the capture to record.
/// A database seat in `snapshot` mode is snapshotted instead (#218): its
/// manifest is offered when `want_manifest` (the destination asked for one),
/// its content streamed otherwise.
fn send_capture(
    work: &SourceWork<'_>,
    (entry, row, wal): Offering<'_>,
    want_manifest: bool,
    events: &Sender<Event>,
    bytes_read: &mut u64,
    sniffed: &mut Option<(Vec<u8>, RefusedSeat)>,
) -> Result<Event> {
    let key = row_key(work.authority, row)?;
    let sink = |index, offset, digest, data: Vec<u8>| {
        send_chunk(work, events, entry, index, offset, digest, data)
    };
    let (chunks, racy) = match capture_file(work, row, wal, bytes_read, sniffed, sink)? {
        Captured::Raw(chunks, racy) => (chunks, racy),
        Captured::Database(opened) => {
            return snapshot_seat(
                work,
                entry,
                row,
                opened,
                want_manifest,
                events,
                bytes_read,
                sniffed,
            );
        }
    };
    let manifest = Manifest::new(chunks);
    Ok(Event::End {
        entry,
        root: manifest.root,
        chunks: u32::try_from(manifest.chunks.len())
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        size: row.size,
        // A racy capture is sent, never recorded (#86): its stat identity
        // cannot vouch for the bytes, so the next run reads it again.
        record: (!racy).then_some((key, manifest)),
        racy,
        bytes_read: *bytes_read,
    })
}

/// Hand one chunk to the sending thread, once its credit is taken.
fn send_chunk(
    work: &SourceWork<'_>,
    events: &Sender<Event>,
    entry: u64,
    index: u32,
    offset: u64,
    digest: [u8; 32],
    data: Vec<u8>,
) -> Result<()> {
    let size = u32::try_from(data.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    work.credit.acquire(u64::from(size))?;
    events
        .send(Event::Chunk {
            header: DataHeader {
                entry,
                index,
                size,
                offset,
                digest,
            },
            data: Arc::new(data),
        })
        .map_err(|_| BulkloadRefusal::WorkerLost)
}

/// A manifest to offer: the row key to record (none when it came from the
/// ledger, or when the capture was racy), the manifest, its retained chunks,
/// and whether the capture was racy (#86).
type Offer = (Option<Vec<u8>>, Manifest, Option<Retained>, bool);

/// What a manifest job produced.
enum Offered {
    /// A manifest to offer.
    Offer(Offer),
    /// No ledger row, and the file does not fit the retention budget: the
    /// caller streams it instead.
    NoRoom,
    /// A snapshot seat's whole answer (#218): its manifest, or its content
    /// streamed in place of one.
    Done(Event),
}

/// A source ledger read, as WP0(g) answers it (OI-1003-Q37: "a corrupt or
/// absent source ledger is treated as empty"). Under
/// [`LedgerSync::Relaxed`] a read that fails is counted
/// (`source_ledger_unreadable`) and answered as a miss: the seat is read,
/// or sniffed, once more, and no row of a damaged ledger is trusted. Under
/// [`LedgerSync::Full`] the failure is returned, as before WP0(g).
fn ledger_read<T>(mode: LedgerSync, read: Result<Option<T>>) -> Result<Option<T>> {
    match read {
        Err(_) if mode == LedgerSync::Relaxed => {
            counters::bump(Counter::SourceLedgerUnreadable);
            Ok(None)
        }
        read => read,
    }
}

/// One entry's manifest: from the ledger when this exact stat identity is
/// recorded (no source read), otherwise from one read of the file, whose
/// chunks are all kept in memory for the requests that follow.
/// [`Offered::NoRoom`] when the retention budget cannot hold the file: the
/// caller streams it instead of reading it twice (#77 review F1). A
/// database seat in `snapshot` mode is snapshotted instead (#218).
#[allow(
    clippy::too_many_arguments,
    reason = "one capture job's inputs, each used once"
)]
fn manifest_capture(
    work: &SourceWork<'_>,
    store: &Store,
    (entry, row, wal): Offering<'_>,
    events: &Sender<Event>,
    bytes_read: &mut u64,
    sniffed: &mut Option<(Vec<u8>, RefusedSeat)>,
) -> Result<Offered> {
    let key = row_key(work.authority, row)?;
    // A database seat is never recorded, so a ledger row names a file that
    // was carried raw under this exact identity.
    if let Some(manifest) = ledger_read(work.ledger, store.capture(&key))? {
        if manifest.size() == Some(row.size) && manifest.chunks.len() <= MAX_MANIFEST_CHUNKS {
            return Ok(Offered::Offer((None, manifest, None, false)));
        }
    }
    // The ledger has no usable row under this key, so the seat is read to
    // build the manifest. This is the whole cost of a row WP0(g) lost; a
    // changed or never recorded seat is counted here too.
    counters::bump(Counter::SourceLedgerMissReads);
    // Every chunk a fresh manifest names must be kept, so the requests that
    // follow are served from memory: a seat is read at most once a session.
    let Some(mut retained) = Retained::reserve(&work.retain, row.size) else {
        return Ok(Offered::NoRoom);
    };
    let captured = capture_file(work, row, wal, bytes_read, sniffed, |_, _, _, data| {
        retained.chunks.push(Arc::new(data));
        Ok(())
    })?;
    match captured {
        Captured::Raw(chunks, racy) => Ok(Offered::Offer((
            (!racy).then_some(key),
            Manifest::new(chunks),
            Some(retained),
            racy,
        ))),
        Captured::Database(opened) => {
            drop(retained);
            snapshot_seat(work, entry, row, opened, true, events, bytes_read, sniffed)
                .map(Offered::Done)
        }
    }
}

/// Send the requested chunks of an offered manifest, from memory when they
/// were retained, from its snapshot slot for a `SQLite` snapshot (#218),
/// otherwise read again at their offsets and re-verified against the
/// manifest under an unchanged stat identity.
#[allow(
    clippy::too_many_arguments,
    reason = "one capture job's inputs, each used once"
)]
fn serve_chunks(
    work: &SourceWork<'_>,
    entry: u64,
    row: &RowSchema,
    manifest: &Manifest,
    indices: &[u32],
    retained: Option<&Retained>,
    events: &Sender<Event>,
    bytes_read: &mut u64,
) -> Result<Event> {
    let mut offsets = Vec::with_capacity(manifest.chunks.len());
    let mut offset = 0_u64;
    for chunk in &manifest.chunks {
        offsets.push(offset);
        offset = offset
            .checked_add(chunk.size)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
    }
    let slot = retained.and_then(|held| held.slot.as_ref());
    let held =
        retained.filter(|held| held.slot.is_none() && held.chunks.len() == manifest.chunks.len());
    let opened = if held.is_none() && slot.is_none() && !indices.is_empty() {
        let file = open_source(work, row)?;
        if StatIdentity::from_metadata(&file.metadata().refuse_at("transfer::serve_chunks")?)
            != StatIdentity::from_row(row)
        {
            return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
        }
        Some(file)
    } else {
        None
    };
    for index in indices {
        let at = *index as usize;
        let spec = manifest
            .chunks
            .get(at)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        let offset = *offsets
            .get(at)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        let data = if let Some(held) = held {
            Arc::clone(
                held.chunks
                    .get(at)
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?,
            )
        } else if let Some(slot) = slot {
            slot_chunk(slot, spec, offset)?
        } else {
            let file = opened
                .as_ref()
                .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
            let mut data = vec![
                0_u8;
                usize::try_from(spec.size)
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
            ];
            // A live truncate gives a short count, never a signal.
            let read = crate::io::sys::pread_full(file, &mut data, offset)
                .map_err(|_| BulkloadRefusal::SourceChangedAfterSnapshot)?;
            if read != data.len() {
                return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
            }
            *bytes_read = bytes_read.saturating_add(spec.size);
            counters::add_len(Counter::SourceFileRead, data.len());
            if counters::hash(Counter::HashCaptureChunk, &data) != spec.digest {
                return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
            }
            Arc::new(data)
        };
        let size = u32::try_from(data.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        work.credit.acquire(u64::from(size))?;
        events
            .send(Event::Chunk {
                header: DataHeader {
                    entry,
                    index: *index,
                    size,
                    offset,
                    digest: spec.digest,
                },
                data,
            })
            .map_err(|_| BulkloadRefusal::WorkerLost)?;
    }
    if let Some(file) = &opened {
        if StatIdentity::from_metadata(&file.metadata().refuse_at("transfer::serve_chunks")?)
            != StatIdentity::from_row(row)
        {
            return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
        }
    }
    Ok(Event::End {
        entry,
        record: None,
        root: manifest.root,
        chunks: u32::try_from(manifest.chunks.len())
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        // The manifest's own size: a snapshot's is not its row's (#218).
        size: manifest.size().unwrap_or(row.size),
        // The caller sets the offer's own `racy`.
        racy: false,
        bytes_read: *bytes_read,
    })
}

/// One chunk of the snapshot a manifest was built from, read again from its
/// slot and checked against its digest: private state, not a source read
/// (#218).
fn slot_chunk(slot: &SlotFile, spec: &ChunkSpec, offset: u64) -> Result<Arc<Vec<u8>>> {
    let mut data =
        vec![0_u8; usize::try_from(spec.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?];
    let read = crate::io::sys::pread_full(&slot.file, &mut data, offset)
        .refuse_at("transfer::slot_chunk")?;
    counters::add_len(Counter::SourceSnapshotRead, read);
    if read != data.len() || counters::hash(Counter::HashCaptureChunk, &data) != spec.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    Ok(Arc::new(data))
}

/// What [`capture_file`] did with a seat.
enum Captured<'w> {
    /// Read and chunked: the chunks, and whether the capture was racy.
    Raw(Vec<ChunkSpec>, bool),
    /// `snapshot` mode only (#218): the seat's first bytes are a `SQLite`
    /// database magic. Nothing past the sniff was read, and no chunk was
    /// handed on; the caller snapshots it ([`snapshot_seat`]).
    Database(Opened<'w>),
}

/// A seat opened for its sniff (#218).
///
/// In `snapshot` mode its descriptor is closed only under the session's
/// backup lock, unless the sniff found no database (#218 review): closing
/// any descriptor of an inode drops every POSIX lock this process holds on
/// it, so a sniff of a hard link closed while another seat's backup of the
/// same store holds `SHARED` would let a writer take `EXCLUSIVE` under that
/// backup. Every `SQLite` connection the source opens lives under that
/// lock, so no close can meet one.
struct Opened<'w> {
    /// The seat, opened beneath the root; its stat identity is the row's.
    /// `None` once closed or released.
    file: Option<std::fs::File>,
    /// The clock read before it was opened, which its racy check uses.
    started_ns: i128,
    /// The lock its descriptor is closed under (`snapshot` mode).
    backup: Option<&'w Mutex<()>>,
}

impl Opened<'_> {
    fn file(&self) -> Result<&std::fs::File> {
        self.file.as_ref().ok_or(BulkloadRefusal::WorkerLost)
    }

    /// The descriptor of a seat that is not a database: read and closed as
    /// any file's.
    fn release(&mut self) -> Result<std::fs::File> {
        self.file.take().ok_or(BulkloadRefusal::WorkerLost)
    }

    /// Close the descriptor; the caller holds the backup lock.
    fn close_locked(&mut self) {
        Self::close(self.file.take());
    }

    fn close(file: Option<std::fs::File>) {
        #[cfg(test)]
        if file.is_some() && !HOLDS_BACKUP.get() {
            SNIFF_CLOSED_UNLOCKED.fetch_add(1, Ordering::Relaxed);
        }
        drop(file);
    }
}

impl Drop for Opened<'_> {
    fn drop(&mut self) {
        let file = self.file.take();
        if file.is_none() {
            return;
        }
        let Some(backup) = self.backup else {
            drop(file);
            return;
        };
        let _one = backup.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        let _held = BackupHeld::mark();
        Self::close(file);
    }
}

/// Read one source file once, chunk it and hash each chunk, handing every
/// chunk to `sink` as `(index, offset, digest, bytes)`. The file's stat
/// identity must match its row before and after the read; a capture that
/// fails either check is refused, and only its caller decides what any
/// already-handed chunk means.
///
/// The first [`SNIFF_BYTES`] are read ahead of the rest. A `SQLite` database
/// or WAL magic there refuses the seat: those bytes are then counted as
/// `source_sniff_bytes`, never as content (`bytes_read`,
/// `read_source_file_bytes`), and `sniffed` is set when the refusal may be
/// remembered (#186): the seat's stat identity did not move across the
/// sniff and the seat is not racy, so the identity vouches for the header
/// that was read. Otherwise the same bytes are the file's first content
/// bytes, counted as such and chunked with the rest. In `snapshot` mode a
/// database magic is not refused: the sniff is counted, and the opened seat
/// is returned for its snapshot ([`Captured::Database`], #218); a WAL magic
/// is refused and remembered as such ([`RefusedSeat::SqliteWalHeader`]).
///
/// Returns the chunks and whether the capture was racy (#86): the seat's
/// mtime or ctime falls within [`RACY_GRANULARITY_NS`] of the clock read
/// before the file was opened, or later than the clock read after the final
/// stat check. A same-size rewrite in that tick can keep the stat identity,
/// so the identity cannot vouch for the bytes read, exactly as in the Git
/// carry census (R-N76): the capture is sent, but never recorded as a reuse
/// key on either side.
#[allow(
    clippy::too_many_lines,
    reason = "one sniff, its refusals and one read, in the order they happen"
)]
fn capture_file<'w>(
    work: &SourceWork<'w>,
    row: &RowSchema,
    beside: Option<&SidecarId>,
    bytes_read: &mut u64,
    sniffed: &mut Option<(Vec<u8>, RefusedSeat)>,
    mut sink: impl FnMut(u32, u64, [u8; 32], Vec<u8>) -> Result<()>,
) -> Result<Captured<'w>> {
    if row.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if crate::walk::SQLITE_SIDECAR_SUFFIXES
        .iter()
        .any(|suffix| row.rel_path.ends_with(suffix))
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    #[cfg(feature = "fault-injection")]
    let file_path = work.root.join(crate::walk::rel_path(&row.rel_path));
    let started_ns = capture_clock(work.root);
    let mut opened = Opened {
        file: Some(open_source(work, row)?),
        started_ns,
        backup: (work.sqlite == SqliteMode::Snapshot).then_some(work.backup),
    };
    let expected = StatIdentity::from_row(row);
    // A seat that moved since the walk is refused, except that in
    // `snapshot` mode it is sniffed first: a live database moving between
    // the walk and its capture is the normal case, and its snapshot is
    // consistent whatever moved; it is just not settled (#218, design
    // section 5.4).
    let moved = StatIdentity::from_metadata(
        &opened
            .file()?
            .metadata()
            .refuse_at("transfer::capture_file")?,
    ) != expected;
    if moved && work.sqlite == SqliteMode::Refuse {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let mut prefix = Vec::new();
    SourceReader {
        file: opened.file()?,
        offset: 0,
    }
    .take(SNIFF_BYTES)
    .read_to_end(&mut prefix)
    .refuse_at("transfer::capture_file")?;
    let database = prefix.starts_with(SQLITE_DATABASE_MAGIC);
    if database && work.sqlite == SqliteMode::Snapshot {
        counters::add_len(Counter::SourceSniff, prefix.len());
        return Ok(Captured::Database(opened));
    }
    // Not a database for this session: closed as any file is.
    let file = opened.release()?;
    let wal = prefix.starts_with(&[0x37, 0x7f, 0x06, 0x82])
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x83]);
    if database || wal {
        counters::add_len(Counter::SourceSniff, prefix.len());
        if moved {
            return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
        }
        #[cfg(test)]
        after_sniff(work.root);
        let unmoved = file
            .metadata()
            .is_ok_and(|after| StatIdentity::from_metadata(&after) == expected);
        if unmoved && !crate::git_carry::racy(row, started_ns, capture_clock(work.root)) {
            let kind = match work.sqlite {
                SqliteMode::Refuse => RefusedSeat::SqliteHeader,
                SqliteMode::Snapshot => RefusedSeat::SqliteWalHeader,
            };
            *sniffed = Some((row_key(work.authority, row)?, kind));
        }
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    // #218 review: in `snapshot` mode a file without the database magic (an
    // encrypted store) beside a `-wal` that holds frames is refused, never
    // carried raw: its main file alone may miss every transaction still in
    // the `-wal`, or hold a checkpoint half done. Its row and the `-wal`'s
    // identity vouch for the refusal together, so it is remembered under
    // its snapshot key when the main file did not move and was not racy. A
    // `-wal` with no frame (empty) leaves the main file whole.
    if work.sqlite == SqliteMode::Snapshot && beside.is_some_and(|wal| wal.size > 0) {
        counters::add_len(Counter::SourceSniff, prefix.len());
        if !moved {
            *sniffed = refused_beside_wal(work, row, &file, started_ns, beside)?;
        }
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    if moved {
        // Read only to tell a database apart: a sniff, never content.
        counters::add_len(Counter::SourceSniff, prefix.len());
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    // Not refused: the sniffed bytes are the file's first content bytes.
    *bytes_read = bytes_read.saturating_add(prefix.len() as u64);
    counters::add_len(Counter::SourceFileRead, prefix.len());
    let reader = CountReader {
        input: SourceReader {
            file: &file,
            offset: prefix.len() as u64,
        },
        count: bytes_read,
    };
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    let _cdc_timer = PhaseTimer(&CDC_HASH_NS, Instant::now());
    for chunk in fastcdc::v2020::StreamCDC::new(
        prefix.as_slice().chain(reader),
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    ) {
        let chunk = chunk.map_err(|_| BulkloadRefusal::Io(None))?;
        fault_mid_read!(chunks.is_empty(), &file_path);
        if chunks.len() >= MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let digest = counters::hash(Counter::HashCaptureChunk, &chunk.data);
        let size = chunk.data.len() as u64;
        let index = u32::try_from(chunks.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        chunks.push(ChunkSpec { digest, size });
        sink(index, offset, digest, chunk.data)?;
        offset = offset
            .checked_add(size)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
    }
    if offset != row.size
        || StatIdentity::from_metadata(&file.metadata().refuse_at("transfer::capture_file")?)
            != expected
    {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let racy = crate::git_carry::racy(row, started_ns, capture_clock(work.root));
    if racy {
        counters::bump(Counter::TransferRacyCaptures);
    }
    Ok(Captured::Raw(chunks, racy))
}

/// The record of a file without the database magic refused beside a `-wal`
/// that holds frames (#218 review): under its snapshot key, when the file
/// did not move across the sniff and is not racy.
fn refused_beside_wal(
    work: &SourceWork<'_>,
    row: &RowSchema,
    file: &std::fs::File,
    started_ns: i128,
    beside: Option<&SidecarId>,
) -> Result<Option<(Vec<u8>, RefusedSeat)>> {
    #[cfg(test)]
    after_sniff(work.root);
    let unmoved = file
        .metadata()
        .is_ok_and(|after| StatIdentity::from_metadata(&after) == StatIdentity::from_row(row));
    if unmoved && !crate::git_carry::racy(row, started_ns, capture_clock(work.root)) {
        return Ok(Some((
            sqlite_key(work.authority, row, beside)?,
            RefusedSeat::SqliteBesideWal,
        )));
    }
    Ok(None)
}

/// The serial of this process's next snapshot slot name (#218).
static SLOT_SERIAL: AtomicU64 = AtomicU64::new(0);

/// `SQLITE_CANTOPEN_SYMLINK`: `SQLITE_OPEN_NOFOLLOW` met a symlink at some
/// component of the path.
const SQLITE_CANTOPEN_SYMLINK: i32 = 0x060E;

/// The first 16 bytes of every `SQLite` database file.
const SQLITE_DATABASE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// A database seat taken as a snapshot (#218).
struct Snapshotted {
    slot: SlotFile,
    size: u64,
    /// The `-wal`'s identity after the backup (`None`: none).
    wal: Option<SidecarId>,
    /// The capture is settled (design section 5.4): neither the main file
    /// nor the `-wal` moved between the walk, the pre-stat and the
    /// post-stat, and neither is stamped within the racy allowance, so the
    /// row and `wal` vouch for the snapshot's bytes.
    settled: bool,
}

/// Snapshot one database seat and answer its entry (#218): announce the
/// snapshot ([`Control::SqliteSnapshot`]), then offer its manifest (its slot
/// kept for the chunk requests, within the slot budget it reserved before
/// its backup) when `want_manifest`, or stream its chunks and `End`. Nothing is recorded in the
/// source ledger: the slot is gone after the entry, so a ledger manifest
/// could never be served again (design section 5.5). An unsettled capture
/// is sent as racy, so neither side keeps a reuse key for it.
#[allow(
    clippy::too_many_arguments,
    reason = "one capture job's inputs, each used once"
)]
fn snapshot_seat(
    work: &SourceWork<'_>,
    entry: u64,
    row: &RowSchema,
    mut opened: Opened<'_>,
    want_manifest: bool,
    events: &Sender<Event>,
    bytes_read: &mut u64,
    sniffed: &mut Option<(Vec<u8>, RefusedSeat)>,
) -> Result<Event> {
    // R4: another user's database would get `-shm` and `-wal` files owned
    // by this one. Refused before `SQLite` opens it, from the descriptor the
    // sniff holds.
    if opened
        .file()?
        .metadata()
        .refuse_at("transfer::snapshot_seat")?
        .uid()
        != work.euid
    {
        return Err(BulkloadRefusal::SqliteSourceNotOwner);
    }
    // The sniff's descriptor is closed under the backup lock: by
    // `take_snapshot` once its backup is done, or by its drop (#218 review).
    let taken = take_snapshot(work, row, &mut opened, bytes_read, sniffed)?;
    drop(opened);
    events
        .send(Event::Snapshot {
            entry,
            size: taken.size,
            wal: taken.wal,
        })
        .map_err(|_| BulkloadRefusal::WorkerLost)?;
    #[cfg(test)]
    after_snapshot(work.root);
    let racy = !taken.settled;
    if want_manifest {
        // The slot already holds its reservation (#218 review).
        let retained = Retained::slot(&work.retain, taken.slot);
        let chunks = chunk_slot(
            retained.slot.as_ref().ok_or(BulkloadRefusal::WorkerLost)?,
            taken.size,
            |_, _, _, _| Ok(()),
        )?;
        return Ok(Event::Manifest {
            entry,
            record: None,
            manifest: Manifest::new(chunks),
            retained: Some(retained),
            racy,
            bytes_read: *bytes_read,
        });
    }
    let slot = taken.slot;
    let chunks = chunk_slot(&slot, taken.size, |index, offset, digest, data| {
        send_chunk(work, events, entry, index, offset, digest, data)
    })?;
    drop(slot);
    let manifest = Manifest::new(chunks);
    Ok(Event::End {
        entry,
        record: None,
        root: manifest.root,
        chunks: u32::try_from(manifest.chunks.len())
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        size: taken.size,
        racy,
        bytes_read: *bytes_read,
    })
}

/// Chunk a snapshot slot as a capture chunks a seat. A private read, counted
/// as `read_source_snapshot_bytes`, never as a source read.
fn chunk_slot(
    slot: &SlotFile,
    size: u64,
    mut sink: impl FnMut(u32, u64, [u8; 32], Vec<u8>) -> Result<()>,
) -> Result<Vec<ChunkSpec>> {
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    let _cdc_timer = PhaseTimer(&CDC_HASH_NS, Instant::now());
    for chunk in fastcdc::v2020::StreamCDC::new(
        SlotReader {
            file: &slot.file,
            offset: 0,
        },
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    ) {
        let chunk = chunk.map_err(|error| match error {
            fastcdc::v2020::Error::IoError(error) => {
                crate::refuse::io(&error, "transfer::chunk_slot")
            }
            // The slot is this process's own file: anything else means its
            // bytes are not the snapshot's.
            _ => BulkloadRefusal::DigestMismatch,
        })?;
        if chunks.len() >= MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let digest = counters::hash(Counter::HashCaptureChunk, &chunk.data);
        let length = chunk.data.len() as u64;
        let index = u32::try_from(chunks.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        chunks.push(ChunkSpec {
            digest,
            size: length,
        });
        sink(index, offset, digest, chunk.data)?;
        offset = offset
            .checked_add(length)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
    }
    if offset == size {
        Ok(chunks)
    } else {
        Err(BulkloadRefusal::DigestMismatch)
    }
}

/// Sequential `pread`s of a snapshot slot, counted as private reads.
struct SlotReader<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

impl Read for SlotReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = crate::io::sys::pread_full(self.file, buffer, self.offset)?;
        self.offset = self.offset.saturating_add(read as u64);
        counters::add_len(Counter::SourceSnapshotRead, read);
        Ok(read)
    }
}

/// The `-wal` beside a seat, as an `lstat` beneath the seat's directory
/// sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WalStat {
    /// No `-wal`.
    Absent,
    /// A regular `-wal` with this identity.
    Present(SidecarId),
    /// Not a regular file, or the `lstat` failed otherwise: it cannot vouch
    /// for anything.
    Unknown,
}

impl WalStat {
    fn of(parent: BorrowedFd<'_>, leaf: &[u8]) -> Self {
        let mut name = leaf.to_vec();
        name.extend_from_slice(b"-wal");
        let Ok(name) = std::ffi::CString::new(name) else {
            return Self::Unknown;
        };
        match crate::io::sys::fstatat_nofollow(parent, &name) {
            Ok(stat) if stat.is_file() => Self::Present(SidecarId {
                dev: stat.node.dev,
                ino: stat.node.ino,
                size: stat.size,
                mtime_ns: stat.mtime_ns,
                ctime_ns: stat.ctime_ns,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::Absent,
            _ => Self::Unknown,
        }
    }

    /// The identity a key carries: `None` when there is no `-wal` (or none
    /// that could be told).
    const fn id(self) -> Option<SidecarId> {
        match self {
            Self::Present(id) => Some(id),
            Self::Absent | Self::Unknown => None,
        }
    }

    /// The size a wal-index rebuild could read.
    const fn size(self) -> u64 {
        match self {
            Self::Present(id) => id.size,
            Self::Absent | Self::Unknown => 0,
        }
    }
}

/// Whether a stamp pair is racy against a capture's clocks, as
/// `git_carry::racy` judges a row (#86).
const fn stamps_racy(mtime_ns: i128, ctime_ns: i128, started_ns: i128, now_ns: i128) -> bool {
    let window = started_ns.saturating_sub(RACY_GRANULARITY_NS);
    mtime_ns >= window || ctime_ns >= window || mtime_ns > now_ns || ctime_ns > now_ns
}

/// Whether a `-wal` before and after a backup lets the capture settle: the
/// same identity, not stamped within the racy allowance, or none before and
/// an empty one after (OI-1003-Q72's file, created by the read itself). An
/// empty `-wal` holds no frame, so its stamps vouch for nothing and are not
/// judged: the database is then its main file, whose own identity and
/// stamps are checked. A `-wal` that could not be told never settles.
fn wal_settled(before: WalStat, after: WalStat, started_ns: i128, now_ns: i128) -> bool {
    match (before, after) {
        (WalStat::Absent, WalStat::Absent) => true,
        (WalStat::Absent, WalStat::Present(created)) => created.size == 0,
        (WalStat::Present(before), WalStat::Present(after)) => {
            before == after
                && (after.size == 0
                    || !stamps_racy(after.mtime_ns, after.ctime_ns, started_ns, now_ns))
        }
        _ => false,
    }
}

/// A seat's directory beneath the root, opened component by component
/// (`None`: the root itself), and its leaf name.
fn seat_directory<'r>(
    work: &SourceWork<'_>,
    row: &'r RowSchema,
) -> Result<(Option<std::os::fd::OwnedFd>, &'r [u8])> {
    Ok(match row.rel_path.iter().rposition(|byte| *byte == b'/') {
        Some(at) => (
            Some(
                crate::io::sys::openat_beneath(
                    work.root_fd,
                    crate::walk::rel_path(row.rel_path.get(..at).unwrap_or_default()),
                    crate::io::OpenMode::Directory,
                )
                .refuse_at("transfer::seat_directory")?,
            ),
            row.rel_path.get(at + 1..).unwrap_or_default(),
        ),
        None => (None, row.rel_path.as_slice()),
    })
}

/// Take one database seat's snapshot into a new slot (#218). The backup is
/// the provider's stepped one, from the canonical path beneath the root;
/// one runs at a time per `serve`. The main file and its `-wal` are stat'ed
/// beneath the seat's directory before and after; the walked inode must
/// still be at the path afterwards, or the snapshot is discarded and
/// refused `PATH_ESCAPES_ROOT` (design section 7.5); so is a path through a
/// symlink at any component, which `SQLITE_OPEN_NOFOLLOW` refuses
/// (`SQLITE_CANTOPEN_SYMLINK`). A refusal the database's bytes alone decide
/// is remembered under the snapshot key when the capture settled (R10).
///
/// Before the backup (#218 review) the slot is reserved against the slot
/// budget, waiting up to [`crate::provider_sqlite::CARRY_WALL`] for room,
/// and the slot directory's filesystem must keep the free-space floor
/// (`--min-free-percent`) with the slot written: either refuses
/// `BUDGET_EXCEEDED`, not remembered. The sniff's descriptor is closed
/// under the backup lock once the backup is done.
#[allow(
    clippy::too_many_lines,
    reason = "one backup between its pre- and post-stats, read top to bottom"
)]
fn take_snapshot(
    work: &SourceWork<'_>,
    row: &RowSchema,
    opened: &mut Opened<'_>,
    bytes_read: &mut u64,
    sniffed: &mut Option<(Vec<u8>, RefusedSeat)>,
) -> Result<Snapshotted> {
    let (directory, leaf) = seat_directory(work, row)?;
    let parent = directory
        .as_ref()
        .map_or(work.root_fd, std::os::fd::AsFd::as_fd);
    let leaf_name = std::ffi::CString::new(leaf).map_err(|_| BulkloadRefusal::PathNotPortable)?;
    let stat_main = || {
        crate::io::sys::fstatat_nofollow(parent, &leaf_name)
            .ok()
            .filter(crate::io::Stat::is_file)
    };
    let opened_node = crate::io::sys::fstat(opened.file()?)
        .refuse_at("transfer::take_snapshot")?
        .node;
    // The slot's room first, outside the backup lock: a thread waiting for
    // another slot to go never holds up a backup that fits.
    let mut reserved = work
        .slot_budget
        .reserve(
            row.size.saturating_add(WalStat::of(parent, leaf).size()),
            crate::provider_sqlite::CARRY_WALL,
        )
        .ok_or(BulkloadRefusal::BudgetExceeded)?;
    let _one = work.backup.lock().unwrap_or_else(PoisonError::into_inner);
    #[cfg(test)]
    let _held = BackupHeld::mark();
    let pre_main = stat_main();
    let pre_wal = WalStat::of(parent, leaf);
    let wal_bound = pre_wal.size();
    room_for_slot(work, &mut reserved, row.size.saturating_add(wal_bound))?;
    let slot_path = work.slots.join(format!(
        "{}-{}.sqlite",
        std::process::id(),
        SLOT_SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let source = work.root.join(crate::walk::rel_path(&row.rel_path));
    #[cfg(test)]
    before_backup(work.root);
    let (report, source_bytes) = crate::provider_sqlite::snapshot_for_carry(
        &source,
        &slot_path,
        work.as_root,
        wal_bound,
        crate::provider_sqlite::CARRY_WALL,
    );
    // Source content read, refused or not (R13).
    *bytes_read = bytes_read.saturating_add(source_bytes);
    // Whatever the backup did, the slot goes when this is dropped, and its
    // reservation with it.
    let slot = std::fs::File::open(&slot_path).map(|file| SlotFile {
        file,
        path: slot_path.clone(),
        reserved,
    });
    if slot.is_err() {
        let _ = std::fs::remove_file(&slot_path);
    }
    let post_main = stat_main();
    let post_wal = WalStat::of(parent, leaf);
    // No `SQLite` connection is open now: the sniff closes under the lock.
    opened.close_locked();
    let now_ns = capture_clock(work.root);
    let same_inode = post_main
        .as_ref()
        .is_some_and(|main| main.node == opened_node);
    let identity = |stat: &crate::io::Stat| StatIdentity {
        dev: stat.node.dev,
        ino: stat.node.ino,
        size: stat.size,
        mtime_ns: stat.mtime_ns,
        ctime_ns: stat.ctime_ns,
    };
    let settled = same_inode
        && pre_main.as_ref().map(identity) == Some(StatIdentity::from_row(row))
        && post_main.as_ref().map(identity) == pre_main.as_ref().map(identity)
        && !crate::git_carry::racy(row, opened.started_ns, now_ns)
        && wal_settled(pre_wal, post_wal, opened.started_ns, now_ns);
    let report = match report {
        Ok(report) => report,
        // A directory of the path swapped for a symlink since the walk:
        // `SQLITE_OPEN_NOFOLLOW` refuses a symlink at any component.
        Err(BulkloadRefusal::SqliteBackupFailed(Some(SQLITE_CANTOPEN_SYMLINK))) => {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        Err(refusal) => {
            if settled {
                if let Some(kind) = RefusedSeat::of_snapshot_refusal(&refusal) {
                    *sniffed = Some((
                        sqlite_key(work.authority, row, post_wal.id().as_ref())?,
                        kind,
                    ));
                }
            }
            return Err(refusal);
        }
    };
    let mut slot = slot.refuse_at("transfer::take_snapshot")?;
    if !same_inode {
        // `SQLite` resolved the path to another file than the walked seat.
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    // A snapshot can outgrow its bound when a writer committed between
    // steps: it keeps its slot only when the budget has that room now.
    if report.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES)
        || !slot.reserved.true_up(report.size)
    {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if !settled {
        counters::bump(Counter::SourceSqliteUnsettled);
    }
    Ok(Snapshotted {
        slot,
        size: report.size,
        wal: post_wal.id(),
        settled,
    })
}

/// A slot of at most `bound` bytes fits (#218 review): its reservation is
/// trued up to `bound` without waiting, and the slot directory's filesystem
/// keeps the free-space floor with it written. The slot shares that
/// filesystem with the private state, most often with the live stores
/// themselves, so it must not take it under the floor, where a live writer
/// could meet `SQLITE_FULL` (S2). `BUDGET_EXCEEDED` otherwise.
fn room_for_slot(work: &SourceWork<'_>, reserved: &mut SlotReservation, bound: u64) -> Result<()> {
    if !reserved.true_up(bound) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    crate::space::check(
        bound,
        crate::space::probe(work.slots)?,
        crate::space::min_free_percent(),
    )
    .map_err(|_| BulkloadRefusal::BudgetExceeded)
}

/// Open a source seat for reading, component by component beneath the
/// source root descriptor (W4 PR 3): a symlink at any component is refused.
fn open_source(work: &SourceWork<'_>, row: &RowSchema) -> Result<std::fs::File> {
    let fd = crate::io::sys::openat_beneath(
        work.root_fd,
        crate::walk::rel_path(&row.rel_path),
        crate::io::OpenMode::Read,
    )
    .refuse_at("transfer::open_source")?;
    Ok(std::fs::File::from(fd))
}

/// Sequential `pread`s of one source file: no shared descriptor offset, no
/// mapping, and a short count at a live truncate.
struct SourceReader<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

impl Read for SourceReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = crate::io::sys::pread_full(self.file, buffer, self.offset)?;
        self.offset = self.offset.saturating_add(read as u64);
        Ok(read)
    }
}

struct CountReader<'a, R> {
    input: R,
    count: &'a mut u64,
}
impl<R: Read> Read for CountReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(buffer)?;
        *self.count = self.count.saturating_add(count as u64);
        counters::add_len(Counter::SourceFileRead, count);
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Destination
// ---------------------------------------------------------------------------

/// What planning one manifest reads on the destination.
struct ReceiveContext<'a> {
    target: &'a Destination,
    store: &'a Store,
    /// The session's output authority, which keys this store's rows.
    authority: &'a [u8],
    /// The outputs this session's superseding publishes displace.
    displaced: &'a SharedDisplaced,
    session: &'a SessionChunks,
    salvage: &'a Salvage,
}

/// Chunks of this store's orphaned temporaries (see
/// [`Destination::salvaged`]): what a crashed session staged and sealed but
/// never published. A resume fills from them instead of asking the source
/// for those chunks. When it finishes it removes them, except those a
/// refused entry staged chunks from, within a bound (#97, #124).
#[derive(Default)]
struct Salvage {
    /// Digest to (salvaged file, offset, size). Hints only: re-verified on use.
    index: HashMap<[u8; 32], (usize, u64, u64)>,
    /// Salvaged files indexed so far.
    indexed: usize,
}

impl Salvage {
    /// Index any temporaries the sweep has salvaged since the last call, by
    /// chunking them as the source does. Unreadable bytes only cost reuse.
    fn refresh(&mut self, target: &Destination) {
        let salvaged = target.salvaged();
        for at in self.indexed..salvaged {
            let Some(file) = target.salvaged_file(at) else {
                continue;
            };
            let mut offset = 0_u64;
            let reader = SalvageReader {
                file: &file,
                offset: 0,
            };
            for chunk in fastcdc::v2020::StreamCDC::new(
                reader,
                crate::hash::CDC_MIN_BYTES,
                crate::hash::CDC_AVG_BYTES,
                crate::hash::CDC_MAX_BYTES,
            ) {
                let Ok(chunk) = chunk else {
                    break;
                };
                counters::add_len(Counter::DestLocalReuseRead, chunk.data.len());
                let digest = counters::hash(Counter::HashDestReuse, &chunk.data);
                let size = chunk.data.len() as u64;
                self.index.entry(digest).or_insert((at, offset, size));
                offset = offset.saturating_add(size);
            }
        }
        self.indexed = salvaged;
    }
}

/// Sequential reads of a salvaged temporary through `pread`, leaving the
/// shared descriptor's offset alone.
struct SalvageReader<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

impl Read for SalvageReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.offset)?;
        self.offset = self.offset.saturating_add(read as u64);
        Ok(read)
    }
}

/// End a receive: record the sweep and directory-creation outcomes, commit
/// every pending output group, retire the salvage, then finish directories
/// and flush the session.
fn finish_receive(
    target: &mut Destination,
    store: &Store,
    committer: Committer<PublishSink>,
    stats: &mut TransferStats,
    salvage_staged: &HashMap<Vec<u8>, Vec<usize>>,
    sidecars: &[SidecarReport],
    snapshot_bases: &HashSet<Vec<u8>>,
) -> Result<()> {
    for (rel_path, outcome) in committer.finish()? {
        match outcome {
            Ok(()) => stats.completed += 1,
            Err(refusal) => {
                stats.refuse(rel_path, refusal.code().to_owned(), refusal.sqlite_code());
            }
        }
    }
    resolve_sidecars(stats, sidecars, snapshot_bases);
    let keep = salvage_to_keep(target, stats, salvage_staged);
    target.retire_salvaged(&keep)?;
    stats.temporaries_removed = target.swept().removed;
    stats.temporaries_left.clone_from(&target.swept().left);
    stats.directories_renamed = target.created().renamed;
    stats
        .directories_fallback
        .clone_from(&target.created().fallback);
    if stats.refusals.is_empty() {
        target.finish_directories(store)?;
    }
    target.flush_session()
}

/// A sidecar the source reported (#218): its relative path and its base's.
type SidecarReport = (Vec<u8>, Vec<u8>);

/// Bind each `SQLite` sidecar the source reported to its base's outcome
/// (#218, review R2), once every outcome of the session is known: covered
/// when the base is held here as a snapshot (published from one in this
/// session and not refused since, or reused or adopted as one); refused by
/// name, `SQLITE_STATE_CHANGED` as v5 refuses every sidecar, otherwise. So
/// a sidecar is never counted covered by its name alone: the `-wal` of a
/// database without the `SQLite` magic (an encrypted store) carried raw, or
/// a user's `notes-journal` beside a plain `notes`, stays a visible refusal.
fn resolve_sidecars(
    stats: &mut TransferStats,
    sidecars: &[SidecarReport],
    snapshot_bases: &HashSet<Vec<u8>>,
) {
    let refused: HashSet<Vec<u8>> = stats
        .refusals
        .iter()
        .map(|(rel_path, _)| rel_path.clone())
        .collect();
    // A snapshot whose group commit failed is not held here.
    stats
        .sqlite_snapshots
        .retain(|rel_path| !refused.contains(rel_path));
    if sidecars.is_empty() {
        return;
    }
    let mut covered = Vec::new();
    let mut uncovered = Vec::new();
    for (rel_path, database) in sidecars {
        if snapshot_bases.contains(database) && !refused.contains(database) {
            covered.push(rel_path.clone());
        } else {
            uncovered.push(rel_path.clone());
        }
    }
    counters::add(Counter::SqliteSidecarsCovered, covered.len() as u64);
    stats.sqlite_sidecars_covered.extend(covered);
    let code = BulkloadRefusal::SqliteStateChanged.code();
    stats.refusals.extend(
        uncovered
            .into_iter()
            .map(|rel_path| (rel_path, code.to_owned())),
    );
}

/// Which salvaged temporaries outlive the session (#124, OI-1002-Q33).
///
/// Salvage saves wire bytes only: a capture is recorded only after its
/// output's group commit returned (`Held`), so no recorded capture's bytes
/// live only in a temporary (R25 strict for what the destination durably
/// held, OI-1001-Q15). A temporary is kept only when an entry refused in
/// this session had staged chunks from it (a byte-touching refusal: the
/// entry got as far as filling its staged file, then failed its
/// verification, publication or group commit), so the retry can fill from
/// it again. An entry refused before it staged anything (a path conflict at
/// its decision, a space preflight, a source-side refusal before content)
/// keeps nothing: those temporaries are removed, so a path refused on every
/// run never keeps them (#97).
///
/// What is kept is bounded by [`SALVAGE_KEEP_FILES`] and
/// [`SALVAGE_KEEP_BYTES`], in salvage order. A temporary past the bound is
/// removed and refused as a value, `SALVAGE_BOUND_EXCEEDED` under its
/// current name: its chunks are sent again by the next run.
fn salvage_to_keep(
    target: &Destination,
    stats: &mut TransferStats,
    salvage_staged: &HashMap<Vec<u8>, Vec<usize>>,
) -> Vec<usize> {
    let mut wanted: Vec<usize> = stats
        .refusals
        .iter()
        .filter_map(|(rel_path, _)| salvage_staged.get(rel_path))
        .flatten()
        .copied()
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let (max_files, max_bytes) = salvage_bound(target.path());
    // A temporary that is gone has nothing left to keep.
    let sized = wanted
        .into_iter()
        .filter_map(|at| target.salvaged_size(at).map(|size| (at, size)));
    let (keep, over) = bound_salvage(sized, max_files, max_bytes);
    for at in over {
        if let Some(rel_path) = target.salvaged_path(at) {
            stats.refusals.push((
                rel_path.to_vec(),
                BulkloadRefusal::SalvageBoundExceeded.code().to_owned(),
            ));
        }
    }
    keep
}

/// Split `(index, size)` salvage candidates, in order, into those kept
/// within `max_files` and `max_bytes` and those over the bound. Greedy in
/// order: a candidate is kept if it fits beside every one kept before it.
fn bound_salvage(
    sized: impl IntoIterator<Item = (usize, u64)>,
    max_files: usize,
    max_bytes: u64,
) -> (Vec<usize>, Vec<usize>) {
    let mut keep = Vec::new();
    let mut over = Vec::new();
    let mut kept_bytes = 0_u64;
    for (at, size) in sized {
        let bytes = kept_bytes.saturating_add(size);
        if keep.len() < max_files && bytes <= max_bytes {
            keep.push(at);
            kept_bytes = bytes;
        } else {
            over.push(at);
        }
    }
    (keep, over)
}

/// The destination's committer, sized to the descriptor budget: a quarter
/// for session reuse, a quarter for staged files queued or grouped for
/// commit, which hold one descriptor each, and an eighth for the outputs
/// its superseding publishes displace ([`Displaced`]).
fn publication_committer(
    state: &Path,
    budget: u64,
    notify: Sender<crate::materialize::GroupOutcomes>,
    displaced: SharedDisplaced,
) -> Result<Committer<PublishSink>> {
    let staged = (budget / 8).clamp(2, crate::io::durable::GROUP_FILES);
    Committer::spawn_with(
        PublishSink::new(Store::open(state)?.into_publisher(PublisherSide::Destination)?)?
            .with_notify(notify)
            .with_displaced(displaced),
        crate::io::durable::Limits {
            group_files: staged,
            queue_depth: usize::try_from(staged).unwrap_or(1),
            ..crate::io::durable::Limits::default()
        },
    )
}

/// What the destination knows of an entry before its content arrives.
struct Seat {
    /// The row its content is checked against: the walked row, or for a
    /// `SQLite` snapshot that row with the snapshot's size (#218).
    row: RowSchema,
    /// The key its output row is recorded under: its row key, or its
    /// snapshot key ([`sqlite_key`], #218).
    key: Vec<u8>,
    /// Its capture record's key (#169): [`unrowed::record_key`], or
    /// [`unrowed::sqlite_record_key`] for a snapshot (#218, R1).
    record: [u8; 32],
    /// Its content is a `SQLite` snapshot ([`Control::SqliteSnapshot`]).
    snapshot: bool,
    /// A refusal decided before any content arrived: a snapshot that does
    /// not fit the destination's free-space floor or the chunk bound (R7).
    failure: Option<BulkloadRefusal>,
}

impl Seat {
    fn file(row: RowSchema, key: Vec<u8>) -> Result<Self> {
        Ok(Self {
            record: unrowed::record_key(&row)?,
            row,
            key,
            snapshot: false,
            failure: None,
        })
    }
}

/// A streamed entry: chunks arrive in order and are written at once.
struct Streaming {
    row: RowSchema,
    key: Vec<u8>,
    record: [u8; 32],
    snapshot: bool,
    staged: Option<StagedFile>,
    specs: Vec<ChunkSpec>,
    offset: u64,
    hints: Vec<ChunkHint>,
    seen: HashSet<[u8; 32]>,
    failure: Option<BulkloadRefusal>,
    /// Streamed in answer to `WantManifest` (the source could not keep the
    /// chunks for a manifest): an existing output at the path is adopted
    /// against the streamed chunks instead of refused.
    adopt: bool,
}

impl Streaming {
    /// One streamed chunk: it must be the next index at the next offset.
    /// It is written once verified; a content fault becomes the entry's
    /// refusal and the stream stays in step.
    fn accept(
        &mut self,
        target: &Destination,
        open: &mut usize,
        header: &DataHeader,
        payload: &[u8],
        verified: bool,
    ) -> Result<()> {
        if header.index as usize != self.specs.len() || header.offset != self.offset {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        // More chunks than any manifest may hold ends the session: nothing
        // grows past the bound (#77 review F4).
        if self.specs.len() >= MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let size = payload.len() as u64;
        let end = self.offset.saturating_add(size);
        if self.failure.is_none() && (!verified || end > self.row.size) {
            self.failure = Some(BulkloadRefusal::DigestMismatch);
        }
        if self.failure.is_none() && self.staged.is_none() {
            if *open >= MAX_OPEN_ENTRIES {
                return Err(BulkloadRefusal::ProtocolStateViolation);
            }
            match target.stage(&self.row) {
                Ok(staged) => {
                    *open += 1;
                    self.staged = Some(staged);
                }
                Err(refusal) => self.failure = Some(refusal),
            }
        }
        if self.failure.is_none() {
            if let Some(staged) = &self.staged {
                if let Err(refusal) = place(staged.file(), payload, &[self.offset]) {
                    self.failure = Some(refusal);
                }
            }
        }
        self.specs.push(ChunkSpec {
            digest: header.digest,
            size,
        });
        if self.seen.insert(header.digest) {
            self.hints.push(ChunkHint {
                digest: header.digest,
                offset: self.offset,
                size,
            });
        }
        self.offset = end;
        Ok(())
    }

    fn new(seat: Seat, adopt: bool) -> Self {
        Self {
            row: seat.row,
            key: seat.key,
            record: seat.record,
            snapshot: seat.snapshot,
            staged: None,
            specs: Vec::new(),
            offset: 0,
            hints: Vec::new(),
            seen: HashSet::new(),
            failure: seat.failure,
            adopt,
        }
    }
}

/// An entry filled from its manifest: local chunks first, then the
/// requested ones.
struct Filling {
    row: RowSchema,
    key: Vec<u8>,
    record: [u8; 32],
    snapshot: bool,
    manifest: Manifest,
    offsets: Vec<u64>,
    plan: Plan,
    expected: VecDeque<u32>,
    failure: Option<BulkloadRefusal>,
}

/// An entry the destination expects content for.
enum Incoming {
    Streaming(Streaming),
    AwaitManifest(Seat),
    Filling(Filling),
}

/// The destination's side of one session.
struct Inbound<'a, W> {
    output: &'a mut W,
    target: &'a mut Destination,
    store: &'a Store,
    authority: Vec<u8>,
    committer: &'a Committer<PublishSink>,
    session: SessionChunks,
    /// The outputs this session's superseding publishes displace (WP0(d)).
    displaced: SharedDisplaced,
    stats: TransferStats,
    incoming: HashMap<u64, Incoming>,
    open: usize,
    fill_locally: bool,
    salvage: Salvage,
    /// Salvaged temporaries each entry staged chunks from, by relative path
    /// (#124): kept past the session only if that entry is refused.
    salvage_staged: HashMap<Vec<u8>, Vec<usize>>,
    /// Entries queued for their group commit, by relative path, awaiting
    /// `Held` (#77 round 2, N1), and the committer's per-group outcomes.
    pending_held: HashMap<Vec<u8>, u64>,
    committed: Receiver<crate::materialize::GroupOutcomes>,
    /// The source has offered every entry.
    walk_done: bool,
    /// Granted payload bytes not yet received.
    granted: u64,
    /// Received payload bytes not yet returned as credit.
    consumed: u64,
    /// OI-1001-Q2 space preflight: payload bytes of entries decided `Send` or
    /// `WantManifest` whose `Held` is not yet sent, by entry, and their sum.
    reserved: HashMap<u64, u64>,
    reserved_bytes: u64,
    /// The destination filesystem as last probed; `None` after a group
    /// commit, so the next admission sees the committed bytes.
    space: Option<crate::space::Space>,
    /// Bytes admitted since the last probe.
    since_probe: u64,
    /// The session's `SQLite` mode, which this side chose (#218).
    sqlite: SqliteMode,
    /// The source's `SqliteSidecar` reports, as (sidecar, base), resolved
    /// against their bases' outcomes when the session ends (R2).
    sidecars: Vec<SidecarReport>,
    /// Bases published from a snapshot, or reused or adopted as one.
    snapshot_bases: HashSet<Vec<u8>>,
}

/// Re-probe the destination filesystem after admitting this many bytes,
/// even without a group commit in between.
const SPACE_REPROBE_BYTES: u64 = 256 * 1024 * 1024;

/// Receive an ordinary-file carry, in the process's [`sqlite_mode`].
///
/// A divergent output is replaced only when it is this store's own,
/// untouched since its row was written (WP0(d)); any other is refused and
/// kept.
///
/// # Errors
/// Refuses protocol errors and unsafe roots. Per-path conflicts remain in stats.
pub fn receive<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    source: &Path,
    source_state: &Path,
    destination: &Path,
    destination_state: &Path,
) -> Result<TransferStats> {
    receive_with(
        input,
        output,
        source,
        source_state,
        destination,
        destination_state,
        sqlite_mode(),
    )
}

/// [`receive`] in the given [`SqliteMode`], which the session's `Open`
/// carries to the source (#218).
///
/// # Errors
/// As [`receive`].
pub fn receive_with<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    source: &Path,
    source_state: &Path,
    destination: &Path,
    destination_state: &Path,
    sqlite: SqliteMode,
) -> Result<TransferStats> {
    let setup_timer = PhaseTimer(&RECV_SETUP_NS, Instant::now());
    let store = Store::open(destination_state)?;
    let mut target = Destination::open(destination, &store)?;
    if store.root().starts_with(target.path()) || target.path().starts_with(store.root()) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let budget = crate::io::limits::descriptor_budget();
    let (notify, group_outcomes) = std::sync::mpsc::channel();
    let displaced = Displaced::shared(usize::try_from((budget / 8).clamp(1, 64)).unwrap_or(1));
    let committer =
        publication_committer(destination_state, budget, notify, Arc::clone(&displaced))?;
    // The committer holds the exclusive publisher, so no temporary of this
    // store is in flight while the root is swept.
    target.sweep_root(&store)?;
    let fill_locally = store.has_output_hints()?;
    write_control(
        output,
        &Control::Open {
            proto: PROTO_VERSION,
            wire_id: wire_id(),
            root: source.as_os_str().as_bytes().to_vec(),
            state: source_state.as_os_str().as_bytes().to_vec(),
            sqlite,
        },
    )?;
    let Frame::Control(Control::Start { authority }) = read_frame(input)? else {
        return Err(BulkloadRefusal::ProtocolStateViolation);
    };
    let target_meta = std::fs::metadata(target.path()).refuse_at("transfer::receive")?;
    let output_authority = postcard::to_stdvec(&(
        &authority,
        target.path().as_os_str().as_bytes(),
        target_meta.dev(),
        target_meta.ino(),
    ))
    .refuse_at("transfer::receive")?;
    write_control(
        output,
        &Control::Credit {
            bytes: CREDIT_WINDOW,
        },
    )?;
    let mut receiver = Inbound {
        output,
        target: &mut target,
        store: &store,
        authority: output_authority,
        committer: &committer,
        session: SessionChunks::with_capacity((budget / 4).clamp(1, SESSION_FILES)),
        displaced,
        stats: TransferStats {
            sqlite_mode: sqlite,
            ..TransferStats::default()
        },
        incoming: HashMap::new(),
        open: 0,
        fill_locally,
        salvage: Salvage::default(),
        salvage_staged: HashMap::new(),
        pending_held: HashMap::new(),
        committed: group_outcomes,
        walk_done: false,
        granted: CREDIT_WINDOW,
        consumed: 0,
        reserved: HashMap::new(),
        reserved_bytes: 0,
        space: None,
        since_probe: 0,
        sqlite,
        sidecars: Vec::new(),
        snapshot_bases: HashSet::new(),
    };
    drop(setup_timer);
    let source_bytes_read = {
        let _stream_timer = PhaseTimer(&RECV_STREAM_NS, Instant::now());
        receiver.run(input)?
    };
    let _tail_timer = PhaseTimer(&RECV_TAIL_NS, Instant::now());
    let Inbound {
        mut stats,
        session,
        salvage_staged,
        sidecars,
        snapshot_bases,
        ..
    } = receiver;
    stats.source_bytes_read = source_bytes_read;
    drop(session);
    finish_receive(
        &mut target,
        &store,
        committer,
        &mut stats,
        &salvage_staged,
        &sidecars,
        &snapshot_bases,
    )?;
    Ok(stats)
}

impl<W: Write> Inbound<'_, W> {
    /// Handle the source's frames until `SourceDone`; returns the source
    /// bytes it reports. Entries are numbered in stream order, and the
    /// session ends only once every entry the destination expected content
    /// for has ended or been refused.
    fn run<R: Read>(&mut self, input: &mut R) -> Result<u64> {
        let mut offered = 0_u64;
        let mut walk_done = None;
        loop {
            // Before blocking on the next frame: answer what has committed,
            // and once the stream is drained, commit and answer the rest
            // (the source waits for every `Held` before it finishes).
            {
                let _settle_timer = PhaseTimer(&RECV_SETTLE_NS, Instant::now());
                self.settle_held()?;
            }
            let frame = {
                let _read_timer = PhaseTimer(&RECV_READ_NS, Instant::now());
                read_frame(input)?
            };
            match frame {
                Frame::Control(Control::Entry { entry, row, wal }) => {
                    if entry != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::ProtocolStateViolation);
                    }
                    offered += 1;
                    self.entry(entry, row, wal)?;
                }
                Frame::Control(Control::SqliteSidecar { rel_path, database })
                    if self.sqlite == SqliteMode::Snapshot =>
                {
                    self.sidecars.push((rel_path, database));
                }
                Frame::Control(Control::SqliteSnapshot { entry, size, wal })
                    if self.sqlite == SqliteMode::Snapshot =>
                {
                    self.sqlite_snapshot(entry, size, wal)?;
                }
                // Only `SQLITE_BACKUP_FAILED` carries `SQLite`'s code.
                Frame::Control(Control::Refused {
                    code, sqlite_code, ..
                }) if sqlite_code.is_some()
                    && code != BulkloadRefusal::SqliteBackupFailed(None).code() =>
                {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                Frame::Control(Control::Refused {
                    entry: None,
                    rel_path,
                    code,
                    sqlite_code,
                }) => self.stats.refuse(rel_path, code, sqlite_code),
                Frame::Control(Control::Refused {
                    entry: Some(entry),
                    rel_path,
                    code,
                    sqlite_code,
                }) => self.refused(entry, &rel_path, code, sqlite_code)?,
                Frame::Control(Control::EngineTemporary { rel_path }) => {
                    self.stats.source_engine_temporaries.push(rel_path);
                }
                Frame::Control(Control::WalkDone { entries }) => {
                    if entries != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::ProtocolStateViolation);
                    }
                    walk_done = Some(entries);
                    self.walk_done = true;
                    self.settle_held()?;
                }
                Frame::Control(Control::Manifest {
                    entry,
                    root,
                    chunks,
                }) => self.manifest(entry, root, chunks)?,
                Frame::Control(Control::End {
                    entry,
                    root,
                    chunks,
                    size,
                    racy,
                }) => self.end(entry, root, chunks, size, racy)?,
                Frame::Data { header, payload } => self.data(&header, &payload)?,
                Frame::Control(Control::SourceDone {
                    entries,
                    source_bytes_read,
                }) if walk_done == Some(entries) && self.incoming.is_empty() => {
                    return Ok(source_bytes_read);
                }
                _ => return Err(BulkloadRefusal::ProtocolStateViolation),
            }
        }
    }
}

impl<W: Write> Inbound<'_, W> {
    fn refuse(&mut self, rel_path: Vec<u8>, refusal: &BulkloadRefusal) {
        self.stats
            .refusals
            .push((rel_path, refusal.code().to_owned()));
    }

    /// Decide one offered entry.
    ///
    /// In `snapshot` mode (#218) a regular entry is looked up under its
    /// snapshot key ([`sqlite_key`], with the `-wal` identity it was offered
    /// with) as well as its row key: an unchanged store is `Reuse`d with
    /// nothing opened on the source. A file that is not a database keeps its
    /// row key, so no existing row changes key (R2). Before its content is
    /// asked for, a path with a `-wal`, `-journal` or `-shm` beside it here
    /// is refused `DESTINATION_OCCUPIED` (R5): a snapshot published there
    /// could be rolled back or overlaid by it. The check reads only this
    /// side, so it is not remembered; removing the sidecar converges.
    fn entry(&mut self, entry: u64, row: RowSchema, wal: Option<SidecarId>) -> Result<()> {
        let census_started = Instant::now();
        let snapshot_mode = self.sqlite == SqliteMode::Snapshot;
        if wal.is_some() && (!snapshot_mode || row.kind != FileKind::Regular) {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        let key = row_key(&self.authority, &row)?;
        let snapshot = if snapshot_mode && row.kind == FileKind::Regular {
            Some((
                sqlite_key(&self.authority, &row, wal.as_ref())?,
                unrowed::sqlite_record_key(&row, wal.as_ref())?,
            ))
        } else {
            None
        };
        let decided = match row.kind {
            FileKind::Directory => self
                .target
                .directory(&row, self.store, &self.authority)
                .map(|()| Decision::Skip),
            FileKind::Symlink => self.target.symlink(&row).map(|()| Decision::Skip),
            FileKind::Regular => self.target.identity(&row).and_then(|identity| {
                if let Some(identity) = &identity {
                    if let Some((snapshot_key, _)) = &snapshot {
                        if self.store.output_matches(snapshot_key, identity)? {
                            self.stats.reused += 1;
                            self.snapshot_applied(&row.rel_path);
                            return Ok(Decision::Reuse);
                        }
                    }
                    if self.store.output_matches(&key, identity)? {
                        self.stats.reused += 1;
                        return Ok(Decision::Reuse);
                    }
                    // Refused before for what the path holds, and neither
                    // the seat nor that file has moved since: the same
                    // answer, and the source reads nothing for it (R25).
                    // Asked ahead of the capture record: a file that did
                    // not verify against this seat cannot prove it either,
                    // and proving hashes the whole output.
                    for remembered in std::iter::once(&key).chain(snapshot.as_ref().map(|(k, _)| k))
                    {
                        if let Some(refused) =
                            self.remembered_refusal(&row, remembered, identity)?
                        {
                            return Err(refused.refusal());
                        }
                    }
                    if self.adopt_unrowed(&row, &key, snapshot.as_ref())? {
                        return Ok(Decision::Reuse);
                    }
                }
                if snapshot_mode && self.target.sqlite_sidecars_present(&row.rel_path)? {
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                // A manifest first only when something here could fill it:
                // an existing output to adopt, or chunks held by published
                // outputs. Otherwise the source reads the file exactly once.
                Ok(
                    if identity.is_some() || self.fill_locally || self.target.salvaged() > 0 {
                        Decision::WantManifest
                    } else {
                        Decision::Send
                    },
                )
            }),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        };
        REUSE_CENSUS_NS.fetch_add(elapsed_ns(census_started), Ordering::Relaxed);
        // OI-1001-Q2: an entry whose bytes would take the destination under
        // its free-space floor is refused as a value, before any of its
        // content is requested. The session continues and stays resumable;
        // nothing durable is read again (R25). A database's snapshot can be
        // as large as its main file and its `-wal` together (#218, R7); its
        // exact size is admitted again when the source announces it.
        let bound = row
            .size
            .saturating_add(wal.as_ref().map_or(0, |wal| wal.size));
        let decided = decided.and_then(|decision| match decision {
            Decision::Send | Decision::WantManifest => self.admit(entry, bound).map(|()| decision),
            other => Ok(other),
        });
        let decision = match decided {
            Ok(decision) => decision,
            Err(refusal) => {
                self.refuse(row.rel_path.clone(), &refusal);
                Decision::Refuse {
                    code: refusal.code().to_owned(),
                }
            }
        };
        match decision {
            Decision::Send => {
                self.incoming.insert(
                    entry,
                    Incoming::Streaming(Streaming::new(Seat::file(row, key)?, false)),
                );
            }
            Decision::WantManifest => {
                self.incoming
                    .insert(entry, Incoming::AwaitManifest(Seat::file(row, key)?));
            }
            Decision::Skip | Decision::Reuse | Decision::Refuse { .. } => (),
        }
        write_control(self.output, &Control::Decide { entry, decision })?;
        fault_point!(ReceiveAfterDecide);
        Ok(())
    }

    /// A database at `rel_path` is held here as a snapshot: published from
    /// one, or reused or adopted as one (#218). Its sidecars are covered.
    fn snapshot_applied(&mut self, rel_path: &[u8]) {
        if self.snapshot_bases.insert(rel_path.to_vec()) {
            self.stats.sqlite_snapshots.push(rel_path.to_vec());
        }
    }

    /// The source announces an entry's content as a `SQLite` snapshot of
    /// `size` bytes, its `-wal` at `wal` after the backup (#218): the entry
    /// is keyed and checked as a snapshot from here on. Only before any of
    /// its content. Its exact size is admitted against the free-space floor
    /// again (R7); one that does not fit, or is past the chunk bound, is
    /// refused when it ends, its content discarded as it arrives.
    fn sqlite_snapshot(&mut self, entry: u64, size: u64, wal: Option<SidecarId>) -> Result<()> {
        let (mut seat, streamed) = match self.incoming.remove(&entry) {
            Some(Incoming::AwaitManifest(seat)) if !seat.snapshot => (seat, false),
            Some(Incoming::Streaming(streaming))
                if !streaming.snapshot
                    && streaming.specs.is_empty()
                    && streaming.staged.is_none()
                    && !streaming.adopt =>
            {
                let seat = Seat {
                    row: streaming.row,
                    key: streaming.key,
                    record: streaming.record,
                    snapshot: false,
                    failure: streaming.failure,
                };
                (seat, true)
            }
            _ => return Err(BulkloadRefusal::ProtocolStateViolation),
        };
        seat.key = sqlite_key(&self.authority, &seat.row, wal.as_ref())?;
        seat.record = unrowed::sqlite_record_key(&seat.row, wal.as_ref())?;
        seat.snapshot = true;
        seat.row.size = size;
        if seat.failure.is_none() {
            if size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
                seat.failure = Some(BulkloadRefusal::BudgetExceeded);
            } else {
                self.release(entry);
                if let Err(refusal) = self.admit(entry, size) {
                    seat.failure = Some(refusal);
                }
            }
        }
        self.incoming.insert(
            entry,
            if streamed {
                Incoming::Streaming(Streaming::new(seat, false))
            } else {
                Incoming::AwaitManifest(seat)
            },
        );
        Ok(())
    }

    /// R25's strict reading (#169): an existing output with no matching row
    /// whose capture record proves it is this entry's capture is queued for
    /// its row as an adopted publication, and the entry is answered `Reuse`,
    /// so the source reads nothing. Its commit outcome reaches the session's
    /// report like any output's. See [`unrowed`]. In `snapshot` mode the
    /// record may instead name the settled snapshot `snapshot` keys (#218,
    /// R1): the output is then adopted under the snapshot key.
    fn adopt_unrowed(
        &mut self,
        row: &RowSchema,
        key: &[u8],
        snapshot: Option<&(Vec<u8>, [u8; 32])>,
    ) -> Result<bool> {
        let record_key = unrowed::record_key(row)?;
        let verdict = match self.target.existing(row) {
            Ok(Some((file, parent))) => {
                match unrowed::prove(&file, row, &record_key, snapshot.map(|(_, record)| record)) {
                    unrowed::Verdict::Proven(identity, hints, is_snapshot) => {
                        let key = match (is_snapshot, snapshot) {
                            (true, Some((snapshot_key, _))) => snapshot_key.clone(),
                            _ => key.to_vec(),
                        };
                        self.committer.submit(Publication::Adopted {
                            record: OutputRecord {
                                key,
                                rel_path: row.rel_path.clone(),
                                identity,
                                racy: false,
                                hints,
                            },
                            file,
                            parent,
                        })?;
                        if is_snapshot {
                            self.snapshot_applied(&row.rel_path);
                        }
                        self.stats.unrowed_adopted += 1;
                        counters::bump(Counter::TransferUnrowedAdopted);
                        return Ok(true);
                    }
                    verdict => verdict,
                }
            }
            Ok(None) => unrowed::Verdict::Other,
            Err(_) => unrowed::Verdict::Unproven,
        };
        if matches!(verdict, unrowed::Verdict::Unproven) {
            self.stats.unrowed_unproven += 1;
            counters::bump(Counter::TransferUnrowedUnproven);
        }
        Ok(false)
    }

    /// The destination's remembered refusal of this entry ([`RefusedOutput`]),
    /// when the file at its path still has the identity it was refused
    /// with. A refusal for a missing exchange stands only while the file
    /// system still has none (probed once a session).
    fn remembered_refusal(
        &self,
        row: &RowSchema,
        key: &[u8],
        identity: &StatIdentity,
    ) -> Result<Option<RefusedOutput>> {
        let Some(refused) = self.store.refused_output(key, identity)? else {
            return Ok(None);
        };
        if refused == RefusedOutput::ExchangeUnsupported && self.target.exchange_supported(row)? {
            return Ok(None);
        }
        counters::bump(Counter::TransferRefusedOutputsRemembered);
        Ok(Some(refused))
    }

    /// Remember that this entry was refused for what its path holds, so an
    /// unchanged rerun refuses it when it is offered ([`RefusedOutput`]).
    /// Only for a capture that was not racy and a file that was `settled`
    /// when it was read ([`verify_settled`]). Best effort: a record that
    /// cannot be written costs one more source read, never the session.
    fn remember_refusal(
        &self,
        key: &[u8],
        settled: Option<StatIdentity>,
        racy: bool,
        refused: RefusedOutput,
    ) {
        if let (false, Some(identity)) = (racy, settled) {
            let _ = self.store.remember_refused_output(key, &identity, refused);
        }
    }

    /// Reserve an entry's bytes against the destination's free-space floor
    /// (OI-1001-Q2). The cached probe already counts every committed byte;
    /// the reservation counts every byte decided and not yet `Held`, so a
    /// partly staged entry is counted twice: conservative, never short.
    fn admit(&mut self, entry: u64, size: u64) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let space = match self.space {
            Some(space) if self.since_probe < SPACE_REPROBE_BYTES => space,
            _ => {
                let space = crate::space::probe(self.target.path())?;
                self.space = Some(space);
                self.since_probe = 0;
                space
            }
        };
        crate::space::check(
            self.reserved_bytes.saturating_add(size),
            space,
            crate::space::min_free_percent(),
        )?;
        self.reserved.insert(entry, size);
        self.reserved_bytes = self.reserved_bytes.saturating_add(size);
        self.since_probe = self.since_probe.saturating_add(size);
        Ok(())
    }

    /// An entry left the decided-not-Held set: its bytes are durable, or it
    /// will write none.
    fn release(&mut self, entry: u64) {
        if let Some(size) = self.reserved.remove(&entry) {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(size);
        }
    }

    /// The source refused an entry's capture after any of its data.
    fn refused(
        &mut self,
        entry: u64,
        rel_path: &[u8],
        code: String,
        sqlite_code: Option<i32>,
    ) -> Result<()> {
        self.release(entry);
        let incoming = self
            .incoming
            .remove(&entry)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        let row = match incoming {
            Incoming::Streaming(Streaming { row, staged, .. }) => {
                if let Some(staged) = staged {
                    self.open -= 1;
                    let _ = staged.discard();
                }
                row
            }
            Incoming::AwaitManifest(seat) => seat.row,
            Incoming::Filling(Filling { row, plan, .. }) => {
                if let Plan::Write(staging) = plan {
                    self.open -= 1;
                    let _ = staging.staged.discard();
                }
                row
            }
        };
        if row.rel_path != rel_path {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        self.stats.refuse(row.rel_path, code, sqlite_code);
        Ok(())
    }

    /// Plan a manifest's entry and request only what cannot be filled here.
    fn manifest(&mut self, entry: u64, root: [u8; 32], chunks: Vec<ChunkSpec>) -> Result<()> {
        if chunks.len() > MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let Some(Incoming::AwaitManifest(Seat {
            row,
            key,
            record,
            snapshot,
            failure,
        })) = self.incoming.remove(&entry)
        else {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        };
        let manifest = Manifest { root, chunks };
        let mut offsets = Vec::with_capacity(manifest.chunks.len());
        let mut offset = 0_u64;
        for chunk in &manifest.chunks {
            offsets.push(offset);
            offset = offset.saturating_add(chunk.size);
        }
        let materialize_started = Instant::now();
        self.salvage.refresh(self.target);
        let mut salvaged_from = Vec::new();
        let plan = if let Some(refusal) = failure {
            Plan::Refuse(refusal)
        } else if manifest.is_consistent() {
            if self.open >= MAX_OPEN_ENTRIES {
                return Err(BulkloadRefusal::ProtocolStateViolation);
            }
            plan_file(
                &ReceiveContext {
                    target: self.target,
                    store: self.store,
                    authority: &self.authority,
                    displaced: &self.displaced,
                    session: &self.session,
                    salvage: &self.salvage,
                },
                &row,
                &manifest,
                &mut salvaged_from,
            )
        } else {
            Plan::Refuse(BulkloadRefusal::DigestMismatch)
        };
        if !salvaged_from.is_empty() {
            salvaged_from.sort_unstable();
            salvaged_from.dedup();
            self.salvage_staged
                .insert(row.rel_path.clone(), salvaged_from);
        }
        MATERIALIZE_NS.fetch_add(elapsed_ns(materialize_started), Ordering::Relaxed);
        let indices = match &plan {
            Plan::Write(staging) => {
                self.open += 1;
                staging.missing.clone()
            }
            Plan::Refuse(_) | Plan::Adopt(..) | Plan::NoExchange(_) => Vec::new(),
        };
        write_control(
            self.output,
            &Control::NeedChunks {
                entry,
                indices: indices.clone(),
            },
        )?;
        self.incoming.insert(
            entry,
            Incoming::Filling(Filling {
                row,
                key,
                record,
                snapshot,
                manifest,
                offsets,
                plan,
                expected: indices.into(),
                failure: None,
            }),
        );
        Ok(())
    }

    /// One data frame: account its credit, verify it against its digest (the
    /// one integrity check on received bytes) and write it where it belongs.
    /// Content faults become the entry's refusal and the stream stays in
    /// step; protocol faults end the session.
    fn data(&mut self, header: &DataHeader, payload: &[u8]) -> Result<()> {
        let _transfer_timer = PhaseTimer(&TRANSFER_NS, Instant::now());
        let size = payload.len() as u64;
        self.granted = self
            .granted
            .checked_sub(size)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        self.stats.bytes_received = self.stats.bytes_received.saturating_add(size);
        let verified = {
            let _verify_timer = PhaseTimer(&RECV_VERIFY_NS, Instant::now());
            payload.len() <= crate::hash::CDC_MAX_BYTES as usize
                && counters::hash(Counter::HashWireVerify, payload) == header.digest
        };
        // A source that could not keep a manifest's chunks streams the entry
        // in place of the manifest (#77 review F1).
        if let Some(Incoming::AwaitManifest(_)) = self.incoming.get(&header.entry) {
            if let Some(Incoming::AwaitManifest(seat)) = self.incoming.remove(&header.entry) {
                self.incoming.insert(
                    header.entry,
                    Incoming::Streaming(Streaming::new(seat, true)),
                );
            }
        }
        match self.incoming.get_mut(&header.entry) {
            Some(Incoming::Streaming(streaming)) => {
                streaming.accept(self.target, &mut self.open, header, payload, verified)?;
            }
            Some(Incoming::Filling(filling)) => {
                if filling.expected.pop_front() != Some(header.index) {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                let at = header.index as usize;
                let spec = filling
                    .manifest
                    .chunks
                    .get(at)
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
                if spec.digest != header.digest
                    || spec.size != size
                    || filling.offsets.get(at) != Some(&header.offset)
                {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                if filling.failure.is_none() {
                    if verified {
                        if let Plan::Write(staging) = &filling.plan {
                            if let Some((_, offsets)) = staging.placements.get(&header.digest) {
                                if let Err(refusal) = place(staging.staged.file(), payload, offsets)
                                {
                                    filling.failure = Some(refusal);
                                }
                            }
                        }
                    } else {
                        filling.failure = Some(BulkloadRefusal::DigestMismatch);
                    }
                }
            }
            _ => return Err(BulkloadRefusal::ProtocolStateViolation),
        }
        self.consumed = self.consumed.saturating_add(size);
        if self.consumed >= CREDIT_RETURN {
            let bytes = std::mem::take(&mut self.consumed);
            self.granted = self.granted.saturating_add(bytes);
            write_control(self.output, &Control::Credit { bytes })?;
        }
        Ok(())
    }

    /// An entry's data is complete: check coverage and the manifest root,
    /// then queue the output for its group commit, or record its refusal.
    /// A `racy` capture is published, but its output row is never kept as a
    /// reuse key (#86).
    fn end(
        &mut self,
        entry: u64,
        root: [u8; 32],
        chunks: u32,
        size: u64,
        racy: bool,
    ) -> Result<()> {
        let incoming = self
            .incoming
            .remove(&entry)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        let (rel_path, snapshot, outcome) = match incoming {
            Incoming::AwaitManifest(_) => return Err(BulkloadRefusal::ProtocolStateViolation),
            Incoming::Streaming(streaming) => {
                if chunks as usize != streaming.specs.len() || size != streaming.offset {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                let rel_path = streaming.row.rel_path.clone();
                let snapshot = streaming.snapshot;
                (
                    rel_path,
                    snapshot,
                    self.end_streaming(streaming, root, racy),
                )
            }
            Incoming::Filling(filling) => {
                if !filling.expected.is_empty()
                    || chunks as usize != filling.manifest.chunks.len()
                    || root != filling.manifest.root
                    || filling.manifest.size() != Some(size)
                {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                let rel_path = filling.row.rel_path.clone();
                let snapshot = filling.snapshot;
                (rel_path, snapshot, self.end_filling(filling, racy))
            }
        };
        if snapshot && outcome.is_ok() {
            self.snapshot_applied(&rel_path);
        }
        // Every End is answered. `held` is sent once the output's group
        // has committed: its file and directory are sealed and the store
        // commit has drained them, so the bytes are durable here under the
        // final name (#77 round 2, N1). Only then may the source record the
        // capture, which therefore never costs a source read again.
        match outcome {
            Ok(()) => {
                self.pending_held.insert(rel_path, entry);
            }
            Err(refusal) => {
                self.refuse(rel_path, &refusal);
                self.release(entry);
                write_control(self.output, &Control::Held { entry, held: false })?;
            }
        }
        self.settle_held()?;
        fault_point!(ReceiveAfterEnd);
        Ok(())
    }

    /// Send `Held` for every entry whose group commit has returned.
    fn answer_held(&mut self) -> Result<()> {
        while let Ok(group) = self.committed.try_recv() {
            // A group committed: the next admission re-probes (OI-1001-Q2).
            self.space = None;
            for (rel_path, held) in group {
                let Some(entry) = self.pending_held.remove(&rel_path) else {
                    continue;
                };
                self.release(entry);
                write_control(self.output, &Control::Held { entry, held })?;
            }
        }
        Ok(())
    }

    /// Once nothing more can arrive before the source waits for its `Held`
    /// answers (every entry offered, none with content outstanding), commit
    /// the open group now and answer every entry.
    fn settle_held(&mut self) -> Result<()> {
        if self.pending_held.is_empty() || !self.walk_done || !self.incoming.is_empty() {
            return self.answer_held();
        }
        self.committer.sync()?;
        self.answer_held()?;
        if self.pending_held.is_empty() {
            Ok(())
        } else {
            Err(BulkloadRefusal::ProtocolStateViolation)
        }
    }

    fn end_streaming(&mut self, streaming: Streaming, root: [u8; 32], racy: bool) -> Result<()> {
        let Streaming {
            row,
            key,
            record,
            snapshot,
            staged,
            specs,
            offset,
            hints,
            failure,
            adopt,
            ..
        } = streaming;
        if staged.is_some() {
            self.open -= 1;
        }
        let checked = failure.map_or(Ok(()), Err).and_then(|()| {
            if offset != row.size || manifest_root(&specs) != root {
                Err(BulkloadRefusal::DigestMismatch)
            } else {
                Ok(())
            }
        });
        let mut supersede = None;
        if checked.is_ok() && adopt {
            // Streamed in place of a manifest: an existing output at the
            // path is adopted against the streamed chunks, as a manifest
            // would have been checked, and the staged copy is dropped. One
            // that holds other bytes is superseded by the staged copy when
            // it is this store's own (WP0(d), #187), and refused otherwise;
            // a snapshot's too (#218, OI-1003-Q146), its sidecars checked
            // once more just before the exchange.
            let existing = self.target.existing(&row).and_then(|existing| {
                let Some((file, parent)) = existing else {
                    return Ok(None);
                };
                let manifest = Manifest {
                    root,
                    chunks: specs,
                };
                let (verified, settled) =
                    verify_settled(self.target.path(), &file, &row, &manifest);
                match verified {
                    Ok(identity) => Ok(Some((file, parent, identity))),
                    Err(BulkloadRefusal::DestinationOccupied) => {
                        supersede = owned_output(self.store, &self.authority, &row, &file)?;
                        if supersede.is_none() {
                            self.remember_refusal(&key, settled, racy, RefusedOutput::Occupied);
                            Err(BulkloadRefusal::DestinationOccupied)
                        } else if self.target.exchange_supported(&row)? {
                            Ok(None)
                        } else {
                            self.remember_refusal(
                                &key,
                                settled,
                                racy,
                                RefusedOutput::ExchangeUnsupported,
                            );
                            Err(BulkloadRefusal::DestinationExchangeUnsupported)
                        }
                    }
                    Err(refusal) => Err(refusal),
                }
            });
            match existing {
                Ok(Some((file, parent, identity))) => {
                    if let Some(staged) = staged {
                        let _ = staged.discard();
                    }
                    return self.adopt(file, parent, &row, (key, record, racy, root), identity);
                }
                Ok(None) => (),
                Err(refusal) => {
                    if let Some(staged) = staged {
                        let _ = staged.discard();
                    }
                    return Err(refusal);
                }
            }
        }
        let mut staged = match (checked, staged) {
            (Ok(()), Some(staged)) => staged,
            // An empty file has no data frame; it is staged here.
            (Ok(()), None) => self.target.stage(&row)?,
            (Err(refusal), staged) => {
                if let Some(staged) = staged {
                    let _ = staged.discard();
                }
                return Err(refusal);
            }
        };
        fault_point!(ReceiveAfterChunks);
        if snapshot {
            if let Err(refusal) = self.verify_snapshot(&staged, &row) {
                let _ = staged.discard();
                return Err(refusal);
            }
            staged.mark_sqlite();
        }
        self.publish(staged, &row, (key, record, racy, root), hints, supersede)
    }

    /// A received snapshot's last checks before it is published (#218):
    /// still no `-wal`, `-journal` or `-shm` beside its path (R5's check
    /// again, as late as it can be made: one appearing after it is a stated
    /// residual), and the staged file is a sound database in journal mode
    /// DELETE whose `integrity_check` says `ok` (design section 9,
    /// `provider_sqlite::verify_received`). A failure refuses the entry; the
    /// caller discards the staged file.
    fn verify_snapshot(&self, staged: &StagedFile, row: &RowSchema) -> Result<()> {
        if self.target.sqlite_sidecars_present(&row.rel_path)? {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        crate::provider_sqlite::verify_received(
            staged.file(),
            &self.target.staged_path(&row.rel_path, staged),
            row.size,
        )
    }

    fn end_filling(&mut self, filling: Filling, racy: bool) -> Result<()> {
        let Filling {
            row,
            key,
            record,
            snapshot,
            manifest,
            plan,
            failure,
            ..
        } = filling;
        let root = manifest.root;
        match plan {
            Plan::Refuse(refusal) => Err(refusal),
            Plan::Adopt(file, parent, verified) => {
                // Verified at the plan: read again only if it moved since.
                let unmoved = verified.filter(|identity| {
                    file.metadata()
                        .is_ok_and(|found| StatIdentity::from_metadata(&found) == *identity)
                });
                let identity = if let Some(identity) = unmoved {
                    identity
                } else {
                    let (verified, settled) =
                        verify_settled(self.target.path(), &file, &row, &manifest);
                    if matches!(verified, Err(BulkloadRefusal::DestinationOccupied)) {
                        self.remember_refusal(&key, settled, racy, RefusedOutput::Occupied);
                    }
                    verified?
                };
                self.adopt(file, parent, &row, (key, record, racy, root), identity)
            }
            Plan::NoExchange(settled) => {
                self.remember_refusal(&key, settled, racy, RefusedOutput::ExchangeUnsupported);
                Err(BulkloadRefusal::DestinationExchangeUnsupported)
            }
            Plan::Write(mut staging) => {
                self.open -= 1;
                if let Some(refusal) = failure {
                    let _ = staging.staged.discard();
                    return Err(refusal);
                }
                fault_point!(ReceiveAfterChunks);
                if snapshot {
                    if let Err(refusal) = self.verify_snapshot(&staging.staged, &row) {
                        let _ = staging.staged.discard();
                        return Err(refusal);
                    }
                    staging.staged.mark_sqlite();
                }
                self.publish(
                    staging.staged,
                    &row,
                    (key, record, racy, root),
                    staging.hints,
                    staging.supersede.map(|owned| *owned),
                )
            }
        }
    }

    /// Queue a verified existing output for its group commit, which seals
    /// it and its directory before the commit (#77 round 2, N4: an adopted
    /// output is reported held only after that commit).
    ///
    /// A non-racy capture's output first gets that capture's record (#169,
    /// [`unrowed::refresh`]), as a staged file does in [`Self::publish`]: if
    /// its row then never commits (a failed group, a crash), the next run
    /// adopts it from the record without a source read. Its row records the
    /// identity after the record's write, and the group's file seal makes
    /// the record durable before the row commits. `record` is the record's
    /// key: a snapshot's is keyed on its settled `-wal` too (#218, R1).
    fn adopt(
        &self,
        file: std::fs::File,
        parent: Arc<std::fs::File>,
        row: &RowSchema,
        (key, record, racy, root): (Vec<u8>, [u8; 32], bool, [u8; 32]),
        identity: StatIdentity,
    ) -> Result<()> {
        let identity = if racy {
            identity
        } else {
            unrowed::refresh(
                &file,
                &unrowed::CaptureRecord {
                    key: record,
                    root,
                    size: row.size,
                },
                identity,
            )?
        };
        self.committer.submit(Publication::Adopted {
            record: OutputRecord {
                key,
                rel_path: row.rel_path.clone(),
                identity,
                racy,
                hints: Vec::new(),
            },
            file,
            parent,
        })
    }

    /// Write the capture record of a non-racy capture (#169), apply the
    /// final mode, and queue a fully written staged file for its group
    /// commit, which seals it (record included), renames it and seals its
    /// directory. The record goes first: a read-only mode would refuse it.
    /// With `supersede`, the group commit exchanges it with this store's
    /// own output at the path instead of renaming it into a free one
    /// (WP0(d), #187). `record` is the record's key (#218, R1).
    fn publish(
        &mut self,
        staged: StagedFile,
        row: &RowSchema,
        (key, record, racy, root): (Vec<u8>, [u8; 32], bool, [u8; 32]),
        hints: Vec<ChunkHint>,
        supersede: Option<OwnedOutput>,
    ) -> Result<()> {
        if !racy {
            unrowed::write_record(
                staged.file(),
                &unrowed::CaptureRecord {
                    key: record,
                    root,
                    size: row.size,
                },
            );
        }
        if let Err(refusal) = crate::io::sys::fchmod(&**staged.file(), row.mode & 0o7777)
            .refuse_at("transfer::publish")
        {
            let _ = staged.discard();
            return Err(refusal);
        }
        fault_point!(MaterializeAfterTempWrite);
        self.session.insert(Arc::clone(staged.file()), &hints);
        let record = PendingOutput {
            key,
            rel_path: row.rel_path.clone(),
            size: row.size,
            racy,
            hints,
        };
        self.committer.submit(match supersede {
            Some(owned) => Publication::Superseding {
                staged,
                record,
                owned,
            },
            None => Publication::Staged { staged, record },
        })
    }
}

/// Chunks this session wrote into outputs that may not have committed yet,
/// readable through the open file. Bounded by the descriptor budget and
/// [`SESSION_FILES`]; older files are found through their committed hints.
struct SessionChunks {
    files: HashMap<u64, Arc<std::fs::File>>,
    order: VecDeque<(u64, Vec<[u8; 32]>)>,
    index: HashMap<[u8; 32], (u64, u64, u64)>,
    next: u64,
    capacity: usize,
}

impl SessionChunks {
    fn with_capacity(files: u64) -> Self {
        Self {
            files: HashMap::new(),
            order: VecDeque::new(),
            index: HashMap::new(),
            next: 0,
            capacity: usize::try_from(files).unwrap_or(1),
        }
    }

    fn insert(&mut self, file: Arc<std::fs::File>, hints: &[ChunkHint]) {
        let slot = self.next;
        self.next = self.next.wrapping_add(1);
        let mut owned = Vec::new();
        for hint in hints {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.index.entry(hint.digest)
            {
                entry.insert((slot, hint.offset, hint.size));
                owned.push(hint.digest);
            }
        }
        if owned.is_empty() {
            return;
        }
        self.files.insert(slot, file);
        self.order.push_back((slot, owned));
        while self.order.len() > self.capacity {
            if let Some((evicted, digests)) = self.order.pop_front() {
                self.files.remove(&evicted);
                for digest in digests {
                    self.index.remove(&digest);
                }
            }
        }
    }

    fn get(&self, digest: &[u8; 32]) -> Option<(&std::fs::File, u64, u64)> {
        let (slot, offset, size) = self.index.get(digest)?;
        Some((self.files.get(slot)?, *offset, *size))
    }
}

/// How the destination will satisfy one manifest.
enum Plan {
    Refuse(BulkloadRefusal),
    /// An existing output to adopt once the entry ends. The identity is the
    /// one its bytes were already verified under, when the plan checked
    /// them (an output of this store, WP0(d)); otherwise they are verified
    /// then.
    Adopt(std::fs::File, Arc<std::fs::File>, Option<StatIdentity>),
    /// This store's own output holds other bytes than the manifest's, on a
    /// file system with no atomic exchange to supersede it with: refused
    /// `DESTINATION_EXCHANGE_UNSUPPORTED` once the entry ends, with nothing
    /// staged and no chunk asked for. The identity is the output's, when
    /// the refusal may be remembered ([`verify_settled`]).
    NoExchange(Option<StatIdentity>),
    Write(Staging),
}

/// [`verify_existing`], and, when the output holds other bytes than the
/// manifest's (`DESTINATION_OCCUPIED`), the identity that answer may be
/// remembered under ([`RefusedOutput`]): the output's, when it was the same
/// before and after it was read and was not stamped within
/// [`RACY_GRANULARITY_NS`] of the read, so that the identity vouches for
/// the bytes that were read, as a seat's does for a capture (#86). `root`
/// is the destination root, whose clock the stamps are judged against.
fn verify_settled(
    root: &Path,
    file: &std::fs::File,
    row: &RowSchema,
    manifest: &Manifest,
) -> (Result<StatIdentity>, Option<StatIdentity>) {
    let started_ns = capture_clock(root);
    let before = file
        .metadata()
        .ok()
        .map(|found| StatIdentity::from_metadata(&found));
    let verified = verify_existing(file, row, manifest);
    if !matches!(verified, Err(BulkloadRefusal::DestinationOccupied)) {
        return (verified, None);
    }
    let settled = before.filter(|before| {
        let window = started_ns.saturating_sub(RACY_GRANULARITY_NS);
        let now_ns = capture_clock(root);
        before.mtime_ns < window
            && before.ctime_ns < window
            && before.mtime_ns <= now_ns
            && before.ctime_ns <= now_ns
            && file
                .metadata()
                .is_ok_and(|after| StatIdentity::from_metadata(&after) == *before)
    });
    (verified, settled)
}

struct Staging {
    staged: StagedFile,
    /// The staged file replaces this store's own output at the path
    /// (WP0(d), #187), which the plan found there with other bytes.
    supersede: Option<Box<OwnedOutput>>,
    /// Chunk indices to request: the first occurrence of each distinct chunk
    /// this destination could not fill, ascending.
    missing: Vec<u32>,
    /// Every offset each distinct chunk occupies, with its size.
    placements: HashMap<[u8; 32], (u64, Vec<u64>)>,
    hints: Vec<ChunkHint>,
}

/// Validate a manifest against its row, adopt an existing output, or stage a
/// new one and fill every chunk this destination already holds. Every
/// salvaged temporary a chunk was staged from is added to `salvaged_from`.
///
/// An existing output that is this store's own, untouched since its row was
/// written, and that no longer holds the manifest's bytes (the seat
/// changed) is superseded (WP0(d), OI-1003-Q18, #187): the new file is
/// staged beside it and filled like any other, the old output's own chunks
/// included through its hints, and its group commit exchanges the two. On
/// a file system with no atomic exchange it is refused
/// `DESTINATION_EXCHANGE_UNSUPPORTED` instead, before anything is staged.
/// Any other existing output is adopted if its bytes verify when the entry
/// ends, and refused `DESTINATION_OCCUPIED` if they do not: it is never
/// replaced. A `SQLite` snapshot's output follows the same rule (#218,
/// OI-1003-Q146).
fn plan_file(
    context: &ReceiveContext<'_>,
    row: &RowSchema,
    manifest: &Manifest,
    salvaged_from: &mut Vec<usize>,
) -> Plan {
    let mut placements: HashMap<[u8; 32], (u64, Vec<u64>)> = HashMap::new();
    let mut order = Vec::new();
    let mut offset = 0_u64;
    for (index, chunk) in manifest.chunks.iter().enumerate() {
        if chunk.size > u64::from(crate::hash::CDC_MAX_BYTES) || chunk.size == 0 {
            return Plan::Refuse(BulkloadRefusal::BudgetExceeded);
        }
        match placements.entry(chunk.digest) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Ok(index) = u32::try_from(index) else {
                    return Plan::Refuse(BulkloadRefusal::BudgetExceeded);
                };
                order.push((chunk.digest, index));
                entry.insert((chunk.size, vec![offset]));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().0 != chunk.size {
                    return Plan::Refuse(BulkloadRefusal::DigestMismatch);
                }
                entry.get_mut().1.push(offset);
            }
        }
        let Some(end) = offset.checked_add(chunk.size) else {
            return Plan::Refuse(BulkloadRefusal::BudgetExceeded);
        };
        offset = end;
    }
    if offset != row.size {
        return Plan::Refuse(BulkloadRefusal::DigestMismatch);
    }
    let supersede = match context.target.existing(row) {
        Ok(Some((file, parent))) => {
            match owned_output(context.store, context.authority, row, &file) {
                Ok(Some(owned)) => {
                    let (verified, settled) =
                        verify_settled(context.target.path(), &file, row, manifest);
                    match verified {
                        // The seat was touched, not changed: nothing to
                        // replace.
                        Ok(identity) => return Plan::Adopt(file, parent, Some(identity)),
                        // Changed. Without an atomic exchange the output
                        // cannot be superseded: say so now, before a byte
                        // is staged or asked of the source. A snapshot
                        // output is superseded the same way (#218,
                        // OI-1003-Q146): its sidecars were checked at
                        // Decide and are checked again just before the
                        // exchange.
                        Err(BulkloadRefusal::DestinationOccupied) => {
                            match context.target.exchange_supported(row) {
                                Ok(true) => Some(Box::new(owned)),
                                Ok(false) => return Plan::NoExchange(settled),
                                Err(refusal) => return Plan::Refuse(refusal),
                            }
                        }
                        Err(refusal) => return Plan::Refuse(refusal),
                    }
                }
                Ok(None) => return Plan::Adopt(file, parent, None),
                Err(refusal) => return Plan::Refuse(refusal),
            }
        }
        Ok(None) => None,
        Err(refusal) => return Plan::Refuse(refusal),
    };
    let staged = match context.target.stage(row) {
        Ok(staged) => staged,
        Err(refusal) => return Plan::Refuse(refusal),
    };
    let mut missing = Vec::new();
    let mut hints = Vec::with_capacity(order.len());
    let mut outputs = HashMap::new();
    for (digest, index) in order {
        let Some((size, offsets)) = placements.get(&digest) else {
            continue;
        };
        if let Some(first) = offsets.first() {
            hints.push(ChunkHint {
                digest,
                offset: *first,
                size: *size,
            });
        }
        let filled =
            local_chunk(context, &mut outputs, &digest, *size, salvaged_from).and_then(|data| {
                data.map(|data| place(staged.file(), &data, offsets))
                    .transpose()
            });
        match filled {
            Ok(Some(())) => (),
            Ok(None) => missing.push(index),
            Err(refusal) => {
                let _ = staged.discard();
                return Plan::Refuse(refusal);
            }
        }
    }
    Plan::Write(Staging {
        staged,
        supersede,
        missing,
        placements,
        hints,
    })
}

/// A chunk this destination already holds, re-read and re-verified: from a
/// file written earlier in this session, or from a published output through
/// a committed hint, newest first (and, for an output this session has
/// superseded at that path, from the old output it displaced). Any mismatch is a miss, never an error,
/// and a miss on one hint falls through to the next. A chunk read from a
/// salvaged temporary adds its index to `salvaged_from`.
fn local_chunk(
    context: &ReceiveContext<'_>,
    outputs: &mut HashMap<Vec<u8>, Option<std::fs::File>>,
    digest: &[u8; 32],
    size: u64,
    salvaged_from: &mut Vec<usize>,
) -> Result<Option<Vec<u8>>> {
    if let Some((file, offset, held)) = context.session.get(digest) {
        if held == size {
            if let Some(data) = read_verified(file, offset, size, digest) {
                return Ok(Some(data));
            }
        }
    }
    if let Some((at, offset, held)) = context.salvage.index.get(digest) {
        if *held == size {
            if let Some(data) = context
                .target
                .salvaged_file(*at)
                .and_then(|file| read_verified(&file, *offset, size, digest))
            {
                salvaged_from.push(*at);
                return Ok(Some(data));
            }
        }
    }
    for hint in context.store.output_chunks(digest)? {
        if hint.size != size {
            continue;
        }
        let found = if let Some(file) = outputs.get(&hint.path) {
            file.as_ref()
                .and_then(|file| read_verified(file, hint.offset, size, digest))
        } else {
            let file = context.target.open_output(&hint.path).ok();
            let found = file
                .as_ref()
                .and_then(|file| read_verified(file, hint.offset, size, digest));
            if outputs.len() < HINT_FILES {
                outputs.insert(hint.path.clone(), file);
            }
            found
        };
        if found.is_some() {
            return Ok(found);
        }
        // The hint may describe an output this session has since replaced
        // at that path (WP0(d)): the old one is still readable.
        let old = context
            .displaced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hint.path);
        let found = old.and_then(|file| read_verified(&file, hint.offset, size, digest));
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

fn read_verified(
    file: &std::fs::File,
    offset: u64,
    size: u64,
    digest: &[u8; 32],
) -> Option<Vec<u8>> {
    let mut data = vec![0_u8; usize::try_from(size).ok()?];
    file.read_exact_at(&mut data, offset).ok()?;
    counters::add_len(Counter::DestLocalReuseRead, data.len());
    (counters::hash(Counter::HashDestReuse, &data) == *digest).then_some(data)
}

fn place(file: &std::fs::File, data: &[u8], offsets: &[u64]) -> Result<()> {
    for offset in offsets {
        crate::io::sys::pwrite_all(file, data, *offset).refuse_at("transfer::place")?;
        counters::add_len(Counter::DestMaterializeWrite, data.len());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Read one bounded frame without searching for delimiters. A data frame's
/// payload is read straight into its own buffer.
///
/// # Errors
/// Refuses truncated, oversized or invalid frames.
pub fn read_frame<R: Read>(input: &mut R) -> Result<Frame> {
    let mut prefix = [0_u8; FRAME_HEADER_BYTES];
    input
        .read_exact(&mut prefix)
        .refuse_at("transfer::read_frame")?;
    let [a, b, c, d, tag] = prefix;
    let length = u32::from_be_bytes([a, b, c, d]) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let body = length.checked_sub(1).ok_or(BulkloadRefusal::FrameCodec)?;
    let frame = if tag == TAG_DATA {
        // Bound the frame before reading any of it, so a short frame can
        // never pull the next frame's bytes in as its header.
        let size = body
            .checked_sub(DATA_HEADER_BYTES)
            .ok_or(BulkloadRefusal::FrameCodec)?;
        if size > MAX_DATA_PAYLOAD {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let mut header = [0_u8; DATA_HEADER_BYTES];
        input
            .read_exact(&mut header)
            .refuse_at("transfer::read_frame")?;
        let header = DataHeader::from_bytes(&header)?;
        if header.size as usize != size {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let mut payload = vec![0_u8; size];
        input
            .read_exact(&mut payload)
            .refuse_at("transfer::read_frame")?;
        Frame::Data { header, payload }
    } else {
        let mut bytes = vec![0_u8; body];
        input
            .read_exact(&mut bytes)
            .refuse_at("transfer::read_frame")?;
        Frame::decode_body(tag, &bytes)?
    };
    counters::bump(Counter::WireFramesReceived);
    counters::add_len(Counter::WireBytesReceived, FRAME_HEADER_BYTES + body);
    Ok(frame)
}

/// Write one data frame: its fixed header and the payload with one gathered
/// write, never copying the payload into a frame buffer.
fn write_data<W: Write>(output: &mut W, header: &DataHeader, payload: &[u8]) -> Result<()> {
    if header.size as usize != payload.len() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    let prefix = data_prefix(header)?;
    let mut slices = [IoSlice::new(&prefix), IoSlice::new(payload)];
    let mut remaining: &mut [IoSlice<'_>] = &mut slices;
    while !remaining.is_empty() {
        match output.write_vectored(remaining) {
            Ok(0) => {
                return Err(crate::refuse::io(
                    &std::io::Error::from(std::io::ErrorKind::WriteZero),
                    "transfer::write_data",
                ))
            }
            Ok(written) => IoSlice::advance_slices(&mut remaining, written),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(crate::refuse::io(&error, "transfer::write_data")),
        }
    }
    output.flush().refuse_at("transfer::write_data")?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, prefix.len() + payload.len());
    Ok(())
}

/// Write one control frame and flush it.
///
/// # Errors
/// Refuses oversized messages and broken transports.
pub fn write_control<W: Write>(output: &mut W, control: &Control) -> Result<()> {
    let encoded = control.encode()?;
    output
        .write_all(&encoded)
        .refuse_at("transfer::write_control")?;
    output.flush().refuse_at("transfer::write_control")?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, encoded.len());
    Ok(())
}

fn path(bytes: Vec<u8>) -> PathBuf {
    std::ffi::OsString::from_vec(bytes).into()
}

#[cfg(test)]
mod tests;
pub(crate) mod unrowed;
