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
//! Flush counts are attempts (a failed flush is still counted). `SQLite`
//! commit counters count successful commits only.
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
    SourceCaptureReuseRead => "read_source_capture_reuse_bytes",
    SourcePackReadback => "read_source_pack_readback_bytes",
    DestLocalReuseRead => "read_dest_local_reuse_bytes",
    DestVerifyRead => "read_dest_verify_existing_bytes",
    OtherChunkRead => "read_other_chunk_bytes",
    HashFileRead => "read_hash_file_bytes",
    // Bytes written, by stage.
    SourcePackWrite => "write_source_pack_bytes",
    DestPackWrite => "write_dest_pack_bytes",
    LegacyChunkWrite => "write_legacy_chunk_bytes",
    DestMaterializeWrite => "write_dest_materialize_bytes",
    // Framed transport, as seen by this process.
    WireFramesSent => "wire_frames_sent",
    WireBytesSent => "wire_bytes_sent",
    WireFramesReceived => "wire_frames_received",
    WireBytesReceived => "wire_bytes_received",
    // BLAKE3 bytes hashed, by purpose.
    HashCaptureChunk => "blake3_capture_chunk_bytes",
    HashCaptureFile => "blake3_capture_file_bytes",
    HashStoreReadVerify => "blake3_store_read_verify_bytes",
    HashLegacyPut => "blake3_legacy_put_bytes",
    HashWireVerify => "blake3_dest_wire_verify_bytes",
    HashDestReuse => "blake3_dest_local_reuse_bytes",
    HashVerifyExisting => "blake3_verify_existing_bytes",
    HashFile => "blake3_hash_file_bytes",
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
    SqliteCommitNs => "sqlite_commit_ns",
    // Destination publication events.
    FilesMaterialized => "files_materialized",
    DirectoriesFinished => "directories_finished",
    // Source captures sent but never recorded: stamped within one timestamp
    // tick of their capture (#86).
    TransferRacyCaptures => "transfer_racy_captures",
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
            Counter::HashCaptureFile,
            Counter::HashStoreReadVerify,
            Counter::HashLegacyPut,
            Counter::HashWireVerify,
            Counter::HashDestReuse,
            Counter::HashVerifyExisting,
            Counter::HashFile,
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

    /// Every `SQLite` commit counted, each one full flush on Darwin.
    #[must_use]
    pub fn sqlite_commits(&self) -> u64 {
        self.sum(&[
            Counter::SqliteSchema,
            Counter::SqliteGroupSource,
            Counter::SqliteGroupDest,
            Counter::SqliteRecordCapture,
            Counter::SqliteDirectoryPending,
            Counter::SqliteDirectoryComplete,
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
