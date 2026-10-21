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
//!   output to adopt, or chunks held by published outputs; with the delta
//!   lever of OI-1003-Q154 on, a local `copy` with the hand-off streams a
//!   seat new to the destination instead). The source sends
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
    owned_beside, owned_output, verify_existing, Destination, Displaced, OwnedOutput,
    PendingOutput, Publication, PublishSink, SharedDisplaced, StagedFile,
};
use crate::transfer_store::{
    row_key, sqlite_key, ChunkHint, LedgerItem, LedgerRecord, LedgerSink, Manifest, OutputRecord,
    PrefixProof, PublisherSide, RefusedOutput, RefusedSeat, Store,
};
use crate::walk::{WalkItem, Walker};
use crate::{BulkloadRefusal, Result, RowSchema};
use handoff::{HandIn, HandOut, Payload, SourceChunk};

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
/// An entry's staged file is kicked into write-back each time its placed
/// bytes cross a multiple of this, while the data still streams
/// (OI-1003-Q143 item 1a, extending OI-1003-Q107). Group mode on Linux only.
const WRITEBACK_KICK_BYTES: u64 = 8 * 1024 * 1024;
/// Receive workers verifying and writing data frames beside the receiving
/// thread, by default (OI-1003-Q143 item 3). Zero is the inline path.
const RECV_WORKERS: usize = 2;
/// The most receive workers [`RECV_WORKERS_ENV`] may ask for.
const RECV_WORKERS_MAX: usize = 16;
/// Sets the receive workers for a process (`0` to [`RECV_WORKERS_MAX`]), for
/// the bench's A/B (OI-1003-Q143 item 3). Unset or malformed keeps the
/// default.
pub const RECV_WORKERS_ENV: &str = "BULKLOAD_RECV_WORKERS";
/// Payload bytes the receive workers may hold queued or in progress. Each
/// job is charged at least [`RECV_JOB_MIN_BYTES`], so empty or tiny frames
/// are bounded too (#217 review).
const RECV_POOL_BYTES: u64 = 8 * 1024 * 1024;
/// The least a job is charged against [`RECV_POOL_BYTES`].
const RECV_JOB_MIN_BYTES: u64 = 4096;
/// Walk frames a batch of new directories may hold back, every kind
/// counted (#217 review); well below [`ENTRY_WINDOW`], so the source always
/// has window to keep offering (OI-1003-Q143 item 2).
const DIRECTORY_BATCH_FRAMES: usize = 256;
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
/// same primary codes a snapshot refusal is remembered by, R10). Since WP3
/// PR 4 it is also the default disposition of the refusal's closure row
/// (`closure-report --transfer`, [`crate::transfer_outcome`]), which an
/// operator review overrides; every other refusal has no default.
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
// Inside the stream (S1, OI-1003-Q143 step 1a): the wait for the open
// group's commit once nothing more can arrive, and the frame loop's ramp to
// its first data frame.
static RECV_DRAIN_NS: AtomicU64 = AtomicU64::new(0);
static RECV_FIRST_DATA_NS: AtomicU64 = AtomicU64::new(0);
// The receive workers (OI-1003-Q143 item 3, #217 review): their verify and
// their writes, summed over workers; the receiving thread's wait on them;
// and the sending thread's writes to the wire.
static RECV_WORKER_VERIFY_NS: AtomicU64 = AtomicU64::new(0);
static RECV_WORKER_PLACE_NS: AtomicU64 = AtomicU64::new(0);
static RECV_POOL_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static SEND_WRITE_NS: AtomicU64 = AtomicU64::new(0);

/// Receiver accounting; any refusal means the requested carry is incomplete.
#[derive(Debug, Default)]
pub struct TransferStats {
    /// Newly materialized or independently verified regular files.
    pub completed: u64,
    /// Entries skipped as excluded ([`set_excluded`]), never landed.
    pub excluded: u64,
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
    /// The OS errno of an `IO` refusal in [`Self::refusals`] that this
    /// side raised, by relative path (S4, TIN-6592): its transfer record
    /// keeps it. A refusal the peer sent carries its code alone (wire v6
    /// has no errno), so none is recorded for it.
    pub refusal_errnos: BTreeMap<Vec<u8>, i32>,
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
    /// Existing directories this store does not own, entered with their
    /// mode kept although it differs from the source's (M2 item 2): by
    /// relative path, with the mode found and the source's.
    pub directories_mode_kept: Vec<(Vec<u8>, u32, u32)>,
    /// Existing regular files with no owning record that held a verified
    /// byte prefix of their source and were extended in place by its tail
    /// (M2r2 A-103): by relative path, with the prefix's length and BLAKE3
    /// and the size the file was extended to. Each is also reported
    /// `prefix-extended` and ledgered in the store's `prefix_extensions`.
    pub prefix_extended: Vec<(Vec<u8>, u64, [u8; 32], u64)>,
    /// Paths refused with an unsettled prefix extension of a crashed run
    /// recorded (M2r2 A-103), with the length the file was proven to hold
    /// then: what is past it was appended since. Reported
    /// `prefix-extension-interrupted`.
    pub prefix_interrupted: Vec<(Vec<u8>, u64)>,
    /// Directories this session finished in its last output group's commit
    /// rather than in a completion commit of their own (OI-1003-Q154 lever
    /// 3; `directories_folded`). 0 when the fold is off
    /// ([`DIRECTORY_FOLD_ENV`]) or could not ride a group.
    pub directories_folded: u64,
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
    /// The source's regular and symlink seats under the listing prefixes
    /// ([`set_listing`]), every one the walk offered, as it named them:
    /// this pull's source listing, the provenance `estate-reseat` tells the
    /// source's sessions from the destination's own by (independent review
    /// of #280, N4).
    pub listed: Vec<Vec<u8>>,
    /// Seats whose bytes were published, reused or adopted beside a
    /// differing live destination file under `--beside SUFFIX`
    /// ([`set_beside_suffix`]): `(source path, beside path)`. Each is
    /// reported `applied-beside` and is an `applied-beside` row of the
    /// session's transfer record, accounted in closure.
    pub applied_beside: Vec<(Vec<u8>, Vec<u8>)>,
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

    /// Record a refusal of `rel_path` that this side raised, with the
    /// `SQLite` code it carries, keeping a bare `IO`'s errno (S4,
    /// TIN-6592).
    fn refuse_here(&mut self, rel_path: Vec<u8>, refusal: &BulkloadRefusal) {
        self.keep_errno(&rel_path, refusal);
        self.refuse(rel_path, refusal.code().to_owned(), refusal.sqlite_code());
    }

    /// Keep the errno of a bare `IO` this side raised for `rel_path` (S4,
    /// TIN-6592).
    fn keep_errno(&mut self, rel_path: &[u8], refusal: &BulkloadRefusal) {
        if let BulkloadRefusal::Io(Some(errno)) = refusal {
            self.refusal_errnos.insert(rel_path.to_vec(), *errno);
        }
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
    /// The receiving thread waiting in `settle_held` for the open group's
    /// commit (`committer.sync()`: its file and directory seals and its
    /// store commit) once every entry is offered and none has content
    /// outstanding: the final drain. Part of [`Self::recv_stream_ns`], and
    /// of [`Self::recv_settle_ns`] when the loop's own settle call drains.
    pub recv_drain_ns: u64,
    /// The frame loop's start to its first data frame: the stream's ramp,
    /// part of [`Self::recv_stream_ns`]. Zero for a copy with no data frame.
    pub recv_first_data_ns: u64,
    /// The receive workers' wire verify, summed over workers (item 3). The
    /// receiving thread's own verify stays in [`Self::recv_verify_ns`].
    pub recv_worker_verify_ns: u64,
    /// The receive workers' writes and write-back kicks, summed over
    /// workers (items 1a and 3).
    pub recv_worker_place_ns: u64,
    /// The receiving thread blocked on the receive workers: their byte
    /// budget, or an entry's last writes before its `End` (item 3).
    pub recv_pool_wait_ns: u64,
    /// The sending thread writing frames to the wire, blocked there when
    /// the wire is full; part of [`Self::send_handle_ns`].
    pub send_write_ns: u64,
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
            recv_drain_ns: RECV_DRAIN_NS.load(Ordering::Relaxed),
            recv_first_data_ns: RECV_FIRST_DATA_NS.load(Ordering::Relaxed),
            recv_worker_verify_ns: RECV_WORKER_VERIFY_NS.load(Ordering::Relaxed),
            recv_worker_place_ns: RECV_WORKER_PLACE_NS.load(Ordering::Relaxed),
            recv_pool_wait_ns: RECV_POOL_WAIT_NS.load(Ordering::Relaxed),
            send_write_ns: SEND_WRITE_NS.load(Ordering::Relaxed),
        }
    }

    /// Space-separated `key=value` pairs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "walk_ns={} walk_wait_ns={} reuse_census_ns={} cdc_hash_ns={} queue_wait_ns={} transfer_ns={} materialize_ns={} send_wait_ns={} send_handle_ns={} recv_read_ns={} recv_settle_ns={} recv_verify_ns={} recv_setup_ns={} recv_stream_ns={} recv_tail_ns={} recv_drain_ns={} recv_first_data_ns={} recv_worker_verify_ns={} recv_worker_place_ns={} recv_pool_wait_ns={} send_write_ns={}",
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
            self.recv_drain_ns,
            self.recv_first_data_ns,
            self.recv_worker_verify_ns,
            self.recv_worker_place_ns,
            self.recv_pool_wait_ns,
            self.send_write_ns,
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
            recv_drain_ns: self.recv_drain_ns.saturating_sub(before.recv_drain_ns),
            recv_first_data_ns: self
                .recv_first_data_ns
                .saturating_sub(before.recv_first_data_ns),
            recv_worker_verify_ns: self
                .recv_worker_verify_ns
                .saturating_sub(before.recv_worker_verify_ns),
            recv_worker_place_ns: self
                .recv_worker_place_ns
                .saturating_sub(before.recv_worker_place_ns),
            recv_pool_wait_ns: self
                .recv_pool_wait_ns
                .saturating_sub(before.recv_pool_wait_ns),
            send_write_ns: self.send_write_ns.saturating_sub(before.send_write_ns),
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

/// What the operator named as a root (#232): a tree, walked beneath its
/// directory, or one regular file, served and landed by itself.
///
/// `copy --file` and `pull --file` choose [`RootKind::File`] on the
/// destination and run the source half as `serve --file`. The wire is
/// unchanged (v6): a file session is a tree session whose root descriptor
/// is the file's parent directory and whose walk visits one name, so it
/// carries one `Entry` named by the file's leaf. The destination lands
/// that entry under DEST's own leaf, in DEST's parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootKind {
    /// A directory root: every seat beneath it is walked.
    #[default]
    Tree,
    /// One regular file: the root descriptor is its parent directory, and
    /// the walk visits only its name.
    File,
}

/// `path`'s parent, or `.` for a bare name.
fn parent_or_dot(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The canonical path a private state root has, or will have once a store
/// creates it: the root itself when it exists, else its nearest existing
/// ancestor, canonical, joined with every missing name below it, since
/// `serve` makes each missing parent ([`ensure_state_parents`], #276).
/// Read-only, so an overlap with a root it must not touch is refused before
/// anything is created (WP1 PR 3, S2).
fn canonical_state(state: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(state) {
        Ok(_) => std::fs::canonicalize(state).refuse_at("transfer::canonical_state"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut missing = vec![state.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?];
            let mut at = parent_or_dot(state);
            loop {
                match std::fs::canonicalize(at) {
                    Ok(mut found) => {
                        found.extend(missing.iter().rev());
                        return Ok(found);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        missing.push(at.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?);
                        at = parent_or_dot(at);
                    }
                    Err(error) => {
                        return Err(crate::refuse::io(&error, "transfer::canonical_state"))
                    }
                }
            }
        }
        Err(error) => Err(crate::refuse::io(&error, "transfer::canonical_state")),
    }
}

/// `serve`'s own precondition (#276): every directory above its state root
/// exists. Each absent one is made here, mode 0700, from the nearest
/// existing ancestor down (`create_dir_all` semantics). The ancestor is the
/// operator's path, symlinks and all, as the store's own earlier components
/// are; every directory made here is made with `mkdirat` beneath the
/// descriptor of the one above it and entered with `O_NOFOLLOW`, so a name
/// swapped for a symlink in between refuses `PATH_ESCAPES_ROOT` and nothing
/// is made through it. An existing directory is left as it is, whatever its
/// mode: the store judges its own root, not the path above it. Each new
/// entry is made durable before the next one beneath it (R-N102).
///
/// `estate-move-source` makes `MOVE_STATE/serve` and `MOVE_STATE/serve/carry`
/// too; this covers a state handed to `serve` by anything else (`pull` by
/// hand, `estate-carry-files --source-state-root`, a newer destination half
/// against an older source tree), whoever drives it.
fn ensure_state_parents(state: &Path) -> Result<()> {
    use crate::counters::CountedSync as _;
    const SITE: &str = "transfer::ensure_state_parents";
    let mut missing = Vec::new();
    let mut at = parent_or_dot(state);
    loop {
        match std::fs::symlink_metadata(at) {
            Ok(_) => break,
            // A missing `.` or `/` has no name to make: the walk ends.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if at.file_name().is_none() {
                    return Err(crate::refuse::io(&error, SITE));
                }
                missing.push(at);
                at = parent_or_dot(at);
            }
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut directory = std::fs::File::open(at).refuse_at(SITE)?;
    for path in missing.iter().rev() {
        let name = path.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
        let name = crate::io::c_name(name.as_bytes()).refuse_at(SITE)?;
        match crate::io::sys::mkdirat(&directory, &name, 0o700) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(crate::refuse::io_in(&error, path, SITE)),
        }
        directory.sync_dir_counted().refuse_at(SITE)?;
        let next = crate::io::sys::open_dir_at(&directory, &name).map_err(|error| {
            if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                BulkloadRefusal::PathEscapesRoot
            } else {
                crate::refuse::io(&error, SITE)
            }
        })?;
        directory = std::fs::File::from(next);
    }
    Ok(())
}

/// The source root of a session, open (#232).
struct SourceRoot {
    /// The directory every read resolves beneath, and that `SQLite` opens a
    /// snapshot seat beneath by path: the root itself for a tree, the
    /// file's parent for a `--file` root.
    dir: PathBuf,
    fd: std::os::fd::OwnedFd,
    /// The one name the walk visits, for a `--file` root.
    seat: Option<std::ffi::CString>,
}

/// Open the source `root` as the `kind` the verb was given: a tree's
/// canonical directory, or a `--file` root's canonical parent with its leaf
/// as given. A `--file` leaf is not looked at here: the single-seat walk
/// judges it after `Start`, so a missing leaf is a `SOURCE_VANISHED` row and
/// a directory or special file a `SOURCE_ROOT_KIND_MISMATCH` row, each
/// carried typed over the wire to the destination's closure (#283 review 2).
///
/// # Errors
/// `SOURCE_ROOT_MISSING`, `SOURCE_ROOT_UNREADABLE` (with the errno), or
/// `SOURCE_ROOT_KIND_MISMATCH` for a regular file named as a tree.
fn open_source_root(root: &Path, kind: RootKind) -> Result<SourceRoot> {
    match kind {
        RootKind::Tree => {
            let fd = crate::io::sys::open_root(root).map_err(|error| {
                // The root resolved, so `ENOTDIR` here is the root itself.
                if error.raw_os_error() == Some(libc::ENOTDIR) {
                    BulkloadRefusal::SourceRootKindMismatch
                } else {
                    crate::refuse::source_root(&error, "transfer::open_source_root")
                }
            })?;
            Ok(SourceRoot {
                dir: root.to_path_buf(),
                fd,
                seat: None,
            })
        }
        RootKind::File => {
            let name = root
                .file_name()
                .ok_or(BulkloadRefusal::SourceRootKindMismatch)?;
            let dir = parent_or_dot(root);
            let fd = crate::io::sys::open_root(dir).map_err(|error| {
                crate::refuse::source_root(&error, "transfer::open_source_root")
            })?;
            let seat =
                crate::io::c_name(name.as_bytes()).refuse_at("transfer::open_source_root")?;
            Ok(SourceRoot {
                dir: dir.to_path_buf(),
                fd,
                seat: Some(seat),
            })
        }
    }
}

/// Where a session lands, as the operator named it (#232).
struct Landing {
    /// The canonical directory the session lands in: DEST for a tree,
    /// DEST's parent for a `--file` root.
    dir: PathBuf,
    /// The path the destination opens, without following its last
    /// component: DEST as the operator gave it for an existing tree (a
    /// symlink there is refused, as before #232), else [`Landing::dir`].
    open: PathBuf,
    /// The one name the session lands, for a `--file` root.
    leaf: Option<Vec<u8>>,
    /// A tree root that is absent: made (the source root's mode, else
    /// 0700) once the source's `Start` arrives.
    create: bool,
}

impl Landing {
    /// The path the S2 checks hold apart from the states and the source:
    /// the tree, or the file itself. A `--file` session writes one name
    /// in its parent and sweeps only its own tagged temporaries there, so a
    /// state beside the file (`~/.zshrc` beside a state under `~`) is apart.
    fn root(&self) -> PathBuf {
        self.leaf.as_ref().map_or_else(
            || self.dir.clone(),
            |leaf| self.dir.join(crate::walk::rel_path(leaf)),
        )
    }
}

/// Resolve the destination the operator named, read-only (#232).
///
/// A tree: an existing directory is used as it is; an absent one is made
/// by the session once its checks pass, so its parent must exist. A
/// `--file` root: DEST's parent must exist, and DEST may be absent or a
/// file (adopted when identical, `DESTINATION_OCCUPIED` when it differs).
///
/// # Errors
/// `DESTINATION_PARENT_MISSING` for an absent parent;
/// `DESTINATION_KIND_MISMATCH` for a non-directory where a tree lands or a
/// directory where a file does.
fn resolve_destination(destination: &Path, kind: RootKind) -> Result<Landing> {
    // A trailing `/` (or `/.`) makes the kernel follow a symlink at the
    // last component, in the lstat below and in the no-follow open after
    // it: DEST is rebuilt from its components, so it is judged, and later
    // opened, as the name it is (#283 verification A).
    let destination: PathBuf = destination.components().collect();
    let destination = destination.as_path();
    let found = match std::fs::symlink_metadata(destination) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(crate::refuse::io(&error, "transfer::resolve_destination")),
    };
    let canonical_parent = || match std::fs::canonicalize(parent_or_dot(destination)) {
        Ok(parent) => Ok(parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(BulkloadRefusal::DestinationParentMissing)
        }
        Err(error) => Err(crate::refuse::io(&error, "transfer::resolve_destination")),
    };
    match kind {
        RootKind::Tree => match found {
            // A symlink at DEST (to a directory, a file or nothing) is never
            // followed: refused typed, before anything is opened or written
            // (#283 review 3; it was IO errno 20 or 40).
            Some(meta) if !meta.is_dir() => Err(BulkloadRefusal::DestinationKindMismatch),
            Some(_) => Ok(Landing {
                dir: std::fs::canonicalize(destination)
                    .refuse_at("transfer::resolve_destination")?,
                open: destination.to_path_buf(),
                leaf: None,
                create: false,
            }),
            None => {
                let name = destination
                    .file_name()
                    .ok_or(BulkloadRefusal::PathNotAbsolute)?;
                let dir = canonical_parent()?.join(name);
                Ok(Landing {
                    open: dir.clone(),
                    dir,
                    leaf: None,
                    create: true,
                })
            }
        },
        RootKind::File => {
            let name = destination
                .file_name()
                .ok_or(BulkloadRefusal::DestinationKindMismatch)?;
            if found.is_some_and(|meta| meta.is_dir()) {
                return Err(BulkloadRefusal::DestinationKindMismatch);
            }
            // DEST's parent is an earlier component of the path the
            // operator named, followed like any other; DEST itself is
            // opened beneath it without following, as a seat in a tree.
            let dir = canonical_parent()?;
            Ok(Landing {
                open: dir.clone(),
                dir,
                leaf: Some(name.as_bytes().to_vec()),
                create: false,
            })
        }
    }
}

/// The source's `Start`: its authority. Anything else first is a protocol
/// violation.
fn read_start<R: Read>(input: &mut R, hand: Option<&HandIn>) -> Result<Vec<u8>> {
    let Wire::Frame(Frame::Control(Control::Start { authority })) = read_wire(input, hand)? else {
        return Err(BulkloadRefusal::ProtocolStateViolation);
    };
    Ok(authority)
}

/// Make an absent destination tree root (#232) in its existing parent, with
/// `mode`: the source root's permission bits when this side knows them
/// (`copy`), else 0700 (`pull`: wire v6 carries no root row), never wider,
/// and narrowed further by the process umask (#283 verification B: a
/// `copy ~/.ssh /new/.ssh` must not land world-listable). Only the root the
/// operator named, never a parent (R2.2: the run writes where it was asked
/// and nowhere else), and only once the S2 checks passed, so a refused
/// session makes nothing. Its entry is made durable before the store that
/// names it opens.
fn make_destination_root(dir: &Path, mode: u32) -> Result<()> {
    use crate::counters::CountedSync as _;
    let name = dir.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let name = crate::io::c_name(name.as_bytes()).refuse_at("transfer::make_destination_root")?;
    let parent =
        std::fs::File::open(parent_or_dot(dir)).refuse_at("transfer::make_destination_root")?;
    match crate::io::sys::mkdirat(&parent, &name, mode & 0o777) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(crate::refuse::io_in(
                &error,
                dir,
                "transfer::make_destination_root",
            ))
        }
    }
    parent
        .sync_dir_counted()
        .refuse_at("transfer::make_destination_root")
}

/// `copy`'s S2 overlap decision, read-only.
///
/// The source and the destination apart, neither state root inside the
/// source, each apart from its own root (WP1 PR 3), and the two state roots
/// apart from each other and from the destination (OI-1003-Q143 item 1b).
/// [`copy_with`] makes it before either store exists, and
/// [`crate::transfer_outcome::record_session`] before its first record
/// write, so a refused overlap writes nothing anywhere (review finding 1).
///
/// # Errors
/// `SNAPSHOT_ROOTS_OVERLAP`, or the refusal of a root that does not resolve.
pub fn copy_roots_apart(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
) -> Result<()> {
    copy_roots_apart_kind(
        source,
        destination,
        source_state,
        destination_state,
        RootKind::Tree,
    )
}

/// [`copy_roots_apart`] for a root of `kind` (#232).
///
/// With [`RootKind::File`] the source is one regular file and DEST the
/// file it lands as, so the check holds the two files apart (a `copy` of a
/// file onto itself is an overlap) and each state apart from each file.
///
/// # Errors
/// As [`copy_roots_apart`], plus the root refusals of
/// [`resolve_destination`] and a source that does not resolve
/// (`SOURCE_ROOT_MISSING`, `SOURCE_ROOT_UNREADABLE`).
pub fn copy_roots_apart_kind(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
    kind: RootKind,
) -> Result<()> {
    copy_roots(source, destination, source_state, destination_state, kind).map(|_| ())
}

/// [`copy_roots_apart_kind`], returning the canonical destination
/// directory that [`copy_with`] keys the loopback hand-off by (DEST for a
/// tree, made by the session when absent; DEST's parent for a file).
fn copy_roots(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
    kind: RootKind,
) -> Result<PathBuf> {
    // A `--file` root resolves only its parent, as `serve` does: the leaf
    // is never followed, and a missing one is the walk's typed row.
    let source_root = match kind {
        RootKind::Tree => std::fs::canonicalize(source),
        RootKind::File => {
            let leaf = source
                .file_name()
                .ok_or(BulkloadRefusal::SourceRootKindMismatch)?;
            std::fs::canonicalize(parent_or_dot(source)).map(|parent| parent.join(leaf))
        }
    }
    .map_err(|error| crate::refuse::source_root(&error, "transfer::copy_roots"))?;
    let landing = resolve_destination(destination, kind)?;
    let destination_root = landing.root();
    if overlaps(&source_root, &destination_root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    // S2 (WP1 PR 3): both halves run at once, so neither state root may be
    // created inside the source, and each must stay apart from its own root,
    // all decided before either store exists.
    let source_state_root = canonical_state(source_state)?;
    let destination_state_root = canonical_state(destination_state)?;
    // The two stores are opened at once (OI-1003-Q143 item 1b), so neither
    // state root may hold or lie inside the other, or inside the
    // destination root that the destination's setup sweeps (#217 review).
    if overlaps(&source_state_root, &source_root)
        || overlaps(&destination_state_root, &source_root)
        || overlaps(&destination_state_root, &destination_root)
        || overlaps(&source_state_root, &destination_root)
        || overlaps(&source_state_root, &destination_state_root)
    {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    Ok(landing.dir)
}

/// `pull`'s local half of the same decision, read-only.
///
/// The destination state root apart from the destination (the source is
/// the remote `serve`'s to check). [`receive_with`] checks it again on the open store.
///
/// # Errors
/// `SNAPSHOT_ROOTS_OVERLAP`, or the refusal of a path that does not resolve.
pub fn receive_roots_apart(destination: &Path, destination_state: &Path) -> Result<()> {
    receive_roots_apart_kind(destination, destination_state, RootKind::Tree)
}

/// [`receive_roots_apart`] for a root of `kind` (#232).
///
/// # Errors
/// As [`receive_roots_apart`], plus the root refusals of
/// [`resolve_destination`].
pub fn receive_roots_apart_kind(
    destination: &Path,
    destination_state: &Path,
    kind: RootKind,
) -> Result<()> {
    if overlaps(
        &resolve_destination(destination, kind)?.root(),
        &canonical_state(destination_state)?,
    ) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    Ok(())
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
/// Both halves run in this process, from this build, over a socket pair
/// made here, so they are joined by the loopback hand-off
/// (OI-1003-Q154 levers 1 and 2; `transfer/handoff.rs`): a chunk's bytes
/// are handed over, not written to the socket, and the destination takes
/// the source's own BLAKE3 of them for its verify; frames are batched and
/// read through a buffer. With [`LOOPBACK_HANDOFF_ENV`] set to `0` it runs
/// the plain protocol instead, every byte over the socket and hashed twice.
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
    copy_session(
        source,
        destination,
        source_state,
        destination_state,
        sqlite,
        RootKind::Tree,
    )
}

/// [`copy`] of a root of `kind` (#232), in the process's [`sqlite_mode`].
///
/// With [`RootKind::File`], `source` is one regular file and `destination`
/// the file it lands as, byte for byte, in an existing parent.
///
/// # Errors
/// As [`copy`], plus the root refusals (`SOURCE_ROOT_*`, `DESTINATION_*`).
pub fn copy_kind(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
    kind: RootKind,
) -> Result<TransferStats> {
    copy_session(
        source,
        destination,
        source_state,
        destination_state,
        sqlite_mode(),
        kind,
    )
}

/// [`copy_with`] of a root of `kind`: both halves of the session in this
/// process, the source half as `serve` would run it with that kind.
fn copy_session(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
    sqlite: SqliteMode,
    kind: RootKind,
) -> Result<TransferStats> {
    let destination_root = copy_roots(source, destination, source_state, destination_state, kind)?;
    // The source root is on this host: an absent DEST root takes its mode
    // (#283 verification B). A `--file` session never makes a root.
    let source_mode = match kind {
        RootKind::Tree => std::fs::metadata(source).ok().map(|meta| meta.mode()),
        RootKind::File => None,
    };
    let (hand_out, hand_in) = if loopback_handoff(&destination_root) {
        let (out, into) = handoff::pair();
        (Some(out), Some(into))
    } else {
        (None, None)
    };
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
                let served = match hand_out {
                    // Lever 2: the destination's frames read in batches.
                    Some(hand) => serve_session(
                        std::io::BufReader::with_capacity(LOOPBACK_READ_BYTES, input),
                        &mut sender,
                        Some(hand),
                        kind,
                    ),
                    None => serve_session(input, &mut sender, None, kind),
                };
                // The source's reader thread holds a clone of this end; shutting
                // it down ends that thread and tells the destination the source
                // is gone, whatever `serve` returned.
                let _ = sender.shutdown(std::net::Shutdown::Both);
                served
            })
            .refuse_at("transfer::copy")?;
        let result = {
            let mut output = receiver.try_clone().refuse_at("transfer::copy")?;
            let sides = ((source, source_state), (destination, destination_state));
            match hand_in {
                // Lever 2: the source's frames read in batches.
                Some(hand) => receive_session(
                    &mut std::io::BufReader::with_capacity(
                        LOOPBACK_READ_BYTES,
                        receiver.try_clone().refuse_at("transfer::copy")?,
                    ),
                    &mut output,
                    sides.0,
                    sides.1,
                    sqlite,
                    Some(hand),
                    (kind, source_mode),
                ),
                None => receive_session(
                    &mut receiver,
                    &mut output,
                    sides.0,
                    sides.1,
                    sqlite,
                    None,
                    (kind, source_mode),
                ),
            }
        };
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        drop(receiver);
        let served = producer.join().map_err(|_| BulkloadRefusal::WorkerLost)?;
        match (result, served) {
            // The destination refused for a reason of its own (its setup,
            // which now runs while the source opens its store, item 1b): the
            // source's broken stream is only the echo of it, whatever the
            // destination's code (an IO with its errno included). A
            // destination that lost its stream is the echo of the source's
            // refusal.
            (Err(refusal), Err(BulkloadRefusal::TransportLost)) => Err(refusal),
            (Err(refusal), Err(_))
                if !matches!(
                    refusal,
                    BulkloadRefusal::Io(_) | BulkloadRefusal::TransportLost
                ) =>
            {
                Err(refusal)
            }
            (result, served) => {
                served?;
                result
            }
        }
    })
}

/// Turns off the loopback hand-off of a local `copy` when `0`.
///
/// For the bench's A/B (OI-1003-Q154 levers 1 and 2): every chunk then
/// crosses the socket and is hashed by both halves. Unset, or anything
/// else, keeps it on. It never turns verification off anywhere: a session
/// without the hand-off verifies every byte.
pub const LOOPBACK_HANDOFF_ENV: &str = "BULKLOAD_LOOPBACK_HANDOFF";

/// Bytes a loopback half reads from the socket at once (lever 2).
const LOOPBACK_READ_BYTES: usize = 64 * 1024;

/// Whether a `copy` into the canonical destination `root` joins its halves
/// by the hand-off: on, unless [`LOOPBACK_HANDOFF_ENV`] is `0` or a test
/// turned it off for `root`.
fn loopback_handoff(root: &Path) -> bool {
    let found = LOOPBACK_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map(|(_, on)| *on);
    found.unwrap_or_else(|| std::env::var(LOOPBACK_HANDOFF_ENV).map_or(true, |value| value != "0"))
}

/// The loopback hand-off by canonical destination root, for tests and
/// benches that compare both paths in one process.
static LOOPBACK_OVERRIDE: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

/// Run `copy`s into the canonical destination `root` with the loopback
/// hand-off on or off (`None`: the default again). Test and bench hook;
/// off only ever adds verification.
#[doc(hidden)]
pub fn set_loopback_handoff(root: &Path, on: Option<bool>) {
    let mut overrides = LOOPBACK_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(on) = on {
        overrides.push((root.to_path_buf(), on));
    }
}

/// Turns on the delta lever of a local `copy` when `1` (OI-1003-Q154).
///
/// A seat new to a destination whose published outputs hold chunks is then
/// streamed instead of asked for by manifest first, as `pull` asks for it
/// (`Inbound::wants_manifest`). Unset, or anything else, keeps it off.
///
/// Off by default: with it on, a new seat's bytes are handed over whole in
/// place of a local fill, so `bytes_received` no longer meets S3's
/// inequality 2 (WP0(c), OI-1003-Q18) for added seats as
/// `tests/s3_transfer_resume.rs` proves it on `copy`. Whether handed bytes
/// are wire bytes there is the operator's to rule (docs/slo.md, the
/// 2026-10-16 amendment); until then the lever is for the bench's A/B. It
/// acts only with the loopback hand-off ([`LOOPBACK_HANDOFF_ENV`]): with
/// the hand-off off, a `copy` decides every entry as `pull` does. Either
/// way the source reads each seat at most once and every byte placed is
/// verified (handed chunks by the source's own digest).
pub const LOCAL_NEW_PATH_STREAM_ENV: &str = "BULKLOAD_LOCAL_NEW_PATH_STREAM";

/// The prefix extension's scope variable (M2r2 A-103).
///
/// `0` turns it off, `all` extends any regular file that qualifies, and
/// anything else (the default)
/// only an append-only one, a `.jsonl` or `.txt` (as `estate-reseat`
/// extends). A file the extension does not try is refused
/// `DESTINATION_OCCUPIED`, as before.
pub const PREFIX_EXTEND_ENV: &str = "BULKLOAD_PREFIX_EXTEND";

/// Which existing files with no owning record a session may extend in
/// place when they hold a verified byte prefix of their seat (M2r2 A-103).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixExtend {
    /// None: every such file is refused `DESTINATION_OCCUPIED`.
    Off,
    /// Append-only files only: a path ending `.jsonl` or `.txt` (default).
    AppendOnly,
    /// Any regular file (`BULKLOAD_PREFIX_EXTEND=all`).
    All,
}

impl PrefixExtend {
    /// Whether this scope covers the path `rel_path`.
    fn covers(self, rel_path: &[u8]) -> bool {
        match self {
            Self::Off => false,
            Self::AppendOnly => rel_path.ends_with(b".jsonl") || rel_path.ends_with(b".txt"),
            Self::All => true,
        }
    }
}

/// The prefix extension by canonical destination root, for tests that
/// compare scopes in one process.
static PREFIX_EXTEND_OVERRIDE: Mutex<Vec<(PathBuf, PrefixExtend)>> = Mutex::new(Vec::new());

/// Run sessions into the canonical destination `root` with the prefix
/// extension at `scope` (`None`: the default again). Test hook.
#[doc(hidden)]
pub fn set_prefix_extend(root: &Path, scope: Option<PrefixExtend>) {
    let mut overrides = PREFIX_EXTEND_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(scope) = scope {
        overrides.push((root.to_path_buf(), scope));
    }
}

/// The prefix extension's scope for the canonical destination `root`
/// ([`PREFIX_EXTEND_ENV`]).
fn prefix_extend_scope(root: &Path) -> PrefixExtend {
    PREFIX_EXTEND_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map_or_else(
            || match std::env::var(PREFIX_EXTEND_ENV).as_deref() {
                Ok("0") => PrefixExtend::Off,
                Ok("all") => PrefixExtend::All,
                _ => PrefixExtend::AppendOnly,
            },
            |(_, scope)| *scope,
        )
}

/// `--beside SUFFIX` on `pull` and `copy` (plan of record 2026-10-19, Step
/// C gaps C1 and C2; SPEC R2.2, C10: a lossless union that never clobbers
/// DEST). Without it an existing destination file bulkload did not write
/// and whose bytes are not the seat's is refused `DESTINATION_OCCUPIED` and
/// nothing is placed. With it the staged bytes are published at
/// `<path><SUFFIX>` instead, the live file untouched, and the entry is
/// accounted `applied-beside` in the session's transfer record, so
/// `closure-report --transfer` counts it as carried, not pending review.
/// A byte-identical file is still adopted, a verified byte prefix is still
/// extended ([`PREFIX_EXTEND_ENV`]), and a symlink that differs is created
/// beside too. A database with a live `-wal`, `-journal` or `-shm` beside
/// it (which no snapshot may land on, OI-1003-Q146) lands its snapshot at
/// `<db><SUFFIX>` under the same flag. A beside path that holds something
/// of its own is refused `DESTINATION_OCCUPIED` as before: nothing is ever
/// replaced but this store's own earlier beside output, by the exchange
/// that supersedes any output of this store (WP0(d)). A later pass reuses
/// or adopts the beside output under its own row, so an unchanged seat
/// reads nothing (S3).
///
/// The suffix is appended verbatim: `.neo-20261019` gives
/// `settings.json.neo-20261019`. It is one name component (not empty, no
/// `/`, no NUL, not `.` or `..`).
static BESIDE_SUFFIX: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// The beside suffix by canonical destination root, for tests that run
/// sessions with and without it in one process.
static BESIDE_OVERRIDE: Mutex<Vec<(PathBuf, Option<Vec<u8>>)>> = Mutex::new(Vec::new());

/// Whether `suffix` may be a `--beside` suffix: one name component.
#[must_use]
pub fn beside_suffix_allowed(suffix: &[u8]) -> bool {
    !suffix.is_empty()
        && !suffix.contains(&b'/')
        && !suffix.contains(&0)
        && suffix != b"."
        && suffix != b".."
}

/// Run every session of this process with `--beside SUFFIX` (`None`: off).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for a suffix [`beside_suffix_allowed`] refuses.
pub fn set_beside_suffix(suffix: Option<Vec<u8>>) -> Result<()> {
    if let Some(suffix) = &suffix {
        if !beside_suffix_allowed(suffix) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
    }
    *BESIDE_SUFFIX.lock().unwrap_or_else(PoisonError::into_inner) = suffix;
    Ok(())
}

/// Run sessions into the canonical destination `root` with `--beside` at
/// `suffix` (`Some(None)`: off for this root; `None`: the process setting
/// again). Test hook.
#[doc(hidden)]
pub fn set_beside(root: &Path, suffix: Option<Option<Vec<u8>>>) {
    let mut overrides = BESIDE_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(suffix) = suffix {
        overrides.push((root.to_path_buf(), suffix));
    }
}

/// The `--beside` suffix sessions into the canonical destination `root`
/// run with.
fn beside_suffix(root: &Path) -> Option<Vec<u8>> {
    BESIDE_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map_or_else(
            || {
                BESIDE_SUFFIX
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            },
            |(_, suffix)| suffix.clone(),
        )
}

/// Seats that land beside under `--beside` whatever their live path holds,
/// absent included (`estate-move`'s Codex index, review of #280 finding 6:
/// the live index is never written by a pull, and the snapshot a pass
/// takes is always at `state_5.sqlite<suffix>`). Relative paths.
static BESIDE_ALWAYS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// Land the seats at `paths` beside under `--beside` whatever their live
/// path holds (an empty list: none).
pub fn set_beside_always(paths: Vec<Vec<u8>>) {
    *BESIDE_ALWAYS.lock().unwrap_or_else(PoisonError::into_inner) = paths;
}

fn beside_always() -> HashSet<Vec<u8>> {
    BESIDE_ALWAYS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

/// The prefixes of the seats a session lists ([`TransferStats::listed`]).
static LISTING: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// The paths sessions into a destination root never land
/// ([`set_excluded`]), by canonical destination root.
static EXCLUDED: Mutex<Vec<(PathBuf, Vec<Vec<u8>>)>> = Mutex::new(Vec::new());

/// Never land `paths` in a session into the destination root `root`.
///
/// Each is relative to `root`, a seat or a whole subtree: the destination
/// answers each such entry `Skip` before anything is made or asked for, so
/// the source reads none of its bytes (the streamed capture: a repository's
/// `objects`, its rebuildable directories and its nested repositories,
/// which travel otherwise or not at all). `None` clears `root`'s list.
pub fn set_excluded(root: &Path, paths: Option<Vec<Vec<u8>>>) {
    let mut all = EXCLUDED.lock().unwrap_or_else(PoisonError::into_inner);
    all.retain(|(at, _)| at != root);
    if let Some(paths) = paths {
        all.push((root.to_path_buf(), paths));
    }
}

fn excluded(root: &Path) -> Vec<Vec<u8>> {
    EXCLUDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map(|(_, paths)| paths.clone())
        .unwrap_or_default()
}

/// Whether `rel_path` is one of `excluded` or beneath one.
fn is_excluded(excluded: &[Vec<u8>], rel_path: &[u8]) -> bool {
    excluded.iter().any(|path| {
        rel_path
            .strip_prefix(path.as_slice())
            .is_some_and(|rest| rest.is_empty() || rest.first() == Some(&b'/'))
    })
}

/// List the source's regular and symlink seats under `prefixes` in every
/// session of this process (an empty list: none).
pub fn set_listing(prefixes: Vec<Vec<u8>>) {
    *LISTING.lock().unwrap_or_else(PoisonError::into_inner) = prefixes;
}

fn listing() -> Vec<Vec<u8>> {
    LISTING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// `rel_path` with the beside `suffix` appended.
#[must_use]
pub fn beside_path(rel_path: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut beside = Vec::with_capacity(rel_path.len() + suffix.len());
    beside.extend_from_slice(rel_path);
    beside.extend_from_slice(suffix);
    beside
}

/// `row` at its beside path: the same seat, named `<path><suffix>`.
fn beside_row(row: &RowSchema, suffix: &[u8]) -> RowSchema {
    RowSchema {
        rel_path: beside_path(&row.rel_path, suffix),
        ..row.clone()
    }
}

use crate::transfer_store::BESIDE_KEY_MARK;

/// The row key of a seat's beside output from its live row key: the keys
/// of one path share the path's prefix ([`transfer_store::path_prefix`]),
/// so the beside key is the same key under the beside path, then
/// [`BESIDE_KEY_MARK`]. Holds for the row key and the snapshot key alike;
/// the row still decodes from the front.
fn beside_key(authority: &[u8], key: &[u8], rel_path: &[u8], beside: &[u8]) -> Result<Vec<u8>> {
    let live = crate::transfer_store::path_prefix(authority, rel_path)?;
    let rest = key
        .strip_prefix(live.as_slice())
        .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
    let mut out = crate::transfer_store::path_prefix(authority, beside)?;
    out.extend_from_slice(rest);
    out.extend_from_slice(BESIDE_KEY_MARK);
    Ok(out)
}

/// The delta lever by canonical destination root, for tests that compare
/// both arms in one process.
static LOCAL_NEW_PATH_STREAM_OVERRIDE: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

/// Run `copy`s into the canonical destination `root` with the delta lever
/// on or off (`None`: the default again). Test and bench hook.
#[doc(hidden)]
pub fn set_local_new_path_stream(root: &Path, on: Option<bool>) {
    let mut overrides = LOCAL_NEW_PATH_STREAM_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(on) = on {
        overrides.push((root.to_path_buf(), on));
    }
}

/// Whether a `copy` with the hand-off into the canonical destination
/// `root` streams seats new to it: off, unless [`LOCAL_NEW_PATH_STREAM_ENV`]
/// is `1` or a test turned it on for `root`.
fn local_new_path_stream(root: &Path) -> bool {
    let found = LOCAL_NEW_PATH_STREAM_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map(|(_, on)| *on);
    found
        .unwrap_or_else(|| std::env::var(LOCAL_NEW_PATH_STREAM_ENV).is_ok_and(|value| value == "1"))
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
    chunks: Vec<SourceChunk>,
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
    /// One chunk, its credit already taken. Its header's digest is the
    /// chunk's own ([`SourceChunk::digest`]).
    Chunk {
        header: DataHeader,
        chunk: SourceChunk,
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

/// A test-only hook run with a seat's relative path before its capture
/// opens it.
#[cfg(test)]
type OpenHook = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Test-only hooks by canonical source root, run before a capture opens a
/// walked seat (S4, TIN-6592): where a writer can still remove it.
#[cfg(test)]
static BEFORE_OPEN: Mutex<Vec<(PathBuf, OpenHook)>> = Mutex::new(Vec::new());

/// Set (or, with `None`, clear) the before-open hook of a canonical source
/// root.
#[cfg(test)]
fn set_before_open(root: &Path, hook: Option<OpenHook>) {
    let mut hooks = BEFORE_OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    hooks.retain(|(hooked_root, _)| hooked_root != root);
    if let Some(hook) = hook {
        hooks.push((root.to_path_buf(), hook));
    }
}

#[cfg(test)]
fn before_open(root: &Path, rel_path: &[u8]) {
    let hook = BEFORE_OPEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(hooked_root, _)| hooked_root == root)
        .map(|(_, hook)| Arc::clone(hook));
    if let Some(hook) = hook {
        hook(rel_path);
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
/// Every chunk goes out as a wire v6 data frame and every frame is flushed
/// as it is written: `serve` never holds the loopback hand-off, which only
/// a local `copy` makes for its own in-process source half.
///
/// # Errors
/// Refuses malformed frames, a peer on another wire, unsafe state roots,
/// source failures and broken I/O.
pub fn serve<R: Read + Send + 'static, W: Write>(input: R, output: &mut W) -> Result<()> {
    serve_session(input, output, None, RootKind::Tree)
}

/// [`serve`] of a root of `kind` (#232).
///
/// `serve --file` serves the one regular file the session's `Open` names,
/// beneath its parent directory, and refuses any other kind of root
/// `SOURCE_ROOT_KIND_MISMATCH`.
///
/// # Errors
/// As [`serve`], plus the root refusals (`SOURCE_ROOT_*`).
pub fn serve_kind<R: Read + Send + 'static, W: Write>(
    input: R,
    output: &mut W,
    kind: RootKind,
) -> Result<()> {
    serve_session(input, output, None, kind)
}

/// [`serve`], with the loopback hand-off when `copy` runs it
/// (OI-1003-Q154 levers 1 and 2): chunks are handed over, not written.
#[allow(clippy::too_many_lines)] // Open, the stores, the threads' scope, SourceDone.
fn serve_session<R: Read + Send + 'static, W: Write>(
    mut input: R,
    output: &mut W,
    hand: Option<HandOut>,
    kind: RootKind,
) -> Result<()> {
    let (root, state, sqlite, as_root) = read_open(&mut input, kind)?;
    // S2 (WP1 PR 3): refuse before `Store::open` creates the state root, so
    // an overlapping state never writes a byte inside the source.
    if overlaps(&canonical_state(&state)?, &root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    // #276: every directory above the state root, when absent; after the
    // check above, so none is ever made inside the source.
    ensure_state_parents(&state)?;
    let store = Store::open(&state)?;
    // Again on the store's own canonical root: a state swapped for a symlink
    // after the check above still refuses before any source read.
    if overlaps(store.root(), &root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    // Every source seat is walked and read beneath this one descriptor
    // (W4 PR 3); its identity is the root's in the authority. For a
    // `--file` root the descriptor is the file's parent (#232).
    let opened = open_source_root(&root, kind)?;
    let root_fd = opened.fd;
    let meta = crate::io::sys::fstat(&root_fd).refuse_at("transfer::serve")?;
    // The authority names the directory walked, with its identity: for a
    // `--file` root its parent, so a later tree session of that parent with
    // the same states keys the file's rows alike and reuses them (#283
    // review 4).
    let authority = postcard::to_stdvec(&(
        store.authority()?,
        opened.dir.as_os_str().as_bytes(),
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
    let walker = match &opened.seat {
        None => Walker::new(root_fd.as_fd(), true)?,
        Some(seat) => Walker::single_seat(root_fd.as_fd(), seat, true)?,
    }
    .with_sqlite(sqlite == SqliteMode::Snapshot);
    let slots = open_slots(store.root(), sqlite)?;
    let backup = Mutex::new(());
    let credit = Arc::new(Credit::new());
    let gate = WalkGate::new();
    let (events_sender, events) = std::sync::mpsc::channel();
    spawn_reader(input, events_sender.clone(), Arc::clone(&credit))?;
    let work = SourceWork {
        root: &opened.dir,
        root_fd: root_fd.as_fd(),
        authority: &authority,
        state: store.root(),
        credit: &credit,
        retain: Arc::new(AtomicU64::new(retain_budget(&opened.dir))),
        ledger,
        sqlite,
        euid: source_euid(&opened.dir),
        as_root,
        backup: &backup,
        slots: &slots,
        slot_budget: Arc::new(SlotBudget::new(slot_budget(&opened.dir))),
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
        let sent = match hand {
            // Lever 2: frames batched until the sending thread waits.
            Some(hand) => send_entries(
                &mut Corked::new(&mut *output),
                Some(hand),
                &committer,
                &events,
                jobs,
                &gate,
            ),
            None => send_entries(
                &mut Plain(&mut *output),
                None,
                &committer,
                &events,
                jobs,
                &gate,
            ),
        };
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
fn read_open<R: Read>(
    input: &mut R,
    kind: RootKind,
) -> Result<(PathBuf, PathBuf, SqliteMode, bool)> {
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
    // #232: a root that is not there, or cannot be resolved, is typed. A
    // `--file` root resolves only its parent: the leaf is the name as
    // given, never followed (a symlink there is carried as a symlink row,
    // never as its target's bytes, R1), and never stat'ed before `Start`,
    // so its own refusal crosses the wire typed (#283 review 1 and 2).
    let given = path(root);
    let root = match kind {
        RootKind::Tree => std::fs::canonicalize(&given)
            .map_err(|error| crate::refuse::source_root(&error, "transfer::read_open"))?,
        RootKind::File => {
            let leaf = given
                .file_name()
                .ok_or(BulkloadRefusal::SourceRootKindMismatch)?;
            std::fs::canonicalize(parent_or_dot(&given))
                .map_err(|error| crate::refuse::source_root(&error, "transfer::read_open"))?
                .join(leaf)
        }
    };
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

/// A writer the sending thread empties before it blocks.
///
/// Over a transport every frame is flushed as it is written ([`Plain`]), as
/// before. In a loopback `copy` with the hand-off (OI-1003-Q154 lever 2)
/// the frames are batched ([`Corked`]): written to the socket only when the
/// sending thread is about to wait for its next event, or a buffer fills.
/// The sending thread waits on nothing but its events (the destination's
/// frames among them), so every frame the destination could wait for is on
/// the socket before the source waits.
trait Uncork: Write {
    /// Write out every frame held back.
    ///
    /// # Errors
    /// The transport's failure.
    fn uncork(&mut self) -> Result<()>;
}

impl<W: Uncork + ?Sized> Uncork for &mut W {
    fn uncork(&mut self) -> Result<()> {
        (**self).uncork()
    }
}

/// Frames written, and flushed, one at a time: every transport's writer.
struct Plain<'a, W>(&'a mut W);

impl<W: Write> Write for Plain<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.write(data)
    }

    fn write_vectored(&mut self, data: &[IoSlice<'_>]) -> std::io::Result<usize> {
        self.0.write_vectored(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write> Uncork for Plain<'_, W> {
    fn uncork(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Bytes a loopback sending thread batches before it writes them.
const CORK_BYTES: usize = 64 * 1024;

/// A loopback `copy`'s batched sending side (lever 2): a frame's own
/// `flush` holds it back; [`Uncork::uncork`] writes them all out.
struct Corked<'a, W: Write>(std::io::BufWriter<&'a mut W>);

impl<'a, W: Write> Corked<'a, W> {
    fn new(output: &'a mut W) -> Self {
        Self(std::io::BufWriter::with_capacity(CORK_BYTES, output))
    }
}

impl<W: Write> Write for Corked<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.write(data)
    }

    fn write_vectored(&mut self, data: &[IoSlice<'_>]) -> std::io::Result<usize> {
        self.0.write_vectored(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: Write> Uncork for Corked<'_, W> {
    fn uncork(&mut self) -> Result<()> {
        self.0
            .flush()
            .map_err(|error| crate::refuse::transport(&error, "transfer::uncork"))
    }
}

/// The sending thread's writes to the wire, timed (`send_write_ns`, #217
/// review): where the source's one serial sending stage blocks on the wire.
struct TimedWrite<W>(W);

impl<W: Uncork> Uncork for TimedWrite<W> {
    fn uncork(&mut self) -> Result<()> {
        let _timer = PhaseTimer(&SEND_WRITE_NS, Instant::now());
        self.0.uncork()
    }
}

impl<W: Write> Write for TimedWrite<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _timer = PhaseTimer(&SEND_WRITE_NS, Instant::now());
        self.0.write(data)
    }

    fn write_vectored(&mut self, data: &[IoSlice<'_>]) -> std::io::Result<usize> {
        let _timer = PhaseTimer(&SEND_WRITE_NS, Instant::now());
        self.0.write_vectored(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _timer = PhaseTimer(&SEND_WRITE_NS, Instant::now());
        self.0.flush()
    }
}

/// The sending thread's view of the session.
struct Outbound<'a, W> {
    output: &'a mut W,
    /// The loopback hand-off, in a local `copy` only (OI-1003-Q154).
    hand: Option<HandOut>,
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
fn send_entries<W: Uncork>(
    output: &mut W,
    hand: Option<HandOut>,
    committer: &Committer<LedgerSink>,
    events: &Receiver<Event>,
    jobs: Sender<Job>,
    gate: &WalkGate,
) -> Result<(u64, u64)> {
    let mut output = TimedWrite(output);
    let mut outbound = Outbound {
        output: &mut output,
        hand,
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
            outbound.output.uncork()?;
            return Ok((outbound.bytes_read, outbound.entries.len() as u64));
        }
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // About to wait: nothing the destination may need stays
                // held back (lever 2's batching; a no-op over a transport).
                outbound.output.uncork()?;
                let waited = Instant::now();
                let event = events.recv().map_err(|_| BulkloadRefusal::WorkerLost)?;
                SEND_WAIT_NS.fetch_add(elapsed_ns(waited), Ordering::Relaxed);
                event
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(BulkloadRefusal::WorkerLost);
            }
        };
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
            Event::Chunk { header, chunk } => self.chunk(&header, chunk)?,
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

    /// Send one chunk: as a data frame over a transport, or, in a loopback
    /// `copy`, handed over with only its header on the socket (OI-1003-Q154
    /// lever 2). The chunk is in the hand-off queue before its frame is
    /// written, so the destination finds it when it reads the frame.
    fn chunk(&mut self, header: &DataHeader, chunk: SourceChunk) -> Result<()> {
        let Some(hand) = &self.hand else {
            return write_data(self.output, header, chunk.bytes());
        };
        let prefix = handoff::handed_prefix(header)?;
        hand.give(chunk)?;
        self.output
            .write_all(&prefix)
            .refuse_at("transfer::handed_frame")?;
        self.output.flush().refuse_at("transfer::handed_frame")?;
        counters::bump(Counter::WireFramesSent);
        counters::add_len(Counter::WireBytesSent, prefix.len());
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
    let sink = |index, offset, chunk| send_chunk(work, events, entry, index, offset, chunk);
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
    chunk: SourceChunk,
) -> Result<()> {
    let size = u32::try_from(chunk.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    work.credit.acquire(u64::from(size))?;
    events
        .send(Event::Chunk {
            header: DataHeader {
                entry,
                index,
                size,
                offset,
                digest: *chunk.digest(),
            },
            chunk,
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

/// Whether a source ledger row's manifest can be offered for `row` without
/// a read: it covers exactly the row's size within the chunk bound. The
/// source offers it ([`manifest_capture`]), and the delta lever keeps such
/// a seat on its manifest (`Inbound::wants_manifest`).
fn ledger_manifest_fits(manifest: &Manifest, row: &RowSchema) -> bool {
    manifest.size() == Some(row.size) && manifest.chunks.len() <= MAX_MANIFEST_CHUNKS
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
        if ledger_manifest_fits(&manifest, row) {
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
    let captured = capture_file(work, row, wal, bytes_read, sniffed, |_, _, chunk| {
        retained.chunks.push(chunk);
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
        let chunk = if let Some(held) = held {
            held.chunks
                .get(at)
                .filter(|chunk| *chunk.digest() == spec.digest)
                .ok_or(BulkloadRefusal::ProtocolStateViolation)?
                .clone()
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
            SourceChunk::checked(data, &spec.digest)
                .ok_or(BulkloadRefusal::SourceChangedAfterSnapshot)?
        };
        send_chunk(work, events, entry, *index, offset, chunk)?;
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
fn slot_chunk(slot: &SlotFile, spec: &ChunkSpec, offset: u64) -> Result<SourceChunk> {
    let mut data =
        vec![0_u8; usize::try_from(spec.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?];
    let read = crate::io::sys::pread_full(&slot.file, &mut data, offset)
        .refuse_at("transfer::slot_chunk")?;
    counters::add_len(Counter::SourceSnapshotRead, read);
    if read != data.len() {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    SourceChunk::checked(data, &spec.digest).ok_or(BulkloadRefusal::DigestMismatch)
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
    mut sink: impl FnMut(u32, u64, SourceChunk) -> Result<()>,
) -> Result<Captured<'w>> {
    if row.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    // Every -wal, -shm and -journal is refused by name, except a Git ref or
    // reflog so named, decided by content (M2r2).
    if let Some(suffix) = crate::walk::SQLITE_SIDECAR_SUFFIXES
        .iter()
        .find(|suffix| row.rel_path.ends_with(suffix))
    {
        let base = row
            .rel_path
            .get(..row.rel_path.len() - suffix.len())
            .unwrap_or_default();
        let is_ref = crate::walk::ref_storage_candidate(&row.rel_path)
            && crate::walk::ref_named_like_sidecar(
                work.root_fd,
                crate::walk::rel_path(&row.rel_path),
                crate::walk::rel_path(base),
            );
        if !is_ref {
            return Err(BulkloadRefusal::SqliteStateChanged);
        }
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
        let chunk = SourceChunk::hashed(chunk.data);
        let size = chunk.len() as u64;
        let index = u32::try_from(chunks.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        chunks.push(ChunkSpec {
            digest: *chunk.digest(),
            size,
        });
        sink(index, offset, chunk)?;
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
            |_, _, _| Ok(()),
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
    let chunks = chunk_slot(&slot, taken.size, |index, offset, chunk| {
        send_chunk(work, events, entry, index, offset, chunk)
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
    mut sink: impl FnMut(u32, u64, SourceChunk) -> Result<()>,
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
        let chunk = SourceChunk::hashed(chunk.data);
        let length = chunk.len() as u64;
        let index = u32::try_from(chunks.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        chunks.push(ChunkSpec {
            digest: *chunk.digest(),
            size: length,
        });
        sink(index, offset, chunk)?;
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
                // S4 (TIN-6592): a directory removed since the walk.
                .map_err(|error| crate::refuse::source_seat(&error, "transfer::seat_directory"))?,
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
    match crate::space::bound(
        bound,
        bound,
        crate::space::probe_limits(work.slots)?,
        crate::space::min_free_percent(),
    ) {
        None => Ok(()),
        Some(_) => Err(BulkloadRefusal::BudgetExceeded),
    }
}

/// Open a source seat for reading, component by component beneath the
/// source root descriptor (W4 PR 3): a symlink at any component is refused.
// S4 (TIN-6592): a walked seat gone before its capture opens it is
// `SOURCE_VANISHED`, never a bare IO: on the wire a refusal is its code
// alone, so an errno never reaches the destination's record.
fn open_source(work: &SourceWork<'_>, row: &RowSchema) -> Result<std::fs::File> {
    #[cfg(test)]
    before_open(work.root, &row.rel_path);
    let fd = crate::io::sys::openat_beneath(
        work.root_fd,
        crate::walk::rel_path(&row.rel_path),
        crate::io::OpenMode::Read,
    )
    .map_err(|error| crate::refuse::source_seat(&error, "transfer::open_source"))?;
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
    (sidecars, snapshot_bases): (&[SidecarReport], &HashSet<Vec<u8>>),
    beside_of: &HashMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    for (rel_path, outcome) in committer.finish()? {
        // A publication under a beside path (`--beside`) is the source
        // path's outcome: accounted `applied-beside` once committed, and
        // refused under the source's name otherwise.
        match (beside_of.get(&rel_path), outcome) {
            (Some(source), Ok(())) => {
                stats.completed += 1;
                stats.applied_beside.push((source.clone(), rel_path));
            }
            (Some(source), Err(refusal)) => stats.refuse_here(source.clone(), &refusal),
            (None, Ok(())) => stats.completed += 1,
            (None, Err(refusal)) => stats.refuse_here(rel_path, &refusal),
        }
    }
    stats.applied_beside.sort();
    stats.applied_beside.dedup();
    counters::add(
        Counter::DestAppliedBeside,
        stats.applied_beside.len() as u64,
    );
    resolve_sidecars(stats, sidecars, snapshot_bases);
    let keep = salvage_to_keep(target, stats, salvage_staged);
    target.retire_salvaged(&keep)?;
    stats.temporaries_removed = target.swept().removed;
    stats.temporaries_left.clone_from(&target.swept().left);
    stats.directories_renamed = target.created().renamed;
    stats
        .directories_fallback
        .clone_from(&target.created().fallback);
    stats
        .directories_mode_kept
        .clone_from(&target.created().mode_kept);
    stats.directories_folded = target.folded();
    // M2r2 A-103: an extension counts once its record committed; a path
    // refused since (its group commit, its capture record) did not.
    let refused: HashSet<&[u8]> = stats
        .refusals
        .iter()
        .map(|(path, _)| path.as_slice())
        .collect();
    let extended: Vec<_> = stats
        .prefix_extended
        .iter()
        .filter(|(path, ..)| !refused.contains(path.as_slice()))
        .cloned()
        .collect();
    counters::add(Counter::DestPrefixExtended, extended.len() as u64);
    stats.prefix_extended = extended;
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
///
/// Its groups wait [`DEST_GROUP_IDLE`] for the next output and commit at
/// least every [`DEST_GROUP_AGE`] (OI-1003-Q154 lever 3: fewer synced
/// commits). Every group still seals its files and directories before its
/// one FULL commit, and every `Held` still follows that commit, so a larger
/// group changes when outputs become durable, never the order (R25,
/// `BulkloadTransfer.tla` `Commit` over any nonempty set of ready seats).
/// The most a crash can leave unrecorded is still one group: at most its
/// file and byte bounds, which are unchanged.
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
            group_idle: dest_group_idle(),
            group_age: Some(DEST_GROUP_AGE),
            ..crate::io::durable::Limits::default()
        },
    )
}

/// How long a destination group waits for its next output before it
/// commits.
///
/// OI-1003-Q154 lever 3. A session's last group commits when its stream
/// drains (`Inbound::settle_held`), whatever is left of this.
/// With the 20 ms [`crate::io::durable::GROUP_IDLE`] before it, a local copy
/// of corpus v1 committed 10 to 18 destination groups on the rig. Expected
/// with this idle, not yet measured there: once or twice, if the rig lands
/// its outputs less than this apart, as sting's informational census did
/// (1 destination group commit against 3 to 5 at 20 ms, 2026-10-15, not of
/// record). The rig series decides it.
const DEST_GROUP_IDLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The longest a destination group stays open (OI-1003-Q154 lever 3): a
/// steady trickle of outputs, each within [`DEST_GROUP_IDLE`] of the last,
/// still commits, and is answered `Held`, at least this often.
const DEST_GROUP_AGE: std::time::Duration = std::time::Duration::from_secs(1);

/// Sets [`DEST_GROUP_IDLE`] in milliseconds for a process.
///
/// For the bench's A/B (OI-1003-Q154 lever 3): `20` is the idle before the
/// lever. Unset or malformed keeps the default. Any value is safe: it only
/// moves where groups end.
pub const DEST_GROUP_IDLE_ENV: &str = "BULKLOAD_DEST_GROUP_IDLE_MS";

fn dest_group_idle() -> std::time::Duration {
    std::env::var(DEST_GROUP_IDLE_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(DEST_GROUP_IDLE, std::time::Duration::from_millis)
}

/// Turns off the directory finish's fold into the session's last output
/// group when `0` (OI-1003-Q154 lever 3; the SOTA study's lever 2).
///
/// For the bench's A/B: off, every directory this session created is
/// finished after the last group as before, each sealed on its own and all
/// completed in a commit of their own (`directories_folded` stays 0). Unset,
/// or anything else, keeps the fold. Either way every completion follows
/// its directory's sealed final mode (`DirectoryRecords.tla`: `Complete`
/// or `GroupFinish`).
pub const DIRECTORY_FOLD_ENV: &str = "BULKLOAD_DIRECTORY_FOLD";

/// The directory fold by canonical destination root, for tests that compare
/// both paths in one process.
static DIRECTORY_FOLD_OVERRIDE: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

/// Run sessions into the canonical destination `root` with the directory
/// fold on or off (`None`: the default again). Test hook.
#[doc(hidden)]
pub fn set_directory_fold(root: &Path, on: Option<bool>) {
    let mut overrides = DIRECTORY_FOLD_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(on) = on {
        overrides.push((root.to_path_buf(), on));
    }
}

/// Whether a session into the canonical destination `root` may fold its
/// directory finish into its last group: on, unless [`DIRECTORY_FOLD_ENV`]
/// is `0` or a test turned it off for `root`.
fn directory_fold(root: &Path) -> bool {
    let found = DIRECTORY_FOLD_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map(|(_, on)| *on);
    found.unwrap_or_else(|| std::env::var(DIRECTORY_FOLD_ENV).map_or(true, |value| value != "0"))
}

/// Where in handling one chunk an entry's content failed. Within one chunk
/// index the order is fixed: its verify (or its size), then the stage of the
/// entry's file, then its write, then its write-back kick (#217 review), so
/// the refusal is the same whichever thread found which failure first.
/// [`Step::Seat`] precedes them all: a refusal decided before any content
/// arrived (a `SQLite` snapshot past the free-space floor or the chunk
/// bound, #218 R7) stands whatever its content then does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Seat,
    Verify,
    Stage,
    Place,
    Kick,
}

/// An entry's first content failure, by chunk index and [`Step`]. Today's
/// inline path refused with the failure it met first, and it met them in
/// index order; the receive workers keep that answer by keeping the least.
#[derive(Clone, Debug)]
struct Failure {
    index: u32,
    step: Step,
    refusal: BulkloadRefusal,
}

/// Keep the earlier of an entry's failure and `found`.
fn fold_failure(failure: &mut Option<Failure>, found: Failure) {
    if failure
        .as_ref()
        .is_none_or(|kept| (found.index, found.step) < (kept.index, kept.step))
    {
        *failure = Some(found);
    }
}

/// An entry's staged file as its chunk writes see it, shared by the jobs
/// that write it: the bytes placed so far, which time its write-back kicks
/// (item 1a), and whether a write has failed, after which none is tried.
struct WriteTarget {
    file: Arc<std::fs::File>,
    placed: AtomicU64,
    failed: std::sync::atomic::AtomicBool,
}

impl WriteTarget {
    fn new(file: &Arc<std::fs::File>, placed: u64) -> Arc<Self> {
        Arc::new(Self {
            file: Arc::clone(file),
            placed: AtomicU64::new(placed),
            failed: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

/// The bytes after which an entry's file is kicked into write-back.
#[cfg(not(any(test, feature = "io-trace")))]
const fn kick_bytes() -> u64 {
    WRITEBACK_KICK_BYTES
}

/// The kick interval; a traced or test build may set a smaller one.
#[cfg(any(test, feature = "io-trace"))]
fn kick_bytes() -> u64 {
    match KICK_BYTES_OVERRIDE.load(Ordering::Relaxed) {
        0 => WRITEBACK_KICK_BYTES,
        bytes => bytes,
    }
}

/// Kick interval in bytes for a traced or test build (0: the default).
#[cfg(any(test, feature = "io-trace"))]
static KICK_BYTES_OVERRIDE: AtomicU64 = AtomicU64::new(0);

/// Milliseconds each receive worker waits before its job, for a traced or
/// test build (0: none).
#[cfg(any(test, feature = "io-trace"))]
static RECV_WORKER_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// Make every receive worker wait `millis` before each job (`0`: none),
/// process-wide, so a trace shows whether `End` waits for its entry's
/// writes (S1 throughput review, 2026-10-09). Only in a traced or test
/// build.
#[cfg(any(test, feature = "io-trace"))]
#[doc(hidden)]
pub fn set_recv_worker_delay(millis: u64) {
    RECV_WORKER_DELAY_MS.store(millis, Ordering::Relaxed);
}

/// Kick every staged file into write-back each `bytes` placed (`0`: the
/// default, [`WRITEBACK_KICK_BYTES`]), process-wide, so a power-loss trace
/// of small files holds kicks (S1 throughput review, 2026-10-09). Only in a
/// traced or test build.
#[cfg(any(test, feature = "io-trace"))]
#[doc(hidden)]
pub fn set_writeback_kick_bytes(bytes: u64) {
    KICK_BYTES_OVERRIDE.store(bytes, Ordering::Relaxed);
}

/// Kick `file` into write-back when `placed` bytes, after `before`, crossed
/// a multiple of the kick interval (OI-1003-Q143 item 1a). An error the
/// kick returns is the entry's refusal, a full device its typed space
/// refusal; write-back errors it does not return are still reported by the
/// file's seal.
fn kick_after(file: &std::fs::File, before: u64, placed: u64) -> Result<()> {
    if !crosses(before, placed, kick_bytes()) {
        return Ok(());
    }
    crate::io::durable::kick_writeback(file).map_err(|error| {
        crate::materialize::space_refusal(crate::refuse::io(&error, "transfer::kick_after"))
    })
}

/// Whether `placed` bytes after `before` cross a multiple of `interval`.
const fn crosses(before: u64, placed: u64, interval: u64) -> bool {
    placed > 0 && interval > 0 && before / interval != before.saturating_add(placed) / interval
}

/// Write `data` at `offsets` of an entry's staged file and kick its
/// write-back when that crosses the interval.
fn place_counted(
    target: &WriteTarget,
    data: &[u8],
    offsets: &[u64],
) -> std::result::Result<(), (Step, BulkloadRefusal)> {
    if target.failed.load(Ordering::Relaxed) {
        return Ok(());
    }
    place(&target.file, data, offsets).map_err(|refusal| (Step::Place, refusal))?;
    let placed = (data.len() as u64).saturating_mul(offsets.len() as u64);
    let before = target.placed.fetch_add(placed, Ordering::Relaxed);
    kick_after(&target.file, before, placed).map_err(|refusal| (Step::Kick, refusal))
}

/// One data frame's verify and write, for a receive worker or inline.
struct RecvJob {
    entry: u64,
    index: u32,
    digest: [u8; 32],
    payload: Payload,
    /// False when the receiving thread already verified it (the chunk that
    /// staged its entry's file, #217 review), or when the in-process source
    /// vouches for it (a handed chunk, [`Payload::vouched`]: OI-1003-Q154
    /// lever 1).
    verify: bool,
    /// Where it is written, or `None` once its entry has failed or when it
    /// fills no offset.
    write: Option<(Arc<WriteTarget>, Vec<u64>)>,
    /// What it holds of [`RECV_POOL_BYTES`].
    charge: u64,
}

/// A finished [`RecvJob`]: its failure, if any, or a worker that panicked.
struct RecvDone {
    entry: u64,
    failure: Option<Failure>,
    charge: u64,
    lost: bool,
}

/// Verify one data frame against its digest (the one integrity check on
/// received bytes; a handed chunk's is the source's own, see
/// [`Payload::vouched`]), then write it. `on_worker` picks the timers.
fn run_recv_job(job: RecvJob, on_worker: bool) -> RecvDone {
    let RecvJob {
        entry,
        index,
        digest,
        payload,
        verify,
        write,
        charge,
    } = job;
    #[cfg(test)]
    #[allow(clippy::panic)]
    if on_worker
        && PANIC_DIGESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&digest)
    {
        panic!("test: a receive worker panics on its job");
    }
    #[cfg(any(test, feature = "io-trace"))]
    if on_worker {
        let delay = RECV_WORKER_DELAY_MS.load(Ordering::Relaxed);
        if delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay));
        }
    }
    let mut failure = None;
    if verify {
        let started = Instant::now();
        let verified = verify_wire(&payload, &digest);
        let timer = if on_worker {
            &RECV_WORKER_VERIFY_NS
        } else {
            &RECV_VERIFY_NS
        };
        timer.fetch_add(elapsed_ns(started), Ordering::Relaxed);
        if !verified {
            failure = Some(Failure {
                index,
                step: Step::Verify,
                refusal: BulkloadRefusal::DigestMismatch,
            });
        }
    }
    if let Some((target, offsets)) = &write {
        if failure.is_none() {
            let started = Instant::now();
            if let Err((step, refusal)) = place_counted(target, payload.bytes(), offsets) {
                failure = Some(Failure {
                    index,
                    step,
                    refusal,
                });
            }
            if on_worker {
                RECV_WORKER_PLACE_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
            }
        }
        if failure.is_some() {
            target.failed.store(true, Ordering::Relaxed);
        }
    }
    RecvDone {
        entry,
        failure,
        charge,
        lost: false,
    }
}

/// The destination's own check of received bytes: within the chunk bound
/// and hashing to `digest` (`blake3_dest_wire_verify_bytes`).
fn verify_wire(payload: &Payload, digest: &[u8; 32]) -> bool {
    payload.len() <= crate::hash::CDC_MAX_BYTES as usize
        && counters::hash(Counter::HashWireVerify, payload.bytes()) == *digest
}

/// One receive worker: run jobs until the queue closes. A job that panics
/// is answered as lost, so the receiving thread ends the session with
/// `WORKER_LOST` instead of waiting on it (#217 review).
fn recv_worker(jobs: &Mutex<Receiver<RecvJob>>, done: &Sender<RecvDone>) {
    loop {
        let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = job else {
            return;
        };
        let (entry, charge) = (job.entry, job.charge);
        let finished =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_recv_job(job, true)))
                .unwrap_or(RecvDone {
                    entry,
                    failure: None,
                    charge,
                    lost: true,
                });
        if done.send(finished).is_err() {
            return;
        }
    }
}

/// The receiving thread's side of the receive workers.
struct RecvPool {
    jobs: Sender<RecvJob>,
    done: Receiver<RecvDone>,
    /// Charged bytes and jobs queued or in progress.
    bytes: u64,
    queued: usize,
}

/// How many receive workers a session at `root` runs.
fn recv_workers(root: &Path) -> usize {
    let found = RECV_WORKERS_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map(|(_, workers)| *workers);
    if let Some(workers) = found {
        return workers.min(RECV_WORKERS_MAX);
    }
    if let Some(workers) = std::env::var(RECV_WORKERS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|workers| *workers <= RECV_WORKERS_MAX)
    {
        return workers;
    }
    let spare =
        std::thread::available_parallelism().map_or(1, |cores| cores.get().saturating_sub(1));
    RECV_WORKERS.min(spare)
}

/// Receive workers by canonical destination root, for tests and benches
/// that compare worker counts in one process (OI-1003-Q143 item 3).
static RECV_WORKERS_OVERRIDE: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

/// Run sessions into the canonical destination `root` with `workers`
/// receive workers (`None`: the default again). Test and bench hook.
#[doc(hidden)]
pub fn set_recv_workers(root: &Path, workers: Option<usize>) {
    let mut overrides = RECV_WORKERS_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(workers) = workers {
        overrides.push((root.to_path_buf(), workers));
    }
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
    /// The staged file as its chunk writes see it.
    target: Option<Arc<WriteTarget>>,
    specs: Vec<ChunkSpec>,
    offset: u64,
    hints: Vec<ChunkHint>,
    seen: HashSet<[u8; 32]>,
    failure: Option<Failure>,
    /// Chunk jobs not yet finished.
    outstanding: usize,
    /// Streamed in answer to `WantManifest` (the source could not keep the
    /// chunks for a manifest): an existing output at the path is adopted
    /// against the streamed chunks instead of refused.
    adopt: bool,
}

impl Streaming {
    /// One streamed chunk: it must be the next index at the next offset.
    /// Returns the job that verifies it and writes it once verified; a
    /// content fault becomes the entry's refusal and the stream stays in
    /// step.
    ///
    /// The chunk that would stage the entry's file is verified here first,
    /// with `verify`, so a corrupt first chunk stages nothing, as before
    /// the receive workers. A chunk the in-process source `vouched` for is
    /// not hashed again (OI-1003-Q154 lever 1).
    fn accept(
        &mut self,
        target: &Destination,
        open: &mut usize,
        header: &DataHeader,
        (payload, vouched): (Payload, bool),
        verify: impl FnOnce(&Payload) -> bool,
    ) -> Result<Option<RecvJob>> {
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
        let failed = |step, refusal| Failure {
            index: header.index,
            step,
            refusal,
        };
        if end > self.row.size {
            fold_failure(
                &mut self.failure,
                failed(Step::Verify, BulkloadRefusal::DigestMismatch),
            );
        }
        let mut job_verifies = !vouched;
        if self.failure.is_none() && self.staged.is_none() {
            job_verifies = false;
            if vouched || verify(&payload) {
                if *open >= MAX_OPEN_ENTRIES {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                match target.stage(&self.row) {
                    Ok(staged) => {
                        *open += 1;
                        self.target = Some(WriteTarget::new(staged.file(), 0));
                        self.staged = Some(staged);
                    }
                    Err(refusal) => fold_failure(&mut self.failure, failed(Step::Stage, refusal)),
                }
            } else {
                fold_failure(
                    &mut self.failure,
                    failed(Step::Verify, BulkloadRefusal::DigestMismatch),
                );
            }
        }
        let write = if self.failure.is_none() {
            self.target
                .as_ref()
                .map(|target| (Arc::clone(target), vec![self.offset]))
        } else {
            None
        };
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
        if !job_verifies && write.is_none() {
            return Ok(None);
        }
        self.outstanding += 1;
        Ok(Some(RecvJob {
            entry: header.entry,
            index: header.index,
            digest: header.digest,
            charge: size.max(RECV_JOB_MIN_BYTES),
            payload,
            verify: job_verifies,
            write,
        }))
    }

    fn new(seat: Seat, adopt: bool) -> Self {
        Self {
            row: seat.row,
            key: seat.key,
            record: seat.record,
            snapshot: seat.snapshot,
            staged: None,
            target: None,
            specs: Vec::new(),
            offset: 0,
            hints: Vec::new(),
            seen: HashSet::new(),
            failure: seat.failure.map(|refusal| Failure {
                index: 0,
                step: Step::Seat,
                refusal,
            }),
            outstanding: 0,
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
    /// The staged file of a [`Plan::Write`] as its chunk writes see it.
    target: Option<Arc<WriteTarget>>,
    expected: VecDeque<u32>,
    failure: Option<Failure>,
    /// Chunk jobs not yet finished.
    outstanding: usize,
}

/// How a session asks for a new path: a regular entry with no file at its
/// path, in a session that has salvaged no temporary
/// ([`Inbound::wants_manifest`]). Decided once, at the session's start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NewPath {
    /// No published output holds a chunk: nothing here could fill it, so
    /// `Send`.
    Stream,
    /// Published outputs hold chunks: `WantManifest`, to fill it across
    /// files (`pull`, `receive`, and `copy` with the hand-off off or without
    /// the delta lever, its default).
    Manifest,
    /// Published outputs hold chunks, but a local `copy` with the hand-off
    /// streams a seat this store holds no reuse row for and the source's
    /// ledger holds no capture of: `Send` (the delta lever, OI-1003-Q154;
    /// [`LOCAL_NEW_PATH_STREAM_ENV`]). A seat whose row is here, its output
    /// removed since, or whose capture the source recorded for another
    /// destination, still asks for its manifest.
    StreamLocal,
}

/// Count one `WantManifest` (`transfer_manifests_asked`) or one new path the
/// delta lever streamed (`transfer_new_paths_streamed`), and, in lib tests,
/// in this thread's `NEW_PATH_DECISIONS` too.
fn count_new_path(counter: Counter) {
    counters::bump(counter);
    #[cfg(test)]
    NEW_PATH_DECISIONS.with(|decided| {
        let (asked, streamed) = decided.get();
        decided.set(if counter == Counter::TransferManifestsAsked {
            (asked + 1, streamed)
        } else {
            (asked, streamed + 1)
        });
    });
}

/// The source half's ledger, read-only, as the delta lever consults it
/// (`Inbound::wants_manifest`). Only in a local `copy` with the hand-off and
/// the lever on: both halves are this process, and `source_state` names the
/// store the source half opened before it sent `Start`.
struct SourceLedger {
    store: Store,
    /// The source's authority from `Start`, which its ledger keys are made
    /// with ([`row_key`]).
    authority: Vec<u8>,
}

#[cfg(test)]
thread_local! {
    /// This thread's entries asked for by manifest first, and new paths
    /// the delta lever streamed, for lib tests: the process counters
    /// (`transfer_manifests_asked`, `transfer_new_paths_streamed`) are
    /// shared with the tests running beside them. A `copy` decides on its
    /// caller's thread.
    static NEW_PATH_DECISIONS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
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
    /// The loopback hand-off, in a local `copy` only (OI-1003-Q154).
    hand: Option<HandIn>,
    /// Test-only mutation: bytes read from the transport are taken as
    /// vouched for, as if a cross-host session skipped its verify. The
    /// proof that it does not must fail under it.
    #[cfg(test)]
    trust_wire: bool,
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
    /// How a new path is asked for ([`Inbound::wants_manifest`]).
    new_path: NewPath,
    /// The source's ledger, for [`NewPath::StreamLocal`] only.
    source_ledger: Option<SourceLedger>,
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
    space: Option<crate::space::Limits>,
    /// Bytes admitted since the last probe.
    since_probe: u64,
    /// The receive workers, or `None` to verify and write inline.
    pool: Option<RecvPool>,
    /// New directories waiting to be created together, and the walk frames
    /// held back behind them (OI-1003-Q143 item 2).
    batch: DirectoryBatch,
    /// A created batch's outcome for each of its directories, taken when
    /// the directory's held entry is decided.
    batched: HashMap<Vec<u8>, Result<()>>,
    /// The session's `SQLite` mode, which this side chose (#218).
    sqlite: SqliteMode,
    /// A `--file` session's one name (#232): the source's single row lands
    /// under it, in the destination directory, whatever the source called
    /// it. `None` for a tree.
    leaf: Option<Vec<u8>>,
    /// The source's `SqliteSidecar` reports, as (sidecar, base), resolved
    /// against their bases' outcomes when the session ends (R2).
    sidecars: Vec<SidecarReport>,
    /// Bases published from a snapshot, or reused or adopted as one.
    snapshot_bases: HashSet<Vec<u8>>,
    /// `--beside SUFFIX` for this session's root ([`beside_suffix`]).
    beside: Option<Vec<u8>>,
    /// Publications queued under a beside path, by that path, with the
    /// source path each stands for: the committer's outcomes name the
    /// beside path, the session's report names the source's.
    beside_of: HashMap<Vec<u8>, Vec<u8>>,
    /// `--beside` (review of #280, finding 4): the destination paths ending
    /// with the suffix that this session already decided to write, a seat's
    /// own live path or another seat's beside path. A second claim of one is
    /// that seat's `DESTINATION_OCCUPIED`, never two publications of one
    /// name. Only suffixed names are kept, so it stays small.
    claimed: HashSet<Vec<u8>>,
    /// `--beside`: seats landed beside whatever their live path holds
    /// ([`set_beside_always`]).
    always: HashSet<Vec<u8>>,
    /// `--beside` (second review of #280, finding 5): directories whose live
    /// path holds a symlink or another kind, landed beside, by live path:
    /// every entry under one is decided under its beside directory.
    redirects: HashMap<Vec<u8>, Vec<u8>>,
    /// The listing prefixes ([`set_listing`]).
    listing: Vec<Vec<u8>>,
    /// The paths never landed ([`set_excluded`]).
    excluded: Vec<Vec<u8>>,
}

/// A walk frame: what a batch of new directories holds back.
enum Walked {
    Entry {
        entry: u64,
        row: RowSchema,
        wal: Option<SidecarId>,
    },
    Refused {
        rel_path: Vec<u8>,
        code: String,
        sqlite_code: Option<i32>,
    },
    EngineTemporary {
        rel_path: Vec<u8>,
    },
}

/// A batch of new directories (OI-1003-Q143 item 2, R-N102).
///
/// The walk is depth-first, so sibling directories are not adjacent in the
/// stream: batching them defers decisions. A batch opens at a directory
/// whose leaf is free; from then on every walk frame is held back, in
/// stream order, and a directory joins the batch as a member when its leaf
/// is free and its parent is the root, a member, or a directory decided
/// before the batch opened. A directory whose parent is held but is no
/// member (an existing directory, whose sweep and adoption seal must come
/// first, #74 N1) closes the batch before it is looked at (#217 review), and
/// so does a directory whose path the batch already holds (a source that
/// offers it twice), so no two members share a key. The batch also closes
/// at `WalkDone`, at [`Destination::batch`] members
/// and at [`DIRECTORY_BATCH_FRAMES`] held frames: walk frames only, so its
/// bounds never depend on timing.
///
/// Closing creates the members level by level, shallowest first
/// ([`Destination::create_directories`]), then decides every held frame in
/// stream order as it would have been decided at once, a member by its
/// creation's outcome. A member whose parent member was not created is
/// decided alone then, against the file system, as without batching.
#[derive(Default)]
struct DirectoryBatch {
    held: Vec<Walked>,
    /// The held directory entries by path: a member, or held only.
    directories: HashMap<Vec<u8>, bool>,
    members: usize,
}

/// Test-only: destination roots whose batches admit a directory under a
/// held, undecided parent, the design the #217 review refuted (finding 1),
/// so a proof can show it fails.
#[cfg(test)]
pub(crate) static ADMIT_UNDER_HELD: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `path`'s parent directory: everything before its last `/`.
fn parent_of(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|byte| *byte == b'/')
        .and_then(|end| path.get(..end))
        .unwrap_or_default()
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
/// Every received byte is hashed here (`blake3_dest_wire_verify_bytes`):
/// `receive_with` never holds the loopback hand-off, and refuses a handed
/// frame `FRAME_CODEC`, as any wire v6 reader does.
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
    receive_session(
        input,
        output,
        (source, source_state),
        (destination, destination_state),
        sqlite,
        None,
        (RootKind::Tree, None),
    )
}

/// [`receive`] of a root of `kind` (#232), in the process's [`sqlite_mode`].
///
/// With [`RootKind::File`] the remote `serve --file` offers one regular
/// file, landed as `destination` (a file path) in its existing parent,
/// under `destination`'s own name.
///
/// # Errors
/// As [`receive`], plus the root refusals (`DESTINATION_*`,
/// `SOURCE_ROOT_KIND_MISMATCH` for a source that offers anything else).
pub fn receive_kind<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    source: &Path,
    source_state: &Path,
    destination: &Path,
    destination_state: &Path,
    kind: RootKind,
) -> Result<TransferStats> {
    receive_session(
        input,
        output,
        (source, source_state),
        (destination, destination_state),
        sqlite_mode(),
        None,
        (kind, None),
    )
}

/// [`receive_with`], with the loopback hand-off when `copy` runs it
/// (OI-1003-Q154 levers 1 and 2): handed chunks are taken from `hand`, and
/// the source's own digest of them stands for this side's verify.
#[allow(clippy::too_many_lines)] // Setup, the workers' scope, the frame loop, the tail.
fn receive_session<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    (source, source_state): (&Path, &Path),
    (destination, destination_state): (&Path, &Path),
    sqlite: SqliteMode,
    hand: Option<HandIn>,
    // The root's kind, and the source root's mode when this side knows it
    // (a local `copy`), which an absent DEST root takes.
    (kind, source_mode): (RootKind, Option<u32>),
) -> Result<TransferStats> {
    let setup_timer = PhaseTimer(&RECV_SETUP_NS, Instant::now());
    // S2: the overlap of the destination's state and root is refused before
    // either store exists, so `Open` can go out first and the source opens
    // its store while this side opens its own (OI-1003-Q143 item 1b). The
    // check on the store's own canonical root below still guards a state
    // swapped for a symlink in between.
    let landing = resolve_destination(destination, kind)?;
    let destination_root = landing.root();
    if overlaps(&canonical_state(destination_state)?, &destination_root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
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
    // The whole window is granted with `Open`, not after this side's setup:
    // a source with nothing to send may finish, and close the stream, while
    // this side still opens its store, so nothing may be written to it after
    // the source's last need (item 1b). The source's reader takes the grant
    // once its store is open.
    write_control(
        output,
        &Control::Credit {
            bytes: CREDIT_WINDOW,
        },
    )?;
    // #232: an absent tree root is made only once the source's `Start`
    // says its root opened as the kind asked for, so a session the source
    // refuses (a missing root, the wrong kind) leaves no empty DEST behind
    // (R4). It costs the first run into an absent root one round trip.
    // This side's store opens first, so its own failure makes nothing at
    // DEST (#283 review 6).
    let store = Store::open(destination_state)?;
    let early_start = if landing.create {
        let authority = read_start(input, hand.as_ref())?;
        make_destination_root(&landing.dir, source_mode.map_or(0o700, |mode| mode & 0o777))?;
        Some(authority)
    } else {
        None
    };
    // A root made here and still empty is removed again when the rest of
    // this side's setup refuses (`remove_dir` takes only an empty
    // directory, so nothing anyone wrote into it is touched).
    let made = landing.create.then_some(landing.dir.as_path());
    let unmade = |refusal: BulkloadRefusal| {
        if let Some(dir) = made {
            let _ = std::fs::remove_dir(dir);
        }
        refusal
    };
    let mut target = Destination::open(&landing.open, &store).map_err(unmade)?;
    if overlaps(store.root(), &destination_root) {
        return Err(unmade(BulkloadRefusal::SnapshotRootsOverlap));
    }
    let budget = crate::io::limits::descriptor_budget();
    let (notify, group_outcomes) = std::sync::mpsc::channel();
    let displaced = Displaced::shared(usize::try_from((budget / 8).clamp(1, 64)).unwrap_or(1));
    let committer =
        publication_committer(destination_state, budget, notify, Arc::clone(&displaced))
            .map_err(unmade)?;
    // The committer holds the exclusive publisher, so no temporary of this
    // store is in flight while the root is swept.
    target.sweep_root(&store).map_err(unmade)?;
    let mut new_path = match (
        store.has_output_hints()?,
        hand.is_some() && local_new_path_stream(target.path()),
    ) {
        (false, _) => NewPath::Stream,
        (true, false) => NewPath::Manifest,
        (true, true) => NewPath::StreamLocal,
    };
    let authority = match early_start {
        Some(authority) => authority,
        None => read_start(input, hand.as_ref())?,
    };
    // The delta lever reads the source's ledger, which the source half
    // opened before `Start`. A ledger this side cannot open leaves the
    // session on the store-wide rule, as with the lever off.
    let source_ledger = if new_path == NewPath::StreamLocal {
        let opened = Store::open_reader(source_state).ok();
        if opened.is_none() {
            new_path = NewPath::Manifest;
        }
        opened.map(|ledger| SourceLedger {
            store: ledger,
            authority: authority.clone(),
        })
    } else {
        None
    };
    let target_meta = std::fs::metadata(target.path()).refuse_at("transfer::receive")?;
    let output_authority = postcard::to_stdvec(&(
        &authority,
        target.path().as_os_str().as_bytes(),
        target_meta.dev(),
        target_meta.ino(),
    ))
    .refuse_at("transfer::receive")?;
    // The receive workers (item 3) live in this scope: a session that ends,
    // by `SourceDone` or by a refusal, closes their queue, and the scope
    // joins them before anything below runs.
    let workers = recv_workers(target.path());
    let (job_queue, jobs) = std::sync::mpsc::channel();
    let jobs = Mutex::new(jobs);
    let (done, finished) = std::sync::mpsc::channel();
    let streamed = std::thread::scope(|scope| -> Result<_> {
        for _ in 0..workers {
            let (jobs, done) = (&jobs, done.clone());
            std::thread::Builder::new()
                .name("bulkload-recv-worker".to_owned())
                .spawn_scoped(scope, move || recv_worker(jobs, &done))
                .refuse_at("transfer::receive")?;
        }
        drop(done);
        #[cfg(test)]
        let trust_wire = TRUST_WIRE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|root| root == target.path());
        let beside = beside_suffix(target.path());
        let excluded_paths = excluded(target.path());
        let mut receiver = Inbound {
            output,
            hand,
            #[cfg(test)]
            trust_wire,
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
            new_path,
            source_ledger,
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
            pool: (workers > 0).then_some(RecvPool {
                jobs: job_queue,
                done: finished,
                bytes: 0,
                queued: 0,
            }),
            batch: DirectoryBatch::default(),
            batched: HashMap::new(),
            sqlite,
            leaf: landing.leaf,
            sidecars: Vec::new(),
            snapshot_bases: HashSet::new(),
            beside,
            beside_of: HashMap::new(),
            claimed: HashSet::new(),
            always: beside_always(),
            redirects: HashMap::new(),
            listing: listing(),
            excluded: excluded_paths,
        };
        drop(setup_timer);
        let source_bytes_read = {
            let _stream_timer = PhaseTimer(&RECV_STREAM_NS, Instant::now());
            receiver.run(input)
        };
        let Inbound {
            stats,
            session,
            salvage_staged,
            pool,
            sidecars,
            snapshot_bases,
            beside_of,
            ..
        } = receiver;
        drop(pool);
        Ok((
            source_bytes_read?,
            stats,
            session,
            salvage_staged,
            sidecars,
            snapshot_bases,
            beside_of,
        ))
    });
    let (
        source_bytes_read,
        mut stats,
        session,
        salvage_staged,
        sidecars,
        snapshot_bases,
        beside_of,
    ) = streamed?;
    let _tail_timer = PhaseTimer(&RECV_TAIL_NS, Instant::now());
    stats.source_bytes_read = source_bytes_read;
    drop(session);
    finish_receive(
        &mut target,
        &store,
        committer,
        &mut stats,
        &salvage_staged,
        (&sidecars, &snapshot_bases),
        &beside_of,
    )?;
    Ok(stats)
}

impl<W: Write> Inbound<'_, W> {
    /// Handle the source's frames until `SourceDone`; returns the source
    /// bytes it reports. Entries are numbered in stream order, and the
    /// session ends only once every entry the destination expected content
    /// for has ended or been refused.
    #[allow(clippy::too_many_lines)] // One arm per frame kind.
    fn run<R: Read>(&mut self, input: &mut R) -> Result<u64> {
        let mut offered = 0_u64;
        let mut walk_done = None;
        // The stream's ramp: from here to the first data frame.
        let mut first_data = Some(Instant::now());
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
                read_wire(input, self.hand.as_ref())?
            };
            let frame = match frame {
                Wire::Frame(frame) => frame,
                // A chunk the in-process source handed over (lever 2).
                Wire::Handed { header, chunk } => {
                    if let Some(started) = first_data.take() {
                        RECV_FIRST_DATA_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
                    }
                    self.data(&header, Payload::Handed(chunk))?;
                    continue;
                }
            };
            match frame {
                Frame::Control(Control::Entry {
                    entry,
                    mut row,
                    wal,
                }) => {
                    if entry != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::ProtocolStateViolation);
                    }
                    self.land(&mut row.rel_path, Some((entry, row.kind)))?;
                    offered += 1;
                    self.walked(Walked::Entry { entry, row, wal })?;
                }
                Frame::Control(Control::SqliteSidecar { rel_path, database })
                    if self.sqlite == SqliteMode::Snapshot =>
                {
                    // A `--file` root has nothing beside it (#232).
                    if self.leaf.is_some() {
                        return Err(BulkloadRefusal::SourceRootKindMismatch);
                    }
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
                    mut rel_path,
                    code,
                    sqlite_code,
                }) => {
                    self.land(&mut rel_path, None)?;
                    self.walked(Walked::Refused {
                        rel_path,
                        code,
                        sqlite_code,
                    })?;
                }
                Frame::Control(Control::Refused {
                    entry: Some(entry),
                    mut rel_path,
                    code,
                    sqlite_code,
                }) => {
                    self.land(&mut rel_path, None)?;
                    self.refused(entry, &rel_path, code, sqlite_code)?;
                }
                Frame::Control(Control::EngineTemporary { mut rel_path }) => {
                    self.land(&mut rel_path, None)?;
                    self.walked(Walked::EngineTemporary { rel_path })?;
                }
                Frame::Control(Control::WalkDone { entries }) => {
                    if entries != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::ProtocolStateViolation);
                    }
                    self.close_batch()?;
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
                Frame::Data { header, payload } => {
                    if let Some(started) = first_data.take() {
                        RECV_FIRST_DATA_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
                    }
                    self.data(&header, Payload::Wire(payload))?;
                }
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
    /// A `--file` session (#232): the source offers one regular file or one
    /// symlink by its own leaf (or that leaf's typed refusal), and it lands
    /// under the destination's leaf, so every record this side keeps (its
    /// row, its reuse key, a refusal) is keyed by the path it lives at
    /// here, and a rerun finds it. Anything else offered (a second entry, a
    /// directory, a path beneath a directory) is not the file the verb was
    /// given. A tree session is untouched.
    fn land(&self, rel_path: &mut Vec<u8>, entry: Option<(u64, FileKind)>) -> Result<()> {
        let Some(leaf) = &self.leaf else {
            return Ok(());
        };
        let beneath = rel_path.contains(&b'/');
        // One regular file, or one symlink carried as a link (R1).
        let other = entry.is_some_and(|(entry, kind)| {
            entry != 0 || !matches!(kind, FileKind::Regular | FileKind::Symlink)
        });
        if beneath || other {
            return Err(BulkloadRefusal::SourceRootKindMismatch);
        }
        leaf.clone_into(rel_path);
        Ok(())
    }
}

impl<W: Write> Inbound<'_, W> {
    /// One walk frame: handled now, or held back by a batch of new
    /// directories (see [`DirectoryBatch`]).
    fn walked(&mut self, frame: Walked) -> Result<()> {
        // An excluded path is skipped before any batch or decision: nothing
        // is made for it and the source reads none of it.
        if let Walked::Entry { entry, row, .. } = &frame {
            if is_excluded(&self.excluded, &row.rel_path) {
                self.stats.excluded += 1;
                return write_control(
                    self.output,
                    &Control::Decide {
                        entry: *entry,
                        decision: Decision::Skip,
                    },
                );
            }
        }
        let cap = self.target.batch();
        if cap <= 1 {
            return self.handle_walked(frame);
        }
        let directory = match &frame {
            Walked::Entry { row, .. } if row.kind == FileKind::Directory => {
                Some(row.rel_path.clone())
            }
            _ => None,
        };
        #[cfg(test)]
        let admit_under_held = ADMIT_UNDER_HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|root| root == self.target.path());
        #[cfg(not(test))]
        let admit_under_held = false;
        if let Some(path) = &directory {
            // Its parent is held and is no member: that parent's decision
            // (its sweep, its adoption's seal) must come before anything is
            // made inside it. A path the batch already holds (a source that
            // offers one directory twice) is decided after the first offer,
            // as one at a time decides it: it adopts what the first made,
            // never a second member at the same key.
            if (self.batch.directories.get(parent_of(path)) == Some(&false) && !admit_under_held)
                || self.batch.directories.contains_key(path)
            {
                self.close_batch()?;
            }
        }
        let member = match (&frame, &directory) {
            (Walked::Entry { row, .. }, Some(path)) => {
                match self.batch.directories.get(parent_of(path)) {
                    Some(true) => true,
                    Some(false) if !admit_under_held => false,
                    _ => self.target.directory_is_new(row),
                }
            }
            _ => false,
        };
        if !member && self.batch.held.is_empty() {
            return self.handle_walked(frame);
        }
        if let Some(path) = directory {
            self.batch.directories.insert(path, member);
        }
        if member {
            self.batch.members += 1;
        }
        self.batch.held.push(frame);
        if self.batch.members >= cap || self.batch.held.len() >= DIRECTORY_BATCH_FRAMES {
            self.close_batch()?;
        }
        Ok(())
    }

    /// A walk frame's own handling.
    /// List `rel_path` as the source's when it is under a listing prefix
    /// ([`set_listing`]).
    fn list(&mut self, rel_path: &[u8]) {
        if self
            .listing
            .iter()
            .any(|prefix| rel_path.starts_with(prefix))
        {
            self.stats.listed.push(rel_path.to_vec());
        }
    }

    fn handle_walked(&mut self, frame: Walked) -> Result<()> {
        match frame {
            Walked::Entry { entry, row, wal } => {
                if matches!(row.kind, FileKind::Regular | FileKind::Symlink) {
                    self.list(&row.rel_path);
                }
                let row = self.redirected(row);
                self.entry(entry, row, wal)
            }
            // A source path the walk refused or skipped is still the
            // source's: listed, so the re-seat never takes a destination
            // file of that name for the destination's own (verification of
            // #280, N4).
            Walked::Refused {
                rel_path,
                code,
                sqlite_code,
            } => {
                self.list(&rel_path);
                self.stats.refuse(rel_path, code, sqlite_code);
                Ok(())
            }
            Walked::EngineTemporary { rel_path } => {
                self.list(&rel_path);
                self.stats.source_engine_temporaries.push(rel_path);
                Ok(())
            }
        }
    }

    /// Create the batch's new directories, level by level, then handle
    /// every frame it held, in stream order.
    fn close_batch(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.batch);
        if batch.held.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let mut members: Vec<(usize, &RowSchema)> = batch
            .held
            .iter()
            .filter_map(|frame| match frame {
                Walked::Entry { row, .. }
                    if batch.directories.get(&row.rel_path) == Some(&true) =>
                {
                    let depth = row.rel_path.split(|byte| *byte == b'/').count();
                    Some((depth, row))
                }
                _ => None,
            })
            .collect();
        members.sort_by_key(|(depth, _)| *depth);
        let mut level_start = 0;
        while let Some((depth, _)) = members.get(level_start) {
            let depth = *depth;
            let level_end = members
                .iter()
                .skip(level_start)
                .position(|(at, _)| *at != depth)
                .map_or(members.len(), |offset| level_start + offset);
            // A member whose parent member was not created is left to be
            // decided alone, as it would be without batching.
            let level: Vec<&RowSchema> = members
                .get(level_start..level_end)
                .unwrap_or_default()
                .iter()
                .map(|(_, row)| *row)
                .filter(|row| {
                    let parent = parent_of(&row.rel_path);
                    batch.directories.get(parent) != Some(&true)
                        || matches!(self.batched.get(parent), Some(Ok(())))
                })
                .collect();
            let created = self
                .target
                .create_directories(&level, self.store, &self.authority);
            for (row, outcome) in level.iter().zip(created) {
                self.batched.insert(row.rel_path.clone(), outcome);
            }
            level_start = level_end;
        }
        REUSE_CENSUS_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
        for frame in batch.held {
            self.handle_walked(frame)?;
        }
        Ok(())
    }

    // The code alone, as before, and a bare `IO`'s errno (TIN-6592).
    fn refuse(&mut self, rel_path: Vec<u8>, refusal: &BulkloadRefusal) {
        self.stats.keep_errno(&rel_path, refusal);
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
    #[allow(clippy::too_many_lines)] // One arm per kind; `--beside` for symlinks and reuse.
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
            FileKind::Directory => self.batched.remove(&row.rel_path).map_or_else(
                || self.decide_directory(&row),
                |created| created.map(|()| Decision::Skip),
            ),
            FileKind::Symlink => self.decide_symlink(&row),
            FileKind::Regular => self.decide_regular(&row, &key, snapshot.as_ref(), snapshot_mode),
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
                count_new_path(Counter::TransferManifestsAsked);
                self.incoming
                    .insert(entry, Incoming::AwaitManifest(Seat::file(row, key)?));
            }
            Decision::Skip | Decision::Reuse | Decision::Refuse { .. } => (),
        }
        write_control(self.output, &Control::Decide { entry, decision })?;
        fault_point!(ReceiveAfterDecide);
        Ok(())
    }

    /// Whether a regular entry that is not reused or refused is asked for by
    /// manifest first (`WantManifest`) rather than streamed (`Send`).
    /// `occupied`: a file exists at its path; `keys`: the entry's row key,
    /// and its snapshot key in `snapshot` mode.
    ///
    /// A manifest first only when something here could fill it: an
    /// existing output to adopt or supersede, a salvaged temporary, or
    /// chunks held by published outputs. Otherwise the source reads the
    /// file exactly once and streams it.
    ///
    /// The delta lever (OI-1003-Q154; off unless
    /// [`LOCAL_NEW_PATH_STREAM_ENV`] is `1`): in a local `copy` with the
    /// hand-off, a seat new to this destination is streamed even when
    /// published outputs hold chunks: no file at its path, no temporary
    /// salvaged in this session, no reuse row under its key here, and no
    /// capture of it in the source's ledger. Both halves are in this
    /// process, so a local fill saves no wire byte: it re-reads and
    /// re-hashes destination bytes in place of the source's handed buffer.
    /// Such a seat has no ledger row to give its manifest, so a manifest
    /// first would make the source read and hash the whole file before the
    /// first byte is placed; streamed, it reads it once, as it is placed.
    ///
    /// Every other entry keeps the manifest: an occupied path (adoption,
    /// the superseding publish's local fill), any salvage (a temporary's
    /// name does not say which path it was staged for), a seat whose reuse
    /// row is here but whose output a third party removed (P79), and a
    /// seat the source's ledger holds a capture of, recorded when it was
    /// carried to another destination from the same source state (one
    /// source fanned out). For the last two the source gives the manifest
    /// from its ledger without a read, and this destination's outputs may
    /// fill it. A source ledger this side cannot read keeps the manifest
    /// too: the lever then decides as the lever off does. `pull`,
    /// `receive`, and a `copy` with the hand-off off keep the store-wide
    /// rule, so a new path is filled across files and only the chunks
    /// nothing here holds cross the wire. The lever's cost is that
    /// cross-file fill for a seat new to both stores in a local `copy`:
    /// its bytes are handed over whole, which S3's inequality 2 as proven
    /// on `copy` counts as received. `BulkloadTransfer.tla`'s `RecvEntry`
    /// chooses freely between `Send` and `WantManifest` for an absent
    /// output, so the lever is inside the model.
    ///
    /// # Errors
    /// Refuses database failures of this side's store.
    fn wants_manifest<'k>(
        &self,
        row: &RowSchema,
        occupied: bool,
        keys: impl Iterator<Item = &'k [u8]>,
    ) -> Result<bool> {
        if occupied || self.target.salvaged() > 0 {
            return Ok(true);
        }
        match self.new_path {
            NewPath::Stream => Ok(false),
            NewPath::Manifest => Ok(true),
            NewPath::StreamLocal => {
                for key in keys {
                    if self.store.holds_output_row(key)? {
                        return Ok(true);
                    }
                }
                if self.source_ledger_holds(row) {
                    return Ok(true);
                }
                count_new_path(Counter::TransferNewPathsStreamed);
                Ok(false)
            }
        }
    }

    /// Whether the source's ledger holds a capture of `row` it would offer
    /// as the manifest without a read ([`ledger_manifest_fits`]). A ledger
    /// read that fails counts as holding it: the entry then keeps the
    /// manifest the lever off asks for. A database seat in `snapshot` mode
    /// is never recorded there, so it is never held.
    fn source_ledger_holds(&self, row: &RowSchema) -> bool {
        let Some(ledger) = &self.source_ledger else {
            return true;
        };
        row_key(&ledger.authority, row)
            .and_then(|key| ledger.store.capture(&key))
            .map_or(true, |capture| {
                capture.is_some_and(|manifest| ledger_manifest_fits(&manifest, row))
            })
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
                    // Before any chunk the only failure is the seat's own.
                    failure: streaming.failure.map(|failure| failure.refusal),
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
                                extended: None,
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
        // M2r2 A-103: refused before the prefix extension existed, and a
        // file it may extend: proved once more (a failed proof is
        // remembered `NotPrefix`, which is honoured).
        if refused == RefusedOutput::Occupied && self.may_extend(row)? {
            return Ok(None);
        }
        // `--beside`: an occupied path is no longer the end of the entry; it
        // lands beside, unless its beside path was refused too and is
        // unchanged since (review of #280, finding 3).
        if self.beside.is_some()
            && matches!(refused, RefusedOutput::Occupied | RefusedOutput::NotPrefix)
            && !self.beside_refusal_stands(row, key, None)?
        {
            return Ok(None);
        }
        counters::bump(Counter::TransferRefusedOutputsRemembered);
        Ok(Some(refused))
    }

    /// Whether the file at `row`'s path is one the prefix extension would
    /// try (M2r2 A-103): in its scope and [`extend_candidate`].
    fn may_extend(&self, row: &RowSchema) -> Result<bool> {
        let root = self.target.path();
        if !prefix_extend_scope(root).covers(&row.rel_path) {
            return Ok(false);
        }
        Ok(self
            .target
            .existing(row)?
            .is_some_and(|(file, _)| extend_candidate(&file, row, root).is_some()))
    }

    /// `row` under the beside directory of its nearest redirected ancestor
    /// (second review of #280, finding 5), or as it is.
    fn redirected(&self, mut row: RowSchema) -> RowSchema {
        if self.redirects.is_empty() {
            return row;
        }
        let mut cut = None;
        for (at, byte) in row.rel_path.iter().enumerate() {
            if *byte == b'/' {
                if let Some(beside) = row
                    .rel_path
                    .get(..at)
                    .and_then(|dir| self.redirects.get(dir))
                {
                    cut = Some((at, beside.clone()));
                }
            }
        }
        if let Some((at, beside)) = cut {
            let mut path = beside;
            path.extend_from_slice(row.rel_path.get(at..).unwrap_or_default());
            row.rel_path = path;
        }
        row
    }

    /// A directory seat: made, or entered when it is there. Under
    /// `--beside` a live path holding a symlink or another kind (sting's
    /// `~/.claude/file-history/<uuid>` links into a repair tree; second
    /// review of #280, finding 5) gets the directory beside it, at
    /// `<path><SUFFIX>`, accounted `applied-beside`, and every entry under
    /// it is decided there; the link is never followed or replaced.
    fn decide_directory(&mut self, row: &RowSchema) -> Result<Decision> {
        if !self.claim(&row.rel_path) {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        match self.target.directory(row, self.store, &self.authority) {
            Err(BulkloadRefusal::DestinationOccupied) if self.beside.is_some() => {
                let suffix = self.beside.clone().unwrap_or_default();
                let beside = beside_row(row, &suffix);
                if !self.claim(&beside.rel_path) {
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                self.target
                    .directory(&beside, self.store, &self.authority)?;
                self.redirects
                    .insert(row.rel_path.clone(), beside.rel_path.clone());
                self.stats
                    .applied_beside
                    .push((row.rel_path.clone(), beside.rel_path));
                Ok(Decision::Skip)
            }
            other => other.map(|()| Decision::Skip),
        }
    }

    /// `--beside` (review of #280, finding 4): claim `path` for this
    /// session when it ends with the suffix. `false` when it is claimed
    /// already: a second seat would write the same name.
    fn claim(&mut self, path: &[u8]) -> bool {
        match &self.beside {
            Some(suffix) if path.ends_with(suffix) => self.claimed.insert(path.to_vec()),
            _ => true,
        }
    }

    /// `--beside` (finding 4): the beside name a planned entry writes is
    /// claimed for the rest of the session.
    fn claim_planned(&mut self, plan: &Plan) {
        match plan {
            Plan::Write(Staging {
                beside: Some(beside),
                ..
            })
            | Plan::Adopt(_, _, _, Some(beside)) => {
                self.claimed.insert(beside.clone());
            }
            _ => {}
        }
    }

    /// A symlink seat: created, or adopted when it is the same link. Under
    /// `--beside` a live path holding something else gets the link beside
    /// it (or adopts it there); the live path is never replaced.
    fn decide_symlink(&mut self, row: &RowSchema) -> Result<Decision> {
        if !self.claim(&row.rel_path) {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        match self.target.symlink(row) {
            Err(BulkloadRefusal::DestinationOccupied) if self.beside.is_some() => {
                let suffix = self.beside.clone().unwrap_or_default();
                let beside = beside_row(row, &suffix);
                if !self.claim(&beside.rel_path) {
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                self.target.symlink(&beside)?;
                self.stats
                    .applied_beside
                    .push((row.rel_path.clone(), beside.rel_path));
                Ok(Decision::Skip)
            }
            other => other.map(|()| Decision::Skip),
        }
    }

    /// Decide a regular seat (see [`Self::entry`]). Under `--beside` a live
    /// path holding a node of another kind (a symlink, review of #280
    /// finding 5) is no refusal: its content is asked for and lands beside.
    fn decide_regular(
        &mut self,
        row: &RowSchema,
        key: &[u8],
        snapshot: Option<&(Vec<u8>, [u8; 32])>,
        snapshot_mode: bool,
    ) -> Result<Decision> {
        if !self.claim(&row.rel_path) {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        // A seat landed beside whatever its live path holds is decided as
        // one whose live path holds another kind: its beside path alone.
        let (identity, other_kind) = if self.lands_beside_always(row) {
            (None, true)
        } else {
            match self.target.identity(row) {
                Err(
                    BulkloadRefusal::DestinationOccupied | BulkloadRefusal::DestinationKindMismatch,
                ) if self.beside.is_some() => (None, true),
                found => (found?, false),
            }
        };
        if let Some(identity) = &identity {
            if let Some((snapshot_key, _)) = snapshot {
                if self.store.output_matches(snapshot_key, identity)? {
                    self.stats.reused += 1;
                    self.snapshot_applied(&row.rel_path);
                    return Ok(Decision::Reuse);
                }
            }
            if self.store.output_matches(key, identity)? {
                self.stats.reused += 1;
                return Ok(Decision::Reuse);
            }
        }
        // `--beside`: this seat's output already beside the live path, from
        // an earlier pass, is reused like any output of this store (S3), and
        // stays accounted.
        if identity.is_some() || other_kind {
            if let Some(reused) = self.reuse_beside(row, key, snapshot)? {
                return Ok(reused);
            }
        }
        if let Some(identity) = &identity {
            // Refused before for what the path holds, and neither the seat
            // nor that file has moved since: the same answer, and the
            // source reads nothing for it (R25). Asked ahead of the capture
            // record: a file that did not verify against this seat cannot
            // prove it either, and proving hashes the whole output.
            for remembered in std::iter::once(key).chain(snapshot.map(|(k, _)| k.as_slice())) {
                if let Some(refused) = self.remembered_refusal(row, remembered, identity)? {
                    return Err(refused.refusal());
                }
            }
            if self.adopt_unrowed(row, key, snapshot)? {
                return Ok(Decision::Reuse);
            }
        }
        let live_sidecar = snapshot_mode && self.target.sqlite_sidecars_present(&row.rel_path)?;
        // Under `--beside` a snapshot whose path has a sidecar beside it is
        // asked for and lands beside (decided when its content is planned,
        // where the beside path is checked the same way).
        if live_sidecar && self.beside.is_none() {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        // `--beside`, a live path of another kind or with a live sidecar: a
        // beside path refused before for what it holds, unchanged since, is
        // refused again with nothing read (review of #280, finding 3).
        if (other_kind || live_sidecar) && self.beside_refusal_stands(row, key, snapshot)? {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        let keys =
            std::iter::once(key).chain(snapshot.map(|(snapshot_key, _)| snapshot_key.as_slice()));
        Ok(
            if self.wants_manifest(row, identity.is_some() || other_kind, keys)? {
                Decision::WantManifest
            } else {
                Decision::Send
            },
        )
    }

    /// Whether `row` lands beside under `--beside` whatever its live path
    /// holds ([`set_beside_always`]).
    fn lands_beside_always(&self, row: &RowSchema) -> bool {
        self.beside.is_some() && self.always.contains(&row.rel_path)
    }

    /// `--beside` (review of #280, finding 3): whether the seat's beside path
    /// still holds the file a beside landing was refused for
    /// ([`RefusedOutput::BesideOccupied`], under its beside key and that
    /// file's identity), by the row key or the snapshot key.
    fn beside_refusal_stands(
        &self,
        row: &RowSchema,
        key: &[u8],
        snapshot: Option<&(Vec<u8>, [u8; 32])>,
    ) -> Result<bool> {
        let Some(suffix) = &self.beside else {
            return Ok(false);
        };
        let beside = beside_row(row, suffix);
        let identity = match self.target.identity(&beside) {
            Ok(Some(identity)) => identity,
            Ok(None)
            | Err(
                BulkloadRefusal::DestinationOccupied | BulkloadRefusal::DestinationKindMismatch,
            ) => return Ok(false),
            Err(refusal) => return Err(refusal),
        };
        for candidate in std::iter::once(key).chain(snapshot.map(|(k, _)| k.as_slice())) {
            let key_beside =
                beside_key(&self.authority, candidate, &row.rel_path, &beside.rel_path)?;
            if self.store.refused_output(&key_beside, &identity)?
                == Some(RefusedOutput::BesideOccupied)
            {
                counters::bump(Counter::TransferRefusedOutputsRemembered);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `--beside` (review of #280, finding 3): remember that this seat's
    /// beside landing was refused, under its live key for what the live
    /// path holds (`live`, when it is a file) and under its beside key for
    /// what the beside path holds, so an unchanged rerun reads nothing.
    fn remember_beside_refusal(
        &self,
        (row, key, racy): (&RowSchema, &[u8], bool),
        live: Option<(StatIdentity, RefusedOutput)>,
        beside: Option<StatIdentity>,
    ) {
        if let Some((identity, refused)) = live {
            self.remember_refusal(key, Some(identity), racy, refused);
        }
        let Some(suffix) = &self.beside else {
            return;
        };
        let beside_rel = beside_path(&row.rel_path, suffix);
        if let Ok(key_beside) = beside_key(&self.authority, key, &row.rel_path, &beside_rel) {
            self.remember_refusal(&key_beside, beside, racy, RefusedOutput::BesideOccupied);
        }
    }

    /// `--beside`: `Reuse` when this seat's output already sits beside the
    /// live file under its beside row (an earlier pass placed it there),
    /// by the row key or, in `snapshot` mode, the snapshot key. The seat is
    /// accounted `applied-beside` again, so the record of this session
    /// keeps naming it.
    fn reuse_beside(
        &mut self,
        row: &RowSchema,
        key: &[u8],
        snapshot: Option<&(Vec<u8>, [u8; 32])>,
    ) -> Result<Option<Decision>> {
        let Some(suffix) = self.beside.clone() else {
            return Ok(None);
        };
        let beside = beside_row(row, &suffix);
        let identity = match self.target.identity(&beside) {
            Ok(Some(identity)) => identity,
            Ok(None)
            | Err(
                BulkloadRefusal::DestinationOccupied | BulkloadRefusal::DestinationKindMismatch,
            ) => return Ok(None),
            Err(refusal) => return Err(refusal),
        };
        for (candidate, is_snapshot) in snapshot
            .map(|(snapshot_key, _)| (snapshot_key.as_slice(), true))
            .into_iter()
            .chain(std::iter::once((key, false)))
        {
            let key_beside =
                beside_key(&self.authority, candidate, &row.rel_path, &beside.rel_path)?;
            if self.store.output_matches(&key_beside, &identity)? {
                if !self.claim(&beside.rel_path) {
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                self.stats.reused += 1;
                if is_snapshot {
                    self.snapshot_applied(&row.rel_path);
                }
                self.stats
                    .applied_beside
                    .push((row.rel_path.clone(), beside.rel_path));
                return Ok(Some(Decision::Reuse));
            }
        }
        Ok(None)
    }

    /// A refusal of an entry whose path a crashed prefix extension left
    /// unsettled (M2r2 A-103): its proven length is reported, so the
    /// operator knows where its own bytes end.
    fn note_interrupted(&mut self, rel_path: &[u8]) {
        if let Ok(Some(intent)) = self.store.prefix_intent(rel_path) {
            self.stats
                .prefix_interrupted
                .push((rel_path.to_vec(), intent.prior_size));
        }
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
        let limits = match self.space {
            Some(limits) if self.since_probe < SPACE_REPROBE_BYTES => limits,
            _ => {
                let limits = crate::space::probe_limits(self.target.path())?;
                self.space = Some(limits);
                self.since_probe = 0;
                limits
            }
        };
        let planned = self.reserved_bytes.saturating_add(size);
        crate::space::check_at(
            self.target.path(),
            (planned, planned),
            limits,
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
        self.settle_entry(entry)?;
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
                // A snapshot's output is never extended (#218).
                Planned {
                    row: &row,
                    extend: !snapshot
                        && prefix_extend_scope(self.target.path()).covers(&row.rel_path),
                    snapshot,
                    beside: self.beside.as_deref(),
                    claimed: &self.claimed,
                    always: self.lands_beside_always(&row),
                },
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
        let (indices, target) = match &plan {
            Plan::Write(staging) => {
                self.open += 1;
                (
                    staging.missing.clone(),
                    Some(WriteTarget::new(staging.staged.file(), staging.placed)),
                )
            }
            Plan::Refuse(_)
            | Plan::Adopt(..)
            | Plan::NoExchange(_)
            | Plan::NotPrefix(_)
            | Plan::BesideRefused { .. } => (Vec::new(), None),
        };
        self.claim_planned(&plan);
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
                target,
                expected: indices.into(),
                failure: None,
                outstanding: 0,
            }),
        );
        Ok(())
    }

    /// One data frame: account its credit, then hand it to the receive
    /// workers (or run it inline), which verify it against its digest (the
    /// one integrity check on received bytes) and write it where it
    /// belongs. Content faults become the entry's refusal and the stream
    /// stays in step; protocol faults end the session, decided here, in
    /// stream order.
    fn data(&mut self, header: &DataHeader, payload: Payload) -> Result<()> {
        let _transfer_timer = PhaseTimer(&TRANSFER_NS, Instant::now());
        let size = payload.len() as u64;
        // Verify once (OI-1003-Q154 lever 1): only a chunk handed over by
        // this process's own source, whose digest is the frame's.
        let vouched = payload.vouched(&header.digest);
        #[cfg(test)]
        let vouched = vouched || self.trust_wire;
        self.granted = self
            .granted
            .checked_sub(size)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        self.stats.bytes_received = self.stats.bytes_received.saturating_add(size);
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
        let job = match self.incoming.get_mut(&header.entry) {
            Some(Incoming::Streaming(streaming)) => streaming.accept(
                self.target,
                &mut self.open,
                header,
                (payload, vouched),
                |payload| {
                    let _verify_timer = PhaseTimer(&RECV_VERIFY_NS, Instant::now());
                    verify_wire(payload, &header.digest)
                },
            )?,
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
                let write = match (&filling.failure, &filling.plan, &filling.target) {
                    (None, Plan::Write(staging), Some(target)) => staging
                        .placements
                        .get(&header.digest)
                        .map(|(_, offsets)| (Arc::clone(target), offsets.clone())),
                    _ => None,
                };
                filling.outstanding += 1;
                Some(RecvJob {
                    entry: header.entry,
                    index: header.index,
                    digest: header.digest,
                    charge: size.max(RECV_JOB_MIN_BYTES),
                    payload,
                    verify: !vouched,
                    write,
                })
            }
            _ => return Err(BulkloadRefusal::ProtocolStateViolation),
        };
        if let Some(job) = job {
            self.dispatch(job)?;
        }
        self.consumed = self.consumed.saturating_add(size);
        if self.consumed >= CREDIT_RETURN {
            let bytes = std::mem::take(&mut self.consumed);
            self.granted = self.granted.saturating_add(bytes);
            write_control(self.output, &Control::Credit { bytes })?;
        }
        Ok(())
    }

    /// Run one chunk job inline, or queue it for the receive workers once
    /// their byte budget has room (OI-1003-Q143 item 3).
    fn dispatch(&mut self, job: RecvJob) -> Result<()> {
        let Some(pool) = &self.pool else {
            let done = run_recv_job(job, false);
            return self.finished(done);
        };
        let charge = job.charge;
        if pool.queued > 0 && pool.bytes.saturating_add(charge) > RECV_POOL_BYTES {
            let _wait_timer = PhaseTimer(&RECV_POOL_WAIT_NS, Instant::now());
            while self.pool.as_ref().is_some_and(|pool| {
                pool.queued > 0 && pool.bytes.saturating_add(charge) > RECV_POOL_BYTES
            }) {
                self.reap()?;
            }
        }
        let pool = self.pool.as_mut().ok_or(BulkloadRefusal::WorkerLost)?;
        pool.jobs
            .send(job)
            .map_err(|_| BulkloadRefusal::WorkerLost)?;
        pool.bytes = pool.bytes.saturating_add(charge);
        pool.queued += 1;
        #[cfg(test)]
        POOL_HIGH_WATER.fetch_max(pool.bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Wait for one receive job to finish and fold its outcome.
    fn reap(&mut self) -> Result<()> {
        let done = self
            .pool
            .as_ref()
            .ok_or(BulkloadRefusal::WorkerLost)?
            .done
            .recv()
            .map_err(|_| BulkloadRefusal::WorkerLost)?;
        if let Some(pool) = self.pool.as_mut() {
            pool.bytes = pool.bytes.saturating_sub(done.charge);
            pool.queued = pool.queued.saturating_sub(1);
        }
        self.finished(done)
    }

    /// Fold a finished chunk job into its entry.
    fn finished(&mut self, done: RecvDone) -> Result<()> {
        if done.lost {
            return Err(BulkloadRefusal::WorkerLost);
        }
        let (outstanding, failure) = match self.incoming.get_mut(&done.entry) {
            Some(Incoming::Streaming(streaming)) => {
                (&mut streaming.outstanding, &mut streaming.failure)
            }
            Some(Incoming::Filling(filling)) => (&mut filling.outstanding, &mut filling.failure),
            _ => return Err(BulkloadRefusal::WorkerLost),
        };
        *outstanding = outstanding.saturating_sub(1);
        if let Some(found) = done.failure {
            fold_failure(failure, found);
        }
        Ok(())
    }

    /// Wait until every chunk job of `entry` has finished, so its file holds
    /// all its writes before it is published or discarded.
    fn settle_entry(&mut self, entry: u64) -> Result<()> {
        let pending = |incoming: &HashMap<u64, Incoming>| match incoming.get(&entry) {
            Some(Incoming::Streaming(streaming)) => streaming.outstanding > 0,
            Some(Incoming::Filling(filling)) => filling.outstanding > 0,
            _ => false,
        };
        if pending(&self.incoming) {
            let _wait_timer = PhaseTimer(&RECV_POOL_WAIT_NS, Instant::now());
            while pending(&self.incoming) {
                self.reap()?;
            }
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
        // Every write of the entry lands before its file is checked,
        // published or discarded (item 3): its submit, so its seal, follows.
        self.settle_entry(entry)?;
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
        // capture, which therefore never costs a source read again. An
        // output queued under its beside path (`--beside`) is awaited
        // under that path, which the committer's outcome names.
        match outcome {
            Ok(beside) => {
                let held_as = match beside {
                    Some(beside) => {
                        self.beside_of.insert(beside.clone(), rel_path);
                        beside
                    }
                    None => rel_path,
                };
                self.pending_held.insert(held_as, entry);
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
        {
            let _drain_timer = PhaseTimer(&RECV_DRAIN_NS, Instant::now());
            if let Some(finish) = self.fold_finish() {
                let outcome = finish.outcome();
                self.committer.sync_last(finish)?;
                self.target.folded_finish(&outcome);
            } else {
                self.committer.sync()?;
            }
        }
        self.answer_held()?;
        if self.pending_held.is_empty() {
            Ok(())
        } else {
            Err(BulkloadRefusal::ProtocolStateViolation)
        }
    }

    /// The directory finish the drain's group may carry (OI-1003-Q154 lever
    /// 3; [`Destination::fold_finish`] sets the final modes): only when
    /// nothing after that group can refuse. Every entry has ended, nothing
    /// is refused so far on this side (the sink checks its own groups'
    /// outcomes), and every `SQLite` sidecar the source reported has a
    /// snapshot base here, so `resolve_sidecars` refuses none. A session
    /// that refuses keeps every directory unfinished (`finish_receive`).
    fn fold_finish(&self) -> Option<crate::materialize::FoldedFinish> {
        if !directory_fold(self.target.path())
            || !self.stats.refusals.is_empty()
            || self
                .sidecars
                .iter()
                .any(|(_, database)| !self.snapshot_bases.contains(database))
        {
            return None;
        }
        self.target.fold_finish()
    }

    /// Returns the beside path when the output was queued under it
    /// (`--beside`), else `None`.
    #[allow(clippy::too_many_lines)] // Verify, adopt, extend or supersede, stage, publish.
    fn end_streaming(
        &mut self,
        streaming: Streaming,
        root: [u8; 32],
        racy: bool,
    ) -> Result<Option<Vec<u8>>> {
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
        let checked = failure
            .map_or(Ok(()), |failure| Err(failure.refusal))
            .and_then(|()| {
                if offset != row.size || manifest_root(&specs) != root {
                    Err(BulkloadRefusal::DigestMismatch)
                } else {
                    Ok(())
                }
            });
        let mut supersede = None;
        let manifest = Manifest {
            root,
            chunks: specs,
        };
        // M2r2 A-103: a grown file with no owning record, extended from the
        // streamed copy (its whole prefix compared, `base` 0).
        let mut grown = None;
        // `--beside`: the beside path this entry lands at, when the live
        // path holds other bytes bulkload did not write, or a snapshot's
        // live path has a sidecar beside it.
        let mut land_beside: Option<Vec<u8>> = None;
        let live_sidecar = checked.is_ok()
            && (self.lands_beside_always(&row)
                || (snapshot
                    && self.beside.is_some()
                    && self.target.sqlite_sidecars_present(&row.rel_path)?));
        // What the live path's refusal may be remembered as (finding 3).
        let mut live_refused: Option<(StatIdentity, RefusedOutput)> = None;
        // `--beside`: a node of another kind at the live path lands beside
        // (review of #280, finding 5).
        let other_kind = checked.is_ok()
            && adopt
            && self.beside.is_some()
            && matches!(
                self.target.existing(&row),
                Err(BulkloadRefusal::DestinationOccupied | BulkloadRefusal::DestinationKindMismatch)
            );
        if live_sidecar || other_kind {
            land_beside = self
                .beside
                .as_deref()
                .map(|suffix| beside_path(&row.rel_path, suffix));
        } else if checked.is_ok() && adopt {
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
                let (verified, settled) =
                    verify_settled(self.target.path(), &file, &row, &manifest);
                match verified {
                    Ok(identity) => Ok(Some((file, parent, identity))),
                    Err(BulkloadRefusal::DestinationOccupied) => {
                        // Under `--beside` a live path is never replaced
                        // (review of #280, finding 1).
                        supersede = if self.beside.is_some() {
                            None
                        } else {
                            owned_output(self.store, &self.authority, &row, &file)?
                        };
                        let root_path = self.target.path();
                        if supersede.is_none()
                            && !snapshot
                            && prefix_extend_scope(root_path).covers(&row.rel_path)
                        {
                            if let Some((identity, prior)) =
                                extend_candidate(&file, &row, root_path)
                            {
                                let writer =
                                    self.target.open_output_append(&row.rel_path).ok().filter(
                                        |writer| {
                                            writer.metadata().is_ok_and(|found| {
                                                StatIdentity::from_metadata(&found) == identity
                                            })
                                        },
                                    );
                                if let Some(writer) = writer {
                                    grown = Some((file, parent, writer, identity, prior));
                                    return Ok(None);
                                }
                            }
                        }
                        if supersede.is_none() && self.beside.is_some() {
                            // `--beside`: the streamed copy lands beside.
                            land_beside = self
                                .beside
                                .as_deref()
                                .map(|suffix| beside_path(&row.rel_path, suffix));
                            live_refused =
                                settled.map(|identity| (identity, RefusedOutput::Occupied));
                            Ok(None)
                        } else if supersede.is_none() {
                            self.note_interrupted(&row.rel_path);
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
                    return self
                        .adopt_settling(file, parent, &row, (key, record, racy, root), identity)
                        .map(|()| None);
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
        if let Some((file, parent, writer, identity, prior)) = grown {
            // A non-empty source streamed whole: its staged copy is here,
            // so any prefix length is judged by comparing it whole.
            let staged = staged.ok_or(BulkloadRefusal::DestinationOccupied)?;
            return self.finish_extend(
                staged,
                Extend {
                    file,
                    parent,
                    writer,
                    identity,
                    prior,
                    base: 0,
                    hasher: blake3::Hasher::new(),
                    hints,
                    straddle: None,
                },
                &row,
                (key, record, racy, root),
            );
        }
        let staged = match (checked, staged) {
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
        // `--beside`: the beside path is decided as a planned entry's is;
        // the staged copy takes the beside name.
        let (row, key) = match &land_beside {
            Some(beside_rel) => {
                // Another seat of this session writes this name (finding 4).
                if !self.claim(beside_rel) {
                    let _ = staged.discard();
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                let beside = RowSchema {
                    rel_path: beside_rel.clone(),
                    ..row.clone()
                };
                let target = beside_target(
                    &ReceiveContext {
                        target: self.target,
                        store: self.store,
                        authority: &self.authority,
                        displaced: &self.displaced,
                        session: &self.session,
                        salvage: &self.salvage,
                    },
                    &beside,
                    &manifest,
                    snapshot,
                );
                let key_beside = beside_key(&self.authority, &key, &row.rel_path, beside_rel);
                match (target, key_beside) {
                    (Ok(BesideTarget::Fresh), Ok(key_beside)) => (beside, key_beside),
                    (Ok(BesideTarget::Supersede(owned)), Ok(key_beside)) => {
                        supersede = Some(*owned);
                        (beside, key_beside)
                    }
                    (Ok(BesideTarget::Adopt(file, parent, identity)), Ok(key_beside)) => {
                        let _ = staged.discard();
                        let placed = beside.rel_path.clone();
                        return self
                            .adopt(
                                file,
                                parent,
                                &beside,
                                (key_beside, record, racy, root),
                                identity,
                            )
                            .map(|()| Some(placed));
                    }
                    (Err((refusal, settled)), _) => {
                        let _ = staged.discard();
                        if refusal == BulkloadRefusal::DestinationOccupied {
                            self.remember_beside_refusal((&row, &key, racy), live_refused, settled);
                        }
                        return Err(refusal);
                    }
                    (_, Err(refusal)) => {
                        let _ = staged.discard();
                        return Err(refusal);
                    }
                }
            }
            None => (row, key),
        };
        let mut staged = if land_beside.is_some() {
            staged.with_suffix(self.beside.as_deref().unwrap_or_default())?
        } else {
            staged
        };
        if snapshot {
            if let Err(refusal) = self.verify_snapshot(&staged, &row) {
                let _ = staged.discard();
                return Err(refusal);
            }
            staged.mark_sqlite();
        }
        self.publish(staged, &row, (key, record, racy, root), hints, supersede)
            .map(|()| land_beside)
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

    /// Returns the beside path when the output was queued under it
    /// (`--beside`), else `None`.
    #[allow(clippy::too_many_lines)] // One arm per plan; `--beside` for adopt and write.
    fn end_filling(&mut self, filling: Filling, racy: bool) -> Result<Option<Vec<u8>>> {
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
            Plan::Adopt(file, parent, verified, beside) => {
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
                    if matches!(verified, Err(BulkloadRefusal::DestinationOccupied))
                        && beside.is_none()
                    {
                        self.note_interrupted(&row.rel_path);
                        self.remember_refusal(&key, settled, racy, RefusedOutput::Occupied);
                    }
                    verified?
                };
                // An adopted beside output is recorded under its own row.
                let Some(beside_rel) = beside else {
                    return self
                        .adopt_settling(file, parent, &row, (key, record, racy, root), identity)
                        .map(|()| None);
                };
                let key_beside = beside_key(&self.authority, &key, &row.rel_path, &beside_rel)?;
                let beside_row = RowSchema {
                    rel_path: beside_rel.clone(),
                    ..row
                };
                self.adopt(
                    file,
                    parent,
                    &beside_row,
                    (key_beside, record, racy, root),
                    identity,
                )
                .map(|()| Some(beside_rel))
            }
            Plan::NotPrefix(identity) => {
                self.note_interrupted(&row.rel_path);
                self.remember_refusal(&key, Some(identity), racy, RefusedOutput::NotPrefix);
                Err(BulkloadRefusal::DestinationOccupied)
            }
            Plan::NoExchange(settled) => {
                self.remember_refusal(&key, settled, racy, RefusedOutput::ExchangeUnsupported);
                Err(BulkloadRefusal::DestinationExchangeUnsupported)
            }
            Plan::BesideRefused {
                refusal,
                live,
                beside,
            } => {
                if refusal == BulkloadRefusal::DestinationOccupied {
                    self.remember_beside_refusal((&row, &key, racy), live, beside);
                }
                Err(refusal)
            }
            Plan::Write(mut staging) => {
                self.open -= 1;
                if let Some(failure) = failure {
                    let _ = staging.staged.discard();
                    return Err(failure.refusal);
                }
                fault_point!(ReceiveAfterChunks);
                if let Some(extend) = staging.extend.take() {
                    return self.finish_extend(
                        staging.staged,
                        *extend,
                        &row,
                        (key, record, racy, root),
                    );
                }
                // `--beside`: published under the seat's beside row.
                let (row, key) = match &staging.beside {
                    Some(beside_rel) => (
                        RowSchema {
                            rel_path: beside_rel.clone(),
                            ..row.clone()
                        },
                        beside_key(&self.authority, &key, &row.rel_path, beside_rel)?,
                    ),
                    None => (row, key),
                };
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
                .map(|()| staging.beside)
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
        self.adopt_as(file, parent, row, (key, record, racy, root), identity, None)
    }

    /// [`Self::adopt`] of a file that holds the seat's bytes: when a crashed
    /// prefix extension left its intent for the path (M2r2 A-103), the
    /// adoption settles it, with its `prefix_extensions` row.
    fn adopt_settling(
        &mut self,
        file: std::fs::File,
        parent: Arc<std::fs::File>,
        row: &RowSchema,
        record: (Vec<u8>, [u8; 32], bool, [u8; 32]),
        identity: StatIdentity,
    ) -> Result<()> {
        let Some(proof) = self.store.prefix_intent(&row.rel_path)? else {
            return self.adopt(file, parent, row, record, identity);
        };
        // The intent's prior bytes must still be this file's first ones.
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0_u8; 1 << 20];
        let mut at = 0_u64;
        while at < proof.prior_size {
            let length =
                usize::try_from((proof.prior_size - at).min(1 << 20)).unwrap_or(usize::MAX);
            let slot = buffer
                .get_mut(..length)
                .ok_or(BulkloadRefusal::BudgetExceeded)?;
            if crate::io::sys::pread_full(&file, slot, at).refuse_at("transfer::adopt_settling")?
                != length
            {
                return self.adopt(file, parent, row, record, identity);
            }
            counters::add_len(Counter::DestVerifyRead, length);
            hasher.update(slot);
            at += length as u64;
        }
        if *hasher.finalize().as_bytes() != proof.prior_blake3 {
            return self.adopt(file, parent, row, record, identity);
        }
        self.adopt_as(
            file,
            parent,
            row,
            record,
            identity,
            Some((proof, Vec::new())),
        )?;
        self.stats.prefix_extended.push((
            row.rel_path.clone(),
            proof.prior_size,
            proof.prior_blake3,
            row.size,
        ));
        Ok(())
    }

    /// [`Self::adopt`], with the record of a prefix extension (M2r2 A-103):
    /// its chunk hints and its `prefix_extensions` row.
    fn adopt_as(
        &self,
        file: std::fs::File,
        parent: Arc<std::fs::File>,
        row: &RowSchema,
        (key, record, racy, root): (Vec<u8>, [u8; 32], bool, [u8; 32]),
        identity: StatIdentity,
        extended: Option<(PrefixProof, Vec<ChunkHint>)>,
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
                extended: extended.as_ref().map(|(proof, _)| *proof),
                hints: extended.map(|(_, hints)| hints).unwrap_or_default(),
            },
            file,
            parent,
        })
    }

    /// Append a grown file's tail (M2r2 A-103, [`plan_extend`]): the file
    /// must be the one the plan proved, untouched since (its identity, by
    /// the descriptor the plan read and by the append descriptor, which the
    /// plan opened and so proved writable); the head of the straddling
    /// chunk it already holds must equal the staged tail's. Then an intent
    /// is committed and the tail appended piece by piece through an
    /// `O_APPEND` descriptor, the file's size checked before and after
    /// every piece, as `estate-reseat` extends: a byte a live writer
    /// appended first is never written over, and the entry is refused
    /// `DESTINATION_OCCUPIED` with every writer's bytes kept. The file is
    /// flushed, the straddling chunk re-hashed against its digest, and its
    /// record queued as an adoption with its `prefix_extensions` row.
    ///
    /// A head that differs is not a prefix: refused `DESTINATION_OCCUPIED`
    /// and remembered `NotPrefix` under the identity the plan proved
    /// settled, so an unchanged rerun reads and sends nothing for it. The
    /// staged tail is removed either way. The file is counted and reported
    /// extended once its record commits (`finish_receive`).
    /// Returns the beside path when the file was not a prefix after all and
    /// `--beside` landed the whole seat beside it ([`Self::beside_from_extend`]).
    fn finish_extend(
        &mut self,
        staged: StagedFile,
        extend: Extend,
        row: &RowSchema,
        record: (Vec<u8>, [u8; 32], bool, [u8; 32]),
    ) -> Result<Option<Vec<u8>>> {
        let Extend {
            file,
            parent,
            writer,
            identity,
            prior,
            base,
            mut hasher,
            hints,
            straddle,
        } = extend;
        // BLAKE3 over the proven `[0, base)`, before the head compare adds
        // to it: what a beside landing re-checks the prefix against.
        let proven = *hasher.clone().finalize().as_bytes();
        let appended = self.append_tail(
            &staged,
            (&file, &writer),
            (identity, prior, base, straddle),
            &mut hasher,
            row,
        );
        let (after, proof) = match appended {
            Ok(done) => {
                let _ = staged.discard();
                done
            }
            // `--beside`: the straddling chunk's head differs, so the file
            // is not the seat's prefix; the seat lands whole beside it,
            // from the file's proven prefix and the staged tail.
            Err(Unextended::NotPrefix) if self.beside.is_some() => {
                let suffix = self.beside.clone().unwrap_or_default();
                return self
                    .beside_from_extend(
                        staged,
                        (&file, identity, base, proven),
                        (row, hints),
                        record,
                        &suffix,
                    )
                    .map(Some);
            }
            Err(Unextended::NotPrefix) => {
                let _ = staged.discard();
                let (key, _, racy, _) = &record;
                self.remember_refusal(key, Some(identity), *racy, RefusedOutput::NotPrefix);
                self.note_interrupted(&row.rel_path);
                return Err(BulkloadRefusal::DestinationOccupied);
            }
            Err(Unextended::Refused(refusal)) => {
                let _ = staged.discard();
                self.note_interrupted(&row.rel_path);
                return Err(refusal);
            }
        };
        self.adopt_as(file, parent, row, record, after, Some((proof, hints)))?;
        self.stats.prefix_extended.push((
            row.rel_path.clone(),
            prior,
            proof.prior_blake3,
            row.size,
        ));
        Ok(None)
    }

    /// `--beside` after a failed prefix extension: the seat's bytes are the
    /// live file's `[0, base)` (proven the manifest's by chunk, and
    /// re-hashed here against `proven` as they are copied: a file changed
    /// since is refused `DESTINATION_OCCUPIED`, nothing placed) followed by
    /// the staged tail `[base, size)`, every byte of which was verified as it
    /// arrived. They are written to a new staged file, which is published
    /// at the beside path as any beside output is ([`beside_target`]); the
    /// staged tail is removed either way. The live file is read, never
    /// written. Returns the beside path.
    #[allow(clippy::too_many_lines)] // Copy the proven prefix, then the tail; judge, publish.
    fn beside_from_extend(
        &mut self,
        tail: StagedFile,
        (file, identity, base, proven): (&std::fs::File, StatIdentity, u64, [u8; 32]),
        (row, hints): (&RowSchema, Vec<ChunkHint>),
        (key, record, racy, root): (Vec<u8>, [u8; 32], bool, [u8; 32]),
        suffix: &[u8],
    ) -> Result<Vec<u8>> {
        const PIECE: u64 = 1 << 20;
        let beside = beside_row(row, suffix);
        let key_beside = beside_key(&self.authority, &key, &row.rel_path, &beside.rel_path)?;
        // Another seat of this session writes this name (finding 4).
        if !self.claim(&beside.rel_path) {
            let _ = tail.discard();
            return Err(BulkloadRefusal::DestinationOccupied);
        }
        let whole = (|| -> Result<StagedFile> {
            let out = self.target.stage(row)?.with_suffix(suffix)?;
            let copied = (|| -> Result<()> {
                let mut buffer = vec![0_u8; usize::try_from(PIECE).unwrap_or(usize::MAX)];
                let mut hasher = blake3::Hasher::new();
                let mut at = 0_u64;
                while at < base {
                    let length = usize::try_from((base - at).min(PIECE)).unwrap_or(usize::MAX);
                    let piece = buffer
                        .get_mut(..length)
                        .ok_or(BulkloadRefusal::BudgetExceeded)?;
                    if crate::io::sys::pread_full(file, piece, at)
                        .refuse_at("transfer::beside_from_extend")?
                        != length
                    {
                        return Err(BulkloadRefusal::DestinationOccupied);
                    }
                    counters::add_len(Counter::DestVerifyRead, length);
                    hasher.update(piece);
                    crate::io::sys::pwrite_all(&**out.file(), piece, at).map_err(|error| {
                        crate::materialize::space_refusal(crate::refuse::io(
                            &error,
                            "transfer::beside_from_extend",
                        ))
                    })?;
                    counters::add_len(Counter::DestMaterializeWrite, length);
                    at += length as u64;
                }
                let moved = file.metadata().map_or(true, |found| {
                    StatIdentity::from_metadata(&found) != identity
                });
                if moved || *hasher.finalize().as_bytes() != proven {
                    return Err(BulkloadRefusal::DestinationOccupied);
                }
                while at < row.size {
                    let length = usize::try_from((row.size - at).min(PIECE)).unwrap_or(usize::MAX);
                    let piece = buffer
                        .get_mut(..length)
                        .ok_or(BulkloadRefusal::BudgetExceeded)?;
                    if crate::io::sys::pread_full(&**tail.file(), piece, at - base)
                        .refuse_at("transfer::beside_from_extend")?
                        != length
                    {
                        return Err(BulkloadRefusal::DigestMismatch);
                    }
                    crate::io::sys::pwrite_all(&**out.file(), piece, at).map_err(|error| {
                        crate::materialize::space_refusal(crate::refuse::io(
                            &error,
                            "transfer::beside_from_extend",
                        ))
                    })?;
                    counters::add_len(Counter::DestMaterializeWrite, length);
                    at += length as u64;
                }
                Ok(())
            })();
            match copied {
                Ok(()) => Ok(out),
                Err(refusal) => {
                    let _ = out.discard();
                    Err(refusal)
                }
            }
        })();
        let _ = tail.discard();
        let out = whole?;
        // The whole file is staged: the beside path is judged against its
        // bytes as a planned entry's is.
        let target = {
            let context = ReceiveContext {
                target: self.target,
                store: self.store,
                authority: &self.authority,
                displaced: &self.displaced,
                session: &self.session,
                salvage: &self.salvage,
            };
            beside_existing(&context, &beside, out.file())
        };
        let supersede = match target {
            Ok(BesideTarget::Fresh) => None,
            Ok(BesideTarget::Supersede(owned)) => Some(*owned),
            Ok(BesideTarget::Adopt(file, parent, identity)) => {
                let _ = out.discard();
                self.adopt(
                    file,
                    parent,
                    &beside,
                    (key_beside, record, racy, root),
                    identity,
                )?;
                return Ok(beside.rel_path);
            }
            Err(refusal) => {
                let _ = out.discard();
                return Err(refusal);
            }
        };
        self.publish(
            out,
            &beside,
            (key_beside, record, racy, root),
            hints,
            supersede,
        )?;
        Ok(beside.rel_path)
    }

    /// The checks and writes of [`Self::finish_extend`]: the file's
    /// identity after the flush, and what was proven of its prior bytes.
    #[allow(clippy::too_many_lines)] // Check, compare, intend, append, flush, re-hash.
    fn append_tail(
        &self,
        staged: &StagedFile,
        (file, writer): (&std::fs::File, &std::fs::File),
        (identity, prior, base, straddle): (StatIdentity, u64, u64, Option<([u8; 32], u64)>),
        hasher: &mut blake3::Hasher,
        row: &RowSchema,
    ) -> std::result::Result<(StatIdentity, PrefixProof), Unextended> {
        const PIECE: u64 = 1 << 20;
        let refused = |refusal| Unextended::Refused(refusal);
        let occupied = || Unextended::Refused(BulkloadRefusal::DestinationOccupied);
        let current = |handle: &std::fs::File| {
            handle
                .metadata()
                .map(|found| StatIdentity::from_metadata(&found))
                .refuse_at("transfer::append_tail")
                .map_err(refused)
        };
        if current(file)? != identity || current(writer)? != identity {
            return Err(occupied());
        }
        let mut held = vec![0_u8; usize::try_from(PIECE).unwrap_or(usize::MAX)];
        let mut tail = vec![0_u8; held.len()];
        // The straddling chunk's head: what the file holds of it must be
        // the staged tail's first bytes.
        let mut at = base;
        while at < prior {
            let length = usize::try_from((prior - at).min(PIECE)).unwrap_or(usize::MAX);
            let (Some(mine), Some(theirs)) = (held.get_mut(..length), tail.get_mut(..length))
            else {
                return Err(refused(BulkloadRefusal::BudgetExceeded));
            };
            let read_mine = crate::io::sys::pread_full(file, mine, at)
                .refuse_at("transfer::append_tail")
                .map_err(refused)?;
            let read_theirs = crate::io::sys::pread_full(&**staged.file(), theirs, at - base)
                .refuse_at("transfer::append_tail")
                .map_err(refused)?;
            counters::add_len(Counter::DestVerifyRead, length);
            if read_mine != length || read_theirs != length {
                return Err(occupied());
            }
            if mine != theirs {
                return Err(Unextended::NotPrefix);
            }
            hasher.update(mine);
            at += length as u64;
        }
        let proof = PrefixProof {
            prior_size: prior,
            prior_blake3: *hasher.finalize().as_bytes(),
        };
        fault_point!(ReceivePrefixBeforeAppend);
        if current(file)? != identity || current(writer)? != identity {
            return Err(occupied());
        }
        // The intent first: a crash after any appended byte leaves the
        // length this file held (`Store::begin_prefix_extension`).
        self.store
            .begin_prefix_extension(&row.rel_path, proof, &identity)
            .map_err(refused)?;
        let size_of = |handle: &std::fs::File| -> std::result::Result<u64, Unextended> {
            Ok(handle
                .metadata()
                .refuse_at("transfer::append_tail")
                .map_err(refused)?
                .len())
        };
        let mut at = prior;
        while at < row.size {
            // Review of #280, finding 8: every piece ends at a record
            // boundary (its last newline; a record longer than a piece is
            // read whole), and is written by one `O_APPEND` write, so a
            // destination appender that slips in between two pieces never
            // lands inside a record of ours. Only the seat's own last
            // bytes, past its last newline, may end without one.
            let piece = record_piece(staged.file(), (at - base, row.size - at), &mut tail)
                .map_err(refused)?;
            let length = piece.len();
            // Ours to append only while the file ends where our last byte
            // does: a live writer's bytes are never written over.
            if size_of(writer)? != at {
                return Err(occupied());
            }
            crate::io::sys::pwrite_all(writer, piece, at).map_err(|error| {
                refused(crate::materialize::space_refusal(crate::refuse::io(
                    &error,
                    "transfer::append_tail",
                )))
            })?;
            counters::add_len(Counter::DestMaterializeWrite, length);
            counters::add_len(Counter::DestPrefixAppendWrite, length);
            at += length as u64;
            if size_of(writer)? != at {
                return Err(occupied());
            }
            #[cfg(test)]
            append_hook(&row.rel_path);
        }
        fault_point!(ReceivePrefixAfterAppend);
        counters::sync_full(writer).map_err(|error| {
            refused(crate::materialize::space_refusal(crate::refuse::io(
                &error,
                "transfer::append_tail",
            )))
        })?;
        fault_point!(ReceivePrefixAfterFlush);
        let after = current(writer)?;
        if after.size != row.size {
            return Err(occupied());
        }
        // A second check of the prefix rule: the straddling chunk, as the
        // file now holds it, is the manifest's.
        if let Some((digest, size)) = straddle {
            let mut chunk = vec![0_u8; usize::try_from(size).unwrap_or(usize::MAX)];
            let read = crate::io::sys::pread_full(file, &mut chunk, base)
                .refuse_at("transfer::append_tail")
                .map_err(refused)?;
            counters::add_len(Counter::DestVerifyRead, read);
            if read != chunk.len() || counters::hash(Counter::HashVerifyExisting, &chunk) != digest
            {
                return Err(occupied());
            }
        }
        Ok((after, proof))
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
        crate::io::durable::start_writeback(staged.file());
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
    /// then. The last field names the beside path when the output adopted
    /// is the seat's beside output (`--beside`), under whose row it is
    /// recorded.
    Adopt(
        std::fs::File,
        Arc<std::fs::File>,
        Option<StatIdentity>,
        Option<Vec<u8>>,
    ),
    /// This store's own output holds other bytes than the manifest's, on a
    /// file system with no atomic exchange to supersede it with: refused
    /// `DESTINATION_EXCHANGE_UNSUPPORTED` once the entry ends, with nothing
    /// staged and no chunk asked for. The identity is the output's, when
    /// the refusal may be remembered ([`verify_settled`]).
    NoExchange(Option<StatIdentity>),
    Write(Staging),
    /// An existing file with no owning record the prefix extension tried
    /// whose bytes are not a prefix of the manifest's (M2r2 A-103): refused
    /// `DESTINATION_OCCUPIED` once the entry ends, remembered `NotPrefix`
    /// under its settled identity, with no chunk asked for.
    NotPrefix(StatIdentity),
    /// `--beside`: the seat lands beside its live path, and the beside path
    /// cannot take it (a foreign file with other bytes, a name another seat
    /// of this session writes, a sidecar): refused once the entry ends, with
    /// nothing staged or asked for. `live` and `beside` are the settled
    /// identities it is remembered under, when known (review of #280,
    /// finding 3).
    BesideRefused {
        refusal: BulkloadRefusal,
        live: Option<(StatIdentity, RefusedOutput)>,
        beside: Option<StatIdentity>,
    },
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
    /// Bytes filled locally, every offset counted (item 1a).
    placed: u64,
    /// Set when the staged file is the tail of an existing destination file
    /// that holds a verified byte prefix of the source (M2r2 A-103): the
    /// staged file holds bytes `[base, size)` and is appended from, never
    /// published.
    extend: Option<Box<Extend>>,
    /// `--beside`: the staged file is published at this path, beside the
    /// live file the plan found differing, under the seat's beside row.
    beside: Option<Vec<u8>>,
}

/// An existing destination file with no owning record whose first
/// `prior` bytes are proven, through `[0, base)`, to be the source's (M2r2
/// A-103). `base` is the offset of the first chunk that does not end within
/// `[0, prior)`: the staged tail starts there, and its head `[base, prior)`
/// is compared with the file's before anything is appended.
struct Extend {
    /// The file, read through this descriptor.
    file: std::fs::File,
    parent: Arc<std::fs::File>,
    /// The same file opened for appending (`O_APPEND`), when it was proven:
    /// a file this user may not write is never a candidate.
    writer: std::fs::File,
    /// Its identity when the plan read it, settled: what the append
    /// requires, and what a failed proof is remembered under.
    identity: StatIdentity,
    /// Its length then.
    prior: u64,
    base: u64,
    /// BLAKE3 over `[0, base)` so far.
    hasher: blake3::Hasher,
    /// Every distinct chunk's first occurrence, at its offset in the file.
    hints: Vec<ChunkHint>,
    /// The digest and size of the chunk at `base`, re-hashed in the file
    /// after the append; `None` when the whole source was staged.
    straddle: Option<([u8; 32], u64)>,
}

/// Why a prefix extension did not happen (M2r2 A-103).
enum Unextended {
    /// The file's bytes are not a prefix of the seat's: remembered.
    NotPrefix,
    /// Any other refusal: the file moved, a writer appended, an I/O error.
    Refused(BulkloadRefusal),
}

/// The unit tests' live-writer hook: run after each appended piece, on
/// whichever thread receives (process-wide; the tests that set it run one
/// at a time).
#[cfg(test)]
type AppendHook = Box<dyn FnMut(&[u8]) + Send>;

#[cfg(test)]
static APPEND_HOOK: Mutex<Option<AppendHook>> = Mutex::new(None);

/// Run `hook` after each piece a prefix extension appends (`None` clears
/// it).
#[cfg(test)]
fn set_append_hook(hook: Option<AppendHook>) {
    *APPEND_HOOK.lock().unwrap_or_else(PoisonError::into_inner) = hook;
}

#[cfg(test)]
fn append_hook(rel_path: &[u8]) {
    if let Some(hook) = APPEND_HOOK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        hook(rel_path);
    }
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
/// What [`plan_file`] plans: the row, whether the prefix extension may try
/// its path, whether its content is a `SQLite` snapshot, and the `--beside`
/// suffix of the session.
#[derive(Clone, Copy)]
struct Planned<'p> {
    row: &'p RowSchema,
    extend: bool,
    snapshot: bool,
    beside: Option<&'p [u8]>,
    /// Suffixed names this session already writes (`Inbound::claimed`).
    claimed: &'p HashSet<Vec<u8>>,
    /// Lands beside whatever its live path holds ([`set_beside_always`]).
    always: bool,
}

/// Where a seat lands beside its live path (`--beside`), decided by what
/// the beside path holds ([`beside_target`]).
enum BesideTarget {
    /// Nothing there: a fresh publish.
    Fresh,
    /// This seat's bytes are there already (this store's own earlier beside
    /// output, or a file that verifies): adopted under the beside row.
    Adopt(std::fs::File, Arc<std::fs::File>, StatIdentity),
    /// This store's own beside output holds other bytes: superseded by the
    /// exchange (WP0(d)).
    Supersede(Box<OwnedOutput>),
}

/// Decide the beside path of `row` (`--beside`): free, this store's own
/// output (adopted when it verifies, superseded when it does not, refused
/// `DESTINATION_EXCHANGE_UNSUPPORTED` with no exchange), a foreign file
/// that verifies (adopted), or a foreign file with other bytes, refused
/// `DESTINATION_OCCUPIED`: nothing is ever replaced beside either. A
/// snapshot's beside path with a sidecar beside it is refused the same way
/// (OI-1003-Q146).
///
/// Ownership is this seat's beside output's only ([`owned_beside`], review of
/// #280, finding 4). A refusal for a foreign file carries that file's
/// settled identity, which it may be remembered under (finding 3).
fn beside_target(
    context: &ReceiveContext<'_>,
    beside: &RowSchema,
    manifest: &Manifest,
    snapshot: bool,
) -> std::result::Result<BesideTarget, (BulkloadRefusal, Option<StatIdentity>)> {
    let plain = |refusal| (refusal, None);
    if snapshot
        && context
            .target
            .sqlite_sidecars_present(&beside.rel_path)
            .map_err(plain)?
    {
        return Err(plain(BulkloadRefusal::DestinationOccupied));
    }
    let Some((file, parent)) = context.target.existing(beside).map_err(plain)? else {
        return Ok(BesideTarget::Fresh);
    };
    let owned = owned_beside(context.store, context.authority, beside, &file).map_err(plain)?;
    let (verified, settled) = verify_settled(context.target.path(), &file, beside, manifest);
    match (verified, owned) {
        (Ok(identity), _) => Ok(BesideTarget::Adopt(file, parent, identity)),
        (Err(BulkloadRefusal::DestinationOccupied), Some(owned)) => {
            if context.target.exchange_supported(beside).map_err(plain)? {
                Ok(BesideTarget::Supersede(Box::new(owned)))
            } else {
                Err(plain(BulkloadRefusal::DestinationExchangeUnsupported))
            }
        }
        (Err(BulkloadRefusal::DestinationOccupied), None) => {
            Err((BulkloadRefusal::DestinationOccupied, settled))
        }
        (Err(refusal), _) => Err(plain(refusal)),
    }
}

/// [`beside_target`] judged against a staged copy of the seat instead of a
/// manifest (a failed prefix extension has the whole seat staged and no
/// manifest at hand): an existing beside file equal to `staged` byte for
/// byte is adopted; one with other bytes is superseded when it is this
/// store's own and refused `DESTINATION_OCCUPIED` otherwise.
fn beside_existing(
    context: &ReceiveContext<'_>,
    beside: &RowSchema,
    staged: &std::fs::File,
) -> Result<BesideTarget> {
    let Some((file, parent)) = context.target.existing(beside)? else {
        return Ok(BesideTarget::Fresh);
    };
    let found = file.metadata().refuse_at("transfer::beside_existing")?;
    if found.len() == beside.size && same_bytes(&file, staged, beside.size)? {
        return Ok(BesideTarget::Adopt(
            file,
            parent,
            StatIdentity::from_metadata(&found),
        ));
    }
    match owned_beside(context.store, context.authority, beside, &file)? {
        Some(owned) if context.target.exchange_supported(beside)? => {
            Ok(BesideTarget::Supersede(Box::new(owned)))
        }
        Some(_) => Err(BulkloadRefusal::DestinationExchangeUnsupported),
        None => Err(BulkloadRefusal::DestinationOccupied),
    }
}

/// Whether the first `size` bytes of `a` and `b` are the same, read in 1
/// MiB pieces (`read_dest_verify_existing_bytes`).
fn same_bytes(a: &std::fs::File, b: &std::fs::File, size: u64) -> Result<bool> {
    const PIECE: u64 = 1 << 20;
    let mut mine = vec![0_u8; usize::try_from(PIECE).unwrap_or(usize::MAX)];
    let mut theirs = vec![0_u8; mine.len()];
    let mut at = 0_u64;
    while at < size {
        let length = usize::try_from((size - at).min(PIECE)).unwrap_or(usize::MAX);
        let (Some(left), Some(right)) = (mine.get_mut(..length), theirs.get_mut(..length)) else {
            return Err(BulkloadRefusal::BudgetExceeded);
        };
        let read_left =
            crate::io::sys::pread_full(a, left, at).refuse_at("transfer::same_bytes")?;
        let read_right =
            crate::io::sys::pread_full(b, right, at).refuse_at("transfer::same_bytes")?;
        counters::add_len(Counter::DestVerifyRead, length);
        if read_left != length || read_right != length || left != right {
            return Ok(false);
        }
        at += length as u64;
    }
    Ok(true)
}

#[allow(clippy::too_many_lines)] // Validate, adopt or supersede, stage, fill, kick.
fn plan_file(
    context: &ReceiveContext<'_>,
    planned: Planned<'_>,
    manifest: &Manifest,
    salvaged_from: &mut Vec<usize>,
) -> Plan {
    let Planned {
        row,
        extend,
        snapshot,
        beside,
        claimed,
        always,
    } = planned;
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
    // `--beside` (plan of record 2026-10-19, C1/C2): a snapshot whose live
    // path has a sidecar beside it never lands there, whatever the path
    // holds; it lands beside.
    // So does a seat that lands beside whatever its live path holds.
    let live_sidecar = (always && beside.is_some())
        || (snapshot
            && beside.is_some()
            && match context.target.sqlite_sidecars_present(&row.rel_path) {
                Ok(present) => present,
                Err(refusal) => return Plan::Refuse(refusal),
            });
    let beside_at = || beside.map(|suffix| beside_path(&row.rel_path, suffix));
    // Set when this entry lands beside its live path: the beside path, and
    // what the live path's refusal may be remembered as (finding 3).
    let mut land_beside: Option<Vec<u8>> = None;
    let mut live_refused: Option<(StatIdentity, RefusedOutput)> = None;
    let mut supersede = match context.target.existing(row) {
        Ok(Some((file, parent))) => {
            // Under `--beside` a live path is never replaced, whoever wrote
            // it (review of #280, finding 1): this store's own output there,
            // adopted or published on an earlier pass, is judged as any
            // other file is. A grown append-only file is extended through
            // `O_APPEND`; any other difference lands beside.
            let owned = if beside.is_some() {
                Ok(None)
            } else {
                owned_output(context.store, context.authority, row, &file)
            };
            match owned {
                Ok(_) if live_sidecar => {
                    land_beside = beside_at();
                    None
                }
                Ok(Some(owned)) => {
                    let (verified, settled) =
                        verify_settled(context.target.path(), &file, row, manifest);
                    match verified {
                        // The seat was touched, not changed: nothing to
                        // replace.
                        Ok(identity) => return Plan::Adopt(file, parent, Some(identity), None),
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
                // M2r2 A-103: a grown file is extended, any other is
                // adopted or refused as before. Under `--beside` a file
                // that is not the seat's (a failed prefix proof, or other
                // bytes, judged now) lands beside instead.
                Ok(None) => {
                    let plan = if extend {
                        plan_extend(context, row, manifest, (file, parent), salvaged_from)
                    } else {
                        Plan::Adopt(file, parent, None, None)
                    };
                    let Some(suffix) = beside else {
                        return plan;
                    };
                    match plan {
                        Plan::Adopt(file, parent, None, None) => {
                            let (verified, settled) =
                                verify_settled(context.target.path(), &file, row, manifest);
                            match verified {
                                Ok(identity) => {
                                    return Plan::Adopt(file, parent, Some(identity), None)
                                }
                                Err(BulkloadRefusal::DestinationOccupied) => {
                                    land_beside = Some(beside_path(&row.rel_path, suffix));
                                    live_refused =
                                        settled.map(|identity| (identity, RefusedOutput::Occupied));
                                }
                                Err(refusal) => return Plan::Refuse(refusal),
                            }
                        }
                        Plan::NotPrefix(identity) => {
                            land_beside = Some(beside_path(&row.rel_path, suffix));
                            live_refused = Some((identity, RefusedOutput::NotPrefix));
                        }
                        other => return other,
                    }
                    None
                }
                Err(refusal) => return Plan::Refuse(refusal),
            }
        }
        Ok(None) if live_sidecar => {
            land_beside = beside_at();
            None
        }
        Ok(None) => None,
        // `--beside`: a node of another kind at the live path (a symlink:
        // review of #280, finding 5) lands beside.
        Err(BulkloadRefusal::DestinationOccupied | BulkloadRefusal::DestinationKindMismatch)
            if beside.is_some() =>
        {
            land_beside = beside_at();
            None
        }
        Err(refusal) => return Plan::Refuse(refusal),
    };
    if let Some(beside_rel) = &land_beside {
        // Another seat of this session writes this name (finding 4).
        if claimed.contains(beside_rel) {
            return Plan::BesideRefused {
                refusal: BulkloadRefusal::DestinationOccupied,
                live: None,
                beside: None,
            };
        }
        let beside = RowSchema {
            rel_path: beside_rel.clone(),
            ..row.clone()
        };
        match beside_target(context, &beside, manifest, snapshot) {
            Ok(BesideTarget::Fresh) => {}
            Ok(BesideTarget::Adopt(file, parent, identity)) => {
                return Plan::Adopt(file, parent, Some(identity), Some(beside_rel.clone()))
            }
            Ok(BesideTarget::Supersede(owned)) => supersede = Some(owned),
            Err((refusal, settled)) => {
                return Plan::BesideRefused {
                    refusal,
                    live: live_refused,
                    beside: settled,
                }
            }
        }
    }
    let staged = match context.target.stage(row) {
        Ok(staged) => staged,
        Err(refusal) => return Plan::Refuse(refusal),
    };
    let staged = match &land_beside {
        Some(_) => match staged.with_suffix(beside.unwrap_or_default()) {
            Ok(staged) => staged,
            Err(refusal) => return Plan::Refuse(refusal),
        },
        None => staged,
    };
    let mut missing = Vec::new();
    let mut hints = Vec::with_capacity(order.len());
    let mut outputs = HashMap::new();
    let mut placed = 0_u64;
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
            Ok(Some(())) => {
                placed = placed.saturating_add(size.saturating_mul(offsets.len() as u64));
            }
            Ok(None) => missing.push(index),
            Err(refusal) => {
                let _ = staged.discard();
                return Plan::Refuse(refusal);
            }
        }
    }
    // What was filled here is written back while the source captures the
    // rest, before `NeedChunks` goes out (item 1a, #217 review): a delta's
    // changed file is mostly local bytes, and no wire chunk would kick them.
    if let Err(refusal) = kick_after(staged.file(), 0, placed) {
        let _ = staged.discard();
        return Plan::Refuse(refusal);
    }
    Plan::Write(Staging {
        staged,
        supersede,
        missing,
        placements,
        hints,
        placed,
        extend: None,
        beside: land_beside,
    })
}

/// M2r2 A-103: an existing regular file with no owning record whose bytes
/// are a strict byte prefix of the manifest's (an append-only transcript
/// that grew on the source) is extended in place by the tail instead of
/// refused `DESTINATION_OCCUPIED`, when [`PREFIX_EXTEND_ENV`]'s scope covers
/// its path.
///
/// Tried only for an [`extend_candidate`] that this user can open for
/// appending and that holds at least one whole chunk of the manifest (a
/// shorter file is refused as before: judging it would take a chunk over
/// the wire for a seat that may be refused). The proof reads the file
/// once: every manifest chunk that ends within its length is read back
/// from it and must hash to the chunk's digest
/// (`read_dest_verify_existing_bytes`); a failed proof is
/// [`Plan::NotPrefix`], with no chunk asked for. The tail from the first
/// chunk that does not end within the file is staged in a temporary of its
/// own and filled as any staged file is (locally held chunks, then the
/// wire); its head, the part of that chunk the file already holds, is
/// compared at the end, before a byte is appended
/// (`Inbound::finish_extend`). A file that is not a candidate keeps
/// today's answer: `Plan::Adopt`, which refuses it `DESTINATION_OCCUPIED`
/// when its bytes are not the manifest's.
fn plan_extend(
    context: &ReceiveContext<'_>,
    row: &RowSchema,
    manifest: &Manifest,
    (file, parent): (std::fs::File, Arc<std::fs::File>),
    salvaged_from: &mut Vec<usize>,
) -> Plan {
    let root = context.target.path();
    let Some((identity, prior)) = extend_candidate(&file, row, root) else {
        return Plan::Adopt(file, parent, None, None);
    };
    // Writable, and the very file read: else not a candidate.
    let writer = match context.target.open_output_append(&row.rel_path) {
        Ok(writer)
            if writer
                .metadata()
                .is_ok_and(|found| StatIdentity::from_metadata(&found) == identity) =>
        {
            writer
        }
        _ => return Plan::Adopt(file, parent, None, None),
    };
    let (base, hasher) = match prove_prefix(&file, row, manifest, (identity, prior)) {
        Proof::Proven(base, hasher) => (base, *hasher),
        Proof::NotPrefix => return Plan::NotPrefix(identity),
        Proof::Untried => return Plan::Adopt(file, parent, None, None),
    };
    let staged = match context.target.stage(row) {
        Ok(staged) => staged,
        Err(refusal) => return Plan::Refuse(refusal),
    };
    let mut placements: HashMap<[u8; 32], (u64, Vec<u64>)> = HashMap::new();
    let mut order = Vec::new();
    let mut hints = Vec::new();
    let mut seen = HashSet::new();
    let mut straddle = None;
    let mut offset = 0_u64;
    for (index, chunk) in manifest.chunks.iter().enumerate() {
        if seen.insert(chunk.digest) {
            hints.push(ChunkHint {
                digest: chunk.digest,
                offset,
                size: chunk.size,
            });
        }
        if offset == base {
            straddle = Some((chunk.digest, chunk.size));
        }
        if offset >= base {
            let Ok(index) = u32::try_from(index) else {
                let _ = staged.discard();
                return Plan::Refuse(BulkloadRefusal::BudgetExceeded);
            };
            let entry = placements.entry(chunk.digest).or_insert_with(|| {
                order.push((chunk.digest, index));
                (chunk.size, Vec::new())
            });
            entry.1.push(offset - base);
        }
        offset = offset.saturating_add(chunk.size);
    }
    let mut missing = Vec::new();
    let mut outputs = HashMap::new();
    let mut placed = 0_u64;
    for (digest, index) in order {
        let Some((size, offsets)) = placements.get(&digest) else {
            continue;
        };
        let filled =
            local_chunk(context, &mut outputs, &digest, *size, salvaged_from).and_then(|data| {
                data.map(|data| place(staged.file(), &data, offsets))
                    .transpose()
            });
        match filled {
            Ok(Some(())) => {
                placed = placed.saturating_add(size.saturating_mul(offsets.len() as u64));
            }
            Ok(None) => missing.push(index),
            Err(refusal) => {
                let _ = staged.discard();
                return Plan::Refuse(refusal);
            }
        }
    }
    if let Err(refusal) = kick_after(staged.file(), 0, placed) {
        let _ = staged.discard();
        return Plan::Refuse(refusal);
    }
    Plan::Write(Staging {
        staged,
        supersede: None,
        missing,
        placements,
        hints: Vec::new(),
        placed,
        extend: Some(Box::new(Extend {
            file,
            parent,
            writer,
            identity,
            prior,
            base,
            hasher,
            hints,
            straddle,
        })),
        beside: None,
    })
}

/// Whether `file` may be extended to `row` (M2r2 A-103): a regular file
/// with one link (a hard link's other name would change with it), this
/// user's, the source's mode, `0 < len < size`, and settled: neither its
/// content nor its inode changed within the racy window of now, by the
/// destination's clock, so no writer is at it (an actively written file is
/// refused as before, and tried again once it is quiet). Its identity and
/// length. `root` is the destination root.
fn extend_candidate(
    file: &std::fs::File,
    row: &RowSchema,
    root: &Path,
) -> Option<(StatIdentity, u64)> {
    let found = file.metadata().ok()?;
    let prior = found.len();
    let identity = StatIdentity::from_metadata(&found);
    let quiet_since = capture_clock(root).saturating_sub(RACY_GRANULARITY_NS);
    (found.is_file()
        && found.nlink() == 1
        && found.uid() == crate::io::sys::effective_uid()
        && prior > 0
        && prior < row.size
        && found.mode() & 0o7777 == row.mode & 0o7777
        && identity.mtime_ns < quiet_since
        && identity.ctime_ns < quiet_since)
        .then_some((identity, prior))
}

/// The next piece of a prefix extension's tail (review of #280, finding 8):
/// read from `staged` at `offset`, at most `remaining` bytes, ending at its
/// last newline. A piece with no newline in its first megabyte is read on
/// until one (or the tail's end), so it is one whole record. `buffer` is
/// grown as needed; the piece is its front.
fn record_piece<'b>(
    staged: &std::fs::File,
    (offset, remaining): (u64, u64),
    buffer: &'b mut Vec<u8>,
) -> Result<&'b [u8]> {
    const PIECE: u64 = 1 << 20;
    let mut read = 0_u64;
    loop {
        let want = (remaining - read).min(PIECE);
        let start = usize::try_from(read).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        let end = usize::try_from(read + want).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        if buffer.len() < end {
            buffer.resize(end, 0);
        }
        let slot = buffer
            .get_mut(start..end)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if crate::io::sys::pread_full(staged, slot, offset + read)
            .refuse_at("transfer::record_piece")?
            != slot.len()
        {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        let newline = slot.iter().rposition(|byte| *byte == b'\n');
        read += want;
        if read == remaining {
            return buffer.get(..end).ok_or(BulkloadRefusal::BudgetExceeded);
        }
        if let Some(at) = newline {
            return buffer
                .get(..start + at + 1)
                .ok_or(BulkloadRefusal::BudgetExceeded);
        }
    }
}

/// What [`prove_prefix`] found.
enum Proof {
    /// `[0, base)` is the manifest's, `base > 0`; BLAKE3 over it so far.
    Proven(u64, Box<blake3::Hasher>),
    /// A chunk within the file is not the manifest's.
    NotPrefix,
    /// Nothing to prove with (the file's bytes all lie in the first
    /// chunk), or the file moved while it was read.
    Untried,
}

/// The proof half of [`plan_extend`] for the candidate `file` of `identity`
/// and length `prior`: every manifest chunk ending within it must be its
/// bytes, and at least one must.
fn prove_prefix(
    file: &std::fs::File,
    row: &RowSchema,
    manifest: &Manifest,
    (identity, prior): (StatIdentity, u64),
) -> Proof {
    if manifest.size() != Some(row.size) {
        return Proof::Untried;
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; crate::hash::CDC_MAX_BYTES as usize];
    let mut offset = 0_u64;
    let mut base = None;
    for chunk in &manifest.chunks {
        let Some(end) = offset.checked_add(chunk.size) else {
            return Proof::Untried;
        };
        if end > prior {
            base = Some(offset);
            break;
        }
        let Some(slot) = usize::try_from(chunk.size)
            .ok()
            .and_then(|size| buffer.get_mut(..size))
        else {
            return Proof::Untried;
        };
        if crate::io::sys::pread_full(file, slot, offset).ok() != Some(slot.len()) {
            return Proof::Untried;
        }
        counters::add_len(Counter::DestVerifyRead, slot.len());
        if counters::hash(Counter::HashVerifyExisting, slot) != chunk.digest {
            return Proof::NotPrefix;
        }
        hasher.update(slot);
        offset = end;
    }
    // At least one whole chunk proven: a file whose held bytes all lie in
    // the first chunk could only be judged with that chunk's bytes from the
    // source, and a refused seat costs no wire byte (P21, #187).
    let Some(base) = base.filter(|base| *base > 0) else {
        return Proof::Untried;
    };
    let moved = file.metadata().map_or(true, |found| {
        StatIdentity::from_metadata(&found) != identity
    });
    if moved {
        return Proof::Untried;
    }
    Proof::Proven(base, Box::new(hasher))
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
    match read_wire(input, None)? {
        Wire::Frame(frame) => Ok(frame),
        Wire::Handed { .. } => Err(BulkloadRefusal::FrameCodec),
    }
}

/// What [`read_wire`] read.
enum Wire {
    /// A wire v6 frame.
    Frame(Frame),
    /// A handed frame: a data header whose chunk the in-process source
    /// handed over (OI-1003-Q154 lever 2).
    Handed {
        header: DataHeader,
        chunk: SourceChunk,
    },
}

/// Read one frame, as [`read_frame`]; with `hand`, a handed frame too,
/// with the chunk it names. Without `hand` a handed frame is refused
/// `FRAME_CODEC` before its header is read: it is no wire v6 frame.
fn read_wire<R: Read>(input: &mut R, hand: Option<&HandIn>) -> Result<Wire> {
    let mut prefix = [0_u8; FRAME_HEADER_BYTES];
    input
        .read_exact(&mut prefix)
        .map_err(|error| crate::refuse::transport(&error, "transfer::read_frame"))?;
    let [a, b, c, d, tag] = prefix;
    let length = u32::from_be_bytes([a, b, c, d]) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let body = length.checked_sub(1).ok_or(BulkloadRefusal::FrameCodec)?;
    if tag == handoff::TAG_HANDED {
        let (Some(hand), handoff::HANDED_BODY_BYTES) = (hand, length) else {
            return Err(BulkloadRefusal::FrameCodec);
        };
        let mut header = [0_u8; DATA_HEADER_BYTES];
        input
            .read_exact(&mut header)
            .map_err(|error| crate::refuse::transport(&error, "transfer::read_frame"))?;
        let header = DataHeader::from_bytes(&header)?;
        if header.size as usize > MAX_DATA_PAYLOAD {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let chunk = hand.take(&header)?;
        counters::bump(Counter::WireFramesReceived);
        counters::add_len(Counter::WireBytesReceived, FRAME_HEADER_BYTES + body);
        return Ok(Wire::Handed { header, chunk });
    }
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
            .map_err(|error| crate::refuse::transport(&error, "transfer::read_frame"))?;
        let header = DataHeader::from_bytes(&header)?;
        if header.size as usize != size {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let mut payload = vec![0_u8; size];
        input
            .read_exact(&mut payload)
            .map_err(|error| crate::refuse::transport(&error, "transfer::read_frame"))?;
        Frame::Data { header, payload }
    } else {
        let mut bytes = vec![0_u8; body];
        input
            .read_exact(&mut bytes)
            .map_err(|error| crate::refuse::transport(&error, "transfer::read_frame"))?;
        Frame::decode_body(tag, &bytes)?
    };
    counters::bump(Counter::WireFramesReceived);
    counters::add_len(Counter::WireBytesReceived, FRAME_HEADER_BYTES + body);
    Ok(Wire::Frame(frame))
}

/// Write one data frame: its fixed header and the payload with one gathered
/// write, never copying the payload into a frame buffer.
fn write_data<W: Write>(output: &mut W, header: &DataHeader, payload: &[u8]) -> Result<()> {
    if header.size as usize != payload.len() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    // Test-only: a chunk sent with one byte flipped, so the destination's
    // verify refuses it (#217 review: a bad chunk mid-stream).
    #[cfg(test)]
    let corrupted;
    #[cfg(test)]
    let payload = if CORRUPT_DIGESTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&header.digest)
    {
        corrupted = payload
            .iter()
            .enumerate()
            .map(|(at, byte)| if at == 0 { !byte } else { *byte })
            .collect::<Vec<u8>>();
        corrupted.as_slice()
    } else {
        payload
    };
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
            Err(error) => return Err(crate::refuse::transport(&error, "transfer::write_data")),
        }
    }
    output
        .flush()
        .map_err(|error| crate::refuse::transport(&error, "transfer::write_data"))?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, prefix.len() + payload.len());
    Ok(())
}

/// Test-only: data frames of these digests are sent corrupted.
#[cfg(test)]
pub(crate) static CORRUPT_DIGESTS: Mutex<Vec<[u8; 32]>> = Mutex::new(Vec::new());

/// Test-only: a receive job of one of these digests panics on its worker.
#[cfg(test)]
pub(crate) static PANIC_DIGESTS: Mutex<Vec<[u8; 32]>> = Mutex::new(Vec::new());

/// Test-only mutation (OI-1003-Q154 lever 1's proof): sessions into these
/// canonical destination roots take bytes read from the transport as
/// vouched for, as a broken verify-once would. The cross-host proof must
/// catch it.
#[cfg(test)]
pub(crate) static TRUST_WIRE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test-only: the most bytes any receive pool held queued or in progress.
#[cfg(test)]
pub(crate) static POOL_HIGH_WATER: AtomicU64 = AtomicU64::new(0);

/// Write one control frame and flush it.
///
/// # Errors
/// Refuses oversized messages and broken transports.
pub fn write_control<W: Write>(output: &mut W, control: &Control) -> Result<()> {
    let encoded = control.encode()?;
    output
        .write_all(&encoded)
        .map_err(|error| crate::refuse::transport(&error, "transfer::write_control"))?;
    output
        .flush()
        .map_err(|error| crate::refuse::transport(&error, "transfer::write_control"))?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, encoded.len());
    Ok(())
}

fn path(bytes: Vec<u8>) -> PathBuf {
    std::ffi::OsString::from_vec(bytes).into()
}

mod handoff;
#[cfg(test)]
mod tests;
pub(crate) mod unrowed;
