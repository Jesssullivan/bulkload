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
//! hands the store's authority to the peer), after the commit of its
//! `root_sealed` marker returned, or after a record commit returned, the
//! state root and its database are named. The same trace with the open's
//! directory seals and marker cut out (the code before #161) must fail, and
//! so must one whose seals come after the marker's commit, so the proof has
//! teeth for the seal and for its order; and a store an earlier run left
//! unsealed is sealed by the next open.
//!
//! Two syscall-order checks cover what the destination image cannot see: the
//! source writes no content bytes at all, so a capture commit references
//! nothing that could be lost (the digest-only ledger, R-N58, which replaces
//! the PR #59 review's M3 pack-seal order), and `--durability=strict` seals
//! every destination file and directory with a full flush (M8).
//!
//! **Superseding publish** (WP0(d), OI-1003-Q18, #187). A rerun over
//! changed seats is traced the same way, from the image the first copy left.
//! In every power-loss state each final name holds the whole old output or
//! the whole new one; no row, replayed from the commits that completed, sits
//! beside bytes it does not describe (the old output with its old row, or
//! the new output with its new row); and an old output displaced under a
//! temporary name is explained by an unsettled intent, which is what lets
//! the next sweep tell it from a temporary. A second trace puts another
//! writer's file in the output's place just before the exchange: that file
//! is at the leaf, or under the temporary name with the intent still
//! recorded, in every state, never lost. Each proof is run again on its
//! trace with one ordering undone (the intent's commit after the exchange,
//! the directory seal ahead of the new row dropped, the seal between the
//! exchange back and the unlink dropped), and must then fail.
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

/// **committed ⇒ durable** and **captured ⇒ held**, for one record of a
/// store commit that completed before the crash.
fn committed_record(
    expected: &Expected,
    view: View<'_>,
    record: &CommitRecord,
) -> Result<(), String> {
    let name = |rel: &[u8]| String::from_utf8_lossy(rel).into_owned();
    match record {
        CommitRecord::Output { rel_path } => match view.get(rel_path) {
            Some(Entry::File { data, .. })
                if expected.files.get(rel_path).map(Vec::as_slice) == Some(data) => {}
            other => {
                return Err(format!(
                    "committed output {} is {:?}",
                    name(rel_path),
                    other.map(|entry| format!("{entry:?}").chars().take(60).collect::<String>())
                ))
            }
        },
        CommitRecord::DirectoryComplete { key } => {
            let rel = key_path(key);
            match view.get(&rel) {
                Some(Entry::Dir { mode }) if expected.directories.get(&rel) == Some(&mode) => {}
                other => return Err(format!("completed directory {} is {other:?}", name(&rel))),
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
        // The state roots lie outside the destination image; the
        // store proofs below check what the marker vouches for. A
        // remembered refusal (#186) names no destination bytes, and
        // a first copy supersedes nothing (the superseding proofs
        // below read those records).
        CommitRecord::DirectoryCreated { node: None, .. }
        | CommitRecord::RootSealed
        | CommitRecord::RefusedSeat { .. }
        | CommitRecord::SupersedeBegun { .. }
        | CommitRecord::SupersedeSettled { .. } => {}
    }
    Ok(())
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
            committed_record(expected, view, record)?;
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

/// Whether `event` syncs one of `nodes`.
fn syncs(event: &Event, nodes: &[NodeId]) -> bool {
    matches!(event, Event::Sync { node, .. } if nodes.contains(node))
}

/// `events` without any sync of `nodes`: the same calls as a run that never
/// sealed those directories.
fn without_seals(events: &[Event], nodes: &[NodeId]) -> Vec<Event> {
    events
        .iter()
        .filter(|event| !syncs(event, nodes))
        .cloned()
        .collect()
}

/// Whether `event` is the commit of a store's `root_sealed` marker.
fn is_marker(event: &Event) -> bool {
    matches!(event, Event::Commit { records, .. }
        if records.iter().any(|record| matches!(record, CommitRecord::RootSealed)))
}

/// `events` as the code before #161 made them: no seal of `nodes` and no
/// `root_sealed` marker.
fn before_the_seal(events: &[Event], nodes: &[NodeId]) -> Vec<Event> {
    without_seals(events, nodes)
        .into_iter()
        .filter(|event| !is_marker(event))
        .collect()
}

/// Whether `events` hold a sync of `node`.
fn seals(events: &[Event], node: NodeId) -> bool {
    events.iter().any(|event| syncs(event, &[node]))
}

/// Commit one record to `store`: the R25 row a power loss must not take.
fn commit_one(store: &Store) {
    store
        .record_capture(b"r25", &Manifest::new(Vec::new()))
        .unwrap();
}

/// #161 (R25): once `Store::open` has returned (`opened` operations in), or
/// the commit of its `root_sealed` marker has, or a record commit has, the
/// state root and its database are named in the crash state. The marker's
/// commit counts on its own: every later open trusts it and seals nothing,
/// so a marker committed before its seal would leave the store unsealed for
/// good, although the open still returns sealed.
fn store_kept(
    events: &[Event],
    view: View<'_>,
    info: &StateInfo,
    opened: usize,
) -> Result<(), String> {
    let committed = |marker: bool| {
        info.commits
            .iter()
            .any(|commit| is_marker(&events[*commit]) == marker)
    };
    let after = if committed(false) {
        "a store commit returned"
    } else if committed(true) {
        "the root_sealed marker committed"
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
        store_kept(events, view, info, opened)
    })
    .unwrap()
}

/// The trace index of the one `root_sealed` marker commit in `events`.
fn marker_commit(events: &[Event]) -> usize {
    let markers: Vec<usize> = (0..events.len())
        .filter(|index| is_marker(&events[*index]))
        .collect();
    assert_eq!(markers.len(), 1, "one marker commit: {events:?}");
    markers[0]
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
    let marker = marker_commit(&opening);
    assert!(
        seals(&opening[..marker], parent_node) && seals(&opening[..marker], root_node),
        "the open seals the parent and the root before its marker commits: {opening:?}"
    );

    // A sealed store carries its marker: reopening it flushes and commits
    // nothing.
    drop(store);
    let (_, reopening) = traced(|| Store::open(&state).unwrap());
    assert!(reopening.is_empty(), "{reopening:?}");
}

/// The proof has teeth (#161): the same calls without the open's directory
/// seals or its marker, as the code made them before #161, can lose the
/// store, and with it the record a commit made durable.
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
    let opening = before_the_seal(&opening, &[parent_node, root_node]);
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

/// The proof has teeth for the marker's order too (#161): an open that
/// commits its `root_sealed` marker before it seals (the seal moved after
/// `COMMIT`) still returns with both directories sealed, yet a power loss
/// between the commit and the seal keeps a marker beside a root that is
/// gone, and every later open trusts that marker.
#[test]
fn a_store_marker_committed_before_its_seal_can_lose_the_store() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("store-marker-first");
    let (parent, image, parent_node) = store_parent(&scratch);
    let state = parent.join("state");
    let (_, opening) = traced(|| Store::open(&state).unwrap());
    let nodes = [parent_node, node(&state)];
    let sealing: Vec<Event> = opening
        .iter()
        .filter(|event| syncs(event, &nodes))
        .cloned()
        .collect();
    assert_eq!(sealing.len(), 2, "one seal each: {opening:?}");
    let mut reordered = without_seals(&opening, &nodes);
    let after_marker = marker_commit(&reordered) + 1;
    reordered.splice(after_marker..after_marker, sealing);
    assert!(seals(&reordered, nodes[0]) && seals(&reordered, nodes[1]));
    let report = check_store(&image, &reordered, reordered.len());
    eprintln!("{}", report.summary(&reordered));
    assert!(
        report.violations.iter().any(|violation| violation
            .message
            .starts_with("the state root is None")
            && violation
                .message
                .ends_with("after the root_sealed marker committed")),
        "a marker committed before its seal must be losable: {}",
        report.summary(&reordered)
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
    let earlier = before_the_seal(&earlier, &[parent_node, root_node]);
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
    let marker = marker_commit(&next);
    assert!(
        seals(&next[..marker], parent_node) && seals(&next[..marker], root_node),
        "the next open seals what the earlier run left, then records it: {next:?}"
    );

    // An open that sealed only what it created would leave it unsealed.
    let next = without_seals(&next, &[parent_node, root_node]);
    let events = [earlier.as_slice(), next.as_slice(), committing.as_slice()].concat();
    let report = check_store(&image, &events, earlier.len() + next.len());
    assert!(!report.violations.is_empty(), "{}", report.summary(&events));
}

// ---------------------------------------------------------------------------
// Superseding publish (WP0(d), OI-1003-Q18, #187)
// ---------------------------------------------------------------------------

fn run_copy(scratch: &Scratch) -> bulkload_agent::transfer::TransferStats {
    copy(
        &scratch.source(),
        &scratch.destination(),
        &scratch.base.join("source-state"),
        &scratch.base.join("destination-state"),
    )
    .unwrap()
}

/// A first copy of [`populate`]'s tree, then `change` applied to the source
/// and settled. Returns the source as it was copied and as it is now.
fn copied_then_changed(scratch: &Scratch, change: impl FnOnce(&Path)) -> (Expected, Expected) {
    populate(&scratch.source());
    let first = run_copy(scratch);
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let before = expected(&scratch.source());
    change(&scratch.source());
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();
    (before, expected(&scratch.source()))
}

/// One rerun with every traced syscall recorded, from the image the first
/// copy left.
fn traced_rerun(scratch: &Scratch) -> (Image, Vec<Event>, bulkload_agent::transfer::TransferStats) {
    let image = Image::scan(&scratch.destination()).unwrap();
    let recorder = Recorder::new();
    let stats = {
        let _process = recorder.attach_process();
        run_copy(scratch)
    };
    (image, recorder.take(), stats)
}

/// The store's view of the superseded paths in one crash state, replayed
/// from the commits that completed: the bytes each path's row vouches for,
/// and the intents recorded and not yet settled, by path.
struct Rows<'a> {
    rows: BTreeMap<Vec<u8>, &'a [u8]>,
    intents: BTreeMap<Vec<u8>, Vec<u8>>,
}

fn replay_rows<'a>(
    before: &'a Expected,
    after: &'a Expected,
    events: &[Event],
    view: View<'_>,
    info: &StateInfo,
) -> Result<Rows<'a>, String> {
    // The first copy committed a row for every file it carried.
    let mut state = Rows {
        rows: before
            .files
            .iter()
            .map(|(rel, data)| (rel.clone(), data.as_slice()))
            .collect(),
        intents: BTreeMap::new(),
    };
    for commit in &info.commits {
        let Event::Commit { records, .. } = &events[*commit] else {
            return Err(format!("event {commit} is not a commit"));
        };
        for record in records {
            match record {
                CommitRecord::SupersedeBegun {
                    rel_path,
                    temp_path,
                    ..
                } => {
                    state.rows.remove(rel_path);
                    state.intents.insert(rel_path.clone(), temp_path.clone());
                }
                CommitRecord::SupersedeSettled { rel_path, restored } => {
                    state.intents.remove(rel_path);
                    if let (true, Some(data)) = (*restored, before.files.get(rel_path)) {
                        state.rows.insert(rel_path.clone(), data);
                    }
                }
                CommitRecord::Output { rel_path } => {
                    if let Some(data) = after.files.get(rel_path) {
                        state.rows.insert(rel_path.clone(), data);
                    }
                    committed_record(after, view, record)?;
                }
                other => committed_record(after, view, other)?,
            }
        }
    }
    Ok(state)
}

/// The superseding invariants of the module docs, for one crash state.
fn superseding_invariant(
    before: &Expected,
    after: &Expected,
    events: &[Event],
    view: View<'_>,
    info: &StateInfo,
) -> Result<(), String> {
    let name = |rel: &[u8]| String::from_utf8_lossy(rel).into_owned();
    let state = replay_rows(before, after, events, view, info)?;
    // A row never sits beside other bytes.
    for (rel, want) in &state.rows {
        if !matches!(view.get(rel), Some(Entry::File { data, .. }) if data == *want) {
            return Err(format!("a row vouches for other bytes at {}", name(rel)));
        }
    }
    for (rel, entry) in view.walk() {
        let Entry::File { data, .. } = entry else {
            continue;
        };
        if is_temporary(&rel) {
            // An old output under a temporary name was displaced by an
            // exchange: its intent must be recorded, and name this path.
            let displaced = before
                .files
                .iter()
                .find(|(path, old)| old.as_slice() == data && after.files.get(*path) != Some(*old));
            if let Some((path, _)) = displaced {
                if state.intents.get(path) != Some(&rel) {
                    return Err(format!(
                        "the old output of {} sits at {} with no intent recorded",
                        name(path),
                        name(&rel)
                    ));
                }
            }
            continue;
        }
        // The whole old output or the whole new one, never a mix.
        let old = before.files.get(&rel).map(Vec::as_slice) == Some(data);
        let new = after.files.get(&rel).map(Vec::as_slice) == Some(data);
        if !old && !new {
            return Err(format!("{} holds neither output", name(&rel)));
        }
    }
    if info.complete {
        for (rel, data) in &after.files {
            if !matches!(view.get(rel), Some(Entry::File { data: held, .. }) if held == data.as_slice())
            {
                return Err(format!(
                    "copy returned but {} is not the new output",
                    name(rel)
                ));
            }
        }
    }
    Ok(())
}

fn superseding_report(
    image: &Image,
    events: &[Event],
    before: &Expected,
    after: &Expected,
) -> Report {
    check_view(image, events, &options(), |view, info| {
        superseding_invariant(before, after, events, view, info)
    })
    .unwrap()
}

fn begins(event: &Event) -> bool {
    matches!(event, Event::Commit { records, .. }
        if records.iter().any(|record| matches!(record, CommitRecord::SupersedeBegun { .. })))
}

/// A rerun that supersedes two of this store's outputs (one grown, one
/// rewritten at its size, in two directories) and adds a seat: every
/// power-loss state keeps the old output with its old row or the new output
/// with its new row. The same trace with the intent's commit moved after
/// the first exchange, or with the directory seals ahead of the new rows
/// dropped, must fail.
#[test]
fn every_power_loss_state_of_a_superseding_rerun_holds_the_old_or_the_new_output() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("supersede");
    let (before, after) = copied_then_changed(&scratch, |source| {
        let mut grown = fs::read(source.join("b")).unwrap();
        grown.extend(noise(91, 7_000));
        fs::write(source.join("b"), grown).unwrap();
        fs::write(source.join("nested/c"), noise(92, 5_000)).unwrap();
        fs::write(source.join("nested/added"), noise(93, 1_000)).unwrap();
    });
    let (image, events, stats) = traced_rerun(&scratch);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(expected(&scratch.destination()).files, after.files);
    let exchanges: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, Event::Exchange { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(exchanges.len(), 2, "both changed outputs are exchanged");
    let begun = events.iter().position(begins).unwrap();
    assert!(
        begun < exchanges[0],
        "the intent commits before any exchange"
    );

    let report = superseding_report(&image, &events, &before, &after);
    eprintln!("{}", report.summary(&events));
    assert!(report.states > report.crash_points);
    assert!(report.passed(), "{}", report.summary(&events));

    // Teeth: the intent's commit after the exchange leaves the old row
    // beside the new output.
    let mut late = events.clone();
    let intent = late.remove(begun);
    late.insert(exchanges[0], intent);
    let report = superseding_report(&image, &late, &before, &after);
    assert!(
        !report.violations.is_empty(),
        "an exchange ahead of its intent must fail: {}",
        report.summary(&late)
    );

    // Teeth: a new row committed without the directory seals that make the
    // exchange durable can outlive it.
    let directories: HashSet<NodeId> = events
        .iter()
        .filter_map(|event| match event {
            Event::Exchange { dir, .. } => Some(*dir),
            _ => None,
        })
        .collect();
    let unsealed: Vec<Event> = events
        .iter()
        .enumerate()
        .filter(|(index, event)| {
            !(*index > exchanges[0]
                && matches!(event, Event::Sync { node, .. } if directories.contains(node)))
        })
        .map(|(_, event)| event.clone())
        .collect();
    assert!(
        unsealed.len() < events.len(),
        "the trace seals its directories"
    );
    let report = superseding_report(&image, &unsealed, &before, &after);
    assert!(
        !report.violations.is_empty(),
        "a row ahead of its directory seal must fail: {}",
        report.summary(&unsealed)
    );
}

const THEIRS: &[u8] = b"another writer's file, which no rerun may lose";

/// No-clobber across a power loss (`MC_wp0d_exchange`, and what
/// `MC_wp0d_check_rename` and `MC_neg_sweep_displaced` break): another
/// writer's file takes the output's place between the publish's last look
/// and its exchange. In every crash state that file is at the leaf, or under
/// the temporary name with the staged file at the leaf and the intent still
/// recorded, which is the state the next sweep exchanges back. The same
/// trace without the seal between the exchange back and the unlink must
/// fail: a crash could keep the unlink and lose the exchange back.
#[test]
fn a_file_a_superseding_publish_displaced_survives_every_power_loss_state() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("displaced");
    let (before, after) = copied_then_changed(&scratch, |source| {
        fs::write(source.join("b"), noise(94, 30_000)).unwrap();
    });
    // The other writer's file waits in the destination, so the image holds
    // it; its move over the leaf is that writer's own call, not a traced one,
    // so the image keeps showing the old output at the leaf until the
    // exchange, and `stray` as a second name of the file throughout.
    let (stray, leaf) = (
        scratch.destination().join("stray"),
        scratch.destination().join("b"),
    );
    fs::write(&stray, THEIRS).unwrap();
    let hook =
        bulkload_agent::materialize::set_before_exchange(&scratch.destination(), b"b", move || {
            fs::rename(&stray, &leaf).unwrap();
        })
        .unwrap();
    let (image, events, stats) = traced_rerun(&scratch);
    drop(hook);
    assert_eq!(
        stats.refusals,
        [(b"b".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(fs::read(scratch.destination().join("b")).unwrap(), THEIRS);
    let exchanges: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, Event::Exchange { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(exchanges.len(), 2, "the exchange, and the exchange back");

    let old = before.files[b"b".as_slice()].as_slice();
    let new = after.files[b"b".as_slice()].as_slice();
    let check = |events: &[Event]| {
        check_view(&image, events, &options(), |view, info| {
            let store = replay_rows(&before, &before, events, view, info)?;
            let holds = |rel: &[u8], want: &[u8]| matches!(view.get(rel), Some(Entry::File { data, .. }) if data == want);
            // Before the exchange the image still shows the old output.
            if holds(b"b", THEIRS) || holds(b"b", old) {
                return Ok(());
            }
            let aside = view
                .walk()
                .into_iter()
                .find(|(rel, entry)| {
                    is_temporary(rel)
                        && matches!(entry, Entry::File { data, .. } if *data == THEIRS)
                })
                .map(|(rel, _)| rel);
            match aside {
                Some(rel) if holds(b"b", new) && store.intents.get(b"b".as_slice()) == Some(&rel) => {
                    Ok(())
                }
                other => Err(format!(
                    "the displaced file is lost or unexplained: leaf {:?}, aside {other:?}",
                    view.get(b"b").map(|entry| matches!(entry, Entry::File { .. }))
                )),
            }
        })
        .unwrap()
    };
    let report = check(&events);
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));

    // Teeth: without the seal between the exchange back and the unlink.
    let Event::Exchange { dir, .. } = events[exchanges[1]] else {
        unreachable!()
    };
    let seal = (exchanges[1]..events.len())
        .find(|index| matches!(&events[*index], Event::Sync { node, .. } if *node == dir))
        .unwrap();
    let unlink = (exchanges[1]..events.len())
        .find(|index| matches!(&events[*index], Event::Unlink { dir: from, .. } if *from == dir))
        .unwrap();
    assert!(
        seal < unlink,
        "the exchange back is sealed before the unlink"
    );
    let mut unsealed = events;
    unsealed.remove(seal);
    let report = check(&unsealed);
    assert!(
        !report.violations.is_empty(),
        "an unlink ahead of the exchange back's seal must fail: {}",
        report.summary(&unsealed)
    );
}
