//! Group commit for the transfer engine (M2 W3).
//!
//! A committer thread collects work items and makes them durable in groups.
//! A group closes at [`GROUP_FILES`] files, [`GROUP_BYTES`] bytes or
//! [`GROUP_IDLE`] without a new item. For each group the sink:
//!
//! 1. seals every file's data with [`seal_file`]: `F_BARRIERFSYNC` on Darwin,
//!    `fsync` elsewhere, or a full flush under [`Durability::Strict`];
//! 2. publishes and seals each touched directory once with [`seal_dir`];
//! 3. fully flushes each touched device other than the store's own;
//! 4. commits the group's records in one `SQLite` WAL transaction. With
//!    `synchronous=FULL` and `fullfsync=ON` that commit's `F_FULLFSYNC`
//!    drains the store's device, which is the only device-cache flush a
//!    group needs when its files share that device.
//!
//! On Linux in group mode a group of [`BATCH_MIN_FILES`] or more outputs is
//! sealed in two device-wide steps instead (S1, OI-1003-Q107): each output's
//! write-back starts as it is queued ([`start_writeback`]); the group then
//! runs one `syncfs` per device its outputs live on ([`seal_device`]), which
//! makes every temporary's data durable under its temporary name; renames
//! every output into place (or, superseding one, exchanges it in: WP0(d));
//! and runs one `syncfs` per touched device again,
//! which makes the new entries durable, before the records commit. The
//! renames never precede the first seal: a name must not survive a power
//! loss that its data did not.
//!
//! The load-bearing order is data before record: a record never commits
//! before the bytes it describes are sealed and, on another device, flushed.
//! A sink that fails stops the committer's callers at their next
//! [`Committer::submit`] or [`Committer::sync`]. Dropping a [`Committer`]
//! closes and commits whatever is pending, so an interrupted transfer keeps
//! the work it finished.

use crate::refuse::RefuseAt as _;
use std::fs::File;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// Close a group once it holds this many files.
pub const GROUP_FILES: u64 = 64;
/// Close a group once it holds this many payload bytes.
pub const GROUP_BYTES: u64 = 256 * 1024 * 1024;
/// Close a group after this long without a new item.
pub const GROUP_IDLE: Duration = Duration::from_millis(20);
/// Queued items before [`Committer::submit`] blocks the producer.
pub const QUEUE_DEPTH: usize = 64;

/// How each file is made durable before its record commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Durability {
    /// A barrier per file and per touched directory; the group's `SQLite`
    /// commit is the one full flush.
    #[default]
    Group,
    /// A full flush (`F_FULLFSYNC` / `fsync`) per file and per directory, for
    /// A/B comparison against [`Durability::Group`].
    Strict,
}

impl std::str::FromStr for Durability {
    type Err = BulkloadRefusal;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "group" => Ok(Self::Group),
            "strict" => Ok(Self::Strict),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }
}

impl std::fmt::Display for Durability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Group => "group",
            Self::Strict => "strict",
        })
    }
}

/// How the SOURCE ledger's row commits reach disk (WP0(g): OI-1003-Q20,
/// adopted with conditions by OI-1003-Q37, built on OI-1003-Q104).
///
/// The source ledger is a cache of what the source already holds: a row is
/// a seat's row key and its chunk manifest, or a remembered refusal (#186).
/// R25 is carried by the destination's committed rows, so losing a ledger
/// row costs at most one more read (or sniff) of its seat, and only where
/// the destination no longer holds that seat.
///
/// The mode covers the ledger's ROW commits and nothing else
/// (`StorePublisher::commit_captures` on a source publisher). The commit
/// that creates a store (its schema and its authority, in `Store::open`),
/// the state root's seals (#161) and every destination commit are
/// `synchronous=FULL`, `fullfsync=ON` in both modes
/// ([`configure_sqlite`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LedgerSync {
    /// `synchronous=NORMAL`, `fullfsync=OFF` on the source publisher's
    /// connection ([`relax_ledger_rows`]). A row commit appends to the WAL
    /// without syncing it, so a power loss may roll back the most recent
    /// row commits; a process crash loses none. A failed row commit is
    /// counted (`source_ledger_commit_failed`) and the transfer goes on
    /// (#163). The default (OI-1003-Q37).
    #[default]
    Relaxed,
    /// `synchronous=FULL`, `fullfsync=ON`: every row commit is durable when
    /// it returns, and the first failed one fails the session, as before
    /// WP0(g). For A/B comparison against [`LedgerSync::Relaxed`].
    Full,
}

impl std::str::FromStr for LedgerSync {
    type Err = BulkloadRefusal;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "relaxed" => Ok(Self::Relaxed),
            "full" => Ok(Self::Full),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }
}

impl std::fmt::Display for LedgerSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Relaxed => "relaxed",
            Self::Full => "full",
        })
    }
}

static MODE: AtomicU8 = AtomicU8::new(0);
static LEDGER_SYNC: AtomicU8 = AtomicU8::new(0);

/// Set the process-wide source-ledger sync mode (`--source-ledger-sync`).
pub fn set_ledger_sync(mode: LedgerSync) {
    LEDGER_SYNC.store(u8::from(mode == LedgerSync::Full), Ordering::Relaxed);
}

/// The process-wide source-ledger sync mode.
#[must_use]
pub fn ledger_sync() -> LedgerSync {
    if LEDGER_SYNC.load(Ordering::Relaxed) == 0 {
        LedgerSync::Relaxed
    } else {
        LedgerSync::Full
    }
}

/// Set the process-wide durability mode (`--durability`).
pub fn set_durability(mode: Durability) {
    MODE.store(u8::from(mode == Durability::Strict), Ordering::Relaxed);
}

/// The process-wide durability mode.
#[must_use]
pub fn durability() -> Durability {
    if MODE.load(Ordering::Relaxed) == 0 {
        Durability::Group
    } else {
        Durability::Strict
    }
}

/// The smallest group sealed with [`seal_device`] rather than file by file:
/// for one file a `syncfs` costs no less than its own flush, and it also
/// flushes whatever else is dirty on that file system.
pub const BATCH_MIN_FILES: usize = 2;

/// The first Linux release whose `syncfs` reports write-back errors.
///
/// From 5.8 a write-back error of any file of the file system is reported
/// (errseq on the superblock). Before it, a failed write could go unreported
/// by a group's device seal, so an older kernel keeps the per-file seals
/// (OI-1003-Q113).
pub const SYNCFS_REPORTS_ERRORS_SINCE: (u32, u32) = (5, 8);

/// Whether a group of `files` outputs is sealed device-wide (Linux 5.8 or
/// later, group mode, at least [`BATCH_MIN_FILES`]).
#[must_use]
pub fn batched(files: usize) -> bool {
    cfg!(target_os = "linux")
        && durability() == Durability::Group
        && files >= BATCH_MIN_FILES
        && syncfs_reports_errors()
}

/// Whether this kernel's `syncfs` reports write-back errors, read once. An
/// unreadable release counts as too old.
fn syncfs_reports_errors() -> bool {
    #[cfg(target_os = "linux")]
    {
        static REPORTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *REPORTS.get_or_init(|| {
            super::sys::kernel_release()
                .as_deref()
                .and_then(release_version)
                .is_some_and(|version| version >= SYNCFS_REPORTS_ERRORS_SINCE)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// `(major, minor)` of a kernel release string such as `6.12.0-211.el10`.
#[must_use]
pub fn release_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Start write-back of a fully written output as it is queued for its group.
///
/// Linux, group mode only, so the group's [`seal_device`] finds less to
/// write. Not a durability step: a failure is ignored, and the group seal
/// still makes the data durable.
pub fn start_writeback(file: &File) {
    #[cfg(target_os = "linux")]
    if durability() == Durability::Group {
        let _ = super::sys::start_writeback(file);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = file;
}

/// Seal every file of the file system holding `handle`: its data, metadata
/// and directory entries (Linux `syncfs`). Only a [`batched`] group calls it.
///
/// # Errors
/// Returns the flush failure; `Unsupported` off Linux.
pub fn seal_device(handle: &File) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        counters::timed(Counter::FlushFs, Counter::FlushFsNs, || {
            super::sys::sync_fs(handle)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = handle;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// Seal one file's data ahead of any record that describes it.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_file(file: &File) -> std::io::Result<()> {
    match durability() {
        Durability::Group => {
            counters::timed(Counter::FlushBarrier, Counter::FlushBarrierNs, || {
                file_seal(file)
            })
        }
        Durability::Strict => counters::timed(Counter::FlushFull, Counter::FlushFullNs, || {
            super::sys::full_flush(file)
        }),
    }
}

/// The group-mode seal for one file: `F_BARRIERFSYNC` on Darwin. Elsewhere
/// `fsync`, not the io layer's `fdatasync` barrier: a mode set with `fchmod`
/// after the last write must be durable with the data (PR #59 review, F4).
#[cfg(target_vendor = "apple")]
fn file_seal(file: &File) -> std::io::Result<()> {
    super::sys::barrier(file)
}

#[cfg(not(target_vendor = "apple"))]
fn file_seal(file: &File) -> std::io::Result<()> {
    super::sys::full_flush(file)
}

#[cfg(test)]
thread_local! {
    static FAIL_DIR_SEALS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, this thread's [`seal_dir`] calls fail with `EIO`
/// before any syscall, as a device error would.
#[cfg(test)]
pub fn fail_dir_seals(on: bool) {
    FAIL_DIR_SEALS.with(|flag| flag.set(on));
}

/// Seal one directory's entries ahead of any record that depends on them.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_dir(directory: &File) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DIR_SEALS.with(std::cell::Cell::get) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    match durability() {
        Durability::Group => {
            counters::timed(Counter::FlushDirBarrier, Counter::FlushDirBarrierNs, || {
                super::sys::barrier_dir(directory)
            })
        }
        Durability::Strict => counters::timed(Counter::FlushDir, Counter::FlushDirNs, || {
            super::sys::full_flush(directory)
        }),
    }
}

/// Make a private state root durable now, whatever the [`Durability`] mode.
///
/// #161, R25: the root's entry in `parent` and the entries it holds (a
/// store's database and WAL). A store's authority leaves the host at Start
/// and its records are committed inside the root, so ordering is not
/// enough: both are on stable media when this returns.
///
/// `parent` is sealed with [`seal_dir`] and `root` with a full flush
/// (counted as `flush_dir`). On Darwin that full flush also drains the
/// drive, and with it the parent's barrier; a parent on another device (the
/// root is a mount point) is fully flushed as well.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_state_root(parent: &File, root: &File) -> std::io::Result<()> {
    seal_dir(parent)?;
    counters::sync_dir(root)?;
    if super::sys::fstat(parent)?.node.dev != super::sys::fstat(root)?.node.dev {
        counters::sync_dir(parent)?;
    }
    Ok(())
}

/// Configure a bulkload-owned `SQLite` store for durable commits: WAL,
/// `synchronous=FULL`, and `fullfsync=ON` with `checkpoint_fullfsync=ON`.
///
/// Bundled `SQLite` on Darwin issues a plain `fsync` unless `fullfsync` is on,
/// and a plain Darwin `fsync` neither drains the device cache nor orders
/// writes, so a commit could be lost or torn by power loss. With these
/// settings each commit syncs the WAL with `F_FULLFSYNC`; a commit that runs
/// an automatic checkpoint (at 1,000 WAL pages) also syncs the WAL and the
/// database file. Checkpoint-on-close is off:
/// the WAL is durable, so closing a store never adds a flush of its own, and
/// automatic checkpoints happen inside later commits.
///
/// Never apply this to a provider database; those are not bulkload's to
/// reconfigure.
///
/// # Errors
/// Refuses if `SQLite` rejects a setting or will not enter WAL mode.
pub fn configure_sqlite(conn: &rusqlite::Connection) -> Result<()> {
    let refuse = |_| BulkloadRefusal::SqliteIntegrityCheckFailed;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(refuse)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    conn.execute_batch(
        "PRAGMA synchronous=FULL;
        PRAGMA fullfsync=ON;
        PRAGMA checkpoint_fullfsync=ON;",
    )
    .map_err(refuse)?;
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(refuse)?;
    Ok(())
}

/// Relax the row commits of a SOURCE ledger connection (WP0(g),
/// [`LedgerSync::Relaxed`]): `synchronous=NORMAL` and `fullfsync=OFF`.
///
/// Call it only on the source publisher's own connection, and only after
/// `Store::open` has returned on it: the store's creation commit (schema
/// and authority) and its root seals are then already durable, and nothing
/// this connection commits afterwards can undo them. In WAL mode a NORMAL
/// commit appends its frames without syncing the WAL. A power loss can
/// then cut the WAL's tail, and recovery stops at the last commit whose
/// frame checksums hold: commits are lost newest first, and a surviving
/// row is never a torn one.
///
/// `checkpoint_fullfsync` stays ON ([`configure_sqlite`] set it): a
/// checkpoint still syncs the WAL and then the database with a full flush
/// (`F_FULLFSYNC` on Darwin), so what a checkpoint moved is durable and
/// the WAL header a later commit rewrites is synced before its frames.
///
/// # Errors
/// Refuses if `SQLite` rejects a setting or does not report it back.
pub fn relax_ledger_rows(conn: &rusqlite::Connection) -> Result<()> {
    let refuse = |_| BulkloadRefusal::SqliteIntegrityCheckFailed;
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
        PRAGMA fullfsync=OFF;",
    )
    .map_err(refuse)?;
    let read = |pragma: &str| -> Result<i64> {
        conn.query_row(pragma, [], |row| row.get(0)).map_err(refuse)
    };
    // 1 is NORMAL. A connection that did not take the settings would commit
    // rows under a mode the counters do not report.
    if read("PRAGMA synchronous")? != 1
        || read("PRAGMA fullfsync")? != 0
        || read("PRAGMA checkpoint_fullfsync")? != 1
    {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    Ok(())
}

/// Which store a sink commits to, for per-side test limits (F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupSide {
    /// The source store (the digest-only capture ledger).
    Source,
    /// The destination store (outputs).
    Destination,
    /// Neither (tests of the committer itself).
    Other,
}

/// The work a [`Committer`] makes durable.
pub trait GroupSink: Send + 'static {
    /// The store this sink commits to.
    const SIDE: GroupSide = GroupSide::Other;

    /// One unit of work.
    type Item: Send + 'static;
    /// What [`Committer::finish`] hands back.
    type Report: Send + 'static;

    /// `(files, bytes)` this item adds to the open group.
    fn weight(item: &Self::Item) -> (u64, u64);

    /// Seal, publish and commit one closed group, in that order.
    fn commit(&mut self, items: Vec<Self::Item>);

    /// The failure that stops this sink, once one group has failed.
    fn failure(&self) -> Option<BulkloadRefusal>;

    /// Called once after the last group has committed.
    fn finish(self) -> Self::Report;
}

/// Group-close and queue limits for one [`Committer`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Close a group once it holds this many files.
    pub group_files: u64,
    /// Close a group once it holds this many payload bytes.
    pub group_bytes: u64,
    /// Close a group after this long without a new item.
    pub group_idle: Duration,
    /// Queued items before [`Committer::submit`] blocks the producer.
    pub queue_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            group_files: GROUP_FILES,
            group_bytes: GROUP_BYTES,
            group_idle: GROUP_IDLE,
            queue_depth: QUEUE_DEPTH,
        }
    }
}

type Failure = std::sync::Arc<std::sync::Mutex<Option<BulkloadRefusal>>>;

enum Message<T> {
    Item(T),
    Sync(SyncSender<()>),
}

/// A running committer thread. Dropping it commits pending work and joins.
pub struct Committer<S: GroupSink> {
    sender: Option<SyncSender<Message<S::Item>>>,
    handle: Option<JoinHandle<S::Report>>,
    failure: Failure,
}

impl<S: GroupSink> Committer<S> {
    /// Start the committer thread for `sink`.
    ///
    /// # Errors
    /// Refuses if the thread cannot be spawned.
    pub fn spawn(sink: S) -> Result<Self> {
        Self::spawn_with(sink, Limits::default())
    }

    /// Start the committer thread for `sink` with explicit `limits`.
    ///
    /// # Errors
    /// Refuses if the thread cannot be spawned.
    pub fn spawn_with(sink: S, limits: Limits) -> Result<Self> {
        #[cfg(feature = "fault-injection")]
        let limits = {
            let mut limits = crate::fault::group_files().map_or(limits, |group_files| Limits {
                group_files,
                ..limits
            });
            if let Some((files, idle)) = crate::fault::side_group(S::SIDE) {
                limits.group_files = files;
                limits.group_idle = idle.unwrap_or(limits.group_idle);
            }
            limits
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(limits.queue_depth.max(1));
        let failure = Failure::default();
        let shared = Failure::clone(&failure);
        let handle = std::thread::Builder::new()
            .name("bulkload-commit".to_owned())
            .spawn(move || run(sink, &receiver, limits, &shared))
            .refuse_at("io::durable::spawn_with")?;
        Ok(Self {
            sender: Some(sender),
            handle: Some(handle),
            failure,
        })
    }

    fn failed(&self) -> Result<()> {
        self.failure
            .lock()
            .map_or(Err(BulkloadRefusal::Io(None)), |failure| {
                failure.clone().map_or(Ok(()), Err)
            })
    }

    /// Queue one item. Blocks while the queue is full.
    ///
    /// # Errors
    /// Returns the sink's failure once a group has failed, or refuses if the
    /// committer thread has stopped.
    pub fn submit(&self, item: S::Item) -> Result<()> {
        self.failed()?;
        self.sender
            .as_ref()
            .ok_or(BulkloadRefusal::Io(None))?
            .send(Message::Item(item))
            .map_err(|_| BulkloadRefusal::Io(None))
    }

    /// Close the open group now and wait until it has committed.
    ///
    /// # Errors
    /// Returns the sink's failure if this or an earlier group failed, or
    /// refuses if the committer thread has stopped.
    pub fn sync(&self) -> Result<()> {
        self.failed()?;
        let (ack, done) = std::sync::mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(BulkloadRefusal::Io(None))?
            .send(Message::Sync(ack))
            .map_err(|_| BulkloadRefusal::Io(None))?;
        done.recv().map_err(|_| BulkloadRefusal::Io(None))?;
        self.failed()
    }

    /// Commit everything pending, stop the thread and return its report.
    ///
    /// # Errors
    /// Refuses [`BulkloadRefusal::WorkerLost`] if the committer thread
    /// panicked.
    pub fn finish(mut self) -> Result<S::Report> {
        drop(self.sender.take());
        self.handle
            .take()
            .ok_or(BulkloadRefusal::WorkerLost)?
            .join()
            .map_err(|_| BulkloadRefusal::WorkerLost)
    }
}

impl<S: GroupSink> Drop for Committer<S> {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct OpenGroup<T> {
    items: Vec<T>,
    files: u64,
    bytes: u64,
}

impl<T> OpenGroup<T> {
    const fn new() -> Self {
        Self {
            items: Vec::new(),
            files: 0,
            bytes: 0,
        }
    }

    fn close<S: GroupSink<Item = T>>(&mut self, sink: &mut S, failure: &Failure) {
        if self.items.is_empty() {
            return;
        }
        counters::bump(Counter::DurableGroups);
        self.files = 0;
        self.bytes = 0;
        sink.commit(std::mem::take(&mut self.items));
        if let Some(refusal) = sink.failure() {
            if let Ok(mut shared) = failure.lock() {
                shared.get_or_insert(refusal);
            }
        }
    }
}

fn run<S: GroupSink>(
    mut sink: S,
    receiver: &Receiver<Message<S::Item>>,
    limits: Limits,
    failure: &Failure,
) -> S::Report {
    let mut group = OpenGroup::new();
    loop {
        let message = if group.items.is_empty() {
            match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(limits.group_idle) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    group.close(&mut sink, failure);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        match message {
            Message::Item(item) => {
                let (files, bytes) = S::weight(&item);
                group.files = group.files.saturating_add(files);
                group.bytes = group.bytes.saturating_add(bytes);
                group.items.push(item);
                if group.files >= limits.group_files || group.bytes >= limits.group_bytes {
                    group.close(&mut sink, failure);
                }
            }
            Message::Sync(ack) => {
                group.close(&mut sink, failure);
                let _ = ack.send(());
            }
        }
    }
    group.close(&mut sink, failure);
    sink.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// OI-1003-Q113: a release is read as `(major, minor)`, and only 5.8 or
    /// later seals a group device-wide.
    #[test]
    fn the_syncfs_floor_reads_the_release_major_and_minor() {
        assert_eq!(
            release_version("6.12.0-211.51.1.el10_2.x86_64"),
            Some((6, 12))
        );
        assert_eq!(release_version("5.8.0"), Some((5, 8)));
        assert_eq!(release_version("5.7.19-arch1"), Some((5, 7)));
        assert_eq!(release_version("4.19"), Some((4, 19)));
        assert_eq!(release_version("garbage"), None);
        assert_eq!(release_version("6"), None);
        assert!(release_version("5.7.19").is_some_and(|v| v < SYNCFS_REPORTS_ERRORS_SINCE));
        assert!(release_version("5.8.0").is_some_and(|v| v >= SYNCFS_REPORTS_ERRORS_SINCE));
        assert!(release_version("5.10.1").is_some_and(|v| v >= SYNCFS_REPORTS_ERRORS_SINCE));
    }

    struct Recorder(Arc<Mutex<Vec<Vec<u64>>>>);

    impl GroupSink for Recorder {
        type Item = u64;
        type Report = usize;

        fn weight(item: &u64) -> (u64, u64) {
            (1, *item)
        }

        fn commit(&mut self, items: Vec<u64>) {
            if let Ok(mut groups) = self.0.lock() {
                groups.push(items);
            }
        }

        fn failure(&self) -> Option<BulkloadRefusal> {
            None
        }

        fn finish(self) -> usize {
            self.0.lock().map_or(0, |groups| groups.len())
        }
    }

    #[test]
    fn groups_close_on_count_bytes_sync_and_drop() -> Result<()> {
        let groups = Arc::new(Mutex::new(Vec::new()));
        let committer = Committer::spawn(Recorder(Arc::clone(&groups)))?;
        for _ in 0..GROUP_FILES {
            committer.submit(1)?;
        }
        committer.submit(GROUP_BYTES)?;
        committer.submit(7)?;
        committer.sync()?;
        committer.submit(9)?;
        let closed = committer.finish()?;
        let sizes: Vec<_> = groups
            .lock()
            .map_err(|_| BulkloadRefusal::Io(None))?
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(closed, sizes.len());
        // 64 files close the first group; one oversized item closes the
        // second; sync closes the third; finish commits the last.
        assert_eq!(sizes, [64, 1, 1, 1]);
        Ok(())
    }

    #[test]
    fn an_idle_group_closes_without_sync() -> Result<()> {
        let groups = Arc::new(Mutex::new(Vec::new()));
        let committer = Committer::spawn(Recorder(Arc::clone(&groups)))?;
        committer.submit(1)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while groups.lock().map_or(0, |groups| groups.len()) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "idle group never closed"
            );
            std::thread::yield_now();
        }
        drop(committer);
        Ok(())
    }

    #[test]
    fn durability_parses_and_round_trips() {
        assert_eq!("group".parse(), Ok(Durability::Group));
        assert_eq!("strict".parse(), Ok(Durability::Strict));
        assert!("fast".parse::<Durability>().is_err());
        assert_eq!(Durability::Strict.to_string(), "strict");
    }
}
