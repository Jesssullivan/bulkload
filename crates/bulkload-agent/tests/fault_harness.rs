//! W7 crash-consistency harness (TIN-4546). Runs only with `fault-injection`.
//!
//! # Crash-resume
//!
//! Each scenario re-runs this test binary as a child that performs one real
//! `copy` with a fault point armed (`BULKLOAD_FAULT=<point>:<nth>`). The child
//! ends itself with `_exit` at that point; nothing here sends it a signal
//! (R-N11). The child is this hash-named test executable rather than the
//! `bulkload-agent` binary, so a concurrent featureless build into a shared
//! target directory cannot swap the process under test. The parent then inspects the
//! crash state and runs `copy` again, clean and in process, and asserts:
//!
//! - **I1** every recorded output's file has its recorded stat identity and
//!   hashes to the committed source manifest for that path;
//! - **I2** no destination leaf under a final name holds partial content, and
//!   `.bulkload-*` temporaries are the only extra names;
//! - **I4** (R-N79) after the resume no `.bulkload-*` temporary remains: the
//!   sweep removed exactly the ones the crash left and recorded none as
//!   ambiguous;
//! - **I3** the resume reads exactly the source bytes of files that had neither
//!   an output record nor a committed capture before the crash, so every
//!   committed file costs 0 source bytes;
//! - the resume converges with only the fixture's own refusal, and the final
//!   destination is byte-identical to the source.
//!
//! # What this harness cannot see
//!
//! `_exit` models a **process crash only**. The kernel page cache survives
//! it, so bytes the process wrote are still there after the crash whether or
//! not they were synced. This harness therefore **cannot detect a missing or
//! misplaced fsync**. It does detect ordering errors that show up without
//! power loss: a record committed before its data, a final name holding
//! partial content, or a resume that cannot adopt what a crash left.
//! Power-loss coverage is the remaining W7 follow-up: a power-loss replay
//! harness, either a syscall-log (ALICE-style) crash-state checker or
//! dm-log-writes replay.
//!
//! # Known violations
//!
//! Tests marked `#[ignore]` with a "known violation" reason assert invariants
//! the v3 engine breaks today (R-N86). They are listed, not fixed, and never
//! count as coverage:
//!
//! - `live_writer_*_leaves_no_source_index`: a refused capture leaves the
//!   victim's chunks and committed `chunk_locations` rows in the source store.
//!
//! Every `materialize.*` point, and `directory.after_mkdir` and
//! `directory.after_pending_record` (an empty directory temporary not yet
//! renamed into place), must leave at least one temporary at the crash, so I4
//! is exercised, not vacuous, at each of them.
//!
//! # Live writer
//!
//! A mid-read hook rewrites, truncates, rename-replaces or same-size rewrites
//! (with the mtime restored) a source file under an open capture. Each must end
//! as a typed refusal with no capture and no output record.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    // Paths in failure messages stay escaped and quoted.
    clippy::unnecessary_debug_formatting
)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bulkload_agent::fault::{
    parse, set_mid_read_hook, Point, FAULT_ENV, FAULT_EXIT_CODE, FAULT_RECEIPT_ENV,
};
use bulkload_agent::freshness::NullCache;
use bulkload_agent::transfer::{copy, TransferStats};
use bulkload_agent::transfer_store::Manifest;
use bulkload_agent::walk::{walk, WalkOptions};
use bulkload_proto::{BulkloadRefusal, RowSchema};

/// `transfer_store::PERSIST_BATCH` (crate-private): chunks per durable batch.
const PERSIST_BATCH: usize = 256;
/// One byte past `PERSIST_BATCH × CDC_MAX`, so the file spans several batches.
const LARGE_BYTES: usize = PERSIST_BATCH * bulkload_agent::hash::CDC_MAX_BYTES as usize + 1;
const SMALL_FILES: usize = 48;
const REFUSED: &str = "refused.db";
const REFUSED_PREFIX_BYTES: u64 = 16;
const TEMP_PREFIX: &[u8] = b".bulkload-";

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        for directory in ["source", "destination", "inspect"] {
            fs::create_dir(base.join(directory)).unwrap();
        }
        Self { base }
    }
    fn source(&self) -> PathBuf {
        self.base.join("source")
    }
    fn destination(&self) -> PathBuf {
        self.base.join("destination")
    }
    fn source_state(&self) -> PathBuf {
        self.base.join("source-state")
    }
    fn destination_state(&self) -> PathBuf {
        self.base.join("destination-state")
    }
    fn run(&self) -> TransferStats {
        copy(
            &self.source(),
            &self.destination(),
            &self.source_state(),
            &self.destination_state(),
        )
        .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Deterministic, incompressible bytes; distinct seeds give distinct content.
fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut bytes = Vec::with_capacity(length + 8);
    while bytes.len() < length {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(length);
    bytes
}

fn large_content() -> &'static [u8] {
    static LARGE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    LARGE.get_or_init(|| noise(0x5EED, LARGE_BYTES))
}

#[derive(Clone, Copy)]
struct Fixture {
    refused: bool,
}

/// The transfer.rs shapes: many small distinct files (half in a nested
/// directory), one file larger than `PERSIST_BATCH × CDC_MAX`, and optionally
/// one file the source refuses by its `SQLite` header.
fn populate(source: &Path, fixture: Fixture) {
    fs::create_dir(source.join("nested")).unwrap();
    for index in 0..SMALL_FILES {
        let parent = if index % 2 == 0 { "" } else { "nested/" };
        let length = 1_024 + (index * 7_919) % 90_000;
        fs::write(
            source.join(format!("{parent}small-{index:03}")),
            noise(index as u64 + 1, length),
        )
        .unwrap();
    }
    fs::write(source.join("large"), large_content()).unwrap();
    if fixture.refused {
        fs::write(source.join(REFUSED), b"SQLite format 3\0refused raw copy").unwrap();
    }
}

/// Every regular file and directory beneath `root`, by relative path.
fn tree(root: &Path) -> BTreeMap<Vec<u8>, fs::Metadata> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<Vec<u8>, fs::Metadata>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            if metadata.is_dir() {
                visit(root, &path, entries);
            }
            entries.insert(relative, metadata);
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn digest(path: &Path) -> [u8; 32] {
    *blake3::hash(&fs::read(path).unwrap()).as_bytes()
}

fn relative(path: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(path))
}

fn is_temporary(path: &[u8]) -> bool {
    path.rsplit(|byte| *byte == b'/')
        .next()
        .is_some_and(|leaf| leaf.starts_with(TEMP_PREFIX))
}

/// Committed rows of one store table, read from a copy of the database (and
/// any hot journal) so the crash state the resume sees is left untouched.
fn committed(state: &Path, scratch: &Path, table: &str) -> Vec<(RowSchema, Vec<u8>)> {
    let database = state.join("transfer.sqlite");
    if !database.exists() {
        return Vec::new();
    }
    let copy = scratch.join(format!(
        "{table}-{}.sqlite",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(&database, &copy).unwrap();
    let journal = state.join("transfer.sqlite-journal");
    let copied_journal = PathBuf::from(format!("{}-journal", copy.display()));
    if journal.exists() {
        fs::copy(&journal, &copied_journal).unwrap();
    }
    let rows = {
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        if exists {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table}"))
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .unwrap()
                .map(|row| {
                    let (key, value) = row.unwrap();
                    let (_authority, schema): (Vec<u8>, RowSchema) =
                        postcard::from_bytes(&key).unwrap();
                    (schema, value)
                })
                .collect();
            rows
        } else {
            Vec::new()
        }
    };
    let _ = fs::remove_file(&copy);
    let _ = fs::remove_file(&copied_journal);
    rows
}

/// Committed `chunk_locations` of one store, digest to size, read from a copy
/// like [`committed`].
fn chunk_index(state: &Path, scratch: &Path) -> BTreeMap<[u8; 32], u64> {
    let database = state.join("transfer.sqlite");
    let copy = scratch.join(format!(
        "chunk-locations-{}.sqlite",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(&database, &copy).unwrap();
    let journal = state.join("transfer.sqlite-journal");
    let copied_journal = PathBuf::from(format!("{}-journal", copy.display()));
    if journal.exists() {
        fs::copy(&journal, &copied_journal).unwrap();
    }
    let index = {
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let mut statement = connection
            .prepare("SELECT digest, size FROM chunk_locations")
            .unwrap();
        let index = statement
            .query_map([], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .map(|row| {
                let (digest, size) = row.unwrap();
                (
                    <[u8; 32]>::try_from(digest.as_slice()).unwrap(),
                    u64::try_from(size).unwrap(),
                )
            })
            .collect();
        index
    };
    let _ = fs::remove_file(&copy);
    let _ = fs::remove_file(&copied_journal);
    index
}

struct CrashState {
    outputs: BTreeMap<Vec<u8>, (u64, u64, u64, i128, i128)>,
    captures: BTreeMap<Vec<u8>, Manifest>,
}

fn crash_state(scratch: &Scratch) -> CrashState {
    let inspect = scratch.base.join("inspect");
    let outputs = committed(&scratch.destination_state(), &inspect, "outputs")
        .into_iter()
        .map(|(row, identity)| (row.rel_path, postcard::from_bytes(&identity).unwrap()))
        .collect();
    let captures = committed(&scratch.source_state(), &inspect, "captures")
        .into_iter()
        .map(|(row, manifest)| (row.rel_path, postcard::from_bytes(&manifest).unwrap()))
        .collect();
    CrashState { outputs, captures }
}

/// I1: each output record names a file with that identity and the committed
/// source content.
fn assert_i1(label: &str, scratch: &Scratch, state: &CrashState) {
    for (path, identity) in &state.outputs {
        let target = scratch.destination().join(relative(path));
        let metadata = fs::symlink_metadata(&target)
            .unwrap_or_else(|error| panic!("{label} I1: recorded output {target:?}: {error}"));
        assert!(metadata.is_file(), "{label} I1: {target:?} not a file");
        let observed = (
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        );
        assert_eq!(
            observed, *identity,
            "{label} I1: identity of {target:?} drifted from its record"
        );
        let manifest = state
            .captures
            .get(path)
            .unwrap_or_else(|| panic!("{label} I1: output {target:?} has no committed capture"));
        let content = digest(&target);
        assert_eq!(
            content, manifest.digest,
            "{label} I1: {target:?} does not hash to its manifest"
        );
        assert_eq!(
            content,
            digest(&scratch.source().join(relative(path))),
            "{label} I1: {target:?} differs from its source"
        );
    }
}

/// I2: every final name holds complete source content; only temporaries are
/// extra. Returns the number of temporaries.
fn assert_i2(label: &str, scratch: &Scratch) -> usize {
    let source = tree(&scratch.source());
    let mut temporaries = 0;
    for (path, metadata) in tree(&scratch.destination()) {
        if is_temporary(&path) {
            assert!(
                metadata.is_file() || metadata.is_dir(),
                "{label} I2: temporary is neither a file nor a directory"
            );
            temporaries += 1;
            continue;
        }
        assert_ne!(path, REFUSED.as_bytes(), "{label} I2: refused file carried");
        let expected = source
            .get(&path)
            .unwrap_or_else(|| panic!("{label} I2: unexpected leaf {:?}", relative(&path)));
        assert_eq!(
            metadata.is_dir(),
            expected.is_dir(),
            "{label} I2: kind of {:?}",
            relative(&path)
        );
        if metadata.is_file() {
            assert_eq!(
                metadata.len(),
                expected.len(),
                "{label} I2: partial leaf {:?}",
                relative(&path)
            );
            assert_eq!(
                digest(&scratch.destination().join(relative(&path))),
                digest(&scratch.source().join(relative(&path))),
                "{label} I2: leaf {:?} holds other content",
                relative(&path)
            );
        }
    }
    temporaries
}

fn source_files(scratch: &Scratch) -> BTreeMap<Vec<u8>, u64> {
    tree(&scratch.source())
        .into_iter()
        .filter(|(path, metadata)| metadata.is_file() && path != REFUSED.as_bytes())
        .map(|(path, metadata)| (path, metadata.len()))
        .collect()
}

/// Run one armed child `copy` to its fault point.
fn crash_child(scratch: &Scratch, point: Point, label: &str) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &scratch.base)
        .env(FAULT_ENV, label)
        .env(FAULT_RECEIPT_ENV, scratch.base.join("receipt"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A detached waiter reaps the child; this thread waits on it against a
    // deadline. On timeout the test fails and the child is left alone: it is
    // never signalled (R-N11).
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let child = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!(
                "{label}: crash child still running after {CHILD_DEADLINE:?} ({timeout}); \
                 left running, not signalled"
            )
        })
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{label}: the child must stop at its fault point (status {:?}, stderr {})",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
    assert_receipt(scratch, point);
}

/// The crash landed at `point`, and a publication point in the store it names.
fn assert_receipt(scratch: &Scratch, point: Point) {
    let receipt = fs::read_to_string(scratch.base.join("receipt")).unwrap();
    let mut lines = receipt.lines();
    assert_eq!(lines.next(), Some(point.name()), "receipt point");
    let store = if point.name().starts_with("publish.source.") {
        Some(scratch.source_state())
    } else if point.name().starts_with("publish.destination.") {
        Some(scratch.destination_state())
    } else {
        None
    };
    let expected = store.map(|store| fs::canonicalize(store).unwrap().display().to_string());
    assert_eq!(
        lines.next().map(str::to_owned),
        expected,
        "{}: crash receipt names the wrong store",
        point.name()
    );
}

/// After a resume: records and names cover every source file, and a
/// refusal-free carry finalized its directory mode.
fn assert_complete(
    label: &str,
    scratch: &Scratch,
    fixture: Fixture,
    files: &BTreeMap<Vec<u8>, u64>,
    after: &CrashState,
) {
    let destination = tree(&scratch.destination());
    for path in files.keys() {
        assert!(
            destination.contains_key(path),
            "{label}: {:?} missing after resume",
            relative(path)
        );
        assert!(
            after.outputs.contains_key(path),
            "{label}: no output record"
        );
    }
    if !fixture.refused {
        let mode = |root: PathBuf| fs::metadata(root.join("nested")).unwrap().mode() & 0o7777;
        assert_eq!(
            mode(scratch.destination()),
            mode(scratch.source()),
            "{label}: directory mode not finalized"
        );
    }
}

/// Points whose crash always leaves a tagged temporary: a file temporary in
/// output publication, a directory temporary before the rename.
fn leaves_temporary(point: Point) -> bool {
    point.name().starts_with("materialize.")
        || matches!(
            point,
            Point::DirectoryAfterMkdir | Point::DirectoryAfterPendingRecord
        )
}

fn crash_resume(point: Point, nth: u64, fixture: Fixture) {
    let label = format!("{}:{nth}", point.name());
    let scratch = Scratch::new(&point.name().replace('.', "-"));
    populate(&scratch.source(), fixture);
    crash_child(&scratch, point, &label);

    let before = crash_state(&scratch);
    assert_i1(&label, &scratch, &before);
    let crash_temporaries = assert_i2(&label, &scratch);

    let files = source_files(&scratch);
    let uncommitted: u64 = files
        .iter()
        .filter(|(path, _)| {
            !before.outputs.contains_key(*path) && !before.captures.contains_key(*path)
        })
        .map(|(_, size)| *size)
        .sum();
    let refused_read = if fixture.refused {
        REFUSED_PREFIX_BYTES
    } else {
        0
    };

    let resumed = scratch.run();

    // Convergence: the resume refuses exactly what a clean run refuses.
    let expected_refusals: Vec<Vec<u8>> = if fixture.refused {
        vec![REFUSED.as_bytes().to_vec()]
    } else {
        Vec::new()
    };
    let refusals: Vec<Vec<u8>> = resumed
        .refusals
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(
        refusals, expected_refusals,
        "{label}: resume refused {:?}",
        resumed.refusals
    );
    // I3: committed files cost 0 source bytes; the rest are read exactly once.
    assert_eq!(
        resumed.source_bytes_read,
        uncommitted + refused_read,
        "{label} I3: resume read {} source bytes; uncommitted files hold {uncommitted}",
        resumed.source_bytes_read
    );
    assert_eq!(
        resumed.reused,
        before.outputs.len() as u64,
        "{label} I3: every recorded output must be reused"
    );
    assert_eq!(
        resumed.completed + resumed.reused,
        files.len() as u64,
        "{label}: every carried file accounted for"
    );

    let after = crash_state(&scratch);
    assert_i1(&format!("{label} resumed"), &scratch, &after);
    let temporaries = assert_i2(&format!("{label} resumed"), &scratch);
    assert_complete(&label, &scratch, fixture, &files, &after);
    // I4: the sweep leaves no temporary behind and removes only the crash's.
    if leaves_temporary(point) {
        assert!(
            crash_temporaries >= 1,
            "{label}: this crash must leave a temporary for I4 to sweep"
        );
    }
    assert_eq!(
        temporaries, 0,
        "{label} I4: a temporary survived the resume"
    );
    assert_eq!(
        resumed.temporaries_removed, crash_temporaries as u64,
        "{label} I4: the sweep must remove exactly the crash's temporaries"
    );
    assert!(
        resumed.temporaries_left.is_empty(),
        "{label} I4: nothing here is ambiguous, yet {:?} was left",
        resumed.temporaries_left
    );
    println!(
        "{label}: outputs_before={} captures_before={} temporaries_at_crash={crash_temporaries} \
         resume_completed={} resume_reused={} resume_source_bytes={} resume_bytes_received={} \
         temporaries_left={temporaries}",
        before.outputs.len(),
        before.captures.len(),
        resumed.completed,
        resumed.reused,
        resumed.source_bytes_read,
        resumed.bytes_received,
    );
}

/// How long a crash child may run before the scenario fails.
const CHILD_DEADLINE: Duration = Duration::from_mins(5);

/// Set only in a child: the scratch base whose trees it copies.
const CHILD_ENV: &str = "BULKLOAD_W7_CHILD_BASE";

/// The child half of every scenario; a no-op in an ordinary test run.
#[test]
fn crash_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    // An armed fault point ends this process inside `copy`. Returning at all
    // means the point was never reached, which the parent reports.
    let outcome = copy(
        &base.join("source"),
        &base.join("destination"),
        &base.join("source-state"),
        &base.join("destination-state"),
    );
    eprintln!("fault point not reached; copy returned {outcome:?}");
}

const WITH_REFUSAL: Fixture = Fixture { refused: true };
const NO_REFUSAL: Fixture = Fixture { refused: false };

macro_rules! scenarios {
    ($($name:ident => $point:ident : $nth:expr, $fixture:expr;)*) => {$(
        #[test]
        fn $name() {
            crash_resume(Point::$point, $nth, $fixture);
        }
    )*};
}

scenarios! {
    publish_source_after_append_first => PublishSourceAfterAppend: 1, WITH_REFUSAL;
    publish_source_after_append_mid => PublishSourceAfterAppend: 8, WITH_REFUSAL;
    publish_source_after_pack_sync_first => PublishSourceAfterPackSync: 1, WITH_REFUSAL;
    publish_source_after_pack_sync_mid => PublishSourceAfterPackSync: 8, WITH_REFUSAL;
    publish_source_after_location_insert_first => PublishSourceAfterLocationInsert: 1, WITH_REFUSAL;
    publish_source_after_location_insert_mid => PublishSourceAfterLocationInsert: 8, WITH_REFUSAL;
    publish_source_after_manifest_insert_first => PublishSourceAfterManifestInsert: 1, WITH_REFUSAL;
    publish_source_after_manifest_insert_mid => PublishSourceAfterManifestInsert: 8, WITH_REFUSAL;
    publish_source_before_commit_first => PublishSourceBeforeCommit: 1, WITH_REFUSAL;
    publish_source_before_commit_mid => PublishSourceBeforeCommit: 8, WITH_REFUSAL;
    publish_source_after_commit_first => PublishSourceAfterCommit: 1, WITH_REFUSAL;
    publish_source_after_commit_mid => PublishSourceAfterCommit: 8, WITH_REFUSAL;
    publish_destination_after_append_first => PublishDestinationAfterAppend: 1, WITH_REFUSAL;
    publish_destination_after_append_mid => PublishDestinationAfterAppend: 30, WITH_REFUSAL;
    publish_destination_after_pack_sync_first => PublishDestinationAfterPackSync: 1, WITH_REFUSAL;
    publish_destination_after_pack_sync_mid => PublishDestinationAfterPackSync: 30, WITH_REFUSAL;
    publish_destination_after_location_insert_first => PublishDestinationAfterLocationInsert: 1, WITH_REFUSAL;
    publish_destination_after_location_insert_mid => PublishDestinationAfterLocationInsert: 20, WITH_REFUSAL;
    publish_destination_after_manifest_insert_first => PublishDestinationAfterManifestInsert: 1, WITH_REFUSAL;
    publish_destination_after_manifest_insert_mid => PublishDestinationAfterManifestInsert: 20, WITH_REFUSAL;
    publish_destination_before_commit_first => PublishDestinationBeforeCommit: 1, WITH_REFUSAL;
    publish_destination_before_commit_mid => PublishDestinationBeforeCommit: 20, WITH_REFUSAL;
    publish_destination_after_commit_first => PublishDestinationAfterCommit: 1, WITH_REFUSAL;
    publish_destination_after_commit_mid => PublishDestinationAfterCommit: 20, WITH_REFUSAL;
    materialize_after_temp_write_first => MaterializeAfterTempWrite: 1, WITH_REFUSAL;
    materialize_after_temp_write_mid => MaterializeAfterTempWrite: 25, WITH_REFUSAL;
    materialize_after_temp_sync_mid => MaterializeAfterTempSync: 25, WITH_REFUSAL;
    materialize_after_link_mid => MaterializeAfterLink: 25, WITH_REFUSAL;
    materialize_after_parent_sync_mid => MaterializeAfterParentSync: 25, WITH_REFUSAL;
    directory_after_mkdir => DirectoryAfterMkdir: 1, NO_REFUSAL;
    directory_after_pending_record => DirectoryAfterPendingRecord: 1, NO_REFUSAL;
    directory_after_rename => DirectoryAfterRename: 1, NO_REFUSAL;
    directory_before_complete => DirectoryBeforeComplete: 1, NO_REFUSAL;
    serve_after_publish_group_first => ServeAfterPublishGroup: 1, WITH_REFUSAL;
    serve_after_publish_group_mid => ServeAfterPublishGroup: 4, WITH_REFUSAL;
    serve_after_content_mid => ServeAfterContent: 25, WITH_REFUSAL;
    serve_before_done => ServeBeforeDone: 1, WITH_REFUSAL;
    receive_after_want_files_first => ReceiveAfterWantFiles: 1, WITH_REFUSAL;
    receive_after_want_files_second => ReceiveAfterWantFiles: 2, WITH_REFUSAL;
    receive_after_chunk_publish_mid => ReceiveAfterChunkPublish: 25, WITH_REFUSAL;
    receive_before_record_output_mid => ReceiveBeforeRecordOutput: 25, WITH_REFUSAL;
    receive_after_record_output_mid => ReceiveAfterRecordOutput: 25, WITH_REFUSAL;
    receive_after_applied_mid => ReceiveAfterApplied: 25, WITH_REFUSAL;
}

#[test]
fn fault_names_round_trip_and_are_unique() {
    for point in Point::ALL {
        assert_eq!(Point::from_name(point.name()), Some(point));
        assert_eq!(
            Point::ALL
                .iter()
                .filter(|other| other.name() == point.name())
                .count(),
            1
        );
    }
}

#[test]
fn fault_spec_parsing_rejects_typos_and_zero() {
    assert_eq!(
        parse("publish.source.after_commit"),
        Some((Point::PublishSourceAfterCommit, 1))
    );
    assert_eq!(
        parse("receive.after_applied:7"),
        Some((Point::ReceiveAfterApplied, 7))
    );
    assert_eq!(parse("receive.after_applied:0"), None);
    assert_eq!(parse("receive.after_applied:x"), None);
    assert_eq!(parse("receive.after_aplied"), None);
    assert_eq!(parse("publish.after_commit"), None);
    assert_eq!(parse(""), None);
}

/// Points whose only crash-resume scenario is an `#[ignore]`d known-violation
/// test. They are listed here and never counted as coverage.
const KNOWN_VIOLATIONS: [Point; 0] = [];

/// Every point is exercised by a passing (non-ignored) scenario in the
/// `scenarios!` table, or is listed in [`KNOWN_VIOLATIONS`] — never both.
#[test]
fn every_fault_point_has_a_scenario() {
    let source = include_str!("fault_harness.rs");
    let start = source.find("\nscenarios! {\n").unwrap();
    let table = &source[start..];
    let table = &table[..table.find("\n}\n").unwrap()];
    let covered = |point: &Point| table.contains(&format!("=> {point:?}:"));
    let known = |point: &Point| KNOWN_VIOLATIONS.contains(point);
    let missing: Vec<&str> = Point::ALL
        .into_iter()
        .filter(|point| !covered(point) && !known(point))
        .map(Point::name)
        .collect();
    assert!(
        missing.is_empty(),
        "fault points without a passing scenario: {missing:?}"
    );
    let both: Vec<&str> = Point::ALL
        .into_iter()
        .filter(|point| covered(point) && known(point))
        .map(Point::name)
        .collect();
    assert!(
        both.is_empty(),
        "known violations also claimed as coverage: {both:?}"
    );
    for point in KNOWN_VIOLATIONS {
        assert!(
            source.contains(&format!("crash_resume(Point::{point:?}, ")),
            "{}: known violation without its ignored test",
            point.name()
        );
    }
}

/// R-N79: after `materialize.after_link` the orphan is a second hard link to a
/// published output. A walk of the crashed destination records it and never
/// carries it, onward carry included; the resume unlinks the temporary name
/// alone, and the published name keeps its inode and content.
#[test]
fn materialize_after_link_sweeps_only_the_temporary_name() {
    let label = format!("{}:25", Point::MaterializeAfterLink.name());
    let scratch = Scratch::new("after-link-sweep");
    populate(&scratch.source(), WITH_REFUSAL);
    crash_child(&scratch, Point::MaterializeAfterLink, &label);

    let destination = scratch.destination();
    let crashed = tree(&destination);
    let orphans: Vec<(&Vec<u8>, &fs::Metadata)> = crashed
        .iter()
        .filter(|(path, _)| is_temporary(path))
        .collect();
    assert_eq!(orphans.len(), 1, "{label}: exactly one orphan");
    let (orphan, orphan_meta) = orphans[0];
    assert_eq!(
        orphan_meta.nlink(),
        2,
        "{label}: the orphan is a second link"
    );
    let published: Vec<&Vec<u8>> = crashed
        .iter()
        .filter(|(path, metadata)| {
            !is_temporary(path) && metadata.is_file() && metadata.ino() == orphan_meta.ino()
        })
        .map(|(path, _)| path)
        .collect();
    assert_eq!(published.len(), 1, "{label}: one published name shares it");
    let published = published[0].clone();

    // No walk carries the orphan: the census records it instead of a row.
    let census = walk(
        &WalkOptions::new(fs::canonicalize(&destination).unwrap()),
        &mut NullCache,
    )
    .unwrap();
    assert!(
        census.rows.iter().all(|row| !is_temporary(&row.rel_path)),
        "{label}: the walk offered a temporary as a row"
    );
    assert_eq!(census.engine_temporaries, vec![orphan.clone()]);
    let onward = scratch.base.join("onward");
    fs::create_dir(&onward).unwrap();
    let carried = copy(
        &destination,
        &onward,
        &scratch.base.join("onward-source-state"),
        &scratch.base.join("onward-destination-state"),
    )
    .unwrap();
    assert!(carried.refusals.is_empty(), "{:?}", carried.refusals);
    assert!(
        tree(&onward).keys().all(|path| !is_temporary(path)),
        "{label}: an onward carry planted the orphan"
    );

    let resumed = scratch.run();
    assert_eq!(resumed.temporaries_removed, 1);
    assert!(resumed.temporaries_left.is_empty());
    assert!(fs::symlink_metadata(destination.join(relative(orphan))).is_err());
    let kept = fs::symlink_metadata(destination.join(relative(&published))).unwrap();
    assert_eq!(
        kept.ino(),
        orphan_meta.ino(),
        "{label}: published inode replaced"
    );
    assert_eq!(kept.nlink(), 1, "{label}: the orphan link was not removed");
    assert_eq!(
        digest(&destination.join(relative(&published))),
        digest(&scratch.source().join(relative(&published))),
    );
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    InPlaceOverwrite,
    Truncate,
    RenameReplace,
    SameSizeMtimeRestored,
}

const VICTIM: &str = "victim";
const VICTIM_BYTES: usize = 4 * 1024 * 1024;

fn mutate(path: &Path, mutation: Mutation) {
    use std::io::{Seek as _, SeekFrom, Write as _};
    match mutation {
        Mutation::InPlaceOverwrite => {
            let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.seek(SeekFrom::Start(VICTIM_BYTES as u64 / 2)).unwrap();
            file.write_all(&vec![0xA5; 65_536]).unwrap();
            file.sync_all().unwrap();
        }
        Mutation::Truncate => {
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_len(VICTIM_BYTES as u64 / 8).unwrap();
            file.sync_all().unwrap();
        }
        Mutation::RenameReplace => {
            let replacement = path.with_file_name(format!("{VICTIM}.replacement"));
            fs::write(&replacement, noise(0xBAD, VICTIM_BYTES)).unwrap();
            fs::rename(&replacement, path).unwrap();
        }
        Mutation::SameSizeMtimeRestored => {
            let before = fs::metadata(path).unwrap();
            let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.seek(SeekFrom::Start(VICTIM_BYTES as u64 / 2)).unwrap();
            file.write_all(&vec![0x5A; 65_536]).unwrap();
            file.sync_all().unwrap();
            let times = [
                libc::timespec {
                    tv_sec: before.atime(),
                    tv_nsec: before.atime_nsec(),
                },
                libc::timespec {
                    tv_sec: before.mtime(),
                    tv_nsec: before.mtime_nsec(),
                },
            ];
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `name` is NUL-terminated and `times` holds the two
            // timespecs utimensat reads; both outlive the call.
            let result =
                unsafe { libc::utimensat(libc::AT_FDCWD, name.as_ptr(), times.as_ptr(), 0) };
            assert_eq!(result, 0, "utimensat: {}", std::io::Error::last_os_error());
            let after = fs::metadata(path).unwrap();
            assert_eq!(
                (after.mtime(), after.mtime_nsec(), after.len()),
                (before.mtime(), before.mtime_nsec(), before.len()),
                "the mutation must be invisible to size and mtime"
            );
        }
    }
}

fn ctime_ns(path: &Path) -> i128 {
    let metadata = fs::metadata(path).unwrap();
    i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec())
}

fn wait_for_later_ctime(reference: &Path, tick: &Path) {
    let reference = ctime_ns(reference);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        fs::write(tick, b"tick").unwrap();
        if ctime_ns(tick) > reference {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the filesystem clock never passed the victim's ctime"
        );
    }
}

/// Run one mutation; assert the typed refusal and a clean destination. The
/// scratch is returned for the known-violation source-index checks.
fn live_writer(mutation: Mutation) -> Scratch {
    let label = format!("{mutation:?}");
    let scratch = Scratch::new("live-writer");
    let source = scratch.source();
    fs::write(source.join(VICTIM), noise(0x71C, VICTIM_BYTES)).unwrap();
    for index in 0..3_u64 {
        fs::write(
            source.join(format!("bystander-{index}")),
            noise(index + 100, 300_000),
        )
        .unwrap();
    }
    // A coarse-grained filesystem clock could give the mutation the victim's
    // own ctime. Busy-poll a scratch file (no sleep) until its ctime is past
    // the victim's, so any later mutation carries a later ctime.
    wait_for_later_ctime(&source.join(VICTIM), &scratch.base.join("tick"));
    let fired = Arc::new(AtomicBool::new(false));
    let hook = {
        let fired = Arc::clone(&fired);
        set_mid_read_hook(&source, move |path| {
            if path.file_name() == Some(std::ffi::OsStr::new(VICTIM))
                && !fired.swap(true, Ordering::SeqCst)
            {
                mutate(path, mutation);
            }
        })
        .unwrap()
    };
    let stats = scratch.run();
    drop(hook);
    assert!(fired.load(Ordering::SeqCst), "{label}: hook never ran");

    let code = BulkloadRefusal::SourceChangedAfterSnapshot.code();
    assert_eq!(
        stats.refusals,
        vec![(VICTIM.as_bytes().to_vec(), code.to_owned())],
        "{label}: expected exactly one typed refusal for the victim"
    );
    assert_eq!(stats.completed, 3, "{label}: bystanders still carried");
    assert!(
        !scratch.destination().join(VICTIM).exists(),
        "{label}: the victim reached its final name"
    );
    let recorded = crash_state(&scratch);
    assert!(
        !recorded.outputs.contains_key(VICTIM.as_bytes()),
        "{label}: output recorded"
    );
    assert!(
        !recorded.captures.contains_key(VICTIM.as_bytes()),
        "{label}: capture committed"
    );
    assert_eq!(assert_i2(&label, &scratch), 0);
    let bystanders = bystander_chunks(&label, &recorded);
    assert_index_is(
        &format!("{label} destination"),
        &scratch,
        &scratch.destination_state(),
        &bystanders,
    );
    scratch
}

/// Distinct chunks of every committed capture, which must all be bystanders.
fn bystander_chunks(label: &str, recorded: &CrashState) -> BTreeMap<[u8; 32], u64> {
    let mut chunks = BTreeMap::new();
    for (path, manifest) in &recorded.captures {
        assert_ne!(path, VICTIM.as_bytes(), "{label}: victim captured");
        for chunk in &manifest.chunks {
            chunks.insert(chunk.digest, chunk.size);
        }
    }
    chunks
}

/// A store's committed chunk rows and its pack cover exactly `expected`.
fn assert_index_is(
    label: &str,
    scratch: &Scratch,
    state: &Path,
    expected: &BTreeMap<[u8; 32], u64>,
) {
    let index = chunk_index(state, &scratch.base.join("inspect"));
    let pack = fs::metadata(state.join("chunks.pack")).unwrap().len();
    assert!(
        index == *expected,
        "{label}: chunk_locations hold {} rows ({} bytes); the bystanders need {} rows",
        index.len(),
        index.values().sum::<u64>(),
        expected.len()
    );
    assert_eq!(
        pack,
        expected.values().sum::<u64>(),
        "{label}: chunks.pack holds {pack} bytes; the bystanders need {}",
        expected.values().sum::<u64>()
    );
}

/// The source store keeps no index row or pack byte for a refused capture.
fn assert_source_index_only_bystanders(mutation: Mutation) {
    let label = format!("{mutation:?}");
    let scratch = live_writer(mutation);
    let recorded = crash_state(&scratch);
    let bystanders = bystander_chunks(&label, &recorded);
    assert_index_is(
        &format!("{label} source"),
        &scratch,
        &scratch.source_state(),
        &bystanders,
    );
}

#[test]
fn live_writer_in_place_overwrite_refuses() {
    live_writer(Mutation::InPlaceOverwrite);
}

#[test]
fn live_writer_truncate_refuses() {
    live_writer(Mutation::Truncate);
}

#[test]
fn live_writer_rename_replace_refuses() {
    live_writer(Mutation::RenameReplace);
}

#[test]
fn live_writer_same_size_mtime_restored_refuses() {
    live_writer(Mutation::SameSizeMtimeRestored);
}

// KNOWN VIOLATION (R-N86), recorded rather than fixed. `capture_uncached`
// sends each batch of chunks to the publisher as it reads them and only
// compares the stat identity after the last batch. By the time the refusal
// is known, the victim's chunks are appended to the source `chunks.pack` and
// their `chunk_locations` rows are committed. No capture or output record
// points at them, so I1 holds, but the source store keeps index rows and pack
// bytes for content that was never a consistent snapshot.
#[test]
#[ignore = "known violation (R-N86): refused capture leaves victim chunks indexed in the source pack; W4 removes the source pack (R-N58)"]
fn live_writer_in_place_overwrite_leaves_no_source_index() {
    assert_source_index_only_bystanders(Mutation::InPlaceOverwrite);
}

#[test]
#[ignore = "known violation (R-N86): refused capture leaves victim chunks indexed in the source pack; W4 removes the source pack (R-N58)"]
fn live_writer_truncate_leaves_no_source_index() {
    assert_source_index_only_bystanders(Mutation::Truncate);
}

#[test]
#[ignore = "known violation (R-N86): refused capture leaves victim chunks indexed in the source pack; W4 removes the source pack (R-N58)"]
fn live_writer_rename_replace_leaves_no_source_index() {
    assert_source_index_only_bystanders(Mutation::RenameReplace);
}

#[test]
#[ignore = "known violation (R-N86): refused capture leaves victim chunks indexed in the source pack; W4 removes the source pack (R-N58)"]
fn live_writer_same_size_mtime_restored_leaves_no_source_index() {
    assert_source_index_only_bystanders(Mutation::SameSizeMtimeRestored);
}
