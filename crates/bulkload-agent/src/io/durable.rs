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
//! The load-bearing order is data before record: a record never commits
//! before the bytes it describes are sealed and, on another device, flushed.
//! A sink that fails stops the committer's callers at their next
//! [`Committer::submit`] or [`Committer::sync`]. Dropping a [`Committer`]
//! closes and commits whatever is pending, so an interrupted transfer keeps
//! the work it finished.

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

static MODE: AtomicU8 = AtomicU8::new(0);

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

/// Seal one file's data ahead of any record that describes it.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_file(file: &File) -> std::io::Result<()> {
    match durability() {
        Durability::Group => {
            counters::timed(Counter::FlushBarrier, Counter::FlushBarrierNs, || {
                super::sys::barrier(file)
            })
        }
        Durability::Strict => counters::timed(Counter::FlushFull, Counter::FlushFullNs, || {
            super::sys::full_flush(file)
        }),
    }
}

/// Seal one directory's entries ahead of any record that depends on them.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_dir(directory: &File) -> std::io::Result<()> {
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

/// The work a [`Committer`] makes durable.
pub trait GroupSink: Send + 'static {
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
    /// Queued items before [`Committer::submit`] blocks the producer.
    pub queue_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            group_files: GROUP_FILES,
            group_bytes: GROUP_BYTES,
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
        let (sender, receiver) = std::sync::mpsc::sync_channel(limits.queue_depth.max(1));
        let failure = Failure::default();
        let shared = Failure::clone(&failure);
        let handle = std::thread::Builder::new()
            .name("bulkload-commit".to_owned())
            .spawn(move || run(sink, &receiver, limits, &shared))?;
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
    /// Refuses if the committer thread panicked.
    pub fn finish(mut self) -> Result<S::Report> {
        drop(self.sender.take());
        self.handle
            .take()
            .ok_or(BulkloadRefusal::Io(None))?
            .join()
            .map_err(|_| BulkloadRefusal::Io(None))
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
            match receiver.recv_timeout(GROUP_IDLE) {
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
