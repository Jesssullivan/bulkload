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

/// Small files in the root and in nested directories whose modes differ
/// from the 0700 they are created with, and one symlink. The directories
/// make two levels of a batch (OI-1003-Q143 item 2): `nested` and `nested2`
/// under the root, then `nested/deeper`, `nested/deeper2` and
/// `nested2/inner` under two parents. `nested2` holds no file of its own,
/// so only the batch's own seal makes `inner`'s name durable before
/// `inner/g` commits.
fn populate(source: &Path) {
    fs::create_dir_all(source.join("nested/deeper")).unwrap();
    fs::create_dir_all(source.join("nested/deeper2")).unwrap();
    fs::create_dir_all(source.join("nested2/inner")).unwrap();
    for (index, (path, length)) in [
        ("a", 3_000),
        ("b", 40_000),
        ("nested/c", 5_000),
        ("nested/d", 9_000),
        ("nested/deeper/e", 2_000),
        ("nested/deeper2/h", 1_000),
        ("nested2/inner/g", 1_500),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(source.join(path), noise(index as u64 + 7, length)).unwrap();
    }
    for (directory, mode) in [
        ("nested", 0o750),
        ("nested/deeper", 0o755),
        ("nested/deeper2", 0o751),
        ("nested2", 0o755),
        ("nested2/inner", 0o750),
    ] {
        fs::set_permissions(source.join(directory), fs::Permissions::from_mode(mode)).unwrap();
    }
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
        | CommitRecord::DirectoryCleared { .. }
        | CommitRecord::RootSealed
        | CommitRecord::RefusedSeat { .. }
        | CommitRecord::SupersedeBegun { .. }
        | CommitRecord::SupersedeSettled { .. } => {}
    }
    Ok(())
}

/// The directory a key names, for a directory record.
fn parent_of(rel: &[u8]) -> &[u8] {
    rel.iter()
        .rposition(|byte| *byte == b'/')
        .map_or(&[][..], |end| &rel[..end])
}

/// **Directory ownership** (R-N102; #217 review, findings 1, 9 and 10),
/// replayed from the commits that completed: a record bound to an inode
/// names it at the record's path, or under a temporary name directly in
/// the record's parent, never inside a directory a sweep could not remove;
/// and every directory at a final name the pre-copy image did not hold is
/// bound by its record to that very inode, covered by an intent, or
/// completed. A cleared record (`DirectoryCleared`: a discard, a swept
/// temporary, a stale record) vouches for nothing.
fn directory_ownership(
    events: &[Event],
    view: View<'_>,
    info: &StateInfo,
    before: &BTreeSet<Vec<u8>>,
) -> Result<(), String> {
    let name = |rel: &[u8]| String::from_utf8_lossy(rel).into_owned();
    let mut bound: BTreeMap<Vec<u8>, Option<NodeId>> = BTreeMap::new();
    let mut completed = BTreeSet::new();
    for commit in &info.commits {
        let Event::Commit { records, .. } = &events[*commit] else {
            continue;
        };
        for record in records {
            match record {
                CommitRecord::DirectoryCreated { key, node, .. } => {
                    completed.remove(&key_path(key));
                    bound.insert(key_path(key), *node);
                }
                CommitRecord::DirectoryComplete { key } => {
                    bound.remove(&key_path(key));
                    completed.insert(key_path(key));
                }
                CommitRecord::DirectoryCleared { key } => {
                    bound.remove(&key_path(key));
                }
                _ => {}
            }
        }
    }
    for (rel, node) in &bound {
        let Some(node) = node else {
            continue;
        };
        for path in view.paths_of(*node) {
            if path != *rel && !(parent_of(&path) == parent_of(rel) && is_temporary(&path)) {
                return Err(format!(
                    "recorded directory {} is reached at {}, inside a directory not yet named",
                    name(rel),
                    name(&path)
                ));
            }
        }
    }
    for (rel, entry) in view.walk() {
        if !matches!(entry, Entry::Dir { .. })
            || before.contains(&rel)
            || rel
                .split(|byte| *byte == b'/')
                .any(|part| part.starts_with(TEMP_PREFIX))
        {
            continue;
        }
        let owned = match bound.get(&rel) {
            Some(Some(node)) => view.node_at(&rel) == Some(*node),
            Some(None) => true,
            None => completed.contains(&rel),
        };
        if !owned {
            return Err(format!(
                "directory {} at its final name is owned by no record",
                name(&rel)
            ));
        }
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
    directory_ownership(events, view, info, &BTreeSet::new())?;
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

/// An exchange of a superseding publish: one of its two names is an output.
/// The plan's probe for the exchange (`Destination::exchange_supported`,
/// once per device) trades two empty temporaries of the store ahead of any
/// intent; it names no output and is not one of these.
fn superseding_exchange(event: &Event) -> bool {
    use bulkload_agent::materialize::temporary_name;
    matches!(event, Event::Exchange { a, b, .. }
        if temporary_name(a).is_none() || temporary_name(b).is_none())
}

/// The probe's exchanges in a trace: both names are temporaries.
fn probe_exchanges(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Exchange { .. }) && !superseding_exchange(event))
        .count()
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
        .filter(|(_, event)| superseding_exchange(event))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(exchanges.len(), 2, "both changed outputs are exchanged");
    assert_eq!(probe_exchanges(&events), 1, "one probe for the device");
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
        .filter(|(_, event)| superseding_exchange(event))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(exchanges.len(), 2, "the exchange, and the exchange back");
    assert_eq!(probe_exchanges(&events), 1, "one probe for the device");

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

// ---------------------------------------------------------------------------
// WP0(g): the relaxed source ledger (OI-1003-Q20, Q37, Q104)
// ---------------------------------------------------------------------------

/// The source ledger's row commits in a trace, in the order they returned:
/// each commit's capture keys.
fn ledger_commits(events: &[Event], source_store: NodeId) -> Vec<Vec<Vec<u8>>> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Commit { store, records } if *store == source_store => Some(
                records
                    .iter()
                    .filter_map(|record| match record {
                        CommitRecord::Capture { key } => Some(key.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .filter(|keys| !keys.is_empty())
        .collect()
}

/// Bytes, or a link's target, by relative path.
type Leaves = BTreeMap<Vec<u8>, Vec<u8>>;

/// The regular files and symlinks under `root`, as [`expected`] reads them.
fn tree(root: &Path) -> (Leaves, Leaves) {
    let found = expected(root);
    (found.files, found.links)
}

/// The source store at `state` after a power loss that kept the first
/// `keep` bytes of its WAL (`pristine`) and nothing after them; the
/// wal-index is never durable state, so it is gone too.
fn lose_ledger_tail(state: &Path, pristine: &[u8], keep: u64) {
    fs::write(
        state.join("transfer.sqlite-wal"),
        &pristine[..usize::try_from(keep).unwrap()],
    )
    .unwrap();
    let _ = fs::remove_file(state.join("transfer.sqlite-shm"));
}

/// WP0(g) (OI-1003-Q20, adopted by OI-1003-Q37): the source ledger's row
/// commits are not synced (`synchronous=NORMAL`), so a power loss may roll
/// the newest of them back. Every such state of a real copy's ledger is
/// built here as `SQLite` recovers from it: the store's WAL is cut at each
/// frame boundary from the end of the creation commit, which is
/// `synchronous=FULL` and so on disk (`transfer_store`'s
/// `only_a_source_ledgers_row_commits_are_relaxed`, and P79's sync-mode
/// floor), to its last byte, and its wal-index is removed.
///
/// The trace gives the order the row commits returned in. In every state:
///
/// - the store's authority is the creation commit's (a new one would re-key
///   every row, `MC_wp0g_authority`);
/// - the ledger holds a prefix of the traced row commits, whole: rows are
///   lost newest first, and none is torn or wrong;
/// - a resume against the destination the copy left reads 0 source bytes
///   and changes nothing: every seat is held by a committed destination
///   row, so the destination answers `Reuse` and the ledger is never asked
///   (R25's committed-row reading, OI-1003-Q40).
///
/// Every prefix is reached, from no row to all of them. Then, with every
/// row lost, a third party removes one output: the resume reads that seat's
/// bytes once and nothing else, and every byte it leaves is the source's.
#[test]
#[allow(clippy::too_many_lines)]
fn every_power_loss_state_of_a_relaxed_ledger_costs_at_most_its_lost_seats() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("relaxed-ledger");
    populate(&scratch.source());
    let state = scratch.base.join("source-state");
    let wal = state.join("transfer.sqlite-wal");
    let database = state.join("transfer.sqlite");

    // The store's creation: schema and authority, committed before any row.
    let created = Store::open(&state).unwrap();
    let authority = created.authority().unwrap();
    drop(created);
    let synced = fs::metadata(&wal).unwrap().len();

    // Two traced copies, the second of two new seats, so the ledger's rows
    // commit in at least two groups whatever the committer's timing.
    let before = bulkload_agent::counters::Counters::snapshot();
    let (_, events) = traced_copy(&scratch);
    let mut commits = ledger_commits(&events, node(&state));
    fs::write(scratch.source().join("f"), noise(101, 7_000)).unwrap();
    fs::write(scratch.source().join("nested/g"), noise(103, 11_000)).unwrap();
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();
    // One reuse row per regular file of the corpus, the two new seats in.
    let files = expected(&scratch.source()).files.len() as u64;
    let (_, events) = traced_copy(&scratch);
    let second = ledger_commits(&events, node(&state));
    assert!(!commits.is_empty() && !second.is_empty());
    commits.extend(second);
    let counted = bulkload_agent::counters::Counters::snapshot().since(before);
    assert!(
        counted.get(bulkload_agent::counters::Counter::SourceLedgerRelaxedCommits)
            >= commits.len() as u64,
        "every row commit of the copy was relaxed"
    );
    let keys: Vec<Vec<u8>> = commits.iter().flatten().cloned().collect();
    assert_eq!(
        keys.len() as u64,
        files,
        "one row per regular file of the corpus"
    );

    let pristine = fs::read(&wal).unwrap();
    let main_before = fs::read(&database).unwrap();
    // A WAL is a 32-byte header and frames of 24 bytes plus one page.
    let page = u64::from(u32::from_be_bytes(pristine[8..12].try_into().unwrap()));
    let frame = 24 + page;
    let length = pristine.len() as u64;
    assert!(
        synced > 32 && (synced - 32).is_multiple_of(frame),
        "{synced} {frame}"
    );
    assert!(
        length > synced && (length - 32).is_multiple_of(frame),
        "{length}"
    );

    let want = tree(&scratch.source());
    let mut reached = BTreeSet::new();
    let mut last = 0;
    let mut cut = synced;
    while cut <= length {
        lose_ledger_tail(&state, &pristine, cut);
        let recovered = Store::open(&state).unwrap();
        assert_eq!(
            recovered.authority().unwrap(),
            authority,
            "cut {cut}: the authority survives every loss of rows"
        );
        let held: Vec<bool> = keys
            .iter()
            .map(|key| recovered.capture(key).unwrap().is_some())
            .collect();
        drop(recovered);
        // A prefix of the commits, whole: no commit is half there, and no
        // later commit survives an earlier one's loss.
        let survived = (0..=commits.len())
            .find(|prefix| {
                let rows: usize = commits[..*prefix].iter().map(Vec::len).sum();
                held.iter()
                    .enumerate()
                    .all(|(row, here)| *here == (row < rows))
            })
            .unwrap_or_else(|| panic!("cut {cut}: not a prefix of the row commits: {held:?}"));
        assert!(survived >= last, "cut {cut}: a longer WAL lost more rows");
        last = survived;
        reached.insert(survived);

        let resumed = run_copy(&scratch);
        assert!(
            resumed.refusals.is_empty(),
            "cut {cut}: {:?}",
            resumed.refusals
        );
        assert_eq!(
            resumed.source_bytes_read,
            0,
            "cut {cut}: {survived} of {} row commits survived, and no held seat is read",
            commits.len()
        );
        assert_eq!(resumed.reused, files, "cut {cut}");
        assert_eq!(tree(&scratch.destination()), want, "cut {cut}");
        cut += frame;
    }
    assert_eq!(
        reached,
        (0..=commits.len()).collect::<BTreeSet<_>>(),
        "every prefix of the row commits is a power-loss state"
    );
    assert_eq!(
        fs::read(&database).unwrap(),
        main_before,
        "no checkpoint moved a row out of the WAL, so the cuts are the whole loss"
    );

    // Every row lost, and a third party removes one output: its seat is
    // read once, and only it.
    lose_ledger_tail(&state, &pristine, synced);
    fs::remove_file(scratch.destination().join("b")).unwrap();
    let resumed = run_copy(&scratch);
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.source_bytes_read, 40_000, "seat b, once");
    assert_eq!(resumed.reused, files - 1, "every seat but b");
    assert_eq!(tree(&scratch.destination()), want);
    assert_eq!(Store::open(&state).unwrap().authority().unwrap(), authority);
    let settled = run_copy(&scratch);
    assert_eq!(settled.source_bytes_read, 0);
    assert_eq!(settled.reused, files);
}

// ---------------------------------------------------------------------------
// Directory batching (OI-1003-Q143 item 2, #217 review)
// ---------------------------------------------------------------------------

/// The level-2 directories of [`populate`] and their two parents, by node.
struct Level {
    members: Vec<NodeId>,
    parents: [NodeId; 2],
    all: Vec<NodeId>,
}

fn level(scratch: &Scratch) -> Level {
    let at = |rel: &str| node(&scratch.destination().join(rel));
    Level {
        members: vec![
            at("nested/deeper"),
            at("nested/deeper2"),
            at("nested2/inner"),
        ],
        parents: [at("nested"), at("nested2")],
        all: [
            "nested",
            "nested2",
            "nested/deeper",
            "nested/deeper2",
            "nested2/inner",
        ]
        .into_iter()
        .map(at)
        .collect(),
    }
}

/// The trace index of the one commit binding every member of the level.
fn level_commit(events: &[Event], level: &Level) -> usize {
    let found: Vec<usize> = (0..events.len())
        .filter(|at| {
            matches!(&events[*at], Event::Commit { records, .. } if level.members.iter().all(|member| records.iter().any(|record| matches!(record, CommitRecord::DirectoryCreated { node: Some(node), .. } if node == member))))
        })
        .collect();
    assert_eq!(found.len(), 1, "one commit binds the whole level");
    found[0]
}

/// Move the event at `from` to just before `to`.
fn moved(events: &[Event], from: usize, to: usize) -> Vec<Event> {
    let mut out = events.to_vec();
    let event = out.remove(from);
    let to = if from < to { to - 1 } else { to };
    out.insert(to, event);
    out
}

fn violations(image: &Image, want: &Expected, events: &[Event]) -> Vec<String> {
    let report = check_view(image, events, &options(), |view, info| {
        invariant(want, events, view, info)
    })
    .unwrap();
    report
        .violations
        .iter()
        .map(|violation| violation.message.clone())
        .collect()
}

/// The batched directory proofs have teeth: one strict-mode copy of
/// [`populate`] (strict, so no batched group's device seal persists the
/// level's names behind the batch's back), then each of the batch's orders
/// undone in its trace, and each must fail:
///
/// - the level's commit moved before its parents' seals: a record names an
///   inode a power loss can still lose;
/// - the level's renames moved before its commit: a directory at its final
///   name with no record;
/// - the second parent's seal after the renames dropped: an output
///   committed inside a directory whose name is not durable;
/// - the finish's one commit moved before the last directory's seal: a
///   completed directory without its final mode.
#[test]
fn the_batched_directory_proofs_have_teeth() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Strict);
    let scratch = Scratch::new("batch-teeth");
    populate(&scratch.source());
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    set_durability(Durability::Group);
    let level = level(&scratch);
    assert!(
        violations(&image, &want, &events).is_empty(),
        "the unmutated trace passes"
    );
    let commit = level_commit(&events, &level);
    let last_mkdir = (0..commit)
        .rev()
        .find(
            |at| matches!(&events[*at], Event::Mkdir { node, .. } if level.members.contains(node)),
        )
        .unwrap();
    // The level's commit before its parents' seals.
    let early = moved(&events, commit, last_mkdir + 1);
    let found = violations(&image, &want, &early);
    assert!(
        found
            .iter()
            .any(|message| message.starts_with("recorded directory inode")),
        "a level committed before its parents' seals: {found:?}"
    );
    // The level's renames before its commit.
    let renames: Vec<usize> = (commit..events.len())
        .filter(
            |at| matches!(&events[*at], Event::Rename { node, .. } if level.members.contains(node)),
        )
        .collect();
    assert_eq!(renames.len(), level.members.len());
    let mut renamed_first = events.clone();
    for (shift, at) in renames.iter().enumerate() {
        renamed_first = moved(&renamed_first, *at, commit + shift);
    }
    let found = violations(&image, &want, &renamed_first);
    assert!(
        found
            .iter()
            .any(|message| message.contains("owned by no record")),
        "renames before the level's commit: {found:?}"
    );
    // The second parent's seal after the renames dropped.
    let last_rename = *renames.last().unwrap();
    let second_seal = (last_rename..events.len())
        .find(|at| matches!(&events[*at], Event::Sync { node, .. } if *node == level.parents[1]))
        .unwrap();
    let mut unsealed = events.clone();
    unsealed.remove(second_seal);
    let found = violations(&image, &want, &unsealed);
    assert!(
        found
            .iter()
            .any(|message| message.contains("nested2/inner/g")),
        "the second parent's seal dropped: {found:?}"
    );
    // The finish's commit before the last directory's seal.
    let finish = (0..events.len())
        .find(|at| {
            matches!(&events[*at], Event::Commit { records, .. } if records.iter().filter(|record| matches!(record, CommitRecord::DirectoryComplete { .. })).count() == level.all.len())
        })
        .expect("one commit completes every directory");
    let last_seal = (0..finish)
        .rev()
        .find(|at| matches!(&events[*at], Event::Sync { node, .. } if level.all.contains(node)))
        .unwrap();
    let completed_first = moved(&events, finish, last_seal);
    let found = violations(&image, &want, &completed_first);
    assert!(
        found
            .iter()
            .any(|message| message.starts_with("completed directory")),
        "the finish committed before the last seal: {found:?}"
    );
}

/// The directory segment alone, exhaustively (#217 review, finding 9): a
/// copy of a tree of directories only, two levels of two siblings each,
/// whose every crash point is explored with every subset of its optional
/// operations (no bounded point), in group mode.
#[test]
fn every_power_loss_state_of_a_directory_batch_is_explored() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("directories-only");
    for directory in ["p", "q", "p/r", "q/s"] {
        fs::create_dir(scratch.source().join(directory)).unwrap();
        fs::set_permissions(
            scratch.source().join(directory),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    let report = check_view(
        &image,
        &events,
        &Options {
            ignore_foreign: true,
            accept_bounded: false,
            exhaustive_limit: 16,
            ..Options::default()
        },
        |view, info| invariant(&want, &events, view, info),
    )
    .unwrap();
    eprintln!("{}", report.summary(&events));
    assert!(report.bounded.is_empty(), "{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
}

/// A whole strict copy, explored exhaustively (S1 throughput review,
/// 2026-10-09, findings 3 and 4): every crash point with every subset of
/// its optional operations. [`populate`]'s strict copy is bounded (30 of
/// its crash points here; the pre-batching fixture's was bounded at 12 to
/// 14 of 60 already, at `dd33936`), so this small tree keeps an exhaustive
/// strict-mode proof of a copy: a file and a symlink in the root, two
/// levels of two sibling directories (one batch, a level with two
/// parents), and a file under a second-level member, so the batch's rule
/// that a member's name is sealed before anything inside it commits is
/// proven with a file present.
#[test]
fn every_power_loss_state_of_a_small_strict_copy_is_explored() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Strict);
    let scratch = Scratch::new("strict-small");
    for directory in ["p", "q", "p/r", "q/s"] {
        fs::create_dir(scratch.source().join(directory)).unwrap();
    }
    fs::write(scratch.source().join("a"), noise(19, 300)).unwrap();
    fs::write(scratch.source().join("q/s/f"), noise(17, 600)).unwrap();
    std::os::unix::fs::symlink("a", scratch.source().join("link")).unwrap();
    for directory in ["p", "q", "p/r", "q/s"] {
        fs::set_permissions(
            scratch.source().join(directory),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();
    let want = expected(&scratch.source());
    let (image, events) = traced_copy(&scratch);
    set_durability(Durability::Group);
    assert!(
        events.iter().any(|event| matches!(event, Event::Commit { records, .. }
            if records.iter().any(|record| matches!(record, CommitRecord::Output { rel_path } if rel_path == b"q/s/f")))),
        "the file under the second level commits"
    );
    let report = check_view(
        &image,
        &events,
        &Options {
            ignore_foreign: true,
            accept_bounded: false,
            exhaustive_limit: 16,
            ..Options::default()
        },
        |view, info| invariant(&want, &events, view, info),
    )
    .unwrap();
    eprintln!("{}", report.summary(&events));
    assert!(report.bounded.is_empty(), "{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
    // Teeth: without the seal of `q` after `q/s`'s rename, `q/s/f` commits
    // inside a directory whose name a power loss can still take.
    let (parent, member) = (
        node(&scratch.destination().join("q")),
        node(&scratch.destination().join("q/s")),
    );
    let renamed = events
        .iter()
        .position(|event| matches!(event, Event::Rename { node, .. } if *node == member))
        .unwrap();
    let seal = (renamed..events.len())
        .find(|at| matches!(&events[*at], Event::Sync { node, .. } if *node == parent))
        .unwrap();
    let mut unsealed = events;
    unsealed.remove(seal);
    let found = violations(&image, &want, &unsealed);
    assert!(
        found.iter().any(|message| message.contains("q/s/f")),
        "the member's parent seal dropped: {found:?}"
    );
}

// ---------------------------------------------------------------------------
// Receive workers and write-back kicks (OI-1003-Q143 items 1a and 3)
// ---------------------------------------------------------------------------

/// **Every write precedes its file's seal** (OI-1003-Q143 item 3: `End`
/// waits for every chunk job of its entry): the writes of `file`, published
/// at `rel`, all come before the seal its output commit relies on, which is
/// the last durable sync of the file itself (a kick is none) or the last
/// device seal before that commit. Returns how many writes it checked.
fn writes_precede_seal(events: &[Event], file: NodeId, rel: &[u8]) -> Result<usize, String> {
    let commit = events
        .iter()
        .position(|event| {
            matches!(event, Event::Commit { records, .. }
                if records.iter().any(|record| matches!(record, CommitRecord::Output { rel_path } if rel_path == rel)))
        })
        .ok_or("the output never commits")?;
    let seal = (0..commit)
        .rev()
        .find(|at| match &events[*at] {
            Event::Sync { node, kind } => {
                matches!(kind, SyncKind::FsSync)
                    || (*node == file && !matches!(kind, SyncKind::Kick))
            }
            _ => false,
        })
        .ok_or("the file is never sealed before its commit")?;
    let mut checked = 0;
    for (at, event) in events.iter().enumerate() {
        if matches!(event, Event::Write { node, .. } if *node == file) {
            if at > seal {
                return Err(format!(
                    "write at event {at} comes after the seal at event {seal} its commit at event {commit} relies on"
                ));
            }
            checked += 1;
        }
    }
    Ok(checked)
}

/// S1 throughput review (2026-10-09), finding 6: a power-loss trace with
/// chunks written by two receive workers and write-back kicked while the
/// data streams. A file of several chunks (and a small one) is copied in
/// group mode with two workers, each waiting 300 ms before its job (so
/// `End` arrives while the file's jobs are still running, and only its
/// wait keeps their writes ahead of the seal), and a 64 KiB kick interval.
/// The ordering is checked first, before the copy's outcome: every write
/// precedes the file's seal. Then the trace holds the chunk writes and, on
/// Linux, the kicks, and every power-loss state is consistent. Last, the
/// trace with one chunk write moved after the file's output commit must
/// fail both the ordering check and the power-loss check.
#[test]
fn every_power_loss_state_of_a_worker_written_copy_is_consistent() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    set_durability(Durability::Group);
    let scratch = Scratch::new("workers");
    fs::write(scratch.source().join("large"), noise(23, 400_000)).unwrap();
    fs::write(scratch.source().join("small"), noise(29, 2_000)).unwrap();
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();
    let want = expected(&scratch.source());
    let destination = fs::canonicalize(scratch.destination()).unwrap();
    bulkload_agent::transfer::set_recv_workers(&destination, Some(2));
    bulkload_agent::transfer::set_writeback_kick_bytes(64 * 1024);
    bulkload_agent::transfer::set_recv_worker_delay(300);
    let before = bulkload_agent::transfer::TransferTiming::snapshot();
    let image = Image::scan(&scratch.destination()).unwrap();
    let recorder = Recorder::new();
    let copied = {
        let _process = recorder.attach_process();
        copy(
            &scratch.source(),
            &scratch.destination(),
            &scratch.base.join("source-state"),
            &scratch.base.join("destination-state"),
        )
    };
    let events = recorder.take();
    let timing = bulkload_agent::transfer::TransferTiming::snapshot().since(before);
    bulkload_agent::transfer::set_recv_worker_delay(0);
    bulkload_agent::transfer::set_writeback_kick_bytes(0);
    bulkload_agent::transfer::set_recv_workers(&destination, None);
    let large = node(&scratch.destination().join("large"));
    let writes = writes_precede_seal(&events, large, b"large").unwrap();
    let stats = copied.unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert!(
        timing.recv_worker_place_ns > 0,
        "the receive workers wrote: {}",
        timing.render()
    );
    assert!(writes >= 4, "{writes} chunk writes");
    let kicks = events
        .iter()
        .filter(
            |event| matches!(event, Event::Sync { node, kind: SyncKind::Kick } if *node == large),
        )
        .count();
    if cfg!(target_os = "linux") {
        assert!(kicks >= 4, "{kicks} kicks while the data streamed");
    }
    let report = check_view(&image, &events, &options(), |view, info| {
        invariant(&want, &events, view, info)
    })
    .unwrap();
    eprintln!("{}", report.summary(&events));
    assert!(report.passed(), "{}", report.summary(&events));
    // Teeth: the file's last chunk write moved after its output commit.
    let last = events
        .iter()
        .rposition(|event| matches!(event, Event::Write { node, .. } if *node == large))
        .unwrap();
    let commit = events
        .iter()
        .position(|event| {
            matches!(event, Event::Commit { records, .. }
                if records.iter().any(|record| matches!(record, CommitRecord::Output { rel_path } if rel_path == b"large")))
        })
        .unwrap();
    let late = moved(&events, last, commit + 1);
    assert!(
        writes_precede_seal(&late, large, b"large").is_err(),
        "a write after the seal passes the ordering check"
    );
    let found = violations(&image, &want, &late);
    assert!(
        found.iter().any(|message| message.contains("large")),
        "a write after the file's commit passes the power-loss check: {found:?}"
    );
}
