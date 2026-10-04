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
use crate::io::durable::Committer;
use crate::materialize::{
    verify_existing, Destination, PendingOutput, Publication, PublishSink, StagedFile,
};
use crate::transfer_store::{
    row_key, ChunkHint, LedgerItem, LedgerSink, Manifest, OutputRecord, PublisherSide, Store,
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

static WALK_NS: AtomicU64 = AtomicU64::new(0);
static WALK_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static REUSE_CENSUS_NS: AtomicU64 = AtomicU64::new(0);
static CDC_HASH_NS: AtomicU64 = AtomicU64::new(0);
static QUEUE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static TRANSFER_NS: AtomicU64 = AtomicU64::new(0);
static MATERIALIZE_NS: AtomicU64 = AtomicU64::new(0);

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
    pub cdc_hash_ns: u64,
    pub queue_wait_ns: u64,
    pub transfer_ns: u64,
    pub materialize_ns: u64,
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
        }
    }

    /// Space-separated `key=value` pairs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "walk_ns={} walk_wait_ns={} reuse_census_ns={} cdc_hash_ns={} queue_wait_ns={} transfer_ns={} materialize_ns={}",
            self.walk_ns,
            self.walk_wait_ns,
            self.reuse_census_ns,
            self.cdc_hash_ns,
            self.queue_wait_ns,
            self.transfer_ns,
            self.materialize_ns,
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
        Ok(_) => Ok(std::fs::canonicalize(state)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = state.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
            let parent = match state.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            Ok(std::fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error.into()),
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
    let source_root = std::fs::canonicalize(source)?;
    let destination_root = std::fs::canonicalize(destination)?;
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
    let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair()?;
    for stream in [&sender, &receiver] {
        tune_stream(stream);
    }
    // Test-only hang guard. It must exceed the slowest single socket stall in
    // the suite, which on a loaded host can be many seconds.
    #[cfg(test)]
    for stream in [&sender, &receiver] {
        stream.set_read_timeout(Some(std::time::Duration::from_mins(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_mins(5)))?;
    }
    std::thread::scope(|scope| -> Result<TransferStats> {
        let producer = std::thread::Builder::new().spawn_scoped(scope, move || {
            let input = sender.try_clone()?;
            let served = serve(input, &mut sender);
            // The source's reader thread holds a clone of this end; shutting
            // it down ends that thread and tells the destination the source
            // is gone, whatever `serve` returned.
            let _ = sender.shutdown(std::net::Shutdown::Both);
            served
        })?;
        let result = {
            let mut output = receiver.try_clone()?;
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
        producer.join().map_err(|_| BulkloadRefusal::Io(None))??;
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
                return Err(BulkloadRefusal::Io(None));
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
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let identity = StatIdentity::from_metadata(&meta);
            *stamp = (*stamp).max(identity.mtime_ns).max(identity.ctime_ns);
            if meta.is_dir() {
                newest(&entry.path(), stamp)?;
            }
        }
        Ok(())
    }
    let started = Instant::now();
    let mut stamp = StatIdentity::from_metadata(&std::fs::symlink_metadata(root)?).ctime_ns;
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
        return Err(BulkloadRefusal::FrameCodec);
    };
    if proto != PROTO_VERSION || id != wire_id() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    let root = std::fs::canonicalize(path(root))?;
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
    let root_fd = crate::io::sys::open_root(&root)?;
    let meta = crate::io::sys::fstat(&root_fd)?;
    let authority = postcard::to_stdvec(&(
        store.authority()?,
        root.as_os_str().as_bytes(),
        meta.node.dev,
        meta.node.ino,
    ))?;
    let committer = Committer::spawn(LedgerSink::new(
        Store::open(&state)?.into_publisher(PublisherSide::Source)?,
    ))?;
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
                })?;
        }
        for _ in 0..CAPTURE_WORKERS {
            let events = events_sender.clone();
            let (work, job_queue) = (&work, &job_queue);
            std::thread::Builder::new()
                .spawn_scoped(scope, move || capture_worker(work, job_queue, &events))?;
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
                    Ok(_) => break BulkloadRefusal::FrameCodec,
                    Err(refusal) => break refusal,
                };
                if events.send(event).is_err() {
                    break BulkloadRefusal::Io(None);
                }
            };
            credit.close();
            let _ = events.send(Event::PeerFailed(failure));
        })?;
    Ok(())
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
    let mut outbound = Outbound {
        output,
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
        let event = events.recv().map_err(|_| BulkloadRefusal::Io(None))?;
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
            self.jobs.send(job).map_err(|_| BulkloadRefusal::Io(None))?;
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
        let index = usize::try_from(entry).map_err(|_| BulkloadRefusal::FrameCodec)?;
        Ok((
            self.entries
                .get_mut(index)
                .ok_or(BulkloadRefusal::FrameCodec)?,
            self.rows
                .get_mut(index)
                .ok_or(BulkloadRefusal::FrameCodec)?,
        ))
    }

    /// Retire an entry whose job a capture thread held, returning its row.
    fn finish_entry(&mut self, entry: u64) -> Result<Arc<RowSchema>> {
        let (slot, row) = self.slot(entry)?;
        if !matches!(slot, SourceEntry::Queued | SourceEntry::Working) {
            return Err(BulkloadRefusal::Io(None));
        }
        *slot = SourceEntry::Done;
        let row = row.take().ok_or(BulkloadRefusal::Io(None))?;
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
                    return Err(BulkloadRefusal::Io(None));
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
                    .ok_or(BulkloadRefusal::FrameCodec)?;
                // A capture is recorded only once the destination holds its
                // bytes durably, so a committed capture never costs a source
                // read again (R25, OI-1001-Q15).
                if let (true, Some((key, manifest))) = (held, record) {
                    self.committer.submit(LedgerItem {
                        entry,
                        key,
                        manifest,
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
            } => {
                self.bytes_read = self.bytes_read.saturating_add(bytes_read);
                let row = self.finish_entry(entry)?;
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
            return Err(BulkloadRefusal::FrameCodec);
        }
        let row = held.as_ref().ok_or(BulkloadRefusal::FrameCodec)?;
        let regular = row.kind == FileKind::Regular;
        let (next, job) = match decision {
            Decision::Skip | Decision::Reuse | Decision::Refuse { .. } => {
                // Retired: nothing more is read or sent for it.
                *held = None;
                (SourceEntry::Done, None)
            }
            Decision::Send | Decision::WantManifest if !regular => {
                return Err(BulkloadRefusal::FrameCodec);
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
        let row = Arc::clone(held.as_ref().ok_or(BulkloadRefusal::FrameCodec)?);
        let SourceEntry::Offered {
            manifest,
            retained,
            record,
            racy,
        } = std::mem::replace(slot, SourceEntry::Working)
        else {
            return Err(BulkloadRefusal::FrameCodec);
        };
        let count = manifest.chunks.len();
        if indices.windows(2).any(|pair| pair.first() >= pair.get(1))
            || indices.iter().any(|index| *index as usize >= count)
        {
            return Err(BulkloadRefusal::FrameCodec);
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
            .map_err(|_| BulkloadRefusal::Io(None))
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
            },
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

fn run_job(work: &SourceWork<'_>, store: &Store, job: Job, events: &Sender<Event>) -> Event {
    let mut bytes_read = 0;
    let (entry, outcome) = match job {
        Job::Send { entry, row } => (
            entry,
            send_capture(work, entry, &row, events, &mut bytes_read),
        ),
        Job::Manifest { entry, row } => (
            entry,
            match manifest_capture(work, store, &row, &mut bytes_read) {
                Ok(Some((record, manifest, retained, racy))) => Ok(Event::Manifest {
                    entry,
                    record,
                    manifest,
                    retained,
                    racy,
                    bytes_read,
                }),
                // No ledger row and no room to keep the chunks: building a
                // manifest first would read the seat twice (R25). Stream it
                // instead; the destination takes data in place of a manifest.
                Ok(None) => send_capture(work, entry, &row, events, &mut bytes_read),
                Err(refusal) => Err(refusal),
            },
        ),
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
    })
}

/// Read, chunk and stream one file; its `End` carries the capture to record.
fn send_capture(
    work: &SourceWork<'_>,
    entry: u64,
    row: &RowSchema,
    events: &Sender<Event>,
    bytes_read: &mut u64,
) -> Result<Event> {
    let key = row_key(work.authority, row)?;
    let (chunks, racy) = capture_file(work, row, bytes_read, |index, offset, digest, data| {
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
            .map_err(|_| BulkloadRefusal::Io(None))
    })?;
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
) -> Result<Option<Offer>> {
    let key = row_key(work.authority, row)?;
    if let Some(manifest) = store.capture(&key)? {
        if manifest.size() == Some(row.size) && manifest.chunks.len() <= MAX_MANIFEST_CHUNKS {
            return Ok(Some((None, manifest, None, false)));
        }
    }
    // Every chunk a fresh manifest names must be kept, so the requests that
    // follow are served from memory: a seat is read at most once a session.
    let Some(mut retained) = Retained::reserve(&work.retain, row.size) else {
        return Ok(None);
    };
    let (chunks, racy) = capture_file(work, row, bytes_read, |_, _, _, data| {
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
        if StatIdentity::from_metadata(&file.metadata()?) != StatIdentity::from_row(row) {
            return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
        }
        Some(file)
    } else {
        None
    };
    for index in indices {
        let at = *index as usize;
        let spec = manifest.chunks.get(at).ok_or(BulkloadRefusal::FrameCodec)?;
        let offset = *offsets.get(at).ok_or(BulkloadRefusal::FrameCodec)?;
        let data = if let Some(held) = held {
            Arc::clone(held.chunks.get(at).ok_or(BulkloadRefusal::FrameCodec)?)
        } else {
            let file = opened.as_ref().ok_or(BulkloadRefusal::Io(None))?;
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
            .map_err(|_| BulkloadRefusal::Io(None))?;
    }
    if let Some(file) = &opened {
        if StatIdentity::from_metadata(&file.metadata()?) != StatIdentity::from_row(row) {
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
    if StatIdentity::from_metadata(&file.metadata()?) != expected {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let mut prefix = Vec::new();
    let mut reader = CountReader {
        input: SourceReader {
            file: &file,
            offset: 0,
        },
        count: bytes_read,
    };
    (&mut reader).take(16).read_to_end(&mut prefix)?;
    if prefix.starts_with(b"SQLite format 3\0")
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x82])
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x83])
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
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
    if offset != row.size || StatIdentity::from_metadata(&file.metadata()?) != expected {
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
    )?;
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
/// commit, which hold one descriptor each.
fn publication_committer(
    state: &Path,
    budget: u64,
    notify: Sender<crate::materialize::GroupOutcomes>,
) -> Result<Committer<PublishSink>> {
    let staged = (budget / 8).clamp(2, crate::io::durable::GROUP_FILES);
    Committer::spawn_with(
        PublishSink::new(Store::open(state)?.into_publisher(PublisherSide::Destination)?)?
            .with_notify(notify),
        crate::io::durable::Limits {
            group_files: staged,
            queue_depth: usize::try_from(staged).unwrap_or(1),
            ..crate::io::durable::Limits::default()
        },
    )
}

/// A streamed entry: chunks arrive in order and are written at once.
struct Streaming {
    row: RowSchema,
    key: Vec<u8>,
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
            return Err(BulkloadRefusal::FrameCodec);
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
                return Err(BulkloadRefusal::FrameCodec);
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

    fn new(row: RowSchema, key: Vec<u8>, adopt: bool) -> Self {
        Self {
            row,
            key,
            staged: None,
            specs: Vec::new(),
            offset: 0,
            hints: Vec::new(),
            seen: HashSet::new(),
            failure: None,
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
    expected: VecDeque<u32>,
    failure: Option<BulkloadRefusal>,
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
}

/// Re-probe the destination filesystem after admitting this many bytes,
/// even without a group commit in between.
const SPACE_REPROBE_BYTES: u64 = 256 * 1024 * 1024;

/// Receive an ordinary-file carry without replacing divergent outputs.
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
    let store = Store::open(destination_state)?;
    let mut target = Destination::open(destination, &store)?;
    if store.root().starts_with(target.path()) || target.path().starts_with(store.root()) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let budget = crate::io::limits::descriptor_budget();
    let (notify, group_outcomes) = std::sync::mpsc::channel();
    let committer = publication_committer(destination_state, budget, notify)?;
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
        },
    )?;
    let Frame::Control(Control::Start { authority }) = read_frame(input)? else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let target_meta = std::fs::metadata(target.path())?;
    let output_authority = postcard::to_stdvec(&(
        authority,
        target.path().as_os_str().as_bytes(),
        target_meta.dev(),
        target_meta.ino(),
    ))?;
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
    };
    let source_bytes_read = receiver.run(input)?;
    let Inbound {
        mut stats,
        session,
        salvage_staged,
        ..
    } = receiver;
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
        loop {
            // Before blocking on the next frame: answer what has committed,
            // and once the stream is drained, commit and answer the rest
            // (the source waits for every `Held` before it finishes).
            self.settle_held()?;
            match read_frame(input)? {
                Frame::Control(Control::Entry { entry, row }) => {
                    if entry != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::FrameCodec);
                    }
                    offered += 1;
                    self.entry(entry, row)?;
                }
                Frame::Control(Control::Refused {
                    entry: None,
                    rel_path,
                    code,
                }) => self.stats.refusals.push((rel_path, code)),
                Frame::Control(Control::Refused {
                    entry: Some(entry),
                    rel_path,
                    code,
                }) => self.refused(entry, &rel_path, code)?,
                Frame::Control(Control::EngineTemporary { rel_path }) => {
                    self.stats.source_engine_temporaries.push(rel_path);
                }
                Frame::Control(Control::WalkDone { entries }) => {
                    if entries != offered || walk_done.is_some() {
                        return Err(BulkloadRefusal::FrameCodec);
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
                _ => return Err(BulkloadRefusal::FrameCodec),
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
    fn entry(&mut self, entry: u64, row: RowSchema) -> Result<()> {
        let census_started = Instant::now();
        let key = row_key(&self.authority, &row)?;
        let decided = match row.kind {
            FileKind::Directory => self
                .target
                .directory(&row, self.store, &self.authority)
                .map(|()| Decision::Skip),
            FileKind::Symlink => self.target.symlink(&row).map(|()| Decision::Skip),
            FileKind::Regular => self.target.identity(&row).and_then(|identity| {
                if let Some(identity) = &identity {
                    if self.store.output_matches(&key, identity)? {
                        self.stats.reused += 1;
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
        let incoming = self
            .incoming
            .remove(&entry)
            .ok_or(BulkloadRefusal::FrameCodec)?;
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
            return Err(BulkloadRefusal::FrameCodec);
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
            return Err(BulkloadRefusal::FrameCodec);
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
                return Err(BulkloadRefusal::FrameCodec);
            }
            plan_file(
                &ReceiveContext {
                    target: self.target,
                    store: self.store,
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
            Plan::Refuse(_) | Plan::Adopt(..) => Vec::new(),
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
            .ok_or(BulkloadRefusal::FrameCodec)?;
        self.stats.bytes_received = self.stats.bytes_received.saturating_add(size);
        let verified = payload.len() <= crate::hash::CDC_MAX_BYTES as usize
            && counters::hash(Counter::HashWireVerify, payload) == header.digest;
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
        match self.incoming.get_mut(&header.entry) {
            Some(Incoming::Streaming(streaming)) => {
                streaming.accept(self.target, &mut self.open, header, payload, verified)?;
            }
            Some(Incoming::Filling(filling)) => {
                if filling.expected.pop_front() != Some(header.index) {
                    return Err(BulkloadRefusal::FrameCodec);
                }
                let at = header.index as usize;
                let spec = filling
                    .manifest
                    .chunks
                    .get(at)
                    .ok_or(BulkloadRefusal::FrameCodec)?;
                if spec.digest != header.digest
                    || spec.size != size
                    || filling.offsets.get(at) != Some(&header.offset)
                {
                    return Err(BulkloadRefusal::FrameCodec);
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
            _ => return Err(BulkloadRefusal::FrameCodec),
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
            .ok_or(BulkloadRefusal::FrameCodec)?;
        let (rel_path, outcome) = match incoming {
            Incoming::AwaitManifest { .. } => return Err(BulkloadRefusal::FrameCodec),
            Incoming::Streaming(streaming) => {
                if chunks as usize != streaming.specs.len() || size != streaming.offset {
                    return Err(BulkloadRefusal::FrameCodec);
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
                    return Err(BulkloadRefusal::FrameCodec);
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
        self.committer.sync()?;
        self.answer_held()?;
        if self.pending_held.is_empty() {
            Ok(())
        } else {
            Err(BulkloadRefusal::Io(None))
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
        let checked = failure.map_or(Ok(()), Err).and_then(|()| {
            if offset != row.size || manifest_root(&specs) != root {
                Err(BulkloadRefusal::DigestMismatch)
            } else {
                Ok(())
            }
        });
        if checked.is_ok() && adopt {
            // Streamed in place of a manifest: an existing output at the
            // path is adopted against the streamed chunks, as a manifest
            // would have been checked, and the staged copy is dropped.
            match self.target.existing(&row) {
                Ok(Some((file, parent))) => {
                    if let Some(staged) = staged {
                        let _ = staged.discard();
                    }
                    let manifest = Manifest {
                        root,
                        chunks: specs,
                    };
                    let identity = verify_existing(&file, &row, &manifest)?;
                    return self.adopt(file, parent, &row, (key, racy), identity);
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
        self.publish(staged, &row, (key, racy), hints)
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
        match plan {
            Plan::Refuse(refusal) => Err(refusal),
            Plan::Adopt(file, parent) => {
                let identity = verify_existing(&file, &row, &manifest)?;
                self.adopt(file, parent, &row, (key, racy), identity)
            }
            Plan::Write(staging) => {
                self.open -= 1;
                if let Some(refusal) = failure {
                    let _ = staging.staged.discard();
                    return Err(refusal);
                }
                fault_point!(ReceiveAfterChunks);
                self.publish(staging.staged, &row, (key, racy), staging.hints)
            }
        }
    }

    /// Queue a verified existing output for its group commit, which seals
    /// it and its directory before the commit (#77 round 2, N4: an adopted
    /// output is reported held only after that commit).
    fn adopt(
        &self,
        file: std::fs::File,
        parent: Arc<std::fs::File>,
        row: &RowSchema,
        (key, racy): (Vec<u8>, bool),
        identity: StatIdentity,
    ) -> Result<()> {
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

    /// Apply the final mode and queue a fully written staged file for its
    /// group commit, which seals it, renames it and seals its directory.
    fn publish(
        &mut self,
        staged: StagedFile,
        row: &RowSchema,
        (key, racy): (Vec<u8>, bool),
        hints: Vec<ChunkHint>,
    ) -> Result<()> {
        if let Err(refusal) = crate::io::sys::fchmod(&**staged.file(), row.mode & 0o7777)
            .map_err(BulkloadRefusal::from)
        {
            let _ = staged.discard();
            return Err(refusal);
        }
        fault_point!(MaterializeAfterTempWrite);
        self.session.insert(Arc::clone(staged.file()), &hints);
        self.committer.submit(Publication::Staged {
            staged,
            record: PendingOutput {
                key,
                rel_path: row.rel_path.clone(),
                size: row.size,
                racy,
                hints,
            },
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
    Adopt(std::fs::File, Arc<std::fs::File>),
    Write(Staging),
}

struct Staging {
    staged: StagedFile,
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
    match context.target.existing(row) {
        Ok(Some((file, parent))) => return Plan::Adopt(file, parent),
        Ok(None) => (),
        Err(refusal) => return Plan::Refuse(refusal),
    }
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
        missing,
        placements,
        hints,
    })
}

/// A chunk this destination already holds, re-read and re-verified: from a
/// file written earlier in this session, or from a published output through
/// a committed hint, newest first. Any mismatch is a miss, never an error,
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
        crate::io::sys::pwrite_all(file, data, *offset)?;
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
    input.read_exact(&mut prefix)?;
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
        input.read_exact(&mut header)?;
        let header = DataHeader::from_bytes(&header)?;
        if header.size as usize != size {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let mut payload = vec![0_u8; size];
        input.read_exact(&mut payload)?;
        Frame::Data { header, payload }
    } else {
        let mut bytes = vec![0_u8; body];
        input.read_exact(&mut bytes)?;
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
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
            Ok(written) => IoSlice::advance_slices(&mut remaining, written),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
    output.flush()?;
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
    output.write_all(&encoded)?;
    output.flush()?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, encoded.len());
    Ok(())
}

fn path(bytes: Vec<u8>) -> PathBuf {
    std::ffi::OsString::from_vec(bytes).into()
}

#[cfg(test)]
mod tests;
