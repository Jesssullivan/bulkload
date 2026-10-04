//! Power-loss harness (R-N88): the ALICE-style crash-state checker run on the
//! syscall trace of a real `copy`. Part of the W7 fault-harness gate; needs
//! the `io-trace` feature.
//!
//! The W7 crash-resume scenarios (`fault_harness.rs`) end the process with
//! `_exit`, so the page cache survives and a missing or misplaced flush is
//! invisible to them. Here every traced syscall of an in-process `copy` is
//! recorded (the process-wide recorder, so the engine's own threads count),
//! and `crash_check::check_view` enumerates the file-system states a power
//! loss could leave on the destination under the checker's persistence model.
//! Each state must satisfy:
//!
//! - **committed ⇒ durable**: every output record a completed store commit
//!   made durable names a file holding the source's bytes; a completed
//!   directory record names a directory with the source's mode; a directory
//!   record bound to an inode names a directory that exists;
//! - **captured ⇒ held** (R25 strict, OI-1001-Q15): every source capture a
//!   completed ledger commit made durable has its bytes in the destination
//!   view, under the final name or a temporary the resume salvages;
//! - **no torn final names**: every regular file under a final (non
//!   `.bulkload-*`) name holds exactly the source's bytes, and every symlink
//!   the source's target;
//! - **returned ⇒ complete**: once `copy` has returned, the destination holds
//!   the whole source tree, with its modes.
//!
//! The store databases are not modelled: a store commit is an
//! `Event::Commit`, durable once it returns. Writes outside the destination
//! image would be foreign, dropped and counted, never silently; since the
//! source keeps no pack (R-N58) a copy makes none, and the test asserts so.
//! A copy's only foreign mutations are its two stores' state roots and
//! databases, created outside the destination.
//!
//! What a store's records need on disk is proven on its own trace (#161,
//! R25): `Store::open` and a record commit, checked from the state root's
//! parent. In every crash state after the open returned (so before Start
//! hands the store's authority to the peer), or after a commit returned, the
//! state root and its database are named. The same trace with the open's
//! directory seals cut out (the code before #161) must fail, so the proof
//! has teeth; and a store an earlier run left unsealed is sealed by the next
//! open.
//!
//! Two syscall-order checks cover what the destination image cannot see: the
//! source writes no content bytes at all, so a capture commit references
//! nothing that could be lost (the digest-only ledger, R-N58, which replaces
//! the PR #59 review's M3 pack-seal order), and `--durability=strict` seals
//! every destination file and directory with a full flush (M8).
//!
//! Tests run one at a time (`SERIAL`): the recorder is process-wide.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unnecessary_debug_formatting
)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use bulkload_agent::crash_check::{check_view, Entry, Image, Options, Report, StateInfo, View};
use bulkload_agent::durable::{set_durability, Durability};
use bulkload_agent::trace::recorder::Recorder;
use bulkload_agent::trace::{CommitRecord, Event, NodeId, SyncKind};
use bulkload_agent::transfer::copy;
use bulkload_agent::transfer_store::{Manifest, Store};
use bulkload_proto::RowSchema;

static SERIAL: Mutex<()> = Mutex::new(());
static NEXT: AtomicU64 = AtomicU64::new(0);

const TEMP_PREFIX: &[u8] = b".bulkload-";

struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-power-loss-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        for directory in ["source", "destination"] {
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Small files in the root and in two nested directories whose modes differ
/// from the 0700 they are created with, and one symlink.
fn populate(source: &Path) {
    fs::create_dir_all(source.join("nested/deeper")).unwrap();
    for (index, (path, length)) in [
        ("a", 3_000),
        ("b", 40_000),
        ("nested/c", 5_000),
        ("nested/d", 9_000),
        ("nested/deeper/e", 2_000),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(source.join(path), noise(index as u64 + 7, length)).unwrap();
    }
    fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(
        source.join("nested/deeper"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("a", source.join("link")).unwrap();
    // Fresh seats are racy for one timestamp tick and their captures are
    // never recorded (#86); settle them so every capture commits and the
    // R25 check on committed captures is not vacuous.
    bulkload_agent::transfer::settle_racy_window(source).unwrap();
}

/// The source tree the invariants compare against.
#[derive(Default)]
struct Expected {
    files: BTreeMap<Vec<u8>, Vec<u8>>,
    directories: BTreeMap<Vec<u8>, u32>,
    links: BTreeMap<Vec<u8>, Vec<u8>>,
}

fn expected(root: &Path) -> Expected {
    fn visit(root: &Path, directory: &Path, expected: &mut Expected) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                expected.links.insert(
                    rel,
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                );
            } else if metadata.is_dir() {
                expected.directories.insert(rel, metadata.mode() & 0o7777);
                visit(root, &path, expected);
            } else {
                expected.files.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = Expected::default();
    visit(root, root, &mut out);
    out
}

/// Run one `copy` with every traced syscall recorded; return the pre-copy
/// destination image and the trace.
fn traced_copy(scratch: &Scratch) -> (Image, Vec<Event>) {
    let image = Image::scan(&scratch.destination()).unwrap();
    let recorder = Recorder::new();
    let stats = {
        let _process = recorder.attach_process();
        copy(
            &scratch.source(),
            &scratch.destination(),
            &scratch.base.join("source-state"),
            &scratch.base.join("destination-state"),
        )
        .unwrap()
    };
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    (image, recorder.take())
}

fn options() -> Options {
    Options {
        ignore_foreign: true,
        // A real copy's trace is long, so some crash points are explored
        // only within the bound. Each is listed in the report's summary: a
        // failing assertion prints it in full; on a passing run libtest
        // captures the `eprintln!`, so run with `--nocapture` to see it.
        accept_bounded: true,
        ..Options::default()
    }
}

fn is_temporary(rel: &[u8]) -> bool {
    rel.rsplit(|byte| *byte == b'/')
        .next()
        .is_some_and(|leaf| leaf.starts_with(TEMP_PREFIX))
}

/// The directory a completion record names: its key is `(authority, rel)`.
fn key_path(key: &[u8]) -> Vec<u8> {
    postcard::from_bytes::<(Vec<u8>, Vec<u8>)>(key).unwrap().1
}

fn invariant(
    expected: &Expected,
    events: &[Event],
    view: View<'_>,
    info: &StateInfo,
) -> Result<(), String> {
    let name = |rel: &[u8]| String::from_utf8_lossy(rel).into_owned();
    for commit in &info.commits {
        let Event::Commit { records, .. } = &events[*commit] else {
            return Err(format!("event {commit} is not a commit"));
        };
        for record in records {
            match record {
                CommitRecord::Output { rel_path } => match view.get(rel_path) {
                    Some(Entry::File { data, .. })
                        if expected.files.get(rel_path).map(Vec::as_slice) == Some(data) => {}
                    other => {
                        return Err(format!(
                            "committed output {} is {:?}",
                            name(rel_path),
                            other.map(|entry| format!("{entry:?}")
                                .chars()
                                .take(60)
                                .collect::<String>())
                        ))
                    }
                },
                CommitRecord::DirectoryComplete { key } => {
                    let rel = key_path(key);
                    match view.get(&rel) {
                        Some(Entry::Dir { mode })
                            if expected.directories.get(&rel) == Some(&mode) => {}
                        other => {
                            return Err(format!("completed directory {} is {other:?}", name(&rel)))
                        }
                    }
                }
                CommitRecord::DirectoryCreated {
                    node: Some(node), ..
                } => {
                    if !matches!(view.node(*node), Some(Entry::Dir { .. })) {
                        return Err(format!("recorded directory inode {node:?} is not named"));
                    }
                }
                // R25 strict (OI-1001-Q15, #77 round 2, N1): a committed
                // capture means its bytes are held durably here, under the
                // final name or a salvageable temporary, so the resume never
                // reads them from the source again.
                CommitRecord::Capture { key } => {
                    let (_, row): (Vec<u8>, RowSchema) = postcard::from_bytes(key).unwrap();
                    if let Some(want) = expected.files.get(&row.rel_path) {
                        let held = view.walk().into_iter().any(|(_, entry)| {
                            matches!(entry, Entry::File { data, .. } if data == want.as_slice())
                        });
                        if !held {
                            return Err(format!(
                                "committed capture {} has no held bytes",
                                name(&row.rel_path)
                            ));
                        }
                    }
                }
                CommitRecord::DirectoryCreated { node: None, .. } => {}
            }
        }
    }
    for (rel, entry) in view.walk() {
        if is_temporary(&rel) {
            continue;
        }
        let good = match entry {
            Entry::File { data, .. } => expected.files.get(&rel).map(Vec::as_slice) == Some(data),
            Entry::Dir { .. } => expected.directories.contains_key(&rel),
            Entry::Symlink { target } => {
                expected.links.get(&rel).map(Vec::as_slice) == Some(target)
            }
        };
        if !good {
            return Err(format!("final name {} holds {entry:?}", name(&rel))
                .chars()
                .take(160)
                .collect());
        }
    }
    if info.complete {
        for (rel, data) in &expected.files {
            if !matches!(view.get(rel), Some(Entry::File { data: held, .. }) if held == data.as_slice())
            {
                return Err(format!("copy returned but {} is not durable", name(rel)));
            }
        }
        for (rel, mode) in &expected.directories {
            if view.get(rel) != Some(Entry::Dir { mode: *mode }) {
                return Err(format!(
                    "copy returned but directory {} is {:?}",
                    name(rel),
                    view.get(rel)
                ));
            }
        }
        for (rel, target) in &expected.links {
            if view.get(rel) != Some(Entry::Symlink { target }) {
                return Err(format!(
                    "copy returned but symlink {} is not durable",
                    name(rel)
                ));
            }
        }
    }
    Ok(())
}

/// Every power-loss state of a real `copy` in the default (group) mode
/// satisfies the invariants in the module docs.
#[test]
fn every_power_loss_state_of_a_copy_is_consistent() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("group");
    populate(&scratch.source());
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Commit { records, .. }
            if records.iter().any(|record| matches!(record, CommitRecord::Output { .. })))),
        "the trace holds destination output commits"
    );
    let report = check_view(&image, &events, &options(), |view, info| {
        invariant(&want, &events, view, info)
    })
    .unwrap();
    eprintln!("{}", report.summary(&events));
    // Wire v5 keeps no source pack (R-N58): the source side makes no traced
    // write at all. The only mutations foreign to the destination image are
    // the two stores' state roots and databases (#161).
    assert_eq!(
        store_creations(&events),
        4,
        "two state roots, two databases"
    );
    assert_eq!(
        report.foreign,
        store_creations(&events),
        "the source side writes no content"
    );
    assert!(
        report.states > report.crash_points,
        "crash points have several states"
    );
    assert!(report.passed(), "{}", report.summary(&events));
}

/// The same under `--durability=strict`.
#[test]
fn every_power_loss_state_of_a_strict_copy_is_consistent() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Strict);
    let scratch = Scratch::new("strict");
    populate(&scratch.source());
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    set_durability(Durability::Group);
    let report = check_view(&image, &events, &options(), |view, info| {
        invariant(&want, &events, view, info)
    })
    .unwrap();
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
}

/// PR #59 review, M6: an entry sealed only by a barrier after the last store
/// commit is made durable by the session's full flush. A copy whose only
/// entry is a symlink has no output or directory commit to drain the drive
/// after it, so once `copy` returns the symlink must survive a power loss.
/// (On Linux the directory seal is already `fsync`, so this holds without
/// the session flush; the check has teeth on Darwin.)
#[test]
fn a_symlink_only_copy_is_durable_once_it_returns() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("symlink-only");
    std::os::unix::fs::symlink("elsewhere", scratch.source().join("only")).unwrap();
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Symlink { .. })),
        "the symlink is traced"
    );
    let report = check_view(&image, &events, &options(), |view, info| {
        invariant(&want, &events, view, info)
    })
    .unwrap();
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
}

/// The destination's nodes: its root and everything created beneath it.
fn destination_nodes(scratch: &Scratch, events: &[Event]) -> HashSet<NodeId> {
    let root = fs::metadata(scratch.destination()).unwrap();
    let mut nodes = HashSet::from([NodeId {
        dev: root.dev(),
        ino: root.ino(),
    }]);
    for event in events {
        match event {
            Event::Create {
                dir: Some(dir),
                node,
                ..
            }
            | Event::Mkdir { dir, node, .. }
            | Event::Symlink { dir, node, .. }
                if nodes.contains(dir) =>
            {
                nodes.insert(*node);
            }
            _ => {}
        }
    }
    nodes
}

/// PR #59 review, M8: `--durability=strict` seals every destination file and
/// directory with a full flush, never a barrier. (On Linux both modes seal
/// with `fsync`, so the check distinguishes the modes on Darwin only.)
#[test]
fn strict_mode_seals_every_destination_node_with_a_full_flush() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let full = if cfg!(target_vendor = "apple") {
        SyncKind::FullFlush
    } else {
        SyncKind::Fsync
    };
    let seal_kinds = |mode: Durability| -> BTreeSet<String> {
        set_durability(mode);
        let scratch = Scratch::new("seal-kinds");
        populate(&scratch.source());
        let (_, events) = traced_copy(&scratch);
        set_durability(Durability::Group);
        let nodes = destination_nodes(&scratch, &events);
        events
            .iter()
            .filter_map(|event| match event {
                Event::Sync { node, kind } if nodes.contains(node) => Some(format!("{kind:?}")),
                _ => None,
            })
            .collect()
    };
    let strict = seal_kinds(Durability::Strict);
    assert_eq!(
        strict,
        BTreeSet::from([format!("{full:?}")]),
        "strict mode may only fully flush destination nodes"
    );
    if cfg!(target_vendor = "apple") {
        let group = seal_kinds(Durability::Group);
        assert!(
            group.contains("Barrier"),
            "group mode barriers on Darwin, so the check can tell the modes apart: {group:?}"
        );
    }
}

/// R-N58 (formerly PR #59 review, M3, the pack-seal order): the source keeps
/// no byte pack. Every traced write lands in a file the trace itself created
/// (a destination temporary), so no capture commit can reference bytes a
/// power loss could take away; and captures are still committed.
#[test]
fn the_source_writes_no_content_bytes() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("ledger-only");
    populate(&scratch.source());
    let (_, events) = traced_copy(&scratch);
    let created: HashSet<NodeId> = events
        .iter()
        .filter_map(|event| match event {
            Event::Create { node, .. }
            | Event::Mkdir { node, .. }
            | Event::Symlink { node, .. } => Some(*node),
            _ => None,
        })
        .collect();
    let mut captures = 0;
    let mut foreign_writes: BTreeMap<(u64, u64), usize> = BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        match event {
            Event::Write { node, .. } if !created.contains(node) => {
                foreign_writes.entry((node.dev, node.ino)).or_insert(index);
            }
            Event::Commit { records, .. }
                if records
                    .iter()
                    .any(|record| matches!(record, CommitRecord::Capture { .. })) =>
            {
                captures += 1;
            }
            _ => {}
        }
    }
    assert!(
        foreign_writes.is_empty(),
        "content written outside the destination's own files: {foreign_writes:?}"
    );
    assert!(captures > 0, "captures were committed");
    assert!(!scratch.base.join("source-state/chunks.pack").exists());
}

/// The traced creations of a copy's two state roots and their databases
/// (#161), which lie outside the destination image.
fn store_creations(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| match event {
            Event::Mkdir { name, .. } => {
                name.as_slice() == b"source-state" || name.as_slice() == b"destination-state"
            }
            Event::Create {
                name: Some(name), ..
            } => name.as_slice() == b"transfer.sqlite",
            _ => false,
        })
        .count()
}

/// The state root each store proof opens, inside its scanned parent.
const STATE: &[u8] = b"state";
/// The state root's database.
const DATABASE: &[u8] = b"state/transfer.sqlite";

fn node(path: &Path) -> NodeId {
    let metadata = fs::metadata(path).unwrap();
    NodeId {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

/// Run `step` with this thread's traced calls recorded. `Store::open` and a
/// record commit run on the calling thread, so no other test's calls can
/// enter the trace.
fn traced<T>(step: impl FnOnce() -> T) -> (T, Vec<Event>) {
    let recorder = Recorder::new();
    let value = {
        let _attached = recorder.attach();
        step()
    };
    (value, recorder.take())
}

/// `events` without any sync of `nodes`: the same calls as a run that never
/// sealed those directories.
fn without_seals(events: &[Event], nodes: &[NodeId]) -> Vec<Event> {
    events
        .iter()
        .filter(|event| !matches!(event, Event::Sync { node, .. } if nodes.contains(node)))
        .cloned()
        .collect()
}

/// Whether `events` hold a sync of `node`.
fn seals(events: &[Event], node: NodeId) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Sync { node: synced, .. } if *synced == node))
}

/// Commit one record to `store`: the R25 row a power loss must not take.
fn commit_one(store: &Store) {
    store
        .record_capture(b"r25", &Manifest::new(Vec::new()))
        .unwrap();
}

/// #161 (R25): once `Store::open` has returned (`opened` operations in) or
/// one of the store's commits has, the state root and its database are named
/// in the crash state.
fn store_kept(view: View<'_>, info: &StateInfo, opened: usize) -> Result<(), String> {
    let after = if !info.commits.is_empty() {
        "a store commit returned"
    } else if info.crash_point >= opened {
        "Store::open returned"
    } else {
        return Ok(());
    };
    if !matches!(view.get(STATE), Some(Entry::Dir { .. })) {
        return Err(format!(
            "the state root is {:?} after {after}",
            view.get(STATE)
        ));
    }
    if !matches!(view.get(DATABASE), Some(Entry::File { .. })) {
        return Err(format!(
            "the store database is {:?} after {after}",
            view.get(DATABASE)
        ));
    }
    Ok(())
}

/// Check every power-loss state of a store trace whose first `opened`
/// events are `Store::open`'s. Every event here lowers to one checker
/// operation (none is a write), so `opened` is also an operation count.
fn check_store(image: &Image, events: &[Event], opened: usize) -> Report {
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Write { .. })),
        "a store trace writes nothing the checker models"
    );
    check_view(image, events, &Options::default(), |view, info| {
        store_kept(view, info, opened)
    })
    .unwrap()
}

/// A scanned, empty parent for one store proof, and its node.
fn store_parent(scratch: &Scratch) -> (PathBuf, Image, NodeId) {
    let parent = scratch.base.join("state-parent");
    fs::create_dir(&parent).unwrap();
    let image = Image::scan(&parent).unwrap();
    let id = node(&parent);
    (parent, image, id)
}

/// #161 (R25): every power-loss state of a fresh `Store::open` and its first
/// record commit keeps the store, and a reopen of a sealed store flushes
/// nothing more.
#[test]
fn every_power_loss_state_of_a_store_open_keeps_the_store() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("store-open");
    let (parent, image, parent_node) = store_parent(&scratch);
    let state = parent.join("state");
    let (store, opening) = traced(|| Store::open(&state).unwrap());
    let root_node = node(&state);
    let ((), committing) = traced(|| commit_one(&store));
    assert!(
        opening
            .iter()
            .any(|event| matches!(event, Event::Mkdir { .. }))
            && opening
                .iter()
                .any(|event| matches!(event, Event::Create { .. })),
        "the state root and its database are traced: {opening:?}"
    );
    let events = [opening.as_slice(), committing.as_slice()].concat();
    let report = check_store(&image, &events, opening.len());
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
    assert!(
        report.states > report.crash_points,
        "crash points have several states"
    );
    assert!(
        seals(&opening, parent_node) && seals(&opening, root_node),
        "the open seals the parent and the root: {opening:?}"
    );

    // A sealed store carries its marker: reopening it flushes nothing.
    drop(store);
    let (_, reopening) = traced(|| Store::open(&state).unwrap());
    assert!(reopening.is_empty(), "{reopening:?}");
}

/// The proof has teeth (#161): the same calls without the open's directory
/// seals, as the code made them before #161, can lose the store, and with it
/// the record a commit made durable.
#[test]
fn a_store_open_without_its_seals_can_lose_the_store() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("store-unsealed");
    let (parent, image, parent_node) = store_parent(&scratch);
    let state = parent.join("state");
    let (store, opening) = traced(|| Store::open(&state).unwrap());
    let root_node = node(&state);
    let ((), committing) = traced(|| commit_one(&store));
    let opening = without_seals(&opening, &[parent_node, root_node]);
    let events = [opening.as_slice(), committing.as_slice()].concat();
    let report = check_store(&image, &events, opening.len());
    eprintln!("{}", report.summary(&events));
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.message.starts_with("the state root is None")),
        "an unsealed state root must be losable: {}",
        report.summary(&events)
    );
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.message.ends_with("after a store commit returned")),
        "a committed record must be losable with it: {}",
        report.summary(&events)
    );
}

/// #161: a store an earlier run created and never sealed (it died between
/// creating the root and sealing it, or it predates #161) is sealed by the
/// next open, before that open returns.
#[test]
fn a_store_an_earlier_run_left_unsealed_is_sealed_by_the_next_open() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("store-adopted");
    let (parent, image, parent_node) = store_parent(&scratch);
    let state = parent.join("state");
    // The earlier run: the open's calls without its seals, and no marker.
    let (store, earlier) = traced(|| Store::open(&state).unwrap());
    drop(store);
    let root_node = node(&state);
    let earlier = without_seals(&earlier, &[parent_node, root_node]);
    rusqlite::Connection::open(state.join("transfer.sqlite"))
        .unwrap()
        .execute("DELETE FROM settings WHERE key = 'root_sealed'", [])
        .unwrap();
    let (store, next) = traced(|| Store::open(&state).unwrap());
    let ((), committing) = traced(|| commit_one(&store));
    let opened = earlier.len() + next.len();
    let events = [earlier.as_slice(), next.as_slice(), committing.as_slice()].concat();
    let report = check_store(&image, &events, opened);
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
    assert!(
        seals(&next, parent_node) && seals(&next, root_node),
        "the next open seals what the earlier run left: {next:?}"
    );

    // An open that sealed only what it created would leave it unsealed.
    let next = without_seals(&next, &[parent_node, root_node]);
    let events = [earlier.as_slice(), next.as_slice(), committing.as_slice()].concat();
    let report = check_store(&image, &events, earlier.len() + next.len());
    assert!(!report.violations.is_empty(), "{}", report.summary(&events));
}
