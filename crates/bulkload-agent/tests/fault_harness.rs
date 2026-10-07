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
//!   the source's content, and hashes to the committed source manifest for
//!   that path when one exists (the source sends before its capture commits,
//!   so an output may be recorded before its capture);
//! - **I2** no destination leaf under a final name holds partial content, and
//!   `.bulkload-*` temporaries are the only extra names;
//! - **I4** (R-N79) after the resume no `.bulkload-*` temporary remains: the
//!   sweep removed exactly the ones the crash left and recorded none as
//!   ambiguous;
//! - **I3** the resume reads exactly the source bytes of files that had neither
//!   an output record nor a committed capture before the crash, so every
//!   committed file costs 0 source bytes (R25, strict per OI-1001-Q15). The
//!   ledger is digest-only (R-N58), so a capture commits only after the
//!   destination reports it holds the bytes durably (`Held`): a sealed
//!   temporary the resume salvages, or a final name it adopts, against the
//!   ledger's manifest;
//! - the resume converges with only the fixture's own refusal, and the final
//!   destination is byte-identical to the source.
//!
//! # Superseding reruns (WP0(d), OI-1003-Q18, #187)
//!
//! A `SUPERSEDING` scenario first copies the fixture cleanly, changes a
//! third of its small files on the source (grown, rewritten at their size,
//! shrunk), and arms the fault point in the rerun that supersedes their
//! outputs. Its crash state must hold, for every path, the old output with
//! its old row or the new output with its new row:
//!
//! - every final name holds the whole old content or the whole new one;
//! - every output row names a file with exactly its recorded identity, and
//!   a row recorded from the seat as it is now names the new bytes, a row
//!   recorded from the seat as it was names the old ones;
//! - `.bulkload-*` temporaries are the only extra names.
//!
//! The resume then converges with no refusal and no temporary left, reads
//! at most the changed files (WP0(c), inequality 1), and a further run reads
//! and receives nothing. `supersede.after_exchange` must leave a displaced
//! old output under a temporary name, which the resume's sweep removes.
//!
//! # What this harness cannot see
//!
//! `_exit` models a **process crash only**. The kernel page cache survives
//! it, so bytes the process wrote are still there after the crash whether or
//! not they were synced. This harness therefore **cannot detect a missing or
//! misplaced fsync**. It does detect ordering errors that show up without
//! power loss: a record committed before its data, a final name holding
//! partial content, or a resume that cannot adopt what a crash left.
//! Power-loss coverage is the remaining W7 follow-up (R-N88, tracked on #49):
//! a power-loss replay harness, either a syscall-log (ALICE-style)
//! crash-state checker or dm-log-writes replay.
//!
//! # Known violations
//!
//! Tests marked `#[ignore = "known violation ..."]` assert invariants the
//! engine breaks today. They are listed in `KNOWN_VIOLATIONS`, not fixed,
//! and never count as coverage. `every_fault_point_has_a_scenario` compares
//! the list with the tests this binary reports under `--list --ignored`, and
//! checks each listed test's ignore reason.
//!
//! The list is empty. The four R-N86 tests it held (a refused capture left
//! the victim's bytes in the source pack) pass since W4 PR 2 removed the
//! pack (R-N58), as `live_writer_*_leaves_no_source_ledger_row`.
//!
//! # Hung scenarios
//!
//! A crash child runs a watchdog: if its fault point is not reached within
//! `CHILD_WATCHDOG` it ends itself with `_exit(CHILD_WATCHDOG_EXIT_CODE)` and
//! the parent fails that scenario by name. The child only ever ends itself;
//! nothing here sends it a signal (R-N11). The parent's own deadline sits just
//! above the watchdog, so a hung scenario fails well inside the CI job limit.
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
    parse, parse_group_limits, set_mid_read_hook, Point, FAULT_ENV, FAULT_EXIT_CODE,
    FAULT_RECEIPT_ENV, GROUP_DESTINATION_ENV, GROUP_FILES_ENV, GROUP_SOURCE_ENV,
};
use bulkload_agent::freshness::NullCache;
use bulkload_agent::materialize::RENAME_UNSUPPORTED_ENV;
use bulkload_agent::transfer::{copy, TransferStats};
use bulkload_agent::transfer_store::Manifest;
use bulkload_agent::walk::{walk, WalkOptions};
use bulkload_proto::{BulkloadRefusal, RowSchema};

/// Maximal chunks in the large fixture file (the v4 pack's batch size).
const LARGE_CHUNKS: usize = 256;
/// `io::durable::GROUP_FILES`: the most outputs one destination group holds.
const GROUP_FILES: usize = 64;
/// One byte past `LARGE_CHUNKS × CDC_MAX`, so the file spans many credit
/// returns (64 MiB against a 16 MiB window).
const LARGE_BYTES: usize = LARGE_CHUNKS * bulkload_agent::hash::CDC_MAX_BYTES as usize + 1;
const SMALL_FILES: usize = 48;
/// More than the fixture's entries: every directory and file of [`populate`].
const ENTRIES: usize = SMALL_FILES + 8;
const REFUSED: &str = "refused.db";
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
    /// The fault is armed in a rerun over changed seats, after a clean first
    /// copy (see "Superseding reruns").
    superseding: bool,
}

/// The transfer.rs shapes: many small distinct files (half in a nested
/// directory), one file larger than `LARGE_CHUNKS × CDC_MAX`, and optionally
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
    // Fresh seats are racy for one timestamp tick and their captures are
    // never recorded (#86); settle them so the source ledger's publication
    // points are reached and I3 counts committed captures.
    bulkload_agent::transfer::settle_racy_window(source).unwrap();
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

/// Whether `bytes` are exactly the chunks `manifest` names, in order, and
/// the manifest's root is the root of those chunks.
fn matches_manifest(bytes: &[u8], manifest: &Manifest) -> bool {
    let mut rest = bytes;
    for chunk in &manifest.chunks {
        let size = usize::try_from(chunk.size).unwrap();
        if rest.len() < size || *blake3::hash(&rest[..size]).as_bytes() != chunk.digest {
            return false;
        }
        rest = &rest[size..];
    }
    rest.is_empty() && manifest.is_consistent()
}

fn relative(path: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(path))
}

fn is_temporary(path: &[u8]) -> bool {
    path.rsplit(|byte| *byte == b'/')
        .next()
        .is_some_and(|leaf| leaf.starts_with(TEMP_PREFIX))
}

/// Committed rows of one store table, read from a copy of the database and
/// its WAL, so the crash state the resume sees is left untouched.
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
    let wal = state.join("transfer.sqlite-wal");
    let copied_wal = PathBuf::from(format!("{}-wal", copy.display()));
    if wal.exists() {
        fs::copy(&wal, &copied_wal).unwrap();
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
    for leftover in [
        copy.clone(),
        copied_wal,
        PathBuf::from(format!("{}-shm", copy.display())),
    ] {
        let _ = fs::remove_file(leftover);
    }
    rows
}

/// Committed `chunk_locations` of one store, digest to size, read from a copy
/// like [`committed`]. A v5 store has no such table: empty.
fn chunk_index(state: &Path, scratch: &Path) -> BTreeMap<[u8; 32], u64> {
    let database = state.join("transfer.sqlite");
    if !database.exists() {
        return BTreeMap::new();
    }
    let copy = scratch.join(format!(
        "chunk-locations-{}.sqlite",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(&database, &copy).unwrap();
    let wal = state.join("transfer.sqlite-wal");
    let copied_wal = PathBuf::from(format!("{}-wal", copy.display()));
    if wal.exists() {
        fs::copy(&wal, &copied_wal).unwrap();
    }
    let index = {
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='chunk_locations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if !exists {
            return BTreeMap::new();
        }
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
    for leftover in [
        copy.clone(),
        copied_wal,
        PathBuf::from(format!("{}-shm", copy.display())),
    ] {
        let _ = fs::remove_file(leftover);
    }
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

/// I1: each output record names a file with that identity and the source's
/// content, and matches the committed capture where one exists. The source
/// sends a file before its capture commits (M2 W3), so a crash can leave an
/// output record whose capture never committed; the resume then reuses the
/// output without reading the source.
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
        if let Some(manifest) = state.captures.get(path) {
            assert!(
                matches_manifest(&fs::read(&target).unwrap(), manifest),
                "{label} I1: {target:?} does not match its manifest"
            );
        }
        assert_eq!(
            digest(&target),
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

/// Run one armed child `copy` to its fault point. Returns the crashing
/// publication group's composition for a publication point. A `mid` child
/// closes every group at one file, so `nth` hits of the group points land at
/// an exact place; a first-hit child keeps the default grouping.
fn crash_child(
    scratch: &Scratch,
    point: Point,
    label: &str,
    mid: bool,
    envs: &[(&str, &str)],
) -> Option<String> {
    let child = run_crash_child(scratch, point, label, mid, envs);
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{label}: the child must stop at its fault point (status {:?}, stderr {})",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
    assert_receipt(scratch, point)
}

/// What a crash child's own last line says when its copy returned.
const NOT_REACHED: &str = "fault point not reached; copy returned";

/// What a crash child of the sweep did.
enum Swept {
    /// It stopped at its fault point; the receipt's group, if it names one.
    Crashed(Option<String>),
    /// Its copy returned first: the point has fewer hits than `nth`.
    OutOfHits,
}

/// [`crash_child`] for the crash sweep, where running out of hits is the
/// end of a point's sweep and not a failure.
fn crash_child_if_reached(scratch: &Scratch, point: Point, label: &str, mid: bool) -> Swept {
    let child = run_crash_child(scratch, point, label, mid, &[]);
    if child.status.code() == Some(0)
        && String::from_utf8_lossy(&child.stderr).contains(NOT_REACHED)
    {
        return Swept::OutOfHits;
    }
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{label}: the child must stop at its fault point or run out of hits (status {:?}, stderr {})",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
    Swept::Crashed(assert_receipt(scratch, point))
}

/// Run one armed crash child to its end and return its output. A hung child
/// fails here, by its watchdog's status or by the parent's deadline.
fn run_crash_child(
    scratch: &Scratch,
    point: Point,
    label: &str,
    mid: bool,
    envs: &[(&str, &str)],
) -> std::process::Output {
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
        .stdin(Stdio::null())
        .env(FAULT_ENV, label)
        .env(FAULT_RECEIPT_ENV, scratch.base.join("receipt"))
        .env(GROUP_FILES_ENV, if mid { "1" } else { "" })
        .envs(envs.iter().copied())
        .envs(
            (point == Point::DirectoryAfterFallbackMkdir).then_some((RENAME_UNSUPPORTED_ENV, "1")),
        )
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
    assert_ne!(
        child.status.code(),
        Some(CHILD_WATCHDOG_EXIT_CODE),
        "{label}: hung; the crash child's watchdog ended it after {CHILD_WATCHDOG:?} \
         (stderr {})",
        String::from_utf8_lossy(&child.stderr)
    );
    child
}

/// The crash landed at `point`, and a publication point in the store it names.
fn assert_receipt(scratch: &Scratch, point: Point) -> Option<String> {
    let receipt = fs::read_to_string(scratch.base.join("receipt")).unwrap();
    let mut lines = receipt.lines();
    assert_eq!(lines.next(), Some(point.name()), "receipt point");
    let source = point.name().starts_with("publish.source.");
    let store = if source {
        Some(scratch.source_state())
    } else if point.name().starts_with("publish.destination.") {
        Some(scratch.destination_state())
    } else {
        None
    };
    let expected = store.map(|store| fs::canonicalize(store).unwrap().display().to_string());
    let mut group = None;
    if let Some(expected) = expected {
        assert_eq!(
            lines.next(),
            Some(expected.as_str()),
            "{}: crash receipt names the wrong store",
            point.name()
        );
        // The group it hit, so a `_mid` failure can be reproduced.
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("{}: publication receipt has no group line", point.name()));
        assert_group(point, source, line);
        group = Some(line.to_owned());
    } else if let Some(path) = directory_path(point) {
        assert_eq!(
            lines.next(),
            Some(format!("directory_create={path}").as_str()),
            "{}: receipt names the wrong directory path",
            point.name()
        );
    }
    assert_eq!(lines.next(), None, "{}: extra receipt lines", point.name());
    group
}

/// The directory-creation path a crash at `point` must report, if any.
const fn directory_path(point: Point) -> Option<&'static str> {
    match point {
        Point::DirectoryAfterMkdir
        | Point::DirectoryAfterPendingRecord
        | Point::DirectoryAfterRename => Some("rename"),
        Point::DirectoryAfterFallbackMkdir => Some("fallback"),
        _ => None,
    }
}

/// `group capture_ids=<ids> chunks=<n>` for one committer group.
///
/// A source ledger group lists the distinct, ascending entry numbers of its
/// captures, and no chunks: the ledger is digest-only (R-N58). A destination
/// group lists one index per output, `0..n` with `1 <= n <= GROUP_FILES`, and
/// the distinct chunks those outputs hold.
fn assert_group(point: Point, source: bool, line: &str) {
    let label = point.name();
    let rest = line
        .strip_prefix("group capture_ids=")
        .unwrap_or_else(|| panic!("{label}: group line {line:?}"));
    let (ids, chunks) = rest
        .split_once(" chunks=")
        .unwrap_or_else(|| panic!("{label}: group line {line:?}"));
    let ids: Vec<usize> = ids
        .split(',')
        .filter(|id| !id.is_empty())
        .map(|id| {
            id.parse()
                .unwrap_or_else(|_| panic!("{label}: id in {line:?}"))
        })
        .collect();
    let chunks: usize = chunks
        .parse()
        .unwrap_or_else(|_| panic!("{label}: chunk count in {line:?}"));
    assert!(
        ids.windows(2).all(|pair| pair[0] < pair[1]),
        "{label}: capture ids not distinct and ascending: {line:?}"
    );
    if source {
        assert!(
            !ids.is_empty() && ids.iter().all(|id| *id < ENTRIES),
            "{label}: source group ids are not entries of the fixture: {line:?}"
        );
        assert_eq!(chunks, 0, "{label}: the ledger holds no chunk bytes");
    } else {
        assert!(
            !ids.is_empty() && ids.len() <= GROUP_FILES,
            "{label}: destination group size: {line:?}"
        );
        assert_eq!(
            ids,
            (0..ids.len()).collect::<Vec<_>>(),
            "{label}: destination group ids: {line:?}"
        );
    }
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

/// Points whose crash always leaves a tagged temporary: a staged file before
/// its rename, a directory temporary before its rename.
const fn leaves_temporary(point: Point) -> bool {
    matches!(
        point,
        Point::MaterializeAfterTempWrite
            | Point::MaterializeAfterTempSeal
            | Point::DirectoryAfterMkdir
            | Point::DirectoryAfterPendingRecord
    )
}

/// #169 (R25 strict): the outputs a crash left published at their final
/// path with the source's bytes and no row (their group's commit never ran).
/// Each is adopted from its capture record, reading 0 source bytes. The
/// fixture's captures are non-racy (`populate`), so each has a record.
fn adoptable<'a>(
    scratch: &Scratch,
    files: &'a BTreeMap<Vec<u8>, u64>,
    before: &CrashState,
) -> Vec<&'a Vec<u8>> {
    files
        .keys()
        .filter(|path| {
            !before.outputs.contains_key(*path)
                && !before.captures.contains_key(*path)
                && fs::read(scratch.destination().join(relative(path))).ok()
                    == fs::read(scratch.source().join(relative(path))).ok()
        })
        .collect()
}

fn crash_resume(point: Point, nth: u64, fixture: Fixture) {
    if fixture.superseding {
        crash_resume_superseding(point, nth);
    } else {
        crash_resume_with(point, nth, fixture, &[], |_| {});
    }
}

/// The small files a superseding scenario changes after its first copy, by
/// index: a third of them, in both directories.
const fn changes_in_place(index: usize) -> bool {
    index % 4 == 1 || index % 8 == 2
}

/// Change a third of the small files on the source: grown, rewritten at
/// their size, or shrunk. Returns each changed path's old content digest.
fn change_seats(source: &Path) -> BTreeMap<Vec<u8>, [u8; 32]> {
    let mut old = BTreeMap::new();
    for index in (0..SMALL_FILES).filter(|index| changes_in_place(*index)) {
        let parent = if index % 2 == 0 { "" } else { "nested/" };
        let rel = format!("{parent}small-{index:03}");
        let path = source.join(&rel);
        let mut content = fs::read(&path).unwrap();
        old.insert(rel.into_bytes(), *blake3::hash(&content).as_bytes());
        match index % 3 {
            0 => content.extend(noise(index as u64 + 1_000, 5_000)),
            1 => content = noise(index as u64 + 2_000, content.len()),
            _ => content.truncate(content.len() / 2),
        }
        fs::write(&path, content).unwrap();
    }
    bulkload_agent::transfer::settle_racy_window(source).unwrap();
    old
}

/// "The old output with its old row or the new output with its new row":
/// every final name holds the whole old or the whole new content, and every
/// output row names a file with its recorded identity whose bytes are the
/// ones the row was recorded from. Returns the number of temporaries.
fn assert_old_or_new(label: &str, scratch: &Scratch, old: &BTreeMap<Vec<u8>, [u8; 32]>) -> usize {
    let source = tree(&scratch.source());
    let mut temporaries = 0;
    for (path, metadata) in tree(&scratch.destination()) {
        if is_temporary(&path) {
            assert!(metadata.is_file(), "{label}: a temporary that is no file");
            temporaries += 1;
            continue;
        }
        let expected = source
            .get(&path)
            .unwrap_or_else(|| panic!("{label}: unexpected leaf {:?}", relative(&path)));
        if !metadata.is_file() {
            assert_eq!(metadata.is_dir(), expected.is_dir(), "{label}: kind");
            continue;
        }
        let held = digest(&scratch.destination().join(relative(&path)));
        assert!(
            held == digest(&scratch.source().join(relative(&path)))
                || old.get(&path) == Some(&held),
            "{label}: {:?} holds neither its old output nor its new one",
            relative(&path)
        );
    }
    let inspect = scratch.base.join("inspect");
    for (row, identity) in committed(&scratch.destination_state(), &inspect, "outputs") {
        let rel = relative(&row.rel_path);
        let target = scratch.destination().join(rel);
        let metadata = fs::symlink_metadata(&target)
            .unwrap_or_else(|error| panic!("{label}: a row names {target:?}: {error}"));
        let recorded: (u64, u64, u64, i128, i128) = postcard::from_bytes(&identity).unwrap();
        let observed = (
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        );
        assert_eq!(
            observed, recorded,
            "{label}: a row sits beside a file it does not describe: {target:?}"
        );
        // The row was recorded from the seat as it is now, or as it was.
        let seat = fs::metadata(scratch.source().join(rel)).unwrap();
        let from_now = row.size == seat.size()
            && row.mtime_ns
                == i128::from(seat.mtime()) * 1_000_000_000 + i128::from(seat.mtime_nsec());
        let held = digest(&target);
        if from_now {
            assert_eq!(
                held,
                digest(&scratch.source().join(rel)),
                "{label}: the new row of {target:?} sits beside other bytes"
            );
        } else {
            assert_eq!(
                Some(&held),
                old.get(&row.rel_path),
                "{label}: the old row of {target:?} sits beside other bytes"
            );
        }
    }
    temporaries
}

/// One superseding scenario (module docs): a clean first copy, a third of
/// the small files changed, the fault armed in the rerun, then a resume.
fn crash_resume_superseding(point: Point, nth: u64) {
    let label = format!("{}:{nth} superseding", point.name());
    let scratch = Scratch::new(&format!("{}-superseding", point.name().replace('.', "-")));
    populate(&scratch.source(), NO_REFUSAL);
    let first = scratch.run();
    assert!(first.refusals.is_empty(), "{label}: {:?}", first.refusals);
    let old = change_seats(&scratch.source());
    let changed: u64 = old
        .keys()
        .map(|path| {
            fs::metadata(scratch.source().join(relative(path)))
                .unwrap()
                .len()
        })
        .sum();

    let spec = format!("{}:{nth}", point.name());
    let group = crash_child(&scratch, point, &spec, nth > 1, &[]);
    let crash_temporaries = assert_old_or_new(&label, &scratch, &old);
    if matches!(
        point,
        Point::SupersedeAfterIntent
            | Point::SupersedeAfterExchange
            | Point::MaterializeAfterTempSeal
    ) {
        assert!(
            crash_temporaries >= 1,
            "{label}: this crash must leave a staged or a displaced file"
        );
    }
    if point == Point::SupersedeAfterExchange {
        let displaced = tree(&scratch.destination())
            .into_keys()
            .filter(|path| is_temporary(path))
            .filter(|path| {
                old.values()
                    .any(|held| *held == digest(&scratch.destination().join(relative(path))))
            })
            .count();
        assert!(displaced >= 1, "{label}: an old output must be displaced");
    }

    let files = source_files(&scratch);
    let resumed = scratch.run();
    assert!(
        resumed.refusals.is_empty(),
        "{label}: {:?}",
        resumed.refusals
    );
    assert!(
        resumed.source_bytes_read <= changed,
        "{label}: inequality 1: read {} of {changed} changed bytes",
        resumed.source_bytes_read
    );
    assert!(
        resumed.reused >= (files.len() - old.len()) as u64,
        "{label}: every unchanged file is reused: {resumed:?}"
    );
    assert_eq!(
        resumed.completed + resumed.reused,
        files.len() as u64,
        "{label}: every carried file accounted for"
    );
    // Converged: every row is a new row beside the new bytes (a changed
    // seat's old capture stays in the source ledger, so I1's per-path
    // manifest check does not apply here), and every leaf is the source's.
    let after = crash_state(&scratch);
    assert_eq!(
        assert_old_or_new(&format!("{label} resumed"), &scratch, &BTreeMap::new()),
        0,
        "{label}: a temporary survived the resume"
    );
    assert_eq!(assert_i2(&format!("{label} resumed"), &scratch), 0);
    assert_complete(&label, &scratch, NO_REFUSAL, &files, &after);
    assert_eq!(
        resumed.temporaries_removed, crash_temporaries as u64,
        "{label}: the sweep must remove exactly the crash's temporaries"
    );
    assert!(
        resumed.temporaries_left.is_empty(),
        "{label}: nothing here is ambiguous, yet {:?} was left",
        resumed.temporaries_left
    );

    let again = scratch.run();
    assert!(again.refusals.is_empty(), "{label}: {:?}", again.refusals);
    assert_eq!(again.source_bytes_read, 0, "{label}: a further run reads 0");
    assert_eq!(again.bytes_received, 0, "{label}: a further run receives 0");
    assert_eq!(again.reused, files.len() as u64, "{label}: all reused");
    println!(
        "{label}: changed={} temporaries_at_crash={crash_temporaries} resume_completed={} \
         resume_reused={} resume_unrowed_adopted={} resume_source_bytes={} \
         resume_bytes_received={}{}",
        old.len(),
        resumed.completed,
        resumed.reused,
        resumed.unrowed_adopted,
        resumed.source_bytes_read,
        resumed.bytes_received,
        group
            .map(|group| format!(" crash_{group}"))
            .unwrap_or_default(),
    );
}

/// [`crash_resume`] with extra environment for the crash child, and a check
/// of the crash state before the resume.
fn crash_resume_with(
    point: Point,
    nth: u64,
    fixture: Fixture,
    envs: &[(&str, &str)],
    check_before: impl FnOnce(&CrashState),
) {
    let label = format!("{}:{nth}", point.name());
    let scratch = Scratch::new(&point.name().replace('.', "-"));
    populate(&scratch.source(), fixture);
    let group = crash_child(&scratch, point, &label, nth > 1, envs);
    resume_after_crash(point, &label, &scratch, fixture, group, check_before);
}

/// [`crash_resume`] for the crash sweep: `false`, with nothing asserted,
/// when the copy has fewer than `nth` hits of `point`.
fn crash_resume_if_reached(point: Point, nth: u64, fixture: Fixture) -> bool {
    let label = format!("{}:{nth}", point.name());
    let scratch = Scratch::new(&point.name().replace('.', "-"));
    populate(&scratch.source(), fixture);
    let Swept::Crashed(group) = crash_child_if_reached(&scratch, point, &label, nth > 1) else {
        return false;
    };
    resume_after_crash(point, &label, &scratch, fixture, group, |_| {});
    true
}

/// The parent half of a scenario, after the child crashed at `point`: check
/// the crash state, resume in process, and assert I1 to I4 and convergence.
fn resume_after_crash(
    point: Point,
    label: &str,
    scratch: &Scratch,
    fixture: Fixture,
    group: Option<String>,
    check_before: impl FnOnce(&CrashState),
) {
    let before = crash_state(scratch);
    check_before(&before);
    assert_i1(label, scratch, &before);
    let crash_temporaries = assert_i2(label, scratch);

    let files = source_files(scratch);
    let adoptable = adoptable(scratch, &files, &before);
    let uncommitted: u64 = files
        .iter()
        .filter(|(path, _)| {
            !before.outputs.contains_key(*path)
                && !before.captures.contains_key(*path)
                && !adoptable.contains(path)
        })
        .map(|(_, size)| *size)
        .sum();
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
    // I3: committed and adopted files cost 0 source bytes; the rest are read
    // exactly once. The refused file's header is never content (#186): a
    // sniff is counted as `source_sniff_bytes`, at most once per run.
    assert_eq!(
        resumed.source_bytes_read, uncommitted,
        "{label} I3: resume read {} source bytes; uncommitted files hold {uncommitted}",
        resumed.source_bytes_read
    );
    assert_eq!(
        resumed.reused,
        before.outputs.len() as u64,
        "{label} I3: every recorded output must be reused"
    );
    assert_eq!(
        resumed.unrowed_adopted,
        adoptable.len() as u64,
        "{label} I3: every published unrowed output must be adopted (#169)"
    );
    assert_eq!(
        resumed.completed + resumed.reused,
        files.len() as u64,
        "{label}: every carried file accounted for"
    );

    let after = crash_state(scratch);
    assert_i1(&format!("{label} resumed"), scratch, &after);
    let temporaries = assert_i2(&format!("{label} resumed"), scratch);
    assert_complete(label, scratch, fixture, &files, &after);
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
         resume_completed={} resume_reused={} resume_unrowed_adopted={} resume_source_bytes={} \
         resume_bytes_received={} temporaries_left={temporaries}{}",
        before.outputs.len(),
        before.captures.len(),
        resumed.completed,
        resumed.reused,
        resumed.unrowed_adopted,
        resumed.source_bytes_read,
        resumed.bytes_received,
        group
            .map(|group| format!(" crash_{group}"))
            .unwrap_or_default(),
    );
}

/// How long a crash child runs before its watchdog ends it. The slowest
/// child (`serve.before_done`) performs a whole copy, 64 MiB file included,
/// before its point; in a debug build on a host at load average ~40 that took
/// over 90 s, so the watchdog sits well above it and well inside the 15-minute
/// CI job.
const CHILD_WATCHDOG: Duration = Duration::from_mins(4);

/// Exit status of a crash child whose watchdog fired: the scenario hung.
const CHILD_WATCHDOG_EXIT_CODE: i32 = 88;

/// How long the parent waits for a crash child: the watchdog plus a margin.
const CHILD_DEADLINE: Duration = Duration::from_secs(270);

/// Set only in a child: the scratch base whose trees it copies.
const CHILD_ENV: &str = "BULKLOAD_W7_CHILD_BASE";

/// The child half of every scenario; a no-op in an ordinary test run.
#[test]
fn crash_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    // A scenario that never reaches its fault point must not hold the CI job
    // open: past the watchdog this child ends itself, never signalled.
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("crash child watchdog: no fault point within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself, with a status the parent reports by scenario.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
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

const WITH_REFUSAL: Fixture = Fixture {
    refused: true,
    superseding: false,
};
const NO_REFUSAL: Fixture = Fixture {
    refused: false,
    superseding: false,
};
const SUPERSEDING: Fixture = Fixture {
    refused: false,
    superseding: true,
};

macro_rules! scenarios {
    ($($name:ident => $point:ident : $nth:expr, $fixture:expr;)*) => {
        $(
            #[test]
            fn $name() {
                crash_resume(Point::$point, $nth, $fixture);
            }
        )*

        /// The table as data: what the crash sweep reads its points from.
        const SCENARIOS: &[(Point, u64, Fixture)] =
            &[$((Point::$point, $nth, $fixture)),*];
    };
}

scenarios! {
    publish_source_after_manifest_insert_first => PublishSourceAfterManifestInsert: 1, WITH_REFUSAL;
    publish_source_after_manifest_insert_mid => PublishSourceAfterManifestInsert: 8, WITH_REFUSAL;
    publish_source_before_commit_first => PublishSourceBeforeCommit: 1, WITH_REFUSAL;
    publish_source_before_commit_mid => PublishSourceBeforeCommit: 8, WITH_REFUSAL;
    publish_source_after_commit_first => PublishSourceAfterCommit: 1, WITH_REFUSAL;
    publish_source_after_commit_mid => PublishSourceAfterCommit: 8, WITH_REFUSAL;
    materialize_after_temp_write_first => MaterializeAfterTempWrite: 1, WITH_REFUSAL;
    materialize_after_temp_write_mid => MaterializeAfterTempWrite: 25, WITH_REFUSAL;
    materialize_after_temp_seal_first => MaterializeAfterTempSeal: 1, WITH_REFUSAL;
    materialize_after_temp_seal_mid => MaterializeAfterTempSeal: 25, WITH_REFUSAL;
    materialize_after_rename_first => MaterializeAfterRename: 1, WITH_REFUSAL;
    materialize_after_rename_mid => MaterializeAfterRename: 25, WITH_REFUSAL;
    publish_destination_after_dir_seal_first => PublishDestinationAfterDirSeal: 1, WITH_REFUSAL;
    publish_destination_after_dir_seal_mid => PublishDestinationAfterDirSeal: 20, WITH_REFUSAL;
    publish_destination_before_commit_first => PublishDestinationBeforeCommit: 1, WITH_REFUSAL;
    publish_destination_before_commit_mid => PublishDestinationBeforeCommit: 20, WITH_REFUSAL;
    publish_destination_after_commit_first => PublishDestinationAfterCommit: 1, WITH_REFUSAL;
    publish_destination_after_commit_mid => PublishDestinationAfterCommit: 20, WITH_REFUSAL;
    directory_after_mkdir => DirectoryAfterMkdir: 1, NO_REFUSAL;
    directory_after_pending_record => DirectoryAfterPendingRecord: 1, NO_REFUSAL;
    directory_after_rename => DirectoryAfterRename: 1, NO_REFUSAL;
    directory_before_complete => DirectoryBeforeComplete: 1, NO_REFUSAL;
    // R-N119: the child's exclusive renames report EINVAL, so directories
    // take the mkdirat fallback; its intent record, committed first, lets the
    // resume adopt the directory the crash left.
    directory_after_fallback_mkdir => DirectoryAfterFallbackMkdir: 1, NO_REFUSAL;
    serve_after_content_mid => ServeAfterContent: 25, WITH_REFUSAL;
    serve_before_done => ServeBeforeDone: 1, WITH_REFUSAL;
    receive_after_decide_first => ReceiveAfterDecide: 1, WITH_REFUSAL;
    receive_after_decide_mid => ReceiveAfterDecide: 30, WITH_REFUSAL;
    receive_after_chunks_mid => ReceiveAfterChunks: 25, WITH_REFUSAL;
    receive_after_end_mid => ReceiveAfterEnd: 25, WITH_REFUSAL;
    // WP0(d), #187: the fault is armed in a rerun that supersedes this
    // store's own outputs of changed seats (module docs).
    supersede_after_intent_first => SupersedeAfterIntent: 1, SUPERSEDING;
    supersede_after_intent_mid => SupersedeAfterIntent: 5, SUPERSEDING;
    supersede_after_exchange_first => SupersedeAfterExchange: 1, SUPERSEDING;
    supersede_after_exchange_mid => SupersedeAfterExchange: 7, SUPERSEDING;
    superseding_after_temp_seal_mid => MaterializeAfterTempSeal: 4, SUPERSEDING;
    superseding_after_dir_seal_first => PublishDestinationAfterDirSeal: 1, SUPERSEDING;
    superseding_after_dir_seal_mid => PublishDestinationAfterDirSeal: 6, SUPERSEDING;
    superseding_before_commit_mid => PublishDestinationBeforeCommit: 3, SUPERSEDING;
    superseding_after_commit_mid => PublishDestinationAfterCommit: 9, SUPERSEDING;
    superseding_receive_after_end_mid => ReceiveAfterEnd: 8, SUPERSEDING;
}

/// Set to `1` by `just crash-sweep`: run the whole (point, nth) sweep.
const SWEEP_ENV: &str = "BULKLOAD_CRASH_SWEEP";
/// How many points the sweep runs at a time (default: a quarter of the
/// cores; each run writes and copies the 64 MiB fixture).
const SWEEP_JOBS_ENV: &str = "BULKLOAD_CRASH_SWEEP_JOBS";
/// No point of this fixture is hit this often; a sweep still crashing here
/// has lost its end.
const SWEEP_LIMIT: u64 = 4096;
/// A point's sweep stops after this many failed hits, so a hang at every
/// hit cannot hold the sweep for hours.
const SWEEP_FAILURES_PER_POINT: usize = 3;

/// One row per point of the `scenarios!` table: its fixture and the largest
/// `nth` the table pins. A point always runs against one fixture.
fn sweep_rows() -> Vec<(Point, Fixture, u64)> {
    let mut rows: Vec<(Point, Fixture, u64)> = Vec::new();
    for (point, nth, fixture) in SCENARIOS {
        if let Some(row) = rows.iter_mut().find(|row| row.0 == *point) {
            assert_eq!(
                row.1.refused,
                fixture.refused,
                "{}: one fixture per point",
                point.name()
            );
            row.2 = row.2.max(*nth);
        } else {
            rows.push((*point, *fixture, *nth));
        }
    }
    rows
}

/// The crash sweep (deep tier, `just crash-sweep`; never a PR gate): for
/// every point of the `scenarios!` table, a crash at every hit `nth = 1, 2,
/// ...` of one copy of the fixture, each followed by the resume and the same
/// I1 to I4 and convergence checks as the pinned scenarios. A point's sweep
/// ends at the first `nth` the copy does not reach, which counts its hits;
/// the count must cover every `nth` the table pins. As in the pinned
/// scenarios, `nth > 1` runs with one file per commit group.
///
/// Without [`SWEEP_ENV`] this only checks the table's shape, so the PR gate
/// keeps the sweep's rows well-formed without running it. The git ingest
/// points have their own table and are not swept here.
#[test]
fn crash_sweep_every_point_and_nth() {
    let rows = sweep_rows();
    assert!(
        !rows.is_empty() && rows.iter().all(|row| row.2 >= 1),
        "the sweep reads one row per point of the scenarios table"
    );
    if std::env::var_os(SWEEP_ENV).is_none_or(|value| value != "1") {
        eprintln!(
            "crash sweep skipped ({} points): set {SWEEP_ENV}=1, or run just crash-sweep",
            rows.len()
        );
        return;
    }
    let jobs = std::env::var(SWEEP_JOBS_ENV)
        .ok()
        .and_then(|jobs| jobs.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).max(1))
        });
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::<String>::new());
    let counts = std::sync::Mutex::new(Vec::<(Point, u64, u64)>::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(rows.len()) {
            scope.spawn(|| {
                while let Some((point, fixture, pinned)) =
                    rows.get(next.fetch_add(1, Ordering::SeqCst)).copied()
                {
                    let mut failed = 0;
                    let mut hits = 0;
                    for nth in 1..=SWEEP_LIMIT {
                        let run = std::panic::catch_unwind(|| {
                            crash_resume_if_reached(point, nth, fixture)
                        });
                        match run {
                            Ok(true) => hits = nth,
                            Ok(false) => break,
                            Err(panic) => {
                                hits = nth;
                                failed += 1;
                                let message = panic
                                    .downcast_ref::<String>()
                                    .map(String::as_str)
                                    .or_else(|| panic.downcast_ref::<&str>().copied())
                                    .unwrap_or("a panic without a message");
                                failures
                                    .lock()
                                    .unwrap()
                                    .push(format!("{}:{nth}: {message}", point.name()));
                                if failed == SWEEP_FAILURES_PER_POINT {
                                    failures.lock().unwrap().push(format!(
                                        "{}: sweep stopped after {failed} failures",
                                        point.name()
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    println!(
                        "crash-sweep: {} hits={hits} pinned_max={pinned}",
                        point.name()
                    );
                    counts.lock().unwrap().push((point, hits, pinned));
                }
            });
        }
    });
    let counts = counts.into_inner().unwrap();
    let failures = failures.into_inner().unwrap();
    let runs: u64 = counts.iter().map(|(_, hits, _)| hits).sum();
    println!(
        "crash-sweep: {} points, {runs} crashes, {} failures, {jobs} at a time",
        counts.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "crash sweep failures:\n{}",
        failures.join("\n")
    );
    for (point, hits, pinned) in counts {
        assert!(
            hits >= pinned && hits < SWEEP_LIMIT,
            "{}: the sweep counted {hits} hits; the table pins {pinned}",
            point.name()
        );
    }
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
        parse("receive.after_end:7"),
        Some((Point::ReceiveAfterEnd, 7))
    );
    assert_eq!(parse("receive.after_end:0"), None);
    assert_eq!(parse("receive.after_end:x"), None);
    assert_eq!(parse("receive.after_edn"), None);
    // The v4 pack and batch points are gone with the pack (R-N58).
    assert_eq!(parse("publish.source.after_append"), None);
    assert_eq!(parse("receive.after_applied"), None);
    assert_eq!(parse("publish.after_commit"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn per_side_group_limits_parse_files_and_idle() {
    assert_eq!(parse_group_limits("1"), Some((1, None)));
    assert_eq!(
        parse_group_limits("64:600000"),
        Some((64, Some(Duration::from_mins(10))))
    );
    assert_eq!(parse_group_limits("0"), None);
    assert_eq!(parse_group_limits("x"), None);
    assert_eq!(parse_group_limits("4:"), None);
    assert_eq!(parse_group_limits(""), None);
}

/// PR #59 review, F3: the relaxed I1 branch, deterministically. The source
/// holds every capture in one group that no idle timer closes, while the
/// destination commits each output as its own group. At the third output
/// commit, outputs are recorded whose captures never committed; I1 accepts
/// them against the source's bytes, and the resume must reuse them without
/// reading the source.
#[test]
fn outputs_recorded_before_their_captures_resume() {
    crash_resume_with(
        Point::PublishDestinationAfterCommit,
        3,
        NO_REFUSAL,
        &[
            (GROUP_SOURCE_ENV, "100000:600000"),
            (GROUP_DESTINATION_ENV, "1"),
        ],
        |before| {
            let early = before
                .outputs
                .keys()
                .filter(|path| !before.captures.contains_key(*path))
                .count();
            assert!(
                early >= 3,
                "the relaxed I1 branch must be hit: {early} outputs recorded before their \
                 captures ({} outputs, {} captures)",
                before.outputs.len(),
                before.captures.len()
            );
        },
    );
}

/// The `#[ignore]` reason every known-violation test carries, verbatim prefix.
const KNOWN_VIOLATION_REASON: &str = "#[ignore = \"known violation";

/// Known violations: `#[ignore]`d tests asserting invariants the engine breaks
/// today, with the fault point each one is the only scenario for, if any.
/// Listed, never counted as coverage.
/// Empty since W4 PR 2: the four R-N86 source-pack tests pass (R-N58).
const KNOWN_VIOLATIONS: [(&str, Option<Point>); 0] = [];

/// The attributes directly above `fn <name>(` (or `pub fn <name>(`): the
/// text between the preceding item's end and the function.
fn attributes_of<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let at = [format!("\nfn {name}("), format!("\npub fn {name}(")]
        .iter()
        .find_map(|needle| source.find(needle.as_str()))?;
    let before = &source[..at];
    let start = before.rfind("\n}").map_or(0, |end| end + 2);
    Some(&before[start..])
}

/// Names this test binary itself reports as ignored: `--list --ignored`.
/// Whatever form an `#[ignore]` takes (indented, same-line, `cfg_attr`),
/// libtest is the authority on what is ignored.
fn ignored_by_libtest() -> Vec<String> {
    let listed = Command::new(std::env::current_exe().unwrap())
        .args(["--list", "--ignored"])
        .env_remove(CHILD_ENV)
        .env_remove(FAULT_ENV)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(listed.status.success(), "--list --ignored failed");
    String::from_utf8(listed.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

/// Every point is exercised by a passing (non-ignored) scenario in the
/// `scenarios!` table, or is a [`KNOWN_VIOLATIONS`] point, never both. The
/// tests libtest reports as ignored are exactly the listed violations, and
/// each carries the known-violation reason.
#[test]
fn every_fault_point_has_a_scenario() {
    let source = include_str!("fault_harness.rs");
    let start = source.find("\nscenarios! {\n").unwrap();
    let table = &source[start..];
    let table = &table[..table.find("\n}\n").unwrap()];
    let covered = |point: &Point| table.contains(&format!("=> {point:?}:"));
    let known = |point: &Point| {
        KNOWN_VIOLATIONS
            .iter()
            .any(|(_, known)| known.as_ref() == Some(point))
    };
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
    let mut ignored = ignored_by_libtest();
    ignored.sort();
    let mut listed: Vec<String> = KNOWN_VIOLATIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    listed.sort();
    assert_eq!(
        ignored, listed,
        "ignored tests must be exactly the listed known violations"
    );
    for (name, point) in KNOWN_VIOLATIONS {
        let attributes = attributes_of(source, name)
            .unwrap_or_else(|| panic!("{name}: listed but not defined here"));
        assert!(
            attributes.contains(KNOWN_VIOLATION_REASON),
            "{name}: ignored without the known-violation reason"
        );
        if let Some(point) = point {
            assert!(
                source.contains(&format!("crash_resume(Point::{point:?}, ")),
                "{}: known violation without its crash scenario",
                point.name()
            );
        }
    }
}

/// R-N79: after `materialize.after_temp_seal` each staged file not yet
/// renamed is an orphan under a tagged temporary name. A walk of the crashed
/// destination records each and never carries it, onward carry included; the
/// resume removes exactly those names, and every published output keeps its
/// content.
#[test]
fn materialize_after_temp_seal_sweeps_only_temporaries() {
    let label = format!("{}:25", Point::MaterializeAfterTempSeal.name());
    let scratch = Scratch::new("after-temp-seal-sweep");
    populate(&scratch.source(), WITH_REFUSAL);
    crash_child(&scratch, Point::MaterializeAfterTempSeal, &label, true, &[]);

    let destination = scratch.destination();
    let crashed = tree(&destination);
    let mut orphans: Vec<Vec<u8>> = crashed
        .iter()
        .filter(|(path, _)| is_temporary(path))
        .map(|(path, metadata)| {
            assert!(metadata.is_file(), "{label}: orphan is not a file");
            assert_eq!(metadata.nlink(), 1, "{label}: an orphan shares an inode");
            path.clone()
        })
        .collect();
    orphans.sort();
    assert!(!orphans.is_empty(), "{label}: the crash left no temporary");

    // No walk carries an orphan: the census records each instead of a row.
    let census = walk(
        &WalkOptions::new(fs::canonicalize(&destination).unwrap()),
        &mut NullCache,
    )
    .unwrap();
    assert!(
        census.rows.iter().all(|row| !is_temporary(&row.rel_path)),
        "{label}: the walk offered a temporary as a row"
    );
    let mut recorded = census.engine_temporaries;
    recorded.sort();
    assert_eq!(recorded, orphans);
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
        "{label}: an onward carry planted an orphan"
    );

    let resumed = scratch.run();
    assert_eq!(resumed.temporaries_removed, orphans.len() as u64);
    assert!(resumed.temporaries_left.is_empty());
    for orphan in &orphans {
        assert!(fs::symlink_metadata(destination.join(relative(orphan))).is_err());
    }
    for (path, metadata) in tree(&scratch.source()) {
        if metadata.is_file() && path != REFUSED.as_bytes() {
            assert_eq!(
                digest(&destination.join(relative(&path))),
                digest(&scratch.source().join(relative(&path))),
                "{label}: {:?}",
                relative(&path)
            );
        }
    }
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

/// Run one mutation; assert the typed refusal, a clean destination, and no
/// chunk index or chunk bytes in either store (this subsumes the four v4
/// `*_leaves_no_source_index` tests). The scratch is returned for the R-N86
/// ledger checks.
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
    // Settle the fresh seats past the racy window (#86), so every
    // bystander's capture is recorded.
    bulkload_agent::transfer::settle_racy_window(&source).unwrap();
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
    // Neither store keeps chunk bytes (R-N58): no chunk rows, no pack.
    for (side, state) in [
        ("destination", scratch.destination_state()),
        ("source", scratch.source_state()),
    ] {
        assert_index_is(
            &format!("{label} {side}"),
            &scratch,
            &state,
            &BTreeMap::new(),
        );
        assert_no_chunk_bytes(&format!("{label} {side}"), &state);
    }
    scratch
}

/// A store's committed `chunk_locations` rows are exactly `expected`.
fn assert_index_is(
    label: &str,
    scratch: &Scratch,
    state: &Path,
    expected: &BTreeMap<[u8; 32], u64>,
) {
    let index = chunk_index(state, &scratch.base.join("inspect"));
    assert!(
        index == *expected,
        "{label}: chunk_locations hold {} rows ({} bytes); expected {} rows",
        index.len(),
        index.values().sum::<u64>(),
        expected.len()
    );
}

/// A store holds no chunk bytes: no `chunks.pack` and no `chunks/` objects.
fn assert_no_chunk_bytes(label: &str, state: &Path) {
    assert!(
        fs::symlink_metadata(state.join("chunks.pack")).is_err(),
        "{label}: a chunks.pack exists"
    );
    assert!(
        fs::symlink_metadata(state.join("chunks")).is_err(),
        "{label}: a chunks directory exists"
    );
}

/// R-N86, under the digest-only ledger (R-N58): a refused live-writer
/// capture leaves no source ledger row and no byte anywhere in the source
/// store, while every bystander's capture is recorded.
fn assert_no_victim_ledger_row(mutation: Mutation) {
    let label = format!("{mutation:?}");
    let scratch = live_writer(mutation);
    let recorded = crash_state(&scratch);
    assert!(
        !recorded.captures.contains_key(VICTIM.as_bytes()),
        "{label}: the refused capture has a ledger row"
    );
    let mut bystanders: Vec<&Vec<u8>> = recorded.captures.keys().collect();
    bystanders.sort();
    assert_eq!(
        bystanders,
        [
            &b"bystander-0".to_vec(),
            &b"bystander-1".to_vec(),
            &b"bystander-2".to_vec()
        ],
        "{label}: every bystander capture is recorded"
    );
    for (path, manifest) in &recorded.captures {
        assert!(
            matches_manifest(
                &fs::read(scratch.source().join(relative(path))).unwrap(),
                manifest
            ),
            "{label}: ledger row for {:?} does not describe its source",
            relative(path)
        );
    }
    assert_no_chunk_bytes(&format!("{label} source"), &scratch.source_state());
}

// One run per mutation. Each test below calls `live_writer(mutation)` first,
// so it asserts the typed refusal, the clean destination and the empty chunk
// stores before its own ledger checks. The four `live_writer_*_refuses`
// tests ran exactly that call and nothing else; OI-1003-Q81 retired them as
// duplicates (property-test plan, retire list A).
//
// R-N86, formerly four known violations: the source committer appended each
// captured chunk to `chunks.pack` before the capture's final stat check, so a
// refused capture left its bytes in the pack, unindexed. W4 PR 2 removes the
// pack (R-N58); the ledger records a capture only after that check passes.
#[test]
fn live_writer_in_place_overwrite_leaves_no_source_ledger_row() {
    assert_no_victim_ledger_row(Mutation::InPlaceOverwrite);
}

#[test]
fn live_writer_truncate_leaves_no_source_ledger_row() {
    assert_no_victim_ledger_row(Mutation::Truncate);
}

#[test]
fn live_writer_rename_replace_leaves_no_source_ledger_row() {
    assert_no_victim_ledger_row(Mutation::RenameReplace);
}

#[test]
fn live_writer_same_size_mtime_restored_leaves_no_source_ledger_row() {
    assert_no_victim_ledger_row(Mutation::SameSizeMtimeRestored);
}
