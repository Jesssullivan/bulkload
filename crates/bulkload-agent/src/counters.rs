//! Process-scope byte, hash, flush and commit counters (M2 W2, TIN-4541).
//!
//! Every verb prints a snapshot of these counters as one machine-readable
//! `key=value` line, so each later design number is measured rather than
//! assumed. Counters are relaxed atomics: cheap enough to leave on in release
//! builds, and exact once the observed operation has joined.
//!
//! Scope is the current process. In the local `copy` verb both protocol halves
//! run in one process, so source and destination stages are distinguished by
//! name, not by process. `serve` prints its own (source-side) counters to
//! stderr because its stdout is the wire.
//!
//! Flush kinds are mutually exclusive:
//! - `flush_full`: `File::sync_all` on a regular file. On Darwin the standard
//!   library implements it as `fcntl(F_FULLFSYNC)`.
//! - `flush_barrier`: a per-file seal: `fcntl(F_BARRIERFSYNC)` on Darwin,
//!   `fsync` elsewhere (group commit), or `sync_data` from [`sync_barrier`]
//!   off Darwin.
//! - `flush_fdatasync`: `File::sync_data` on a regular file. On Darwin the
//!   standard library implements it as `fcntl(F_FULLFSYNC)` too.
//! - `flush_dir`: a full sync of a directory descriptor (`sync_all`, which is
//!   `F_FULLFSYNC` on Darwin).
//! - `flush_dir_barrier`: a group-commit directory seal (`F_BARRIERFSYNC` on
//!   Darwin, `fsync` elsewhere).
//!
//! Every file-backed bulkload `SQLite` store runs in WAL mode with
//! `synchronous=FULL` and `fullfsync=ON` (`io::durable::configure_sqlite`
//! refuses to open one otherwise). Each counted commit therefore syncs the
//! WAL with at least one `F_FULLFSYNC` on Darwin. A commit that runs an
//! automatic checkpoint also syncs the WAL and the database file. The derived
//! `full_flushes_total` counts one full flush per commit, so it is a lower
//! bound when a checkpoint ran.
//!
//! The one exception is the SOURCE ledger's row commits under
//! `LedgerSync::Relaxed` (WP0(g), OI-1003-Q20 and Q37; the default): they
//! run `synchronous=NORMAL`, `fullfsync=OFF` and do not sync the WAL
//! (`io::durable::relax_ledger_rows`). Each is counted in
//! `sqlite_group_source_commits` and in `source_ledger_relaxed_commits`;
//! `full_flushes_total` leaves the relaxed ones out. A relaxed commit that
//! runs an automatic checkpoint does sync (the WAL, then the database), and
//! that sync is not counted, so `full_flushes_total` stays a lower bound.
//!
//! Flush counts are attempts (a failed flush is still counted). `SQLite`
//! commit counters count successful commits only.
//!
//! Git children (`git bundle create`, `git pack-objects`) that pack a
//! capture's objects from the source object store, and the fetch that
//! completes a retained capture for blob reuse, are measured from the
//! child's own resource usage, collected by `wait4` when it exits:
//! `read_source_pack_readback_bytes` is `ru_inblock` x 512. On Linux that is
//! exactly the child's `/proc/<pid>/io` `read_bytes` (storage reads, 512-byte
//! units). On Darwin `ru_inblock` counts block input operations, not sectors,
//! so x 512 is a **lower bound**. On both, reads served from the page cache
//! and mmap'd pack windows already resident are invisible: the counter is a
//! lower bound on what the child read, never an over-count.
//!
//! The logical measure is `write_source_pack_bytes` and
//! `write_source_pack_objects`: every object in a capture pack was read from
//! an object store to be written. That bounds a self-contained pack's reads,
//! not a thin one's. A grouped item or a chained link (OI-1003-Q42) deltas
//! against preferred bases its prerequisites hold: each base is read to
//! delta against and never written, so for those captures
//! `write_source_pack_bytes` counts the pack, not what packing read. The next
//! pass's reuse fetch of such a capture completes it with `index-pack
//! --fix-thin`, reading every base from the source object store:
//! `read_source_capture_reuse_bytes` counts those bases (the written pack's
//! growth) beside the bundle's length.
//!
//! Every counter is incremented by production code; a declared counter
//! nothing increments would read as a measured zero, so the
//! `every_counter_is_incremented_somewhere` scan refuses one (WP2: the
//! pack-store era's `read_other_chunk`, `write_dest_pack`,
//! `write_legacy_chunk`, `blake3_capture_file`, `blake3_store_read_verify`
//! and `blake3_legacy_put` were removed for that reason).
//!
//! `source_wal_index_touched` counts the provider `SQLite` snapshots that
//! touched their source's wal-index (`<db>-shm`): the first of S2's two stated
//! source writes (OI-1003-Q36, which extends OI-1003-Q16). A backup-API read of a
//! WAL-mode source is WAL-aware: it opens the wal-index read-write, maps it
//! shared and takes `fcntl` locks on it. It creates the file when no live
//! connection has, and rebuilds it when none holds it; beside a live writer
//! it often leaves every byte as it was. "Touched" covers all of these. Each
//! WAL-aware snapshot adds 1 when a `-shm` sits beside its source after the
//! read, whether or not the file's bytes or metadata moved, so 0 means no
//! snapshot opened a source wal-index. It is an upper bound in two corners,
//! where `SQLite` never opens the `-shm` that is counted: a rollback-journal
//! database with a stray `-shm` beside it, and a snapshot refused before its
//! first read of the source.
//!
//! `source_wal_created` counts the provider `SQLite` snapshots that left a
//! `-wal` beside a source that had none: S2's second stated source write
//! (OI-1003-Q72, which extends OI-1003-Q36 to the empty `-wal`). A WAL-aware
//! open of a WAL-mode database with no `-wal` (checkpointed and closed, or
//! opened by a writer that has not read it yet) creates a zero-byte one.
//! Each snapshot adds 1 when no `-wal` sat beside its source before the read
//! and one does after it, so the same source adds 1 once and 0 on later
//! snapshots while that file stays. The read-only connection cannot append a
//! frame, so the file it creates is empty; a writer that arrives during the
//! read and creates the `-wal` itself is counted too, which makes the counter
//! an upper bound and never an undercount. The main database, and a `-wal`
//! that existed before the read, are never written (P75).
//!
//! Not counted as flushes: syncs done by child processes. `git` children
//! spawned by the Git carry verbs flush on their own, so the flush counters
//! are a lower bound on the syncs a verb causes.

use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

macro_rules! counters {
    ($($variant:ident => $name:literal,)*) => {
        /// One process-scope counter.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Counter {
            $(
                #[doc = concat!("Printed as `", $name, "`.")]
                $variant,
            )*
        }

        impl Counter {
            /// Every counter, in print order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The stable `key` printed for this counter.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

counters! {
    // Bytes read, by stage.
    SourceFileRead => "read_source_file_bytes",
    // Header bytes read from a source file only to refuse it (a SQLite or
    // WAL magic, #186): never content, so never part of
    // `read_source_file_bytes` or a transfer's `source_bytes_read`. A
    // refused seat whose stat identity is unchanged is not sniffed again.
    SourceSniff => "source_sniff_bytes",
    // Retained capture bundles a Git capture fetched to reuse their blobs
    // (logical: the bundle's length per fetch, plus the delta bases a thin
    // bundle's fetch read from the source object store to complete it).
    SourceCaptureReuseRead => "read_source_capture_reuse_bytes",
    // Storage reads by the git children that pack a capture's objects or
    // complete a retained one for reuse (see the module notes: a lower
    // bound, page-cache hits are invisible).
    SourcePackReadback => "read_source_pack_readback_bytes",
    DestLocalReuseRead => "read_dest_local_reuse_bytes",
    DestVerifyRead => "read_dest_verify_existing_bytes",
    HashFileRead => "read_hash_file_bytes",
    // A bundle copied into a private stage before a restore or import reads it.
    BundleStageRead => "read_bundle_stage_bytes",
    // Bytes written, by stage.
    // Capture bundles written by git children from source objects, and the
    // objects their packs hold.
    SourcePackWrite => "write_source_pack_bytes",
    SourcePackObjects => "write_source_pack_objects",
    BundleStageWrite => "write_bundle_stage_bytes",
    DestMaterializeWrite => "write_dest_materialize_bytes",
    // Framed transport, as seen by this process.
    WireFramesSent => "wire_frames_sent",
    WireBytesSent => "wire_bytes_sent",
    WireFramesReceived => "wire_frames_received",
    WireBytesReceived => "wire_bytes_received",
    // BLAKE3 bytes hashed, by purpose.
    HashCaptureChunk => "blake3_capture_chunk_bytes",
    HashWireVerify => "blake3_dest_wire_verify_bytes",
    HashDestReuse => "blake3_dest_local_reuse_bytes",
    HashVerifyExisting => "blake3_verify_existing_bytes",
    HashFile => "blake3_hash_file_bytes",
    HashBundleStage => "blake3_bundle_stage_bytes",
    HashOther => "blake3_other_bytes",
    // Flushes, by kind (mutually exclusive), with worker-summed nanoseconds.
    FlushFull => "flush_full_count",
    FlushFullNs => "flush_full_ns",
    FlushBarrier => "flush_barrier_count",
    FlushBarrierNs => "flush_barrier_ns",
    FlushFdatasync => "flush_fdatasync_count",
    FlushFdatasyncNs => "flush_fdatasync_ns",
    FlushDir => "flush_dir_count",
    FlushDirNs => "flush_dir_ns",
    FlushDirBarrier => "flush_dir_barrier_count",
    FlushDirBarrierNs => "flush_dir_barrier_ns",
    // Group commit (io::durable) and transport tuning.
    DurableGroups => "durable_groups",
    TransportTuned => "transport_buffers_raised",
    PublishLinkFallback => "publish_link_fallback",
    // SQLite commits, by kind. Autocommit statements count as one commit each.
    SqliteSchema => "sqlite_schema_commits",
    SqliteGroupSource => "sqlite_group_source_commits",
    SqliteGroupDest => "sqlite_group_dest_commits",
    SqliteRecordCapture => "sqlite_record_capture_commits",
    SqliteDirectoryPending => "sqlite_directory_pending_commits",
    SqliteDirectoryComplete => "sqlite_directory_complete_commits",
    // A superseding publish's record, written before its exchange, and a
    // record a crash left, settled by the next sweep (WP0(d), #187).
    SqliteSupersede => "sqlite_supersede_commits",
    // A destination's refusal of an entry for what its path holds,
    // remembered so a rerun refuses it without a source read (#187 review).
    SqliteRefusedOutput => "sqlite_refused_output_commits",
    SqliteCommitNs => "sqlite_commit_ns",
    // Destination publication events.
    FilesMaterialized => "files_materialized",
    DirectoriesFinished => "directories_finished",
    // Source captures sent but never recorded: stamped within one timestamp
    // tick of their capture (#86).
    TransferRacyCaptures => "transfer_racy_captures",
    // Ledger and output rows of a store written before the racy guard,
    // deleted on its first open by this engine (#125): each costs one source
    // read of its seat.
    TransferLegacyRowsInvalidated => "transfer_legacy_rows_invalidated",
    // R25's strict reading (#169): outputs a crash left durable with no row,
    // adopted on resume from their capture record without a source read;
    // existing outputs with no matching row that no record proved, each
    // asked for by manifest (a source read) as before; and staged files whose
    // capture record could not be written (no extended attributes).
    TransferUnrowedAdopted => "transfer_unrowed_adopted",
    TransferUnrowedUnproven => "transfer_unrowed_unproven",
    TransferCaptureRecordsUnset => "transfer_capture_records_unset",
    // Refused seats answered from the source ledger's record of their
    // refusal, without opening the file (#186, R25).
    TransferRefusedSeatsRemembered => "transfer_refused_seats_remembered",
    // Entries the destination refused when they were offered, from its
    // record of an earlier refusal of the same seat against the same file
    // at the path: the source reads nothing for them (#187 review, R25).
    TransferRefusedOutputsRemembered => "transfer_refused_outputs_remembered",
    // Outputs of this store replaced by a changed seat's new bytes (WP0(d),
    // #187), and exchanges undone because the displaced file was not this
    // store's own.
    OutputsSuperseded => "outputs_superseded",
    SupersedeRestored => "supersede_restored",
    // Probes of a destination device for the atomic exchange a superseding
    // publish needs: one per device and session, at the first changed seat
    // found there (#187 review).
    ExchangeProbes => "exchange_probes",
    // WP0(g) (OI-1003-Q20, Q37; #163), the source ledger under
    // `LedgerSync::Relaxed`. Row commits that returned without a sync of
    // the WAL (`synchronous=NORMAL`): each is also a
    // `sqlite_group_source_commits`, and none is a full flush.
    SourceLedgerRelaxedCommits => "source_ledger_relaxed_commits",
    // Row commits that failed and were counted, not fatal, and the rows
    // they held: each such row costs at most one more read of its seat.
    SourceLedgerCommitFailed => "source_ledger_commit_failed",
    SourceLedgerRowsDropped => "source_ledger_rows_dropped",
    // Ledger reads that failed (a damaged ledger) and were answered as a
    // miss: an unreadable ledger is an empty one.
    SourceLedgerUnreadable => "source_ledger_unreadable",
    // Seats read to build a manifest the destination asked for because the
    // ledger had no row under the seat's key. Every row a crash lost that
    // costs a read is counted here; so is a seat whose identity changed or
    // that was never recorded, which makes this an upper bound on the rows
    // lost and re-read. A seat the destination holds is answered `Reuse`
    // and never reaches the ledger, whatever the ledger lost.
    SourceLedgerMissReads => "source_ledger_miss_reads",
    // Metadata censuses of a Git checkout (one walk of its worktree each).
    CensusWalks => "census_walks",
    // Provider `SQLite` snapshots whose WAL-aware read left a wal-index
    // (`<db>-shm`) beside its source, changed or not: a stated S2 source
    // write (OI-1003-Q36).
    SourceWalIndexTouched => "source_wal_index_touched",
    // Provider `SQLite` snapshots that left a `-wal` beside a source that had
    // none: the empty `-wal` a WAL-aware open creates (OI-1003-Q72).
    SourceWalCreated => "source_wal_created",
}

const COUNT: usize = Counter::ALL.len();

static VALUES: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];

fn slot(counter: Counter) -> Option<&'static AtomicU64> {
    VALUES.get(counter as usize)
}

/// Add `amount` to `counter`.
pub fn add(counter: Counter, amount: u64) {
    if let Some(value) = slot(counter) {
        value.fetch_add(amount, Ordering::Relaxed);
    }
}

/// Add a byte length to `counter`.
pub fn add_len(counter: Counter, length: usize) {
    add(counter, u64::try_from(length).unwrap_or(u64::MAX));
}

/// Increment `counter` by one.
pub fn bump(counter: Counter) {
    add(counter, 1);
}

/// Nanoseconds elapsed since `started`, saturating.
#[must_use]
pub fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Record one successful `SQLite` commit of `kind` that took `started.elapsed()`.
///
/// Only an `Ok` outcome is counted (count and nanoseconds); a failed or
/// rolled-back statement is not a commit. Returns whether it was counted.
pub fn sqlite_commit<T, E>(kind: Counter, started: Instant, outcome: &Result<T, E>) -> bool {
    if outcome.is_ok() {
        bump(kind);
        add(Counter::SqliteCommitNs, elapsed_ns(started));
    }
    outcome.is_ok()
}

/// BLAKE3 of `data`, counted under `purpose`.
#[must_use]
pub fn hash(purpose: Counter, data: &[u8]) -> [u8; 32] {
    add_len(purpose, data.len());
    *blake3::hash(data).as_bytes()
}

/// Feed `data` to a streaming hasher, counted under `purpose`.
pub fn update(hasher: &mut blake3::Hasher, purpose: Counter, data: &[u8]) {
    add_len(purpose, data.len());
    hasher.update(data);
}

/// Run `flush`, counting one `count` and its nanoseconds under `time`.
///
/// # Errors
/// Returns the flush failure.
pub fn timed(
    count: Counter,
    time: Counter,
    flush: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    bump(count);
    let started = Instant::now();
    let result = flush();
    add(time, elapsed_ns(started));
    result
}

/// `sync_all` a regular file (`F_FULLFSYNC` on Darwin, `fsync` elsewhere),
/// counted as `flush_full`. Goes through `io::sys`, so the R-N88 trace sees it.
///
/// # Errors
/// Returns the flush failure.
pub fn sync_full(file: &File) -> std::io::Result<()> {
    timed(Counter::FlushFull, Counter::FlushFullNs, || {
        crate::io::sys::full_flush(file)
    })
}

/// `sync_data` a regular file, counted as `flush_fdatasync`: `fdatasync` on
/// Linux; on Darwin, as the standard library does, `F_FULLFSYNC`.
///
/// # Errors
/// Returns the flush failure.
pub fn sync_data(file: &File) -> std::io::Result<()> {
    timed(Counter::FlushFdatasync, Counter::FlushFdatasyncNs, || {
        data_sync(file)
    })
}

#[cfg(target_os = "linux")]
fn data_sync(file: &File) -> std::io::Result<()> {
    crate::io::sys::data_sync(file)
}

#[cfg(not(target_os = "linux"))]
fn data_sync(file: &File) -> std::io::Result<()> {
    crate::io::sys::full_flush(file)
}

/// `fcntl(F_BARRIERFSYNC)` a regular file, counted as `flush_barrier`.
///
/// Orders the file's writes before later writes without draining the device
/// cache. On Linux `sys::barrier` is `fdatasync`, still counted here.
///
/// # Errors
/// Returns the flush failure.
pub fn sync_barrier(file: &File) -> std::io::Result<()> {
    timed(Counter::FlushBarrier, Counter::FlushBarrierNs, || {
        crate::io::sys::barrier(file)
    })
}

/// `sync_all` a directory descriptor, counted as `flush_dir`.
///
/// # Errors
/// Returns the flush failure.
pub fn sync_dir(directory: &File) -> std::io::Result<()> {
    timed(Counter::FlushDir, Counter::FlushDirNs, || {
        crate::io::sys::full_flush(directory)
    })
}

/// Counted flushes in method position, for `File::open(p)?.sync_…()?` chains.
pub trait CountedSync {
    /// [`sync_full`]: a regular file, `flush_full`.
    ///
    /// # Errors
    /// Returns the flush failure.
    fn sync_file_counted(&self) -> std::io::Result<()>;

    /// [`sync_dir`]: a directory descriptor, `flush_dir`.
    ///
    /// # Errors
    /// Returns the flush failure.
    fn sync_dir_counted(&self) -> std::io::Result<()>;
}

impl CountedSync for File {
    fn sync_file_counted(&self) -> std::io::Result<()> {
        sync_full(self)
    }

    fn sync_dir_counted(&self) -> std::io::Result<()> {
        sync_dir(self)
    }
}

/// A point-in-time copy of every counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters([u64; COUNT]);

impl Counters {
    /// Snapshot every counter without resetting other observers.
    #[must_use]
    pub fn snapshot() -> Self {
        let mut values = [0_u64; COUNT];
        for (value, atomic) in values.iter_mut().zip(VALUES.iter()) {
            *value = atomic.load(Ordering::Relaxed);
        }
        Self(values)
    }

    /// Difference from an earlier snapshot.
    #[must_use]
    pub fn since(self, before: Self) -> Self {
        let mut values = self.0;
        for (value, earlier) in values.iter_mut().zip(before.0) {
            // Counters are monotonic u64s; wrapping keeps the delta exact
            // even across a (theoretical) wrap of the underlying atomic.
            *value = value.wrapping_sub(earlier);
        }
        Self(values)
    }

    /// The value of one counter.
    #[must_use]
    pub fn get(&self, counter: Counter) -> u64 {
        self.0.get(counter as usize).copied().unwrap_or(0)
    }

    fn sum(&self, counters: &[Counter]) -> u64 {
        counters.iter().fold(0_u64, |total, counter| {
            total.saturating_add(self.get(*counter))
        })
    }

    /// Sum of every BLAKE3 purpose.
    #[must_use]
    pub fn blake3_total(&self) -> u64 {
        self.sum(&[
            Counter::HashCaptureChunk,
            Counter::HashWireVerify,
            Counter::HashDestReuse,
            Counter::HashVerifyExisting,
            Counter::HashFile,
            Counter::HashBundleStage,
            Counter::HashOther,
        ])
    }

    /// BLAKE3 bytes hashed by the destination half of a transfer.
    #[must_use]
    pub fn blake3_destination(&self) -> u64 {
        self.sum(&[
            Counter::HashWireVerify,
            Counter::HashDestReuse,
            Counter::HashVerifyExisting,
        ])
    }

    /// Every `SQLite` commit counted. Each is one full flush on Darwin,
    /// except the source ledger's relaxed row commits (WP0(g)).
    #[must_use]
    pub fn sqlite_commits(&self) -> u64 {
        self.sum(&[
            Counter::SqliteSchema,
            Counter::SqliteGroupSource,
            Counter::SqliteGroupDest,
            Counter::SqliteRecordCapture,
            Counter::SqliteDirectoryPending,
            Counter::SqliteDirectoryComplete,
            Counter::SqliteSupersede,
            Counter::SqliteRefusedOutput,
        ])
    }

    /// Explicit full flushes plus `SQLite` commits (see the module notes).
    #[must_use]
    pub fn full_flushes_total(&self) -> u64 {
        self.sum(&[
            Counter::FlushFull,
            Counter::FlushFdatasync,
            Counter::FlushDir,
        ])
        .saturating_add(self.sqlite_commits())
        .saturating_sub(self.get(Counter::SourceLedgerRelaxedCommits))
    }

    /// Space-separated `key=value` pairs, ending with the derived totals.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut line = String::new();
        for (counter, value) in Counter::ALL.iter().zip(self.0) {
            if !line.is_empty() {
                line.push(' ');
            }
            let _ = write!(line, "{}={value}", counter.name());
        }
        let _ = write!(
            line,
            " blake3_total_bytes={} blake3_dest_bytes={} sqlite_commits_total={} full_flushes_total={}",
            self.blake3_total(),
            self.blake3_destination(),
            self.sqlite_commits(),
            self.full_flushes_total(),
        );
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_rendered() {
        let mut names: Vec<_> = Counter::ALL.iter().map(|counter| counter.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Counter::ALL.len());
        let before = Counters::snapshot();
        add(Counter::HashOther, 7);
        let _ = hash(Counter::HashOther, b"abc");
        let after = Counters::snapshot().since(before);
        assert!(after.get(Counter::HashOther) >= 10);
        assert!(after.blake3_total() >= 10);
        assert!(after.render().contains("blake3_other_bytes="));
        assert!(after.render().contains("blake3_total_bytes="));
    }

    // WP2 (OI-1003-Q15): a counter that nothing increments reads as a
    // measured zero. Every variant must be named by production code outside
    // this module (a source scan, so a test-only increment does not count).
    #[test]
    fn every_counter_is_incremented_somewhere() -> std::io::Result<()> {
        fn sources(dir: &std::path::Path, out: &mut String) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name == "tests") {
                        continue;
                    }
                    sources(&path, out)?;
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.ends_with("tests.rs")
                {
                    let text = std::fs::read_to_string(&path)?;
                    // Production code only: stop at the inline test module.
                    let mut production = text.split("mod tests {").next().unwrap_or("");
                    if path.ends_with("counters.rs") {
                        // Here only the flush wrappers count, not the table
                        // that declares the variants nor the derived sums.
                        let start = production.find("/// Add `amount`").unwrap_or(0);
                        let end = production.find("impl Counters {").unwrap_or(start);
                        production = production.get(start..end).unwrap_or("");
                    }
                    out.push_str(production);
                }
            }
            Ok(())
        }
        let mut text = String::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut text,
        )?;
        let unused: Vec<String> = Counter::ALL
            .iter()
            .map(|counter| format!("{counter:?}"))
            .filter(|name| !text.contains(&format!("Counter::{name}")))
            .collect();
        assert!(unused.is_empty(), "never incremented: {unused:?}");
        Ok(())
    }

    #[test]
    fn sqlite_commits_count_only_on_ok() {
        // Counters are process-global and other tests commit concurrently,
        // so the decision is checked through the return value.
        assert!(sqlite_commit(
            Counter::SqliteSchema,
            Instant::now(),
            &Ok::<(), ()>(())
        ));
        assert!(!sqlite_commit(
            Counter::SqliteSchema,
            Instant::now(),
            &Err::<(), ()>(())
        ));
        let wrapped = Counters([1; COUNT]).since(Counters([u64::MAX; COUNT]));
        assert_eq!(wrapped.get(Counter::SqliteSchema), 2);
    }

    #[test]
    fn flush_wrappers_count_by_kind() -> std::io::Result<()> {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("bulkload-counters-{}", std::process::id()));
        let file = File::create(&path)?;
        let before = Counters::snapshot();
        sync_full(&file)?;
        sync_data(&file)?;
        sync_barrier(&file)?;
        File::open(&dir)?.sync_dir_counted()?;
        let after = Counters::snapshot().since(before);
        std::fs::remove_file(&path)?;
        assert!(after.get(Counter::FlushFull) >= 1);
        assert!(after.get(Counter::FlushFdatasync) >= 1);
        assert!(after.get(Counter::FlushBarrier) >= 1);
        assert!(after.get(Counter::FlushDir) >= 1);
        Ok(())
    }
}
