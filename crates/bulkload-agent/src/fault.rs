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
//! This models a process crash, not a power cut: bytes the kernel already
//! accepted survive in the page cache whether or not they were synced.
//! Power-loss replay is a separate harness.
//!
//! An `BULKLOAD_FAULT` value that names no point, or has an `nth` of zero or a
//! non-number, ends the process at the first fault point it reaches with
//! [`FAULT_SPEC_INVALID_EXIT_CODE`], so a typo can never pass as a clean run.
//!
//! # Fault points
//!
//! Store publication, in `StorePublisher::publish_group`. The source capture
//! publisher and the destination chunk publisher share this code, so a hit
//! count spans both sides of a local `copy`.
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `publish.after_append` | chunk bytes appended to `chunks.pack`, not synced, not indexed |
//! | `publish.after_pack_sync` | pack tail synced, not indexed |
//! | `publish.after_location_insert` | open transaction with chunk locations (hot journal) |
//! | `publish.after_manifest_insert` | open transaction with locations and captures (hot journal) |
//! | `publish.before_commit` | the whole group staged, `COMMIT` not issued (hot journal) |
//! | `publish.after_commit` | the group committed, no acknowledgement sent |
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
//! Directory publication, in `Destination::directory` and `finish_directories`:
//!
//! | Name | Crash leaves |
//! |------|--------------|
//! | `directory.after_mkdir` | a new 0700 directory with no pending-directory record |
//! | `directory.after_pending_record` | the 0700 directory and its pending record |
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

/// A named crash point in the durability path. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Point {
    /// `publish.after_append`
    PublishAfterAppend,
    /// `publish.after_pack_sync`
    PublishAfterPackSync,
    /// `publish.after_location_insert`
    PublishAfterLocationInsert,
    /// `publish.after_manifest_insert`
    PublishAfterManifestInsert,
    /// `publish.before_commit`
    PublishBeforeCommit,
    /// `publish.after_commit`
    PublishAfterCommit,
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
    pub const ALL: [Self; 21] = [
        Self::PublishAfterAppend,
        Self::PublishAfterPackSync,
        Self::PublishAfterLocationInsert,
        Self::PublishAfterManifestInsert,
        Self::PublishBeforeCommit,
        Self::PublishAfterCommit,
        Self::MaterializeAfterTempWrite,
        Self::MaterializeAfterTempSync,
        Self::MaterializeAfterLink,
        Self::MaterializeAfterParentSync,
        Self::DirectoryAfterMkdir,
        Self::DirectoryAfterPendingRecord,
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
            Self::PublishAfterAppend => "publish.after_append",
            Self::PublishAfterPackSync => "publish.after_pack_sync",
            Self::PublishAfterLocationInsert => "publish.after_location_insert",
            Self::PublishAfterManifestInsert => "publish.after_manifest_insert",
            Self::PublishBeforeCommit => "publish.before_commit",
            Self::PublishAfterCommit => "publish.after_commit",
            Self::MaterializeAfterTempWrite => "materialize.after_temp_write",
            Self::MaterializeAfterTempSync => "materialize.after_temp_sync",
            Self::MaterializeAfterLink => "materialize.after_link",
            Self::MaterializeAfterParentSync => "materialize.after_parent_sync",
            Self::DirectoryAfterMkdir => "directory.after_mkdir",
            Self::DirectoryAfterPendingRecord => "directory.after_pending_record",
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
    match armed() {
        Armed::Invalid => {
            eprintln!("bulkload-agent: {FAULT_ENV} names no fault point");
            terminate(FAULT_SPEC_INVALID_EXIT_CODE);
        }
        Armed::Point(armed, nth) if *armed == point => {
            if HITS.fetch_add(1, Ordering::SeqCst).saturating_add(1) == *nth {
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
