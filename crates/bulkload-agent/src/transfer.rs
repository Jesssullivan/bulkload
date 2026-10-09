//! Native resumable transfer over one full-duplex framed stream (wire v5).
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
use std::collections::{HashMap, HashSet, VecDeque};
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
    DATA_HEADER_BYTES, FRAME_HEADER_BYTES, MAX_DATA_PAYLOAD, MAX_FRAME_BYTES, TAG_DATA,
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
    row_key, ChunkHint, LedgerItem, LedgerRecord, LedgerSink, Manifest, OutputRecord,
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
    /// Outputs skipped using a previously persisted completion.
    pub reused: u64,
    /// Content bytes received across the transport.
    pub bytes_received: u64,
    /// Actual source file bytes read while preparing missing captures.
    pub source_bytes_read: u64,
    /// Relative paths and refusal codes. Contents and credentials are never logged.
    pub refusals: Vec<(Vec<u8>, String)>,
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
}

impl TransferStats {
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

/// Run the same framed protocol locally over a bounded Unix stream pair.
///
/// # Errors
/// Refuses overlapping roots, malformed data and transport or source failures.
pub fn copy(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
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
            receive(
                &mut receiver,
                &mut output,
                source,
                source_state,
                destination,
                destination_state,
            )
        };
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        drop(receiver);
        let served = producer.join().map_err(|_| BulkloadRefusal::WorkerLost)?;
        match (result, served) {
            // The destination refused for a reason of its own (its setup,
            // which now runs while the source opens its store, item 1b): the
            // source's broken stream is only the echo of it.
            (Err(refusal), Err(_)) if !matches!(refusal, BulkloadRefusal::Io(_)) => Err(refusal),
            (result, served) => {
                served?;
                result
            }
        }
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
/// their reservation against [`RETAIN_BYTES`], released on drop.
struct Retained {
    chunks: Vec<Arc<Vec<u8>>>,
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
            reserved: bytes,
            budget: Arc::clone(budget),
        })
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        self.budget.fetch_add(self.reserved, Ordering::AcqRel);
    }
}

/// Work for a capture thread.
enum Job {
    /// Read, chunk and stream entry `entry`.
    Send { entry: u64, row: Arc<RowSchema> },
    /// Produce entry `entry`'s manifest, from the ledger when it can.
    Manifest { entry: u64, row: Arc<RowSchema> },
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
        /// The refusal to remember under this row key (#186): the seat's
        /// header gave it, and the seat was not racy when it was sniffed.
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

/// What every capture thread shares.
struct SourceWork<'a> {
    /// The canonical source root, which keys test-only hooks and clocks.
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
    let Frame::Control(Control::Open {
        proto,
        wire_id: id,
        root,
        state,
    }) = read_frame(&mut input)?
    else {
        return Err(BulkloadRefusal::ProtocolStateViolation);
    };
    if proto != PROTO_VERSION || id != wire_id() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    let root = std::fs::canonicalize(path(root)).refuse_at("transfer::serve")?;
    let state = path(state);
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
    let walker = Walker::new(root_fd.as_fd(), true)?;
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

/// The sending thread's writes to the wire, timed (`send_write_ns`, #217
/// review): where the source's one serial sending stage blocks on the wire.
struct TimedWrite<W>(W);

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
    committer: &'a Committer<LedgerSink>,
    jobs: Sender<Job>,
    gate: &'a WalkGate,
    /// Walked rows not yet offered, in walk order.
    pending: VecDeque<RowSchema>,
    /// Whether the walk thread has yielded its last item.
    walked: bool,
    /// One slot per offered entry, by entry number.
    entries: Vec<SourceEntry>,
    /// Each offered entry's row, dropped once the entry is retired.
    rows: Vec<Option<Arc<RowSchema>>>,
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
    let mut output = TimedWrite(output);
    let mut outbound = Outbound {
        output: &mut output,
        committer,
        jobs,
        gate,
        pending: VecDeque::new(),
        walked: false,
        entries: Vec::new(),
        rows: Vec::new(),
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
            let Some(row) = self.pending.pop_front() else {
                break;
            };
            let entry = self.entries.len() as u64;
            write_control(
                self.output,
                &Control::Entry {
                    entry,
                    row: row.clone(),
                },
            )?;
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
                self.pending.push_back(row);
                return Ok(());
            }
            WalkItem::Refused(seat) => write_control(
                self.output,
                &Control::Refused {
                    entry: None,
                    rel_path: seat.rel_path,
                    code: seat.refusal.code().to_owned(),
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
                    },
                )?;
            }
        }
        Ok(())
    }

    fn decide(&mut self, entry: u64, decision: &Decision) -> Result<()> {
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
                }),
            ),
            Decision::WantManifest => (
                SourceEntry::Queued,
                Some(Job::Manifest {
                    entry,
                    row: Arc::clone(row),
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
    // What a capture's header sniff refused, to remember (#186).
    let mut sniffed = None;
    let (entry, seat, outcome) = match job {
        Job::Send { entry, row } => {
            let outcome = remembered_refusal(work, store, &row).and_then(|()| {
                send_capture(work, entry, &row, events, &mut bytes_read, &mut sniffed)
            });
            (entry, Some(row), outcome)
        }
        Job::Manifest { entry, row } => {
            let outcome = remembered_refusal(work, store, &row).and_then(|()| {
                match manifest_capture(work, store, &row, &mut bytes_read, &mut sniffed) {
                    Ok(Some((record, manifest, retained, racy))) => Ok(Event::Manifest {
                        entry,
                        record,
                        manifest,
                        retained,
                        racy,
                        bytes_read,
                    }),
                    // No ledger row and no room to keep the chunks: building
                    // a manifest first would read the seat twice (R25).
                    // Stream it instead; the destination takes data in place
                    // of a manifest.
                    Ok(None) => {
                        send_capture(work, entry, &row, events, &mut bytes_read, &mut sniffed)
                    }
                    Err(refusal) => Err(refusal),
                }
            });
            (entry, Some(row), outcome)
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
            None,
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
        // Remembered under the seat's row key, as a capture would be.
        remember: sniffed.and_then(|refused| {
            let row = seat.as_ref()?;
            Some((row_key(work.authority, row).ok()?, refused))
        }),
    })
}

/// #186 (R25): a seat whose refusal the ledger remembers under this exact
/// row key (its path and stat identity) is refused again from that record,
/// with the same code, before the file is opened: 0 source bytes. A seat
/// whose identity moved has another key, and is sniffed again.
fn remembered_refusal(work: &SourceWork<'_>, store: &Store, row: &RowSchema) -> Result<()> {
    ledger_read(
        work.ledger,
        store.refused_seat(&row_key(work.authority, row)?),
    )?
    .map_or(Ok(()), |refused| {
        counters::bump(Counter::TransferRefusedSeatsRemembered);
        Err(refused.refusal())
    })
}

/// Read, chunk and stream one file; its `End` carries the capture to record.
fn send_capture(
    work: &SourceWork<'_>,
    entry: u64,
    row: &RowSchema,
    events: &Sender<Event>,
    bytes_read: &mut u64,
    sniffed: &mut Option<RefusedSeat>,
) -> Result<Event> {
    let key = row_key(work.authority, row)?;
    let sink = |index, offset, digest, data: Vec<u8>| {
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
    };
    let (chunks, racy) = capture_file(work, row, bytes_read, sniffed, sink)?;
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

/// A manifest to offer: the row key to record (none when it came from the
/// ledger, or when the capture was racy), the manifest, its retained chunks,
/// and whether the capture was racy (#86).
type Offer = (Option<Vec<u8>>, Manifest, Option<Retained>, bool);

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
/// chunks are all kept in memory for the requests that follow. `None` when
/// the retention budget cannot hold the file: the caller streams it instead
/// of reading it twice (#77 review F1).
fn manifest_capture(
    work: &SourceWork<'_>,
    store: &Store,
    row: &RowSchema,
    bytes_read: &mut u64,
    sniffed: &mut Option<RefusedSeat>,
) -> Result<Option<Offer>> {
    let key = row_key(work.authority, row)?;
    if let Some(manifest) = ledger_read(work.ledger, store.capture(&key))? {
        if manifest.size() == Some(row.size) && manifest.chunks.len() <= MAX_MANIFEST_CHUNKS {
            return Ok(Some((None, manifest, None, false)));
        }
    }
    // The ledger has no usable row under this key, so the seat is read to
    // build the manifest. This is the whole cost of a row WP0(g) lost; a
    // changed or never recorded seat is counted here too.
    counters::bump(Counter::SourceLedgerMissReads);
    // Every chunk a fresh manifest names must be kept, so the requests that
    // follow are served from memory: a seat is read at most once a session.
    let Some(mut retained) = Retained::reserve(&work.retain, row.size) else {
        return Ok(None);
    };
    let (chunks, racy) = capture_file(work, row, bytes_read, sniffed, |_, _, _, data| {
        retained.chunks.push(Arc::new(data));
        Ok(())
    })?;
    Ok(Some((
        (!racy).then_some(key),
        Manifest::new(chunks),
        Some(retained),
        racy,
    )))
}

/// Send the requested chunks of an offered manifest, from memory when they
/// were retained, otherwise read again at their offsets and re-verified
/// against the manifest under an unchanged stat identity.
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
    let held = retained.filter(|held| held.chunks.len() == manifest.chunks.len());
    let opened = if held.is_none() && !indices.is_empty() {
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
        size: row.size,
        // The caller sets the offer's own `racy`.
        racy: false,
        bytes_read: *bytes_read,
    })
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
/// bytes, counted as such and chunked with the rest.
///
/// Returns the chunks and whether the capture was racy (#86): the seat's
/// mtime or ctime falls within [`RACY_GRANULARITY_NS`] of the clock read
/// before the file was opened, or later than the clock read after the final
/// stat check. A same-size rewrite in that tick can keep the stat identity,
/// so the identity cannot vouch for the bytes read, exactly as in the Git
/// carry census (R-N76): the capture is sent, but never recorded as a reuse
/// key on either side.
fn capture_file(
    work: &SourceWork<'_>,
    row: &RowSchema,
    bytes_read: &mut u64,
    sniffed: &mut Option<RefusedSeat>,
    mut sink: impl FnMut(u32, u64, [u8; 32], Vec<u8>) -> Result<()>,
) -> Result<(Vec<ChunkSpec>, bool)> {
    if row.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if row.rel_path.ends_with(b"-wal")
        || row.rel_path.ends_with(b"-shm")
        || row.rel_path.ends_with(b"-journal")
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    #[cfg(feature = "fault-injection")]
    let file_path = work.root.join(crate::walk::rel_path(&row.rel_path));
    let started_ns = capture_clock(work.root);
    let file = open_source(work, row)?;
    let expected = StatIdentity::from_row(row);
    if StatIdentity::from_metadata(&file.metadata().refuse_at("transfer::capture_file")?)
        != expected
    {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let mut prefix = Vec::new();
    SourceReader {
        file: &file,
        offset: 0,
    }
    .take(SNIFF_BYTES)
    .read_to_end(&mut prefix)
    .refuse_at("transfer::capture_file")?;
    if prefix.starts_with(b"SQLite format 3\0")
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x82])
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x83])
    {
        counters::add_len(Counter::SourceSniff, prefix.len());
        #[cfg(test)]
        after_sniff(work.root);
        let unmoved = file
            .metadata()
            .is_ok_and(|after| StatIdentity::from_metadata(&after) == expected);
        if unmoved && !crate::git_carry::racy(row, started_ns, capture_clock(work.root)) {
            *sniffed = Some(RefusedSeat::SqliteHeader);
        }
        return Err(BulkloadRefusal::SqliteStateChanged);
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
    Ok((chunks, racy))
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
) -> Result<()> {
    for (rel_path, outcome) in committer.finish()? {
        match outcome {
            Ok(()) => stats.completed += 1,
            Err(refusal) => stats.refusals.push((rel_path, refusal.code().to_owned())),
        }
    }
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

/// Where in handling one chunk an entry's content failed. Within one chunk
/// index the order is fixed: its verify (or its size), then the stage of the
/// entry's file, then its write, then its write-back kick (#217 review), so
/// the refusal is the same whichever thread found which failure first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
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
    payload: Vec<u8>,
    /// False when the receiving thread already verified it: the chunk that
    /// staged its entry's file (#217 review).
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
/// received bytes), then write it. `on_worker` picks the timers.
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
        let verified = payload.len() <= crate::hash::CDC_MAX_BYTES as usize
            && counters::hash(Counter::HashWireVerify, &payload) == digest;
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
            if let Err((step, refusal)) = place_counted(target, &payload, offsets) {
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

/// A streamed entry: chunks arrive in order and are written at once.
struct Streaming {
    row: RowSchema,
    key: Vec<u8>,
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
    /// the receive workers.
    fn accept(
        &mut self,
        target: &Destination,
        open: &mut usize,
        header: &DataHeader,
        payload: Vec<u8>,
        verify: impl FnOnce(&[u8]) -> bool,
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
        let mut job_verifies = true;
        if self.failure.is_none() && self.staged.is_none() {
            job_verifies = false;
            if verify(&payload) {
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

    fn new(row: RowSchema, key: Vec<u8>, adopt: bool) -> Self {
        Self {
            row,
            key,
            staged: None,
            target: None,
            specs: Vec::new(),
            offset: 0,
            hints: Vec::new(),
            seen: HashSet::new(),
            failure: None,
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

/// An entry the destination expects content for.
enum Incoming {
    Streaming(Streaming),
    AwaitManifest { row: RowSchema, key: Vec<u8> },
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
    /// The receive workers, or `None` to verify and write inline.
    pool: Option<RecvPool>,
    /// New directories waiting to be created together, and the walk frames
    /// held back behind them (OI-1003-Q143 item 2).
    batch: DirectoryBatch,
    /// A created batch's outcome for each of its directories, taken when
    /// the directory's held entry is decided.
    batched: HashMap<Vec<u8>, Result<()>>,
}

/// A walk frame: what a batch of new directories holds back.
enum Walked {
    Entry { entry: u64, row: RowSchema },
    Refused { rel_path: Vec<u8>, code: String },
    EngineTemporary { rel_path: Vec<u8> },
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

/// Receive an ordinary-file carry. A divergent output is replaced only when
/// it is this store's own, untouched since its row was written (WP0(d)); any
/// other is refused and kept.
///
/// # Errors
/// Refuses protocol errors and unsafe roots. Per-path conflicts remain in stats.
#[allow(clippy::too_many_lines)] // Setup, the workers' scope, the frame loop, the tail.
pub fn receive<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    source: &Path,
    source_state: &Path,
    destination: &Path,
    destination_state: &Path,
) -> Result<TransferStats> {
    let setup_timer = PhaseTimer(&RECV_SETUP_NS, Instant::now());
    // S2: the overlap of the destination's state and root is refused before
    // either store exists, so `Open` can go out first and the source opens
    // its store while this side opens its own (OI-1003-Q143 item 1b). The
    // check on the store's own canonical root below still guards a state
    // swapped for a symlink in between.
    let destination_root = std::fs::canonicalize(destination).refuse_at("transfer::receive")?;
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
        let mut receiver = Inbound {
            output,
            target: &mut target,
            store: &store,
            authority: output_authority,
            committer: &committer,
            session: SessionChunks::with_capacity((budget / 4).clamp(1, SESSION_FILES)),
            displaced,
            stats: TransferStats::default(),
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
            pool: (workers > 0).then_some(RecvPool {
                jobs: job_queue,
                done: finished,
                bytes: 0,
                queued: 0,
            }),
            batch: DirectoryBatch::default(),
            batched: HashMap::new(),
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
            ..
        } = receiver;
        drop(pool);
        Ok((source_bytes_read?, stats, session, salvage_staged))
    });
    let (source_bytes_read, mut stats, session, salvage_staged) = streamed?;
    let _tail_timer = PhaseTimer(&RECV_TAIL_NS, Instant::now());
    stats.source_bytes_read = source_bytes_read;
    drop(session);
    finish_receive(&mut target, &store, committer, &mut stats, &salvage_staged)?;
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
                read_frame(input)?
            };
            match frame {
                Frame::Control(Control::Entry { entry, row }) => {
                    if entry != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::ProtocolStateViolation);
                    }
                    offered += 1;
                    self.walked(Walked::Entry { entry, row })?;
                }
                Frame::Control(Control::Refused {
                    entry: None,
                    rel_path,
                    code,
                }) => self.walked(Walked::Refused { rel_path, code })?,
                Frame::Control(Control::Refused {
                    entry: Some(entry),
                    rel_path,
                    code,
                }) => self.refused(entry, &rel_path, code)?,
                Frame::Control(Control::EngineTemporary { rel_path }) => {
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
                    self.data(&header, payload)?;
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
    /// One walk frame: handled now, or held back by a batch of new
    /// directories (see [`DirectoryBatch`]).
    fn walked(&mut self, frame: Walked) -> Result<()> {
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
    fn handle_walked(&mut self, frame: Walked) -> Result<()> {
        match frame {
            Walked::Entry { entry, row } => self.entry(entry, row),
            Walked::Refused { rel_path, code } => {
                self.stats.refusals.push((rel_path, code));
                Ok(())
            }
            Walked::EngineTemporary { rel_path } => {
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

    fn refuse(&mut self, rel_path: Vec<u8>, refusal: &BulkloadRefusal) {
        self.stats
            .refusals
            .push((rel_path, refusal.code().to_owned()));
    }

    /// Decide one offered entry.
    fn entry(&mut self, entry: u64, row: RowSchema) -> Result<()> {
        let census_started = Instant::now();
        let key = row_key(&self.authority, &row)?;
        let decided = match row.kind {
            FileKind::Directory => match self.batched.remove(&row.rel_path) {
                Some(created) => created.map(|()| Decision::Skip),
                None => self
                    .target
                    .directory(&row, self.store, &self.authority)
                    .map(|()| Decision::Skip),
            },
            FileKind::Symlink => self.target.symlink(&row).map(|()| Decision::Skip),
            FileKind::Regular => self.target.identity(&row).and_then(|identity| {
                if let Some(identity) = &identity {
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
                    if let Some(refused) = self.remembered_refusal(&row, &key, identity)? {
                        return Err(refused.refusal());
                    }
                    if self.adopt_unrowed(&row, &key)? {
                        return Ok(Decision::Reuse);
                    }
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
        // nothing durable is read again (R25).
        let decided = decided.and_then(|decision| match decision {
            Decision::Send | Decision::WantManifest => {
                self.admit(entry, row.size).map(|()| decision)
            }
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
                self.incoming
                    .insert(entry, Incoming::Streaming(Streaming::new(row, key, false)));
            }
            Decision::WantManifest => {
                self.incoming
                    .insert(entry, Incoming::AwaitManifest { row, key });
            }
            Decision::Skip | Decision::Reuse | Decision::Refuse { .. } => (),
        }
        write_control(self.output, &Control::Decide { entry, decision })?;
        fault_point!(ReceiveAfterDecide);
        Ok(())
    }

    /// R25's strict reading (#169): an existing output with no matching row
    /// whose capture record proves it is this entry's capture is queued for
    /// its row as an adopted publication, and the entry is answered `Reuse`,
    /// so the source reads nothing. Its commit outcome reaches the session's
    /// report like any output's. See [`unrowed`].
    fn adopt_unrowed(&mut self, row: &RowSchema, key: &[u8]) -> Result<bool> {
        let record_key = unrowed::record_key(row)?;
        let verdict = match self.target.existing(row) {
            Ok(Some((file, parent))) => match unrowed::prove(&file, row, &record_key) {
                unrowed::Verdict::Proven(identity, hints) => {
                    self.committer.submit(Publication::Adopted {
                        record: OutputRecord {
                            key: key.to_vec(),
                            rel_path: row.rel_path.clone(),
                            identity,
                            racy: false,
                            hints,
                        },
                        file,
                        parent,
                    })?;
                    self.stats.unrowed_adopted += 1;
                    counters::bump(Counter::TransferUnrowedAdopted);
                    return Ok(true);
                }
                verdict => verdict,
            },
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
    fn refused(&mut self, entry: u64, rel_path: &[u8], code: String) -> Result<()> {
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
            Incoming::AwaitManifest { row, .. } => row,
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
        self.stats.refusals.push((row.rel_path, code));
        Ok(())
    }

    /// Plan a manifest's entry and request only what cannot be filled here.
    fn manifest(&mut self, entry: u64, root: [u8; 32], chunks: Vec<ChunkSpec>) -> Result<()> {
        if chunks.len() > MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let Some(Incoming::AwaitManifest { row, key }) = self.incoming.remove(&entry) else {
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
        let plan = if manifest.is_consistent() {
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
        let (indices, target) = match &plan {
            Plan::Write(staging) => {
                self.open += 1;
                (
                    staging.missing.clone(),
                    Some(WriteTarget::new(staging.staged.file(), staging.placed)),
                )
            }
            Plan::Refuse(_) | Plan::Adopt(..) | Plan::NoExchange(_) => (Vec::new(), None),
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
    fn data(&mut self, header: &DataHeader, payload: Vec<u8>) -> Result<()> {
        let _transfer_timer = PhaseTimer(&TRANSFER_NS, Instant::now());
        let size = payload.len() as u64;
        self.granted = self
            .granted
            .checked_sub(size)
            .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
        self.stats.bytes_received = self.stats.bytes_received.saturating_add(size);
        // A source that could not keep a manifest's chunks streams the entry
        // in place of the manifest (#77 review F1).
        if let Some(Incoming::AwaitManifest { .. }) = self.incoming.get(&header.entry) {
            if let Some(Incoming::AwaitManifest { row, key }) = self.incoming.remove(&header.entry)
            {
                self.incoming.insert(
                    header.entry,
                    Incoming::Streaming(Streaming::new(row, key, true)),
                );
            }
        }
        let job = match self.incoming.get_mut(&header.entry) {
            Some(Incoming::Streaming(streaming)) => {
                streaming.accept(self.target, &mut self.open, header, payload, |payload| {
                    let _verify_timer = PhaseTimer(&RECV_VERIFY_NS, Instant::now());
                    payload.len() <= crate::hash::CDC_MAX_BYTES as usize
                        && counters::hash(Counter::HashWireVerify, payload) == header.digest
                })?
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
                    verify: true,
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
        let (rel_path, outcome) = match incoming {
            Incoming::AwaitManifest { .. } => return Err(BulkloadRefusal::ProtocolStateViolation),
            Incoming::Streaming(streaming) => {
                if chunks as usize != streaming.specs.len() || size != streaming.offset {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                let rel_path = streaming.row.rel_path.clone();
                (rel_path, self.end_streaming(streaming, root, racy))
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
                (rel_path, self.end_filling(filling, racy))
            }
        };
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
        {
            let _drain_timer = PhaseTimer(&RECV_DRAIN_NS, Instant::now());
            self.committer.sync()?;
        }
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
        if checked.is_ok() && adopt {
            // Streamed in place of a manifest: an existing output at the
            // path is adopted against the streamed chunks, as a manifest
            // would have been checked, and the staged copy is dropped. One
            // that holds other bytes is superseded by the staged copy when
            // it is this store's own (WP0(d), #187), and refused otherwise.
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
                    return self.adopt(file, parent, &row, (key, racy, root), identity);
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
        self.publish(staged, &row, (key, racy, root), hints, supersede)
    }

    fn end_filling(&mut self, filling: Filling, racy: bool) -> Result<()> {
        let Filling {
            row,
            key,
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
                self.adopt(file, parent, &row, (key, racy, root), identity)
            }
            Plan::NoExchange(settled) => {
                self.remember_refusal(&key, settled, racy, RefusedOutput::ExchangeUnsupported);
                Err(BulkloadRefusal::DestinationExchangeUnsupported)
            }
            Plan::Write(staging) => {
                self.open -= 1;
                if let Some(failure) = failure {
                    let _ = staging.staged.discard();
                    return Err(failure.refusal);
                }
                fault_point!(ReceiveAfterChunks);
                self.publish(
                    staging.staged,
                    &row,
                    (key, racy, root),
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
    /// the record durable before the row commits.
    fn adopt(
        &self,
        file: std::fs::File,
        parent: Arc<std::fs::File>,
        row: &RowSchema,
        (key, racy, root): (Vec<u8>, bool, [u8; 32]),
        identity: StatIdentity,
    ) -> Result<()> {
        let identity = if racy {
            identity
        } else {
            unrowed::refresh(
                &file,
                &unrowed::CaptureRecord {
                    key: unrowed::record_key(row)?,
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
    /// (WP0(d), #187).
    fn publish(
        &mut self,
        staged: StagedFile,
        row: &RowSchema,
        (key, racy, root): (Vec<u8>, bool, [u8; 32]),
        hints: Vec<ChunkHint>,
        supersede: Option<OwnedOutput>,
    ) -> Result<()> {
        if !racy {
            match unrowed::record_key(row) {
                Ok(record_key) => unrowed::write_record(
                    staged.file(),
                    &unrowed::CaptureRecord {
                        key: record_key,
                        root,
                        size: row.size,
                    },
                ),
                Err(refusal) => {
                    let _ = staged.discard();
                    return Err(refusal);
                }
            }
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
    /// Bytes filled locally, every offset counted (item 1a).
    placed: u64,
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
/// replaced.
#[allow(clippy::too_many_lines)] // Validate, adopt or supersede, stage, fill, kick.
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
                        // is staged or asked of the source.
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
            Err(error) => return Err(crate::refuse::io(&error, "transfer::write_data")),
        }
    }
    output.flush().refuse_at("transfer::write_data")?;
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
