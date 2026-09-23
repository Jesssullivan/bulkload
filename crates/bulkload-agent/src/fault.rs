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
//! Power-loss coverage is the remaining W7 follow-up: a syscall-log
//! (ALICE-style) crash-state checker, or dm-log-writes replay.
//!
//! # Crash receipt
//!
//! With `BULKLOAD_FAULT_RECEIPT=<path>` set, the process writes the armed
//! point's name, and for publication points the canonical store root on a
//! second line, to `<path>` just before it exits. Harnesses use it to check
//! which store a crash landed in.
//!
//! An `BULKLOAD_FAULT` value that names no point, or has an `nth` of zero or a
//! non-number, ends the process at the first fault point it reaches with
//! [`FAULT_SPEC_INVALID_EXIT_CODE`], so a typo can never pass as a clean run.
//!
//! # Fault points
//!
//! Store publication, in `StorePublisher::publish_group`. The source capture
//! publisher and the destination chunk publisher share this code but not these
//! points: each stage exists once per store, `publish.source.<stage>` and
//! `publish.destination.<stage>`, so an `nth` hit always lands in a known
//! store. A crash receipt (below) also records that store's root.
//!
//! | Stage | Crash leaves (in the named store) |
//! |-------|-----------------------------------|
//! | `after_append` | chunk bytes appended to `chunks.pack`, not synced, not indexed |
//! | `after_pack_sync` | pack tail synced, not indexed |
//! | `after_location_insert` | open transaction with chunk locations (hot journal) |
//! | `after_manifest_insert` | open transaction with locations and captures (hot journal) |
//! | `before_commit` | the whole group staged, `COMMIT` not issued (hot journal) |
//! | `after_commit` | the group committed, no acknowledgement sent |
//!
//! Output publication, in `Destination::file`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `materialize.after_temp_write` | a complete, unsynced `.bulkload-*` temporary at mode 0600 |
//! | `materialize.after_temp_sync` | the temporary at its final mode, synced, not linked |
//! | `materialize.after_link` | the final name linked to the synced temporary; parent not synced |
//! | `materialize.after_parent_sync` | the link durable; the temporary name still present |
//!
//! Every temporary one of these leaves carries the destination store's tag and
//! is removed by the next invocation's sweep; see `materialize`.
//!
//! Directory publication, in `Destination::directory` and `finish_directories`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `directory.after_mkdir` | an empty 0700 `.bulkload-<tag>-d-*` temporary directory, no record |
//! | `directory.after_pending_record` | the temporary, its parent synced, and a record bound to its inode; not renamed |
//! | `directory.after_rename` | the 0700 directory under its final name, bound record; parent not synced |
//! | `directory.before_complete` | the final mode applied and synced; the pending record still present |
//!
//! Protocol boundaries, in `transfer`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `serve.after_publish_group` | a committed source group whose files were not yet offered |
//! | `serve.after_content` | one file offered and acknowledged `Applied` by the receiver |
//! | `serve.before_done` | every batch sent, `TransferDone` not sent |
//! | `receive.after_want_files` | a batch census answered, no content received for it |
//! | `receive.after_chunk_publish` | one file's received chunks committed, the file not materialized |
//! | `receive.before_record_output` | a verified output under its final name with no output record |
//! | `receive.after_record_output` | a verified output and its output record; `Applied` not sent |
//! | `receive.after_applied` | the output recorded and `Applied` sent |
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

/// A named crash point in the durability path. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Point {
    /// `publish.source.after_append`
    PublishSourceAfterAppend,
    /// `publish.source.after_pack_sync`
    PublishSourceAfterPackSync,
    /// `publish.source.after_location_insert`
    PublishSourceAfterLocationInsert,
    /// `publish.source.after_manifest_insert`
    PublishSourceAfterManifestInsert,
    /// `publish.source.before_commit`
    PublishSourceBeforeCommit,
    /// `publish.source.after_commit`
    PublishSourceAfterCommit,
    /// `publish.destination.after_append`
    PublishDestinationAfterAppend,
    /// `publish.destination.after_pack_sync`
    PublishDestinationAfterPackSync,
    /// `publish.destination.after_location_insert`
    PublishDestinationAfterLocationInsert,
    /// `publish.destination.after_manifest_insert`
    PublishDestinationAfterManifestInsert,
    /// `publish.destination.before_commit`
    PublishDestinationBeforeCommit,
    /// `publish.destination.after_commit`
    PublishDestinationAfterCommit,
    /// `materialize.after_temp_write`
    MaterializeAfterTempWrite,
    /// `materialize.after_temp_sync`
    MaterializeAfterTempSync,
    /// `materialize.after_link`
    MaterializeAfterLink,
    /// `materialize.after_parent_sync`
    MaterializeAfterParentSync,
    /// `directory.after_mkdir`
    DirectoryAfterMkdir,
    /// `directory.after_pending_record`
    DirectoryAfterPendingRecord,
    /// `directory.after_rename`
    DirectoryAfterRename,
    /// `directory.before_complete`
    DirectoryBeforeComplete,
    /// `serve.after_publish_group`
    ServeAfterPublishGroup,
    /// `serve.after_content`
    ServeAfterContent,
    /// `serve.before_done`
    ServeBeforeDone,
    /// `receive.after_want_files`
    ReceiveAfterWantFiles,
    /// `receive.after_chunk_publish`
    ReceiveAfterChunkPublish,
    /// `receive.before_record_output`
    ReceiveBeforeRecordOutput,
    /// `receive.after_record_output`
    ReceiveAfterRecordOutput,
    /// `receive.after_applied`
    ReceiveAfterApplied,
}

impl Point {
    /// Every fault point, in durability-path order.
    pub const ALL: [Self; 28] = [
        Self::PublishSourceAfterAppend,
        Self::PublishSourceAfterPackSync,
        Self::PublishSourceAfterLocationInsert,
        Self::PublishSourceAfterManifestInsert,
        Self::PublishSourceBeforeCommit,
        Self::PublishSourceAfterCommit,
        Self::PublishDestinationAfterAppend,
        Self::PublishDestinationAfterPackSync,
        Self::PublishDestinationAfterLocationInsert,
        Self::PublishDestinationAfterManifestInsert,
        Self::PublishDestinationBeforeCommit,
        Self::PublishDestinationAfterCommit,
        Self::MaterializeAfterTempWrite,
        Self::MaterializeAfterTempSync,
        Self::MaterializeAfterLink,
        Self::MaterializeAfterParentSync,
        Self::DirectoryAfterMkdir,
        Self::DirectoryAfterPendingRecord,
        Self::DirectoryAfterRename,
        Self::DirectoryBeforeComplete,
        Self::ServeAfterPublishGroup,
        Self::ServeAfterContent,
        Self::ServeBeforeDone,
        Self::ReceiveAfterWantFiles,
        Self::ReceiveAfterChunkPublish,
        Self::ReceiveBeforeRecordOutput,
        Self::ReceiveAfterRecordOutput,
        Self::ReceiveAfterApplied,
    ];

    /// The name `BULKLOAD_FAULT` uses for this point.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PublishSourceAfterAppend => "publish.source.after_append",
            Self::PublishSourceAfterPackSync => "publish.source.after_pack_sync",
            Self::PublishSourceAfterLocationInsert => "publish.source.after_location_insert",
            Self::PublishSourceAfterManifestInsert => "publish.source.after_manifest_insert",
            Self::PublishSourceBeforeCommit => "publish.source.before_commit",
            Self::PublishSourceAfterCommit => "publish.source.after_commit",
            Self::PublishDestinationAfterAppend => "publish.destination.after_append",
            Self::PublishDestinationAfterPackSync => "publish.destination.after_pack_sync",
            Self::PublishDestinationAfterLocationInsert => {
                "publish.destination.after_location_insert"
            }
            Self::PublishDestinationAfterManifestInsert => {
                "publish.destination.after_manifest_insert"
            }
            Self::PublishDestinationBeforeCommit => "publish.destination.before_commit",
            Self::PublishDestinationAfterCommit => "publish.destination.after_commit",
            Self::MaterializeAfterTempWrite => "materialize.after_temp_write",
            Self::MaterializeAfterTempSync => "materialize.after_temp_sync",
            Self::MaterializeAfterLink => "materialize.after_link",
            Self::MaterializeAfterParentSync => "materialize.after_parent_sync",
            Self::DirectoryAfterMkdir => "directory.after_mkdir",
            Self::DirectoryAfterPendingRecord => "directory.after_pending_record",
            Self::DirectoryAfterRename => "directory.after_rename",
            Self::DirectoryBeforeComplete => "directory.before_complete",
            Self::ServeAfterPublishGroup => "serve.after_publish_group",
            Self::ServeAfterContent => "serve.after_content",
            Self::ServeBeforeDone => "serve.before_done",
            Self::ReceiveAfterWantFiles => "receive.after_want_files",
            Self::ReceiveAfterChunkPublish => "receive.after_chunk_publish",
            Self::ReceiveBeforeRecordOutput => "receive.before_record_output",
            Self::ReceiveAfterRecordOutput => "receive.after_record_output",
            Self::ReceiveAfterApplied => "receive.after_applied",
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

fn receipt(point: Point, store: Option<&Path>) {
    if let Some(path) = std::env::var_os(FAULT_RECEIPT_ENV) {
        let mut body = format!("{}\n", point.name());
        if let Some(store) = store {
            body.push_str(&store.display().to_string());
            body.push('\n');
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
