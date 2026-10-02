//! Crash-consistency fault injection. Compiled only with `fault-injection`.
//!
//! A shipped build contains none of this: without the feature every
//! `fault_point!` and `fault_mid_read!` call site expands to an empty block.
//!
//! # Model
//!
//! `BULKLOAD_FAULT=<point>[:<nth>]` arms exactly one named point. The `nth` hit
//! of that point in this process (default `1`, counted across all threads)
//! ends the process on the spot with [`libc::_exit`] and the fixed status
//! [`FAULT_EXIT_CODE`]. `_exit` runs no destructors, no `atexit` handlers and
//! no stdio flushes, and it never returns, so the on-disk state is exactly
//! what the durability path had made visible when the point was reached.
//!
//! The process only ever terminates itself. Nothing here sends a signal to any
//! process, this one included (R-N11).
//!
//! # What `_exit` can and cannot prove
//!
//! This models a **process crash only**. Bytes the kernel already accepted
//! survive in the page cache whether or not they were synced, so a harness
//! built on these points **cannot detect a missing or misplaced fsync**. It
//! does detect ordering bugs visible without power loss, such as a record
//! committed before its data is written, a final name exposed before its
//! content is complete, or a resume that cannot adopt what a crash left.
//! Power-loss coverage is the remaining W7 follow-up (R-N88, tracked on #49):
//! a syscall-log (ALICE-style) crash-state checker, or dm-log-writes replay.
//!
//! # Crash receipt
//!
//! With `BULKLOAD_FAULT_RECEIPT=<path>` set, the process writes the armed
//! point's name, and for publication points the canonical store root on a
//! second line and the crashing group's composition on a third
//! (`group capture_ids=<ids> chunks=<n>`), to `<path>` just before it exits.
//! For a directory-creation point the second line is instead the creation
//! path in use: `directory_create=rename` or `directory_create=fallback`
//! (R-N119). Harnesses use it to check which store a crash landed in, to
//! reproduce a failing `_mid` crash from the group it hit, and to see which
//! directory path a crash interrupted.
//!
//! An `BULKLOAD_FAULT` value that names no point, or has an `nth` of zero or a
//! non-number, ends the process at the first fault point it reaches with
//! [`FAULT_SPEC_INVALID_EXIT_CODE`], so a typo can never pass as a clean run.
//!
//! # Fault points
//!
//! Source ledger groups, in `StorePublisher::commit_captures` (the source
//! store's committer thread). The ledger is digest-only (R-N58): there is no
//! pack, so nothing is written or sealed before the transaction.
//!
//! | Name | Crash leaves (in the source store) |
//! |------|------------------------------------|
//! | `publish.source.after_manifest_insert` | an open transaction with the group's captures |
//! | `publish.source.before_commit` | the whole group staged, `COMMIT` not issued |
//! | `publish.source.after_commit` | the group committed |
//!
//! Destination output groups, in `StagedFile::publish`, `PublishSink::commit`
//! and `StorePublisher::commit_outputs` (the destination committer thread),
//! plus the staging step on the receiving thread:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `materialize.after_temp_write` | a complete `.bulkload-*` temporary at its final mode, not sealed, not queued |
//! | `materialize.after_temp_seal` | the temporary sealed, not renamed |
//! | `materialize.after_rename` | the final name holding the sealed file; no output record |
//! | `publish.destination.after_dir_seal` | a group's files renamed and its directories sealed; no records |
//! | `publish.destination.before_commit` | the group's records staged, `COMMIT` not issued |
//! | `publish.destination.after_commit` | the group's output records and chunk hints committed |
//!
//! Every temporary one of these leaves carries the destination store's tag and
//! is removed by the next invocation's sweep; see `materialize`.
//!
//! Directory publication, in `Destination::directory` and `finish_directories`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `directory.after_mkdir` | an empty 0700 `.bulkload-<tag>-d-*` temporary directory, no record |
//! | `directory.after_pending_record` | the temporary, its parent sealed, and a record bound to its inode; not renamed |
//! | `directory.after_rename` | the 0700 directory under its final name, bound record; parent sealed, not flushed |
//! | `directory.after_fallback_mkdir` | with no no-replace rename (R-N119): a 0700 directory under its final name, no record |
//! | `directory.before_complete` | one directory's final mode applied and sealed; its pending record still present |
//!
//! Protocol boundaries, in `transfer`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `serve.after_content` | one entry's `End` sent; its capture may not be committed |
//! | `serve.before_done` | every capture committed, `SourceDone` not sent |
//! | `receive.after_decide` | one entry decided, no content received for it |
//! | `receive.after_chunks` | one file's chunks verified, covering it, and written to its temporary |
//! | `receive.after_end` | an entry's `End` handled; its output may still be waiting on its group |
//!
//! # Group size
//!
//! Group commit closes a group by count, size or idle time, so how many
//! groups a run has depends on timing. With [`GROUP_FILES_ENV`] set to `N`,
//! every committer closes a group at `N` files. Only `N = 1` makes `nth` hits
//! of the group points exact: with a larger `N` the idle timeout can still
//! close a group early.
//!
//! # Live-writer hook
//!
//! [`set_mid_read_hook`] registers a mutation for every source file under a
//! root. The capture path runs it once per file, after the first content chunk
//! has been read and before the remainder is. Tests use it to rewrite, truncate
//! or replace a file under an open capture.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Environment variable that arms one fault point: `<point>[:<nth>]`.
pub const FAULT_ENV: &str = "BULKLOAD_FAULT";

/// Exit status of a process that reached its armed fault point.
pub const FAULT_EXIT_CODE: i32 = 86;

/// Exit status of a process whose `BULKLOAD_FAULT` value is malformed.
pub const FAULT_SPEC_INVALID_EXIT_CODE: i32 = 87;

/// Optional path the process writes its crash receipt to before exiting.
pub const FAULT_RECEIPT_ENV: &str = "BULKLOAD_FAULT_RECEIPT";

/// Optional group size, in files, for every committer (see "Group size").
pub const GROUP_FILES_ENV: &str = "BULKLOAD_FAULT_GROUP_FILES";

/// Optional group limits for the source store's committer (F3).
///
/// The value is `<files>[:<idle_ms>]`. It overrides [`GROUP_FILES_ENV`] on
/// that side, so a scenario can, for example, hold every capture until the
/// end while outputs commit one at a time (PR #59 review).
pub const GROUP_SOURCE_ENV: &str = "BULKLOAD_FAULT_GROUP_SOURCE";

/// As [`GROUP_SOURCE_ENV`], for the destination store's committer.
pub const GROUP_DESTINATION_ENV: &str = "BULKLOAD_FAULT_GROUP_DESTINATION";

/// The per-side group limits asked for: `(files, idle)`. `None` when unset or
/// malformed (zero files, or a non-numeric part).
#[must_use]
pub fn side_group(
    side: crate::io::durable::GroupSide,
) -> Option<(u64, Option<std::time::Duration>)> {
    let name = match side {
        crate::io::durable::GroupSide::Source => GROUP_SOURCE_ENV,
        crate::io::durable::GroupSide::Destination => GROUP_DESTINATION_ENV,
        crate::io::durable::GroupSide::Other => return None,
    };
    parse_group_limits(&std::env::var(name).ok()?)
}

/// Parse a per-side group value, `<files>[:<idle_ms>]`.
#[must_use]
pub fn parse_group_limits(value: &str) -> Option<(u64, Option<std::time::Duration>)> {
    let (files, idle) = match value.split_once(':') {
        Some((files, idle)) => (files, Some(idle)),
        None => (value, None),
    };
    let files = files.parse().ok().filter(|files| *files > 0)?;
    let idle = match idle {
        Some(idle) => Some(std::time::Duration::from_millis(idle.parse().ok()?)),
        None => None,
    };
    Some((files, idle))
}

/// The group size [`GROUP_FILES_ENV`] asks for, if it names a positive count.
#[must_use]
pub fn group_files() -> Option<u64> {
    std::env::var(GROUP_FILES_ENV)
        .ok()?
        .parse()
        .ok()
        .filter(|files| *files > 0)
}

/// A named crash point in the durability path. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Point {
    /// `publish.source.after_manifest_insert`
    PublishSourceAfterManifestInsert,
    /// `publish.source.before_commit`
    PublishSourceBeforeCommit,
    /// `publish.source.after_commit`
    PublishSourceAfterCommit,
    /// `materialize.after_temp_write`
    MaterializeAfterTempWrite,
    /// `materialize.after_temp_seal`
    MaterializeAfterTempSeal,
    /// `materialize.after_rename`
    MaterializeAfterRename,
    /// `publish.destination.after_dir_seal`
    PublishDestinationAfterDirSeal,
    /// `publish.destination.before_commit`
    PublishDestinationBeforeCommit,
    /// `publish.destination.after_commit`
    PublishDestinationAfterCommit,
    /// `directory.after_mkdir`
    DirectoryAfterMkdir,
    /// `directory.after_pending_record`
    DirectoryAfterPendingRecord,
    /// `directory.after_rename`
    DirectoryAfterRename,
    /// `directory.after_fallback_mkdir`
    DirectoryAfterFallbackMkdir,
    /// `directory.before_complete`
    DirectoryBeforeComplete,
    /// `serve.after_content`
    ServeAfterContent,
    /// `serve.before_done`
    ServeBeforeDone,
    /// `receive.after_decide`
    ReceiveAfterDecide,
    /// `receive.after_chunks`
    ReceiveAfterChunks,
    /// `receive.after_end`
    ReceiveAfterEnd,
}

impl Point {
    /// Every fault point, in durability-path order.
    pub const ALL: [Self; 19] = [
        Self::PublishSourceAfterManifestInsert,
        Self::PublishSourceBeforeCommit,
        Self::PublishSourceAfterCommit,
        Self::MaterializeAfterTempWrite,
        Self::MaterializeAfterTempSeal,
        Self::MaterializeAfterRename,
        Self::PublishDestinationAfterDirSeal,
        Self::PublishDestinationBeforeCommit,
        Self::PublishDestinationAfterCommit,
        Self::DirectoryAfterMkdir,
        Self::DirectoryAfterPendingRecord,
        Self::DirectoryAfterRename,
        Self::DirectoryAfterFallbackMkdir,
        Self::DirectoryBeforeComplete,
        Self::ServeAfterContent,
        Self::ServeBeforeDone,
        Self::ReceiveAfterDecide,
        Self::ReceiveAfterChunks,
        Self::ReceiveAfterEnd,
    ];

    /// The name `BULKLOAD_FAULT` uses for this point.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PublishSourceAfterManifestInsert => "publish.source.after_manifest_insert",
            Self::PublishSourceBeforeCommit => "publish.source.before_commit",
            Self::PublishSourceAfterCommit => "publish.source.after_commit",
            Self::MaterializeAfterTempWrite => "materialize.after_temp_write",
            Self::MaterializeAfterTempSeal => "materialize.after_temp_seal",
            Self::MaterializeAfterRename => "materialize.after_rename",
            Self::PublishDestinationAfterDirSeal => "publish.destination.after_dir_seal",
            Self::PublishDestinationBeforeCommit => "publish.destination.before_commit",
            Self::PublishDestinationAfterCommit => "publish.destination.after_commit",
            Self::DirectoryAfterMkdir => "directory.after_mkdir",
            Self::DirectoryAfterPendingRecord => "directory.after_pending_record",
            Self::DirectoryAfterRename => "directory.after_rename",
            Self::DirectoryAfterFallbackMkdir => "directory.after_fallback_mkdir",
            Self::DirectoryBeforeComplete => "directory.before_complete",
            Self::ServeAfterContent => "serve.after_content",
            Self::ServeBeforeDone => "serve.before_done",
            Self::ReceiveAfterDecide => "receive.after_decide",
            Self::ReceiveAfterChunks => "receive.after_chunks",
            Self::ReceiveAfterEnd => "receive.after_end",
        }
    }

    /// Look a point up by its `BULKLOAD_FAULT` name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.name() == name)
    }
}

/// Parse a `<point>[:<nth>]` value. `nth` counts from 1.
#[must_use]
pub fn parse(spec: &str) -> Option<(Point, u64)> {
    let (name, nth) = match spec.split_once(':') {
        Some((name, nth)) => (name, nth.parse::<u64>().ok()?),
        None => (spec, 1),
    };
    if nth == 0 {
        return None;
    }
    Some((Point::from_name(name)?, nth))
}

enum Armed {
    Disarmed,
    Invalid,
    Point(Point, u64),
}

static ARMED: OnceLock<Armed> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);

fn armed() -> &'static Armed {
    ARMED.get_or_init(|| match std::env::var(FAULT_ENV) {
        Err(std::env::VarError::NotPresent) => Armed::Disarmed,
        Err(std::env::VarError::NotUnicode(_)) => Armed::Invalid,
        Ok(spec) => parse(&spec).map_or(Armed::Invalid, |(point, nth)| Armed::Point(point, nth)),
    })
}

fn terminate(code: i32) -> ! {
    // SAFETY: `_exit` takes no pointers, is async-signal-safe and never
    // returns. Skipping destructors is the point: the process ends with only
    // the state the durability path has already handed to the kernel.
    unsafe { libc::_exit(code) }
}

/// Record one hit of `point`; the armed `nth` hit ends the process.
pub fn hit(point: Point) {
    hit_at(point, None);
}

/// [`hit`] for a point inside a store; the receipt names `store`.
pub fn hit_in(point: Point, store: &Path) {
    hit_at(point, Some(store));
}

std::thread_local! {
    static GROUP: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static DIRECTORY: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// Clears this thread's group note when the group's publication ends.
#[must_use = "the group note is cleared as soon as the guard is dropped"]
pub struct GroupNote(());

impl Drop for GroupNote {
    fn drop(&mut self) {
        GROUP.with(|group| *group.borrow_mut() = None);
    }
}

/// Describe the publication group this thread is publishing, until the
/// returned guard drops.
///
/// A crash receipt for a publication point records it. Each store's committer
/// publishes on its own thread, so the note is thread-local; it is cleared
/// per group, so no receipt names a finished group. A source group lists the
/// entry numbers of its captures, distinct and ascending, with no chunks (the
/// ledger is digest-only); a destination group lists one index per output,
/// `0..n`.
pub fn note_group(capture_ids: &[usize], chunks: usize) -> GroupNote {
    let ids: Vec<String> = capture_ids.iter().map(ToString::to_string).collect();
    let description = format!("group capture_ids={} chunks={chunks}", ids.join(","));
    GROUP.with(|group| *group.borrow_mut() = Some(description));
    GroupNote(())
}

/// Name the directory-creation path this thread is on (`rename` or
/// `fallback`); a crash receipt for a directory-creation point records it.
pub fn note_directory(path: Option<&'static str>) {
    DIRECTORY.with(|directory| directory.set(path));
}

const fn creates_directory(point: Point) -> bool {
    matches!(
        point,
        Point::DirectoryAfterMkdir
            | Point::DirectoryAfterPendingRecord
            | Point::DirectoryAfterRename
            | Point::DirectoryAfterFallbackMkdir
    )
}

fn receipt(point: Point, store: Option<&Path>) {
    if let Some(path) = std::env::var_os(FAULT_RECEIPT_ENV) {
        let mut body = format!("{}\n", point.name());
        if let Some(store) = store {
            body.push_str(&store.display().to_string());
            body.push('\n');
            if let Some(group) = GROUP.with(|group| group.borrow().clone()) {
                body.push_str(&group);
                body.push('\n');
            }
        } else if creates_directory(point) {
            if let Some(directory) = DIRECTORY.with(std::cell::Cell::get) {
                body.push_str("directory_create=");
                body.push_str(directory);
                body.push('\n');
            }
        }
        // Best effort: the page cache outlives `_exit`, no sync is needed.
        let _ = std::fs::write(path, body);
    }
}

fn hit_at(point: Point, store: Option<&Path>) {
    match armed() {
        Armed::Invalid => {
            eprintln!("bulkload-agent: {FAULT_ENV} names no fault point");
            terminate(FAULT_SPEC_INVALID_EXIT_CODE);
        }
        Armed::Point(armed, nth) if *armed == point => {
            if HITS.fetch_add(1, Ordering::SeqCst).saturating_add(1) == *nth {
                receipt(point, store);
                terminate(FAULT_EXIT_CODE);
            }
        }
        Armed::Disarmed | Armed::Point(..) => (),
    }
}

type Mutation = Arc<dyn Fn(&Path) + Send + Sync>;

static MID_READ_HOOKS: Mutex<Vec<(u64, PathBuf, Mutation)>> = Mutex::new(Vec::new());
static NEXT_HOOK: AtomicU64 = AtomicU64::new(0);

/// Removes its mid-read hook when dropped.
#[must_use = "the hook is removed as soon as the guard is dropped"]
pub struct MidReadHook(u64);

impl Drop for MidReadHook {
    fn drop(&mut self) {
        MID_READ_HOOKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _, _)| *id != self.0);
    }
}

/// Run `mutation` on every source file captured beneath `root`, once per
/// capture, after the first chunk has been read. `root` is canonicalized.
///
/// # Errors
/// Refuses a root that cannot be canonicalized.
pub fn set_mid_read_hook<F>(root: &Path, mutation: F) -> std::io::Result<MidReadHook>
where
    F: Fn(&Path) + Send + Sync + 'static,
{
    let root = std::fs::canonicalize(root)?;
    let id = NEXT_HOOK.fetch_add(1, Ordering::Relaxed);
    MID_READ_HOOKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((id, root, Arc::new(mutation)));
    Ok(MidReadHook(id))
}

/// Run every hook registered for a root containing `path`.
pub fn mid_read(path: &Path) {
    let matching: Vec<Mutation> = MID_READ_HOOKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(_, root, _)| path.starts_with(root))
        .map(|(_, _, mutation)| Arc::clone(mutation))
        .collect();
    for mutation in matching {
        mutation(path);
    }
}
