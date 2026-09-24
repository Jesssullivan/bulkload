//! Native resumable transfer over framed bidirectional stdio.
//!
//! Only missing chunks cross the wire. The source sends each captured chunk
//! from memory while a committer thread appends it to the private source pack,
//! so a resumed transfer need not re-read the source. The destination writes
//! each output directly from the received chunks, each verified against its
//! digest, and publishes it through group commit ([`crate::io::durable`]).
//! Destination completion binds both source and output identity. Enumeration
//! still walks the tree. This is not a no-rewalk performance claim.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const BATCH_ROWS: usize = 32;
const CAPTURE_WORKERS: usize = 4;
// Each event carries at most PERSIST_BATCH payloads (64 MiB). Eight queued
// events plus four producer batches stay below 1 GiB.
const CAPTURE_QUEUE: usize = CAPTURE_WORKERS * 2;
// Captured chunks held in memory for sending; beyond this a capture's chunks
// are read back from the sealed pack instead.
const RETAIN_BYTES: u64 = 512 * 1024 * 1024;
// At most this many files keep an open descriptor for chunk reuse before
// their group commits; fewer when the descriptor budget is smaller.
const SESSION_FILES: u64 = 256;
// Outputs held open while one manifest is filled from committed hints.
const HINT_FILES: usize = 16;
// Manifests are bounded separately. The existing full census remains O(N).
const MAX_MANIFEST_CHUNKS: usize = 131_072;

static WALK_NS: AtomicU64 = AtomicU64::new(0);
static REUSE_CENSUS_NS: AtomicU64 = AtomicU64::new(0);
static CDC_HASH_NS: AtomicU64 = AtomicU64::new(0);
static QUEUE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static TRANSFER_NS: AtomicU64 = AtomicU64::new(0);
static MATERIALIZE_NS: AtomicU64 = AtomicU64::new(0);

use bulkload_proto::frame::{ChunkSpec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES};
use bulkload_proto::FileKind;

use crate::counters::{self, Counter};
use crate::freshness::{NullCache, StatIdentity};
use crate::io::durable::Committer;
use crate::materialize::{
    verify_existing, Destination, PendingOutput, Publication, PublishSink, StagedFile,
};
use crate::transfer_store::{
    row_key, ChunkData, ChunkHint, Manifest, OutputRecord, PackItem, PackSink, PreparedEvent,
    PublisherSide, Store, PERSIST_BATCH,
};
use crate::walk::{walk, WalkOptions};
use crate::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};

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

/// Cumulative process-scope phase counters; concurrent transfers may overlap.
#[derive(Clone, Copy, Debug)]
pub struct TransferTiming {
    pub walk_ns: u64,
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
            "walk_ns={} reuse_census_ns={} cdc_hash_ns={} queue_wait_ns={} transfer_ns={} materialize_ns={}",
            self.walk_ns,
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

fn send_prepared(
    sender: &std::sync::mpsc::SyncSender<PreparedEvent>,
    event: PreparedEvent,
) -> Result<()> {
    let started = Instant::now();
    let sent = sender.send(event).map_err(|_| BulkloadRefusal::Io(None));
    QUEUE_WAIT_NS.fetch_add(elapsed_ns(started), Ordering::Relaxed);
    sent
}

struct SendBatchContext<'a> {
    root: &'a Path,
    authority: &'a [u8],
    store: &'a Store,
    committer: &'a Committer<PackSink>,
    batch: &'a [RowSchema],
    needed: &'a [bool],
}

struct ReceiveContext<'a> {
    target: &'a Destination,
    store: &'a Store,
    authority: &'a [u8],
    committer: &'a Committer<PublishSink>,
    session: &'a mut SessionChunks,
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
    if source_root.starts_with(&destination_root) || destination_root.starts_with(&source_root) {
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
            let mut input = sender.try_clone()?;
            serve(&mut input, &mut sender)
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

/// Serve one transfer request from a caller-authenticated stdio transport.
///
/// # Errors
/// Refuses malformed frames, unsafe state roots, source failures and broken I/O.
pub fn serve<R: Read, W: Write>(input: &mut R, output: &mut W) -> Result<()> {
    let FrameKind::TransferOpen { root, state } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let root = std::fs::canonicalize(path(root))?;
    let state = path(state);
    let store = Store::open(&state)?;
    if store.root().starts_with(&root) || root.starts_with(store.root()) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let meta = std::fs::metadata(&root)?;
    let authority = postcard::to_stdvec(&(
        store.authority()?,
        root.as_os_str().as_bytes(),
        meta.dev(),
        meta.ino(),
    ))?;
    let committer = Committer::spawn(PackSink::new(
        Store::open(&state)?.into_publisher(PublisherSide::Source)?,
    ))?;
    write_frame(
        output,
        FrameKind::TransferStart {
            authority: authority.clone(),
        },
    )?;
    let walk_started = Instant::now();
    let mut census = walk(
        &WalkOptions {
            cross_device: true,
            ..WalkOptions::new(root.clone())
        },
        &mut NullCache,
    )?;
    WALK_NS.fetch_add(elapsed_ns(walk_started), Ordering::Relaxed);
    census
        .rows
        .sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    for refused in census.refusals {
        write_frame(
            output,
            FrameKind::Refusal {
                code: refused.refusal.code().to_owned(),
                rel_path: refused.rel_path,
            },
        )?;
    }
    // Recorded, never carried: the receiver reports each one (R-N79).
    for rel_path in census.engine_temporaries {
        write_frame(output, FrameKind::EngineTemporary { rel_path })?;
    }
    let mut source_bytes_read = 0;
    let mut rows = 0;
    for batch in census.rows.chunks(BATCH_ROWS) {
        rows += batch.len() as u64;
        write_frame(
            output,
            FrameKind::TransferBatch {
                rows: batch.to_vec(),
            },
        )?;
        let FrameKind::WantFiles { needed } = read_frame(input)?.kind else {
            return Err(BulkloadRefusal::FrameCodec);
        };
        if needed.len() != batch.len() {
            return Err(BulkloadRefusal::FrameCodec);
        }
        if batch
            .iter()
            .zip(&needed)
            .any(|(row, needed)| *needed && row.kind != FileKind::Regular)
        {
            return Err(BulkloadRefusal::FrameCodec);
        }
        source_bytes_read += send_batch(
            input,
            output,
            &SendBatchContext {
                root: &root,
                authority: &authority,
                store: &store,
                committer: &committer,
                batch,
                needed: &needed,
            },
        )?;
    }
    // Every capture of this transfer is committed, or the transfer fails,
    // before the receiver is told the source is done.
    committer.sync()?;
    fault_point!(ServeBeforeDone);
    write_frame(
        output,
        FrameKind::TransferDone {
            rows,
            source_bytes_read,
        },
    )?;
    committer.finish()?
}

/// Chunks of one capture held in memory until its file has been sent.
#[derive(Default)]
struct Retained {
    chunks: HashMap<[u8; 32], ChunkData>,
    bytes: u64,
    /// Over budget: this capture's chunks are read back from the pack instead.
    spilled: bool,
}

/// Every capture's retained chunks, bounded by [`RETAIN_BYTES`] in total.
#[derive(Default)]
struct RetainedSet {
    captures: HashMap<usize, Retained>,
    bytes: u64,
}

impl RetainedSet {
    fn hold(&mut self, capture_id: usize, chunks: Vec<([u8; 32], ChunkData)>) {
        let bytes = chunks.iter().fold(0_u64, |total, (_, data)| {
            total.saturating_add(data.len() as u64)
        });
        let held = self.captures.entry(capture_id).or_default();
        if held.spilled {
            return;
        }
        if self.bytes.saturating_add(bytes) > RETAIN_BYTES {
            self.bytes = self.bytes.saturating_sub(held.bytes);
            *held = Retained {
                spilled: true,
                ..Retained::default()
            };
            return;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        held.bytes = held.bytes.saturating_add(bytes);
        held.chunks.extend(chunks);
    }

    fn take(&mut self, capture_id: usize) -> Retained {
        let held = self.captures.remove(&capture_id).unwrap_or_default();
        self.bytes = self.bytes.saturating_sub(held.bytes);
        held
    }
}

/// What the capture workers of one batch share.
#[derive(Clone, Copy)]
struct CaptureWork<'a> {
    root: &'a Path,
    authority: &'a [u8],
    state: &'a Path,
    batch: &'a [RowSchema],
    order: &'a [usize],
}

/// Start the capture workers for one batch. They take needed rows largest
/// first, so the longest capture starts before the short ones.
fn spawn_captures<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    work: CaptureWork<'scope>,
    next: &'scope AtomicUsize,
    sender: &std::sync::mpsc::SyncSender<PreparedEvent>,
) -> Result<()> {
    let CaptureWork {
        root,
        authority,
        state,
        batch,
        order,
    } = work;
    for _ in 0..CAPTURE_WORKERS.min(order.len()) {
        let sender = sender.clone();
        std::thread::Builder::new().spawn_scoped(scope, move || {
            let opened = Store::open_reader(state);
            while let Some(index) = order.get(next.fetch_add(1, Ordering::Relaxed)).copied() {
                let Some(row) = batch.get(index) else {
                    break;
                };
                let mut bytes = 0;
                let prepared = opened
                    .as_ref()
                    .map_or(Err(BulkloadRefusal::Io(None)), |store| {
                        capture(root, authority, row, store, index, &sender, &mut bytes)
                    });
                if let Err(refusal) = prepared {
                    if send_prepared(
                        &sender,
                        PreparedEvent::Refused {
                            capture_id: index,
                            bytes_read: bytes,
                            refusal,
                        },
                    )
                    .is_err()
                    {
                        break;
                    }
                }
                if opened.is_err() {
                    break;
                }
            }
        })?;
    }
    Ok(())
}

fn send_batch<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    context: &SendBatchContext<'_>,
) -> Result<u64> {
    let batch = context.batch;
    let mut order: Vec<usize> = context
        .needed
        .iter()
        .enumerate()
        .filter_map(|(index, needed)| needed.then_some(index))
        .collect();
    order.sort_by_key(|index| std::cmp::Reverse(batch.get(*index).map_or(0, |row| row.size)));
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| -> Result<u64> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(CAPTURE_QUEUE);
        spawn_captures(
            scope,
            CaptureWork {
                root: context.root,
                authority: context.authority,
                state: context.store.root(),
                batch,
                order: &order,
            },
            &next,
            &sender,
        )?;
        drop(sender);
        let mut retained = RetainedSet::default();
        let mut bytes_read = 0_u64;
        let mut completed = 0;
        // After a send failure no new row is started, but captures already in
        // flight are still recorded, so a resumed transfer need not re-read them.
        let mut failure = None;
        while let Ok(event) = receiver.recv() {
            let (capture_id, read, captured) = match event {
                PreparedEvent::Chunks { capture_id, chunks } => {
                    context.committer.submit(PackItem::Chunks {
                        capture_id,
                        chunks: chunks.clone(),
                    })?;
                    retained.hold(capture_id, chunks);
                    continue;
                }
                PreparedEvent::Complete {
                    capture_id,
                    bytes_read,
                    key,
                    manifest,
                    reused,
                } => {
                    if !reused {
                        context.committer.submit(PackItem::Capture {
                            key,
                            manifest: manifest.clone(),
                        })?;
                    }
                    (capture_id, bytes_read, Ok(manifest))
                }
                PreparedEvent::Refused {
                    capture_id,
                    bytes_read,
                    refusal,
                } => {
                    context.committer.submit(PackItem::Refused { capture_id })?;
                    (capture_id, bytes_read, Err(refusal))
                }
            };
            let held = retained.take(capture_id);
            bytes_read = bytes_read.saturating_add(read);
            completed += 1;
            if failure.is_some() {
                continue;
            }
            match send_file(input, output, context, capture_id, captured, &held) {
                Ok(()) => fault_point!(ServeAfterContent),
                Err(error) => {
                    next.store(order.len(), Ordering::Relaxed);
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if completed != order.len() {
            return Err(BulkloadRefusal::Io(None));
        }
        write_frame(output, FrameKind::BatchDone)?;
        Ok(bytes_read)
    })
}

/// Announce one requested row, then send its content or its refusal.
fn send_file<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    context: &SendBatchContext<'_>,
    capture_id: usize,
    captured: Result<Manifest>,
    held: &Retained,
) -> Result<()> {
    let row = context
        .batch
        .get(capture_id)
        .ok_or(BulkloadRefusal::FrameCodec)?;
    write_frame(
        output,
        FrameKind::FileContent {
            index: u32::try_from(capture_id).map_err(|_| BulkloadRefusal::FrameCodec)?,
        },
    )?;
    match captured {
        Ok(manifest) => send_content(input, output, &manifest, context, row, held),
        Err(refusal) => write_frame(
            output,
            FrameKind::Refusal {
                code: refusal.code().to_owned(),
                rel_path: row.rel_path.clone(),
            },
        ),
    }
}

/// Send one captured file: its manifest, then each chunk the receiver asks
/// for, from memory when retained and otherwise from the sealed pack.
fn send_content<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    manifest: &Manifest,
    context: &SendBatchContext<'_>,
    row: &RowSchema,
    held: &Retained,
) -> Result<()> {
    write_frame(
        output,
        FrameKind::Manifest {
            digest: manifest.digest,
            chunks: manifest.chunks.clone(),
        },
    )?;
    let FrameKind::WantChunks { digests } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let permitted: HashSet<_> = manifest.chunks.iter().map(|chunk| chunk.digest).collect();
    let mut requested = HashSet::new();
    let mut synced = false;
    for digest in digests {
        if !permitted.contains(&digest) || !requested.insert(digest) {
            return Err(BulkloadRefusal::FrameCodec);
        }
        if let Some(data) = held.chunks.get(&digest) {
            write_chunk(output, &digest, data)?;
            continue;
        }
        if !synced {
            // The chunk was not retained: wait for the pack to seal and commit
            // what has been appended, then read it back.
            context.committer.sync()?;
            synced = true;
        }
        match context
            .store
            .chunk_for(&digest, Counter::SourcePackReadback)
        {
            Ok(Some(data)) => write_chunk(output, &digest, &data)?,
            result => {
                let refusal = result.err().unwrap_or(BulkloadRefusal::SealedObjectMissing);
                write_frame(
                    output,
                    FrameKind::Refusal {
                        code: refusal.code().to_owned(),
                        rel_path: row.rel_path.clone(),
                    },
                )?;
                break;
            }
        }
    }
    if !matches!(read_frame(input)?.kind, FrameKind::Applied { .. }) {
        return Err(BulkloadRefusal::FrameCodec);
    }
    Ok(())
}

/// End a receive: record the sweep and directory-creation outcomes, commit
/// every pending output group, then finish directories and flush the session.
fn finish_receive(
    target: &mut Destination,
    store: &Store,
    committer: Committer<PublishSink>,
    stats: &mut TransferStats,
) -> Result<()> {
    stats.temporaries_removed = target.swept().removed;
    stats.temporaries_left.clone_from(&target.swept().left);
    stats.directories_renamed = target.created().renamed;
    stats
        .directories_fallback
        .clone_from(&target.created().fallback);
    for (rel_path, outcome) in committer.finish()? {
        match outcome {
            Ok(()) => stats.completed += 1,
            Err(refusal) => stats.refusals.push((rel_path, refusal.code().to_owned())),
        }
    }
    if stats.refusals.is_empty() {
        target.finish_directories(store)?;
    }
    target.flush_session()
}

/// The destination's committer, sized to the descriptor budget: a quarter
/// for session reuse, a quarter for staged files queued or grouped for
/// commit, which hold one descriptor each.
fn publication_committer(state: &Path, budget: u64) -> Result<Committer<PublishSink>> {
    let staged = (budget / 8).clamp(2, crate::io::durable::GROUP_FILES);
    Committer::spawn_with(
        PublishSink::new(Store::open(state)?.into_publisher(PublisherSide::Destination)?)?,
        crate::io::durable::Limits {
            group_files: staged,
            queue_depth: usize::try_from(staged).unwrap_or(1),
            ..crate::io::durable::Limits::default()
        },
    )
}

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
    let committer = publication_committer(destination_state, budget)?;
    // The committer holds the exclusive publisher, so no temporary of this
    // store is in flight while the root is swept.
    target.sweep_root(&store)?;
    write_frame(
        output,
        FrameKind::TransferOpen {
            root: source.as_os_str().as_bytes().to_vec(),
            state: source_state.as_os_str().as_bytes().to_vec(),
        },
    )?;
    let FrameKind::TransferStart { authority } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let target_meta = std::fs::metadata(target.path())?;
    let output_authority = postcard::to_stdvec(&(
        authority,
        target.path().as_os_str().as_bytes(),
        target_meta.dev(),
        target_meta.ino(),
    ))?;
    let mut session = SessionChunks::with_capacity((budget / 4).clamp(1, SESSION_FILES));
    let mut stats = TransferStats::default();
    let mut rows = 0;
    loop {
        match read_frame(input)?.kind {
            FrameKind::TransferBatch { rows: batch } => {
                if batch.is_empty() || batch.len() > BATCH_ROWS {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                rows += batch.len() as u64;
                let census_started = Instant::now();
                let mut needed = batch
                    .iter()
                    .map(|row| prepare_row(&mut target, &store, &output_authority, row, &mut stats))
                    .collect::<Result<Vec<_>>>()?;
                REUSE_CENSUS_NS.fetch_add(elapsed_ns(census_started), Ordering::Relaxed);
                write_frame(
                    output,
                    FrameKind::WantFiles {
                        needed: needed.clone(),
                    },
                )?;
                fault_point!(ReceiveAfterWantFiles);
                loop {
                    match read_frame(input)?.kind {
                        FrameKind::FileContent { index } => {
                            let index = index as usize;
                            let requested =
                                needed.get_mut(index).ok_or(BulkloadRefusal::FrameCodec)?;
                            if !*requested {
                                return Err(BulkloadRefusal::FrameCodec);
                            }
                            *requested = false;
                            receive_content(
                                input,
                                output,
                                &mut ReceiveContext {
                                    target: &target,
                                    store: &store,
                                    authority: &output_authority,
                                    committer: &committer,
                                    session: &mut session,
                                },
                                batch.get(index).ok_or(BulkloadRefusal::FrameCodec)?,
                                &mut stats,
                            )?;
                        }
                        FrameKind::BatchDone if needed.iter().all(|value| !*value) => break,
                        _ => return Err(BulkloadRefusal::FrameCodec),
                    }
                }
            }
            FrameKind::Refusal { code, rel_path } => stats.refusals.push((rel_path, code)),
            FrameKind::EngineTemporary { rel_path } => {
                stats.source_engine_temporaries.push(rel_path);
            }
            FrameKind::TransferDone {
                rows: sent,
                source_bytes_read,
            } if sent == rows => {
                stats.source_bytes_read = source_bytes_read;
                drop(session);
                finish_receive(&mut target, &store, committer, &mut stats)?;
                return Ok(stats);
            }
            _ => return Err(BulkloadRefusal::FrameCodec),
        }
    }
}

fn prepare_row(
    target: &mut Destination,
    store: &Store,
    authority: &[u8],
    row: &RowSchema,
    stats: &mut TransferStats,
) -> Result<bool> {
    let key = row_key(authority, row)?;
    let preparation = match row.kind {
        FileKind::Directory => target.directory(row, store, authority).map(|()| false),
        FileKind::Symlink => target.symlink(row).map(|()| false),
        FileKind::Regular => target.identity(row).and_then(|identity| {
            if let Some(identity) = identity {
                if store.output_matches(&key, &identity)? {
                    stats.reused += 1;
                    return Ok(false);
                }
            }
            Ok(true)
        }),
        _ => Err(BulkloadRefusal::FieldDomainViolation),
    };
    let needed = match preparation {
        Ok(needed) => needed,
        Err(refusal) => {
            stats
                .refusals
                .push((row.rel_path.clone(), refusal.code().to_owned()));
            false
        }
    };
    Ok(needed)
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
    /// Chunks to request, in first-occurrence order.
    missing: Vec<[u8; 32]>,
    /// Every offset each distinct chunk occupies, with its size.
    placements: HashMap<[u8; 32], (u64, Vec<u64>)>,
    hints: Vec<ChunkHint>,
}

fn receive_content<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    context: &mut ReceiveContext<'_>,
    row: &RowSchema,
    stats: &mut TransferStats,
) -> Result<()> {
    let key = row_key(context.authority, row)?;
    let manifest = match read_frame(input)?.kind {
        FrameKind::Manifest { digest, chunks } if chunks.len() <= MAX_MANIFEST_CHUNKS => {
            Manifest { digest, chunks }
        }
        FrameKind::Refusal { code, rel_path } => {
            stats.refusals.push((rel_path, code));
            return Ok(());
        }
        _ => return Err(BulkloadRefusal::FrameCodec),
    };
    let materialize_started = Instant::now();
    let plan = plan_file(context, row, &manifest);
    MATERIALIZE_NS.fetch_add(elapsed_ns(materialize_started), Ordering::Relaxed);
    let applied = match plan {
        Plan::Refuse(refusal) => {
            write_frame(
                output,
                FrameKind::WantChunks {
                    digests: Vec::new(),
                },
            )?;
            Err(refusal)
        }
        Plan::Adopt(file, parent) => {
            write_frame(
                output,
                FrameKind::WantChunks {
                    digests: Vec::new(),
                },
            )?;
            verify_existing(&file, row, &manifest).and_then(|identity| {
                context.committer.submit(Publication::Adopted {
                    record: OutputRecord {
                        key,
                        rel_path: row.rel_path.clone(),
                        identity,
                        hints: Vec::new(),
                    },
                    file,
                    parent,
                })
            })
        }
        Plan::Write(staging) => write_staged(input, output, context, row, key, staging, stats)?,
    };
    write_frame(
        output,
        FrameKind::Applied {
            success: applied.is_ok(),
        },
    )?;
    fault_point!(ReceiveAfterApplied);
    if let Err(refusal) = applied {
        stats
            .refusals
            .push((row.rel_path.clone(), refusal.code().to_owned()));
    }
    Ok(())
}

/// Request a staged file's missing chunks, write them, and queue the file
/// for its group commit. The outer error is a transport fault; the inner
/// result is this file's outcome.
fn write_staged<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    context: &mut ReceiveContext<'_>,
    row: &RowSchema,
    key: Vec<u8>,
    staging: Staging,
    stats: &mut TransferStats,
) -> Result<Result<()>> {
    write_frame(
        output,
        FrameKind::WantChunks {
            digests: staging.missing.clone(),
        },
    )?;
    let received = {
        let _transfer_timer = PhaseTimer(&TRANSFER_NS, Instant::now());
        receive_chunks(input, &staging, stats)
    };
    let Staging { staged, hints, .. } = staging;
    let received = match received {
        Ok(received) => received,
        Err(error) => {
            let _ = staged.discard();
            return Err(error);
        }
    };
    fault_point!(ReceiveAfterChunks);
    Ok(
        match received.and_then(|()| {
            staged
                .file()
                .set_permissions(std::fs::Permissions::from_mode(row.mode & 0o7777))
                .map_err(BulkloadRefusal::from)
        }) {
            Ok(()) => {
                fault_point!(MaterializeAfterTempWrite);
                context.session.insert(Arc::clone(staged.file()), &hints);
                context.committer.submit(Publication::Staged {
                    staged,
                    record: PendingOutput {
                        key,
                        rel_path: row.rel_path.clone(),
                        size: row.size,
                        hints,
                    },
                })
            }
            Err(refusal) => {
                let _ = staged.discard();
                Err(refusal)
            }
        },
    )
}

/// Validate a manifest against its row, adopt an existing output, or stage a
/// new one and fill every chunk this destination already holds.
fn plan_file(context: &ReceiveContext<'_>, row: &RowSchema, manifest: &Manifest) -> Plan {
    let mut placements: HashMap<[u8; 32], (u64, Vec<u64>)> = HashMap::new();
    let mut order = Vec::new();
    let mut offset = 0_u64;
    for chunk in &manifest.chunks {
        if chunk.size > u64::from(crate::hash::CDC_MAX_BYTES) {
            return Plan::Refuse(BulkloadRefusal::BudgetExceeded);
        }
        match placements.entry(chunk.digest) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                order.push(chunk.digest);
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
    for digest in order {
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
        let filled = local_chunk(context, &mut outputs, &digest, *size).and_then(|data| {
            data.map(|data| place(staged.file(), &data, offsets))
                .transpose()
        });
        match filled {
            Ok(Some(())) => (),
            Ok(None) => missing.push(digest),
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
/// file written earlier in this session, from a committed output hint, or
/// from the store's pack. Any mismatch is a miss, never an error.
fn local_chunk(
    context: &ReceiveContext<'_>,
    outputs: &mut HashMap<Vec<u8>, Option<std::fs::File>>,
    digest: &[u8; 32],
    size: u64,
) -> Result<Option<Vec<u8>>> {
    if let Some((file, offset, held)) = context.session.get(digest) {
        if held == size {
            if let Some(data) = read_verified(file, offset, size, digest) {
                return Ok(Some(data));
            }
        }
    }
    if let Some(hint) = context.store.output_chunk(digest)? {
        if hint.size == size {
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
            if let Some(data) = found {
                return Ok(Some(data));
            }
        }
    }
    Ok(context
        .store
        .chunk_for(digest, Counter::DestLocalReuseRead)
        .ok()
        .flatten()
        .filter(|data| data.len() as u64 == size))
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
        file.write_all_at(data, *offset)?;
        counters::add_len(Counter::DestMaterializeWrite, data.len());
    }
    Ok(())
}

/// Read exactly the requested chunks, verify each against its digest (the
/// one integrity check on received bytes) and write it at every offset it
/// occupies. Transport faults are errors; content faults are refusals, and
/// the remaining frames are still read so the stream stays in step.
fn receive_chunks<R: Read>(
    input: &mut R,
    staging: &Staging,
    stats: &mut TransferStats,
) -> Result<Result<()>> {
    let mut failure = None;
    for expected in &staging.missing {
        match read_frame(input)?.kind {
            FrameKind::Chunk { digest, data } if digest == *expected => {
                stats.bytes_received = stats.bytes_received.saturating_add(data.len() as u64);
                if failure.is_some() {
                    continue;
                }
                let Some((size, offsets)) = staging.placements.get(&digest) else {
                    failure = Some(BulkloadRefusal::FrameCodec);
                    continue;
                };
                if data.len() as u64 != *size
                    || data.len() > crate::hash::CDC_MAX_BYTES as usize
                    || counters::hash(Counter::HashWireVerify, &data) != digest
                {
                    failure = Some(BulkloadRefusal::DigestMismatch);
                    continue;
                }
                if let Err(refusal) = place(staging.staged.file(), &data, offsets) {
                    failure = Some(refusal);
                }
            }
            FrameKind::Refusal { .. } => return Ok(Err(BulkloadRefusal::SealedObjectMissing)),
            _ => return Err(BulkloadRefusal::FrameCodec),
        }
    }
    Ok(failure.map_or(Ok(()), Err))
}

fn capture(
    root: &Path,
    authority: &[u8],
    row: &RowSchema,
    store: &Store,
    capture_id: usize,
    sender: &std::sync::mpsc::SyncSender<PreparedEvent>,
    bytes_read: &mut u64,
) -> Result<()> {
    if row.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let key = row_key(authority, row)?;
    if let Some(manifest) = store.capture(&key)? {
        if manifest.chunks.len() > MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let available =
            manifest
                .chunks
                .iter()
                .try_fold(true, |available, chunk| -> Result<bool> {
                    Ok(available
                        && store
                            .chunk_for(&chunk.digest, Counter::SourceCaptureReuseRead)?
                            .is_some())
                })?;
        if available {
            send_prepared(
                sender,
                PreparedEvent::Complete {
                    capture_id,
                    bytes_read: *bytes_read,
                    key,
                    manifest,
                    reused: true,
                },
            )?;
            return Ok(());
        }
    }
    capture_uncached(root, row, key, capture_id, sender, bytes_read)
}

fn capture_uncached(
    root: &Path,
    row: &RowSchema,
    key: Vec<u8>,
    capture_id: usize,
    sender: &std::sync::mpsc::SyncSender<PreparedEvent>,
    bytes_read: &mut u64,
) -> Result<()> {
    if row.rel_path.ends_with(b"-wal")
        || row.rel_path.ends_with(b"-shm")
        || row.rel_path.ends_with(b"-journal")
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let file_path = root.join(path(row.rel_path.clone()));
    let mut file = crate::hash::open_nofollow(&file_path)?;
    let expected = StatIdentity::from_row(row);
    if StatIdentity::from_metadata(&file.metadata()?) != expected {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let mut prefix = Vec::new();
    let mut reader = CountReader {
        input: &mut file,
        count: bytes_read,
    };
    (&mut reader).take(16).read_to_end(&mut prefix)?;
    if prefix.starts_with(b"SQLite format 3\0")
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x82])
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x83])
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let mut hasher = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut pending = Vec::with_capacity(PERSIST_BATCH);
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
        counters::update(&mut hasher, Counter::HashCaptureFile, &chunk.data);
        let digest = counters::hash(Counter::HashCaptureChunk, &chunk.data);
        chunks.push(ChunkSpec {
            digest,
            size: chunk.data.len() as u64,
        });
        pending.push((digest, Arc::new(chunk.data)));
        if pending.len() == PERSIST_BATCH {
            send_prepared(
                sender,
                PreparedEvent::Chunks {
                    capture_id,
                    chunks: std::mem::take(&mut pending),
                },
            )?;
        }
    }
    if !pending.is_empty() {
        send_prepared(
            sender,
            PreparedEvent::Chunks {
                capture_id,
                chunks: pending,
            },
        )?;
    }
    if StatIdentity::from_metadata(&file.metadata()?) != expected {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let manifest = Manifest {
        digest: *hasher.finalize().as_bytes(),
        chunks,
    };
    send_prepared(
        sender,
        PreparedEvent::Complete {
            capture_id,
            bytes_read: *bytes_read,
            key,
            manifest,
            reused: false,
        },
    )?;
    Ok(())
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

/// Read one bounded frame without searching for delimiters.
///
/// # Errors
/// Refuses truncated/oversized/invalid frames and incompatible protocol versions.
pub fn read_frame<R: Read>(input: &mut R) -> Result<Frame> {
    let mut header = [0_u8; LENGTH_PREFIX_BYTES];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let mut bytes = header.to_vec();
    bytes.resize(LENGTH_PREFIX_BYTES + length, 0);
    input.read_exact(
        bytes
            .get_mut(LENGTH_PREFIX_BYTES..)
            .ok_or(BulkloadRefusal::FrameCodec)?,
    )?;
    counters::bump(Counter::WireFramesReceived);
    counters::add_len(Counter::WireBytesReceived, bytes.len());
    Ok(Frame::decode(&bytes)?.0)
}

/// Write one `Chunk` frame straight from borrowed bytes, byte-identical to
/// [`write_frame`] with an owned [`FrameKind::Chunk`].
fn write_chunk<W: Write>(output: &mut W, digest: &[u8; 32], data: &[u8]) -> Result<()> {
    let encoded = Frame::encode_chunk(digest, data)?;
    output.write_all(&encoded)?;
    output.flush()?;
    counters::bump(Counter::WireFramesSent);
    counters::add_len(Counter::WireBytesSent, encoded.len());
    Ok(())
}

/// Write one bounded frame and flush the request/reply boundary.
///
/// # Errors
/// Refuses oversized messages and broken transports.
pub fn write_frame<W: Write>(output: &mut W, kind: FrameKind) -> Result<()> {
    let encoded = Frame::new(kind).encode()?;
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Corpus {
        base: PathBuf,
    }
    impl Corpus {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "tcfs-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&base).unwrap();
            for path in ["source", "destination"] {
                std::fs::create_dir(base.join(path)).unwrap();
            }
            Self { base }
        }
        fn run(&self) -> Result<TransferStats> {
            copy(
                &self.base.join("source"),
                &self.base.join("destination"),
                &self.base.join("source-state"),
                &self.base.join("destination-state"),
            )
        }
    }
    impl Drop for Corpus {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// Breaks the serve side's transport at a protocol event, not a byte
    /// count. `write_frame` hands each frame to `write` and then calls
    /// `flush`, so every flush closes one whole frame. Once the serve side
    /// has offered the last requested file and sent its manifest, the next
    /// frame (that file's first chunk) is cut in half and the transport
    /// fails. A capture commits before its file is offered, so at that
    /// event every capture is durable and every earlier file was
    /// `Applied`, whatever order the capture workers finished in.
    struct Interrupted<W> {
        output: W,
        frame: Vec<u8>,
        offered: usize,
        last: usize,
        state: Cut,
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Cut {
        Passing,
        Truncate,
        Broken,
    }
    impl<W: Write> Write for Interrupted<W> {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let allowed = match self.state {
                Cut::Broken => return Err(std::io::ErrorKind::BrokenPipe.into()),
                Cut::Truncate => {
                    self.state = Cut::Broken;
                    data.len().div_ceil(2)
                }
                Cut::Passing => data.len(),
            };
            let count = self.output.write(data.get(..allowed).unwrap())?;
            self.frame.extend_from_slice(data.get(..count).unwrap());
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.state == Cut::Passing {
                let (frame, _) = Frame::decode(&self.frame).unwrap();
                match frame.kind {
                    FrameKind::FileContent { .. } => self.offered += 1,
                    FrameKind::Manifest { .. } if self.offered == self.last => {
                        self.state = Cut::Truncate;
                    }
                    _ => (),
                }
            }
            self.frame.clear();
            self.output.flush()
        }
    }

    #[test]
    fn interrupted_transport_resumes_completed_captures_without_source_reads() {
        const FILES: usize = 3;
        let corpus = Corpus::new();
        for index in 0..FILES {
            let mut state = index as u64 + 1;
            let bytes: Vec<u8> = (0..524_288)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    u8::try_from(state >> 56).unwrap()
                })
                .collect();
            std::fs::write(
                corpus.base.join("source").join(format!("file-{index}")),
                bytes,
            )
            .unwrap();
        }
        let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        for stream in [&sender, &receiver] {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
        }
        std::thread::scope(|scope| {
            let producer = scope.spawn(move || {
                let mut input = sender.try_clone().unwrap();
                serve(
                    &mut input,
                    &mut Interrupted {
                        output: sender,
                        frame: Vec::new(),
                        offered: 0,
                        last: FILES,
                        state: Cut::Passing,
                    },
                )
            });
            let mut output = receiver.try_clone().unwrap();
            let outcome = receive(
                &mut receiver,
                &mut output,
                &corpus.base.join("source"),
                &corpus.base.join("source-state"),
                &corpus.base.join("destination"),
                &corpus.base.join("destination-state"),
            );
            drop(output);
            drop(receiver);
            assert!(producer.join().unwrap().is_err());
            assert!(outcome.is_err());
        });
        // Every file but the last was applied; the last never reached a name.
        assert_eq!(
            std::fs::read_dir(corpus.base.join("destination"))
                .unwrap()
                .count(),
            FILES - 1
        );
        let resumed = corpus.run().unwrap();
        assert!(resumed.refusals.is_empty());
        assert_eq!(resumed.reused, FILES as u64 - 1);
        assert_eq!(resumed.completed, 1);
        assert_eq!(resumed.source_bytes_read, 0);
        for index in 0..FILES {
            let relative = format!("file-{index}");
            assert_eq!(
                std::fs::read(corpus.base.join("source").join(&relative)).unwrap(),
                std::fs::read(corpus.base.join("destination").join(relative)).unwrap()
            );
        }
    }

    #[test]
    fn chunk_batches_round_trip_and_resume_large_file() {
        let corpus = Corpus::new();
        let mut seed = 1_u32;
        let bytes: Vec<u8> = (0..=(PERSIST_BATCH * crate::hash::CDC_MAX_BYTES as usize))
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed.to_le_bytes().first().copied().unwrap()
            })
            .collect();
        std::fs::write(corpus.base.join("source/large"), &bytes).unwrap();
        let first = corpus.run().unwrap();
        assert!(first.refusals.is_empty());
        assert_eq!(first.source_bytes_read, bytes.len() as u64);
        assert_eq!(
            std::fs::read(corpus.base.join("destination/large")).unwrap(),
            bytes
        );
        let resumed = corpus.run().unwrap();
        assert!(resumed.refusals.is_empty());
        assert_eq!(resumed.source_bytes_read, 0);
        assert_eq!(resumed.bytes_received, 0);
    }

    #[test]
    fn parallel_batches_resume_and_continue_after_a_refused_file() {
        let corpus = Corpus::new();
        let bytes = vec![71_u8; 65_536];
        for index in 0..70 {
            std::fs::write(
                corpus.base.join("source").join(format!("file-{index:03}")),
                &bytes,
            )
            .unwrap();
        }
        std::fs::write(
            corpus.base.join("source/file-035.db"),
            b"SQLite format 3\0refused",
        )
        .unwrap();
        let first = corpus.run().unwrap();
        assert_eq!(first.completed, 70);
        assert_eq!(first.refusals.len(), 1);
        assert_eq!(first.source_bytes_read, 70 * 65_536 + 16);
        assert_eq!(first.bytes_received, 65_536);
        for index in 0..70 {
            assert_eq!(
                std::fs::read(
                    corpus
                        .base
                        .join("destination")
                        .join(format!("file-{index:03}"))
                )
                .unwrap(),
                bytes
            );
        }
        let second = corpus.run().unwrap();
        assert_eq!(second.reused, 70);
        assert_eq!(second.refusals.len(), 1);
        assert_eq!(second.source_bytes_read, 16);
        assert_eq!(second.bytes_received, 0);
    }

    #[test]
    fn round_trip_resumes_without_source_reads_and_preserves_divergence() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir(source.join("nested")).unwrap();
        let bytes: Vec<u8> = (0..800_000)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        std::fs::write(source.join("nested/data"), &bytes).unwrap();
        std::fs::write(source.join(".credential"), b"account-file").unwrap();
        std::fs::set_permissions(
            source.join(".credential"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::os::unix::fs::symlink("../elsewhere", source.join("link")).unwrap();
        let first = corpus.run().unwrap();
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(
            std::fs::read(destination.join("nested/data")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read_link(destination.join("link")).unwrap(),
            Path::new("../elsewhere")
        );
        assert_eq!(
            std::fs::metadata(destination.join(".credential"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let second = corpus.run().unwrap();
        assert_eq!(second.reused, 2);
        assert_eq!(second.source_bytes_read, 0);
        assert_eq!(second.bytes_received, 0);
        std::fs::set_permissions(
            destination.join("nested"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let metadata_conflict = corpus.run().unwrap();
        assert_eq!(metadata_conflict.refusals.len(), 1);
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::set_permissions(
            destination.join("nested"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(destination.join(".credential"), b"sting-unique").unwrap();
        let third = corpus.run().unwrap();
        assert_eq!(third.refusals.len(), 1);
        assert_eq!(
            std::fs::read(destination.join(".credential")).unwrap(),
            b"sting-unique"
        );
        assert_eq!(
            std::fs::read(source.join(".credential")).unwrap(),
            b"account-file"
        );
    }

    #[test]
    fn sqlite_headers_and_destination_symlinks_are_not_raw_copied() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::write(source.join("credentials.db"), b"SQLite format 3\0opaque").unwrap();
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/secret"), b"secret").unwrap();
        let outside = corpus.base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, destination.join("nested")).unwrap();
        let result = corpus.run().unwrap();
        assert_eq!(result.refusals.len(), 3);
        assert!(!destination.join("credentials.db").exists());
        assert!(!outside.join("secret").exists());
    }

    #[test]
    fn sweep_removes_only_this_stores_temporaries_and_records_the_rest() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/data"), vec![3_u8; 70_000]).unwrap();
        // Payload that merely shares the prefix, or matches only the untagged
        // form, is carried; a tagged temporary in the source is recorded by
        // the walk, forwarded, and never carried.
        std::fs::write(source.join(".bulkload-notes"), b"operator notes").unwrap();
        std::fs::write(source.join(".bulkload-2026-09"), b"september").unwrap();
        std::fs::write(source.join(".bulkload-fedcba9876543210-9-9"), b"orphan").unwrap();
        let first = corpus.run().unwrap();
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(
            first.source_engine_temporaries,
            vec![b".bulkload-fedcba9876543210-9-9".to_vec()]
        );
        assert!(destination.join(".bulkload-notes").exists());
        assert_eq!(
            std::fs::read(destination.join(".bulkload-2026-09")).unwrap(),
            b"september"
        );
        assert!(!destination.join(".bulkload-fedcba9876543210-9-9").exists());

        let tag = {
            let store = Store::open(&corpus.base.join("destination-state")).unwrap();
            crate::materialize::temporary_tag(&store.authority().unwrap())
        };
        let own = |mark: &str, serial: u32| {
            let mut name = b".bulkload-".to_vec();
            name.extend_from_slice(&tag);
            name.extend_from_slice(format!("{mark}-1-{serial}").as_bytes());
            PathBuf::from(std::ffi::OsString::from_vec(name))
        };
        let foreign = if tag.as_slice() == b"0123456789abcdef" {
            ".bulkload-abcdef0123456789-1-5"
        } else {
            ".bulkload-0123456789abcdef-1-5"
        };
        let outside = corpus.base.join("outside");
        std::fs::write(&outside, b"not ours").unwrap();
        // Removed: an orphan copy, a second link to a published output, and
        // an empty directory temporary.
        std::fs::write(destination.join(own("", 1)), b"partial").unwrap();
        std::fs::hard_link(
            destination.join("nested/data"),
            destination.join("nested").join(own("", 2)),
        )
        .unwrap();
        std::fs::create_dir(destination.join(own("-d", 7))).unwrap();
        // Left and recorded: this store's file tag on a symlink and on a
        // directory, a non-empty directory temporary, another store's tag,
        // and the untagged form. `.bulkload-2026-09` is outside the grammar
        // (a leading zero), so it is not even considered.
        std::os::unix::fs::symlink(&outside, destination.join(own("", 3))).unwrap();
        std::fs::create_dir(destination.join(own("", 4))).unwrap();
        std::fs::create_dir(destination.join(own("-d", 8))).unwrap();
        std::fs::write(destination.join(own("-d", 8)).join("held"), b"kept").unwrap();
        std::fs::write(destination.join(foreign), b"another store").unwrap();
        std::fs::write(destination.join(".bulkload-77-6"), b"untagged").unwrap();

        let second = corpus.run().unwrap();
        assert!(second.refusals.is_empty(), "{:?}", second.refusals);
        assert_eq!(second.temporaries_removed, 3);
        let mut left = second.temporaries_left;
        left.sort();
        let mut expected = vec![
            own("", 3).as_os_str().as_bytes().to_vec(),
            own("", 4).as_os_str().as_bytes().to_vec(),
            own("-d", 8).as_os_str().as_bytes().to_vec(),
            foreign.as_bytes().to_vec(),
            b".bulkload-77-6".to_vec(),
        ];
        expected.sort();
        assert_eq!(left, expected);
        assert!(!destination.join(own("", 1)).exists());
        assert!(std::fs::symlink_metadata(destination.join("nested").join(own("", 2))).is_err());
        assert!(std::fs::symlink_metadata(destination.join(own("-d", 7))).is_err());
        let data = std::fs::metadata(destination.join("nested/data")).unwrap();
        assert_eq!(data.nlink(), 1);
        assert_eq!(
            std::fs::read(destination.join("nested/data")).unwrap(),
            vec![3_u8; 70_000]
        );
        assert!(std::fs::symlink_metadata(destination.join(own("", 3)))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&outside).unwrap(), b"not ours");
        assert!(destination.join(own("", 4)).is_dir());
        assert_eq!(
            std::fs::read(destination.join(own("-d", 8)).join("held")).unwrap(),
            b"kept"
        );
        assert!(destination.join(foreign).is_file());
        assert!(destination.join(".bulkload-77-6").is_file());
        assert_eq!(
            std::fs::read(destination.join(".bulkload-notes")).unwrap(),
            b"operator notes"
        );
    }

    #[test]
    fn temporary_grammar_is_exact() {
        use crate::materialize::{temporary_name, TemporaryName};
        assert_eq!(
            temporary_name(b".bulkload-0123456789abcdef-12-0"),
            Some(TemporaryName::File(*b"0123456789abcdef"))
        );
        assert_eq!(
            temporary_name(b".bulkload-0123456789abcdef-d-12-0"),
            Some(TemporaryName::Directory(*b"0123456789abcdef"))
        );
        assert_eq!(
            temporary_name(b".bulkload-4242-7"),
            Some(TemporaryName::Untagged)
        );
        for name in [
            b".bulkload-".as_slice(),
            b".bulkload-notes",
            b".bulkload-0123456789ABCDEF-1-2",
            b".bulkload-0123456789abcde-1-2",
            b".bulkload-0123456789abcdef-1-2-3",
            b".bulkload-0123456789abcdef-x-1-2",
            b".bulkload-0123456789abcdef-1-",
            b".bulkload-0123456789abcdef-01-2",
            b".bulkload-0123456789abcdef-d-1-00",
            b".bulkload-0123456789abcdef-1-99999999999999999999",
            b".bulkload-0123456789abcdef-+1-2",
            b".bulkload-012-7",
            b".bulkload-1-2.tmp",
            b".bulkload--1",
            b"x.bulkload-1-2",
        ] {
            assert_eq!(temporary_name(name), None, "{}", name.escape_ascii());
        }
    }

    #[test]
    fn directories_fall_back_to_mkdir_without_a_no_replace_rename() {
        // R-N119: the hook makes this thread's no-replace renames report
        // EINVAL, as on a filesystem that lacks them.
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::materialize::force_rename_unsupported(false);
            }
        }
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir_all(source.join("outer/inner")).unwrap();
        std::fs::write(source.join("outer/inner/data"), b"payload").unwrap();
        std::fs::set_permissions(source.join("outer"), std::fs::Permissions::from_mode(0o750))
            .unwrap();
        let first = {
            let _restore = Restore;
            crate::materialize::force_rename_unsupported(true);
            corpus.run().unwrap()
        };
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(first.directories_renamed, 0);
        assert_eq!(
            first.directories_fallback,
            vec![b"outer".to_vec(), b"outer/inner".to_vec()]
        );
        assert_eq!(
            std::fs::metadata(destination.join("outer")).unwrap().mode() & 0o7777,
            0o750
        );
        assert_eq!(
            std::fs::read(destination.join("outer/inner/data")).unwrap(),
            b"payload"
        );
        // No directory temporary survives the fallback.
        let leftovers: Vec<_> = std::fs::read_dir(&destination)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.as_bytes().starts_with(b".bulkload-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // Without the hook the same shape takes the rename path.
        let renamed = Corpus::new();
        std::fs::create_dir(renamed.base.join("source/outer")).unwrap();
        let stats = renamed.run().unwrap();
        assert_eq!(stats.directories_renamed, 1);
        assert!(stats.directories_fallback.is_empty());
    }

    #[test]
    fn interrupted_directory_mode_finalization_resumes_only_its_owned_inode() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir(source.join("nested")).unwrap();
        let mut row = walk(&WalkOptions::new(source), &mut NullCache)
            .unwrap()
            .rows
            .remove(0);
        row.mode = 0o40_555;
        let store = Store::open(&corpus.base.join("destination-state")).unwrap();
        {
            let mut first = Destination::open(&destination, &store).unwrap();
            first.directory(&row, &store, b"authority").unwrap();
            assert_eq!(
                std::fs::metadata(destination.join("nested"))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let mut resumed = Destination::open(&destination, &store).unwrap();
        resumed.directory(&row, &store, b"authority").unwrap();
        resumed.finish_directories(&store).unwrap();
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .mode()
                & 0o777,
            0o555
        );
    }
}
