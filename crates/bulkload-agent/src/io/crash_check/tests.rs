//! Proofs of the crash-state checker on toy protocols (R-N88).
//!
//! The protocol under test publishes a new file `data` beside a durable file
//! `keep`: write a temporary, sync it, rename it into place, sync the
//! directory. The invariant is the one the engine relies on: `keep` is intact,
//! `data` is either absent or holds exactly the new bytes, and once the
//! protocol has returned, `data` holds them. The correct protocol must pass;
//! dropping the file sync or the directory sync must fail. Hand-written traces
//! prove the model on every platform; under `io-trace` the same protocols run
//! through the real `sys` wrappers and are checked from their recorded trace.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::Path;

use super::{
    check, check_view, BarrierScope, Entry, Image, Options, Report, StateInfo, View, MAX_EXHAUSTIVE,
};
use crate::io::trace::{Event, SyncKind};
use crate::io::NodeId;

const NEW: &[u8] = b"new-bytes";
const KEEP: &[u8] = b"keep-bytes";

const fn n(ino: u64) -> NodeId {
    NodeId { dev: 1, ino }
}

const ROOT: NodeId = n(1);
const TMP: NodeId = n(2);
const KEEP_NODE: NodeId = n(3);
const LEDGER: NodeId = n(4);

fn initial() -> Image {
    Image::empty(ROOT)
        .with_file(b"keep", KEEP_NODE, KEEP)
        .with_file(b"ledger", LEDGER, b"")
}

/// `keep` intact; `data` absent or exactly `NEW`; `NEW` once complete.
fn publish_invariant(root: &Path, info: &StateInfo) -> Result<(), String> {
    let keep = std::fs::read(root.join("keep")).map_err(|error| format!("keep: {error}"))?;
    if keep != KEEP {
        return Err(format!("keep changed to {keep:?}"));
    }
    match std::fs::read(root.join("data")) {
        Ok(bytes) if bytes == NEW => Ok(()),
        Ok(bytes) => Err(format!(
            "data is published with the wrong bytes {:?}",
            String::from_utf8_lossy(&bytes)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if info.complete {
                Err("the protocol returned but data is not durable".to_owned())
            } else {
                Ok(())
            }
        }
        Err(error) => Err(format!("data: {error}")),
    }
}

fn write(node: NodeId, data: &[u8]) -> Event {
    Event::Write {
        node,
        offset: 0,
        data: data.to_vec(),
        digest: *blake3::hash(data).as_bytes(),
    }
}

/// create tmp, write, [file sync], rename tmp -> data, [dir sync], [ledger
/// full flush].
fn publish_trace(
    file_sync: Option<SyncKind>,
    dir_sync: Option<SyncKind>,
    ledger: bool,
) -> Vec<Event> {
    let mut events = vec![
        Event::Create {
            dir: Some(ROOT),
            name: Some(b"tmp".to_vec()),
            node: TMP,
            mode: 0o600,
        },
        write(TMP, NEW),
    ];
    if let Some(kind) = file_sync {
        events.push(Event::Sync { node: TMP, kind });
    }
    events.push(Event::Rename {
        node: TMP,
        from_dir: ROOT,
        from: b"tmp".to_vec(),
        to_dir: ROOT,
        to: b"data".to_vec(),
    });
    if let Some(kind) = dir_sync {
        events.push(Event::Sync { node: ROOT, kind });
    }
    if ledger {
        events.push(Event::Sync {
            node: LEDGER,
            kind: SyncKind::FullFlush,
        });
    }
    events
}

fn run(events: &[Event], options: &Options) -> Report {
    let report = check(&initial(), events, options, publish_invariant).unwrap();
    eprintln!("{}", report.summary(events));
    report
}

#[test]
fn correct_publish_passes_under_every_durable_sync_pair() {
    for (file, dir) in [
        (SyncKind::Fsync, SyncKind::Fsync),
        (SyncKind::DataSync, SyncKind::Fsync),
        (SyncKind::FullFlush, SyncKind::FullFlush),
    ] {
        for scope in [BarrierScope::Object, BarrierScope::Device] {
            let options = Options {
                barrier_scope: scope,
                ..Options::default()
            };
            let report = run(&publish_trace(Some(file), Some(dir), false), &options);
            assert!(report.passed(), "{file:?}/{dir:?} must pass");
            assert!(
                report.bounded.is_empty(),
                "toy traces are checked exhaustively"
            );
            assert!(
                report.states > report.crash_points,
                "some crash points have several states"
            );
        }
    }
}

#[test]
fn publish_without_the_file_sync_fails() {
    for dir in [SyncKind::Fsync, SyncKind::FullFlush] {
        let events = publish_trace(None, Some(dir), false);
        let report = run(&events, &Options::default());
        assert!(!report.passed(), "a rename can persist ahead of the data");
        assert!(report
            .violations
            .iter()
            .any(|violation| violation.message.contains("wrong bytes")
                && violation.lost.contains(&1)));
    }
}

#[test]
fn publish_without_the_dir_sync_fails() {
    for file in [SyncKind::Fsync, SyncKind::DataSync, SyncKind::FullFlush] {
        let report = run(&publish_trace(Some(file), None, false), &Options::default());
        assert!(
            !report.passed(),
            "the rename is not durable without a dir sync"
        );
        assert!(report
            .violations
            .iter()
            .all(|violation| violation.message.contains("not durable")));
    }
}

/// The W3 group protocol on Darwin: barrier the file, rename, barrier the
/// directory, and let the group's `SQLite` `F_FULLFSYNC` (here: the ledger)
/// drain the drive. Under the default device-wide barrier model (R-N103,
/// Apple `fcntl(2)`) the rename cannot reach stable media ahead of the file's
/// barriered bytes, and the drain makes both durable: the protocol passes.
const fn strict() -> Options {
    Options {
        barrier_scope: BarrierScope::Object,
        exhaustive_limit: 12,
        sector: None,
        accept_bounded: false,
        commit_drains: cfg!(target_vendor = "apple"),
        ignore_foreign: false,
    }
}

#[test]
fn barrier_group_protocol_passes_under_the_default_device_wide_model() {
    assert_eq!(Options::default().barrier_scope, BarrierScope::Device);
    let events = publish_trace(Some(SyncKind::Barrier), Some(SyncKind::Barrier), true);
    let report = run(&events, &Options::default());
    assert!(report.passed(), "{}", report.summary(&events));
    assert!(report.bounded.is_empty());

    // Without the drain nothing is durable: barriers order, they do not flush.
    let undrained = publish_trace(Some(SyncKind::Barrier), Some(SyncKind::Barrier), false);
    let report = run(&undrained, &Options::default());
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.message.contains("not durable")));
    assert!(report
        .violations
        .iter()
        .all(|violation| !violation.message.contains("wrong bytes")));
}

/// The same protocol under the strict per-object barrier option: the
/// directory barrier does not order the file's bytes, so a crash before the
/// drain can leave `data` published without them. The drain still makes the
/// completed protocol durable.
#[test]
fn barrier_group_protocol_fails_under_the_strict_per_object_model() {
    let events = publish_trace(Some(SyncKind::Barrier), Some(SyncKind::Barrier), true);
    let report = run(&events, &strict());
    assert!(!report.passed());
    assert!(report
        .violations
        .iter()
        .all(|violation| violation.message.contains("wrong bytes") && !violation.lost.is_empty()));
}

#[test]
fn kicks_are_not_durable() {
    let report = run(
        &publish_trace(Some(SyncKind::Kick), Some(SyncKind::Fsync), false),
        &Options::default(),
    );
    assert!(!report.passed());
}

#[test]
fn torn_writes_are_modelled_and_the_correct_protocol_still_passes() {
    let options = Options {
        sector: Some(3),
        ..Options::default()
    };
    let good = run(
        &publish_trace(Some(SyncKind::Fsync), Some(SyncKind::Fsync), false),
        &options,
    );
    assert!(good.passed());
    assert_eq!(
        good.ops,
        3 + 4,
        "the 9-byte write splits into three sectors"
    );
    let torn = run(&publish_trace(None, Some(SyncKind::Fsync), false), &options);
    assert!(torn
        .violations
        .iter()
        .any(|violation| violation.message.contains("wrong bytes")));
}

#[test]
fn a_new_directory_is_a_dependency_of_its_entries() {
    let dir = n(10);
    let events = vec![
        Event::Mkdir {
            dir: ROOT,
            name: b"d".to_vec(),
            node: dir,
            mode: 0o755,
        },
        Event::Create {
            dir: Some(dir),
            name: Some(b"x".to_vec()),
            node: n(11),
            mode: 0o644,
        },
        Event::Sync {
            node: dir,
            kind: SyncKind::Fsync,
        },
    ];
    // The root is never synced, so the mkdir (event 0) stays optional while
    // the fsync of `d` makes the create (event 1) durable. Without the parent
    // edge the checker would build states where the create persisted and the
    // mkdir did not; the materializer hides that (the orphaned directory is
    // unreachable), so the check is on the persisted set itself.
    let mut child_only = 0_usize;
    let mut states_with_child = 0_usize;
    let report = check(&initial(), &events, &Options::default(), |_, info| {
        if info.persisted.contains(&1) {
            states_with_child += 1;
            if !info.persisted.contains(&0) {
                child_only += 1;
            }
        }
        Ok(())
    })
    .unwrap();
    assert!(report.passed());
    assert!(states_with_child > 0, "the durable create must appear");
    assert_eq!(child_only, 0, "a create persisted without its parent mkdir");
}

#[test]
fn a_bounded_crash_point_is_logged_not_silent() {
    let mut events = Vec::new();
    for index in 0..6_u64 {
        events.push(Event::Create {
            dir: Some(ROOT),
            name: Some(format!("f{index}").into_bytes()),
            node: n(100 + index),
            mode: 0o644,
        });
        events.push(write(n(100 + index), b"x"));
    }
    let options = Options {
        exhaustive_limit: 4,
        ..Options::default()
    };
    // Count the files present in each complete state: the bounded family
    // must include drop-one states (11 of the 12 mutations persisted).
    let mut drop_one_seen = false;
    let report = check(&initial(), &events, &options, |_, info| {
        if info.complete && info.persisted.len() == 11 {
            drop_one_seen = true;
        }
        Ok(())
    })
    .unwrap();
    let summary = report.summary(&events);
    eprintln!("{summary}");
    assert!(!report.bounded.is_empty());
    assert!(!report.exhaustive());
    assert!(
        drop_one_seen,
        "the bound must still try every drop-one state"
    );
    let last = report.bounded.last().unwrap();
    assert_eq!(last.crash_point, 12);
    assert_eq!(last.optional, 12);
    assert_eq!(last.skipped, (1_u128 << 12) - last.explored as u128);
    // Prefix, required, 12 drop-one and 12 keep-one states, deduplicated.
    assert!(last.explored <= 2 + 2 * 12);
    assert!(summary.contains("bound: crash point 12 has 12 optional mutations"));
    assert!(summary.contains("exhaustive=false"));

    // Review probe P2: with no violations a bounded run is still not a pass
    // unless the caller opts in.
    assert!(report.violations.is_empty());
    assert!(
        !report.passed(),
        "a bounded run must not report passed by default"
    );
    let accepted = check(
        &initial(),
        &events,
        &Options {
            accept_bounded: true,
            ..options
        },
        |_, _| Ok(()),
    )
    .unwrap();
    assert!(accepted.passed());
    assert!(!accepted.exhaustive());
}

/// `fchmod` is metadata: `fdatasync` does not make it durable, `fsync` does.
/// A link made durable by a directory sync survives; an unsynced unlink may
/// be lost.
#[test]
fn mode_link_and_unlink_follow_the_model() {
    let events = vec![
        Event::Link {
            node: KEEP_NODE,
            dir: ROOT,
            name: b"alias".to_vec(),
        },
        Event::Sync {
            node: ROOT,
            kind: SyncKind::Fsync,
        },
        Event::SetMode {
            node: KEEP_NODE,
            mode: 0o600,
        },
        Event::Sync {
            node: KEEP_NODE,
            kind: SyncKind::DataSync,
        },
        Event::Unlink {
            dir: ROOT,
            name: b"keep".to_vec(),
        },
    ];
    let mut final_modes = std::collections::BTreeSet::new();
    let mut final_keep = std::collections::BTreeSet::new();
    let report = check(&initial(), &events, &Options::default(), |root, info| {
        assert!(info.crash_point <= info.ops);
        assert_eq!(info.complete, info.crash_point == info.ops);
        if info.crash_point >= 2 && !root.join("alias").exists() {
            return Err("a synced link was lost".to_owned());
        }
        if info.complete {
            use std::os::unix::fs::PermissionsExt as _;
            let meta = std::fs::metadata(root.join("alias")).map_err(|error| error.to_string())?;
            final_modes.insert(meta.permissions().mode() & 0o777);
            final_keep.insert(root.join("keep").exists());
        }
        Ok(())
    })
    .unwrap();
    assert!(report.passed(), "{}", report.summary(&events));
    assert_eq!(
        final_modes,
        [0o600, 0o644].into(),
        "fdatasync leaves the mode volatile"
    );
    assert_eq!(
        final_keep,
        [false, true].into(),
        "the unsynced unlink may be lost"
    );

    let durable_mode = vec![
        Event::SetMode {
            node: KEEP_NODE,
            mode: 0o600,
        },
        Event::Sync {
            node: KEEP_NODE,
            kind: SyncKind::Fsync,
        },
    ];
    let report = check(
        &initial(),
        &durable_mode,
        &Options::default(),
        |root, info| {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(root.join("keep"))
                .map_err(|error| error.to_string())?
                .permissions()
                .mode();
            if info.complete && mode & 0o777 != 0o600 {
                return Err(format!("mode {mode:o} after fsync"));
            }
            Ok(())
        },
    )
    .unwrap();
    assert!(report.passed());
}

/// A scanned tree with an empty trace materializes back to itself.
#[test]
fn a_scanned_tree_materializes_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
    std::fs::write(dir.path().join("top"), b"top").unwrap();
    std::fs::write(dir.path().join("sub/deeper/leaf"), b"leaf").unwrap();
    let image = Image::scan(dir.path()).unwrap();
    let report = check(&image, &[], &Options::default(), |root, _| {
        let top = std::fs::read(root.join("top")).map_err(|error| error.to_string())?;
        let leaf =
            std::fs::read(root.join("sub/deeper/leaf")).map_err(|error| error.to_string())?;
        if top == b"top" && leaf == b"leaf" {
            Ok(())
        } else {
            Err("the tree changed".to_owned())
        }
    })
    .unwrap();
    assert!(report.passed());
    assert_eq!((report.crash_points, report.states), (1, 1));

    // Symlinks are modelled: scanned, materialized and read back literally.
    std::os::unix::fs::symlink("top", dir.path().join("link")).unwrap();
    let image = Image::scan(dir.path()).unwrap();
    let report = check(
        &image,
        &[],
        &Options::default(),
        |root, _| match std::fs::read_link(root.join("link")) {
            Ok(target) if target == Path::new("top") => Ok(()),
            other => Err(format!("link read back as {other:?}")),
        },
    )
    .unwrap();
    assert!(report.passed());
    let fifo = dir.path().join("fifo");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    assert!(
        Image::scan(dir.path()).is_err(),
        "special files are not modelled"
    );
}

/// Barrier-seal a file, rename it into place and barrier the directory, then
/// let a store commit on the same drive return (the W3 group protocol).
fn group_commit_trace(store: NodeId) -> Vec<Event> {
    use crate::io::trace::CommitRecord;
    let mut events = publish_trace(Some(SyncKind::Barrier), Some(SyncKind::Barrier), false);
    // Every record kind rides along; the checker reads none of them itself.
    events.push(Event::Commit {
        store,
        records: vec![
            CommitRecord::Output {
                rel_path: b"data".to_vec(),
            },
            CommitRecord::Capture { key: b"k".to_vec() },
            CommitRecord::DirectoryCreated {
                key: b"d".to_vec(),
                node: None,
                mode: 0o755,
            },
            CommitRecord::DirectoryComplete { key: b"d".to_vec() },
            CommitRecord::RootSealed,
        ],
    });
    events
}

/// A committed output record must name a durable file with the new bytes.
fn committed_invariant(view: View<'_>, info: &StateInfo) -> Result<(), String> {
    if info.commits.is_empty() {
        return Ok(());
    }
    match view.get(b"data") {
        Some(Entry::File { data, .. }) if data == NEW => Ok(()),
        other => Err(format!("committed record names {other:?}")),
    }
}

/// Model rule 5: with `commit_drains` (Darwin, `fullfsync=ON`) a store
/// commit drains its drive, so barrier-sealed entries on that drive are
/// durable once it returns; without the drain, or on another drive, they are
/// not.
#[test]
fn a_store_commit_drains_only_its_own_drive() {
    let draining = Options {
        commit_drains: true,
        ..Options::default()
    };
    let report = check_view(
        &initial(),
        &group_commit_trace(LEDGER),
        &draining,
        committed_invariant,
    )
    .unwrap();
    assert!(
        report.passed(),
        "{}",
        report.summary(&group_commit_trace(LEDGER))
    );

    let plain = Options {
        commit_drains: false,
        ..Options::default()
    };
    let report = check_view(
        &initial(),
        &group_commit_trace(LEDGER),
        &plain,
        committed_invariant,
    )
    .unwrap();
    assert!(
        !report.passed(),
        "a commit that does not drain leaves the barriers volatile"
    );

    let elsewhere = NodeId { dev: 2, ino: 99 };
    let report = check_view(
        &initial(),
        &group_commit_trace(elsewhere),
        &draining,
        committed_invariant,
    )
    .unwrap();
    assert!(
        !report.passed(),
        "a drain of another drive does not make this one durable"
    );
}

/// `StateInfo::commits` lists exactly the commits completed before the crash.
#[test]
fn commits_are_reported_once_they_return() {
    let events = group_commit_trace(LEDGER);
    let commit_event = events.len() - 1;
    let mut seen = BTreeSet::new();
    check_view(&initial(), &events, &Options::default(), |_, info| {
        assert_eq!(!info.commits.is_empty(), info.complete);
        if info.complete {
            assert_eq!(info.commits, vec![commit_event]);
        }
        seen.insert(info.commits.len());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [0, 1].into());
}

/// `check_view` and `check` agree on every toy protocol.
#[test]
fn the_in_memory_view_agrees_with_materialized_states() {
    let traces = [
        publish_trace(Some(SyncKind::Fsync), Some(SyncKind::Fsync), false),
        publish_trace(None, Some(SyncKind::Fsync), false),
        publish_trace(Some(SyncKind::Fsync), None, false),
        publish_trace(Some(SyncKind::Barrier), Some(SyncKind::Barrier), true),
    ];
    for events in traces {
        for options in [Options::default(), strict()] {
            let on_disk = check(&initial(), &events, &options, publish_invariant).unwrap();
            let in_memory = check_view(&initial(), &events, &options, |view, info| {
                let keep = match view.get(b"keep") {
                    Some(Entry::File { data, .. }) => data,
                    other => return Err(format!("keep: {other:?}")),
                };
                if keep != KEEP {
                    return Err("keep changed".to_owned());
                }
                match view.get(b"data") {
                    Some(Entry::File { data, .. }) if data == NEW => Ok(()),
                    Some(Entry::File { .. }) => Err("wrong bytes".to_owned()),
                    None if info.complete => Err("not durable".to_owned()),
                    None => Ok(()),
                    Some(other) => Err(format!("data is {other:?}")),
                }
            })
            .unwrap();
            assert_eq!(on_disk.states, in_memory.states);
            assert_eq!(on_disk.violations.len(), in_memory.violations.len());
        }
    }
}

/// A mutation of a node outside the image is refused by default, and with
/// `ignore_foreign` dropped and counted; a foreign full flush still drains.
#[test]
fn foreign_nodes_are_refused_or_counted_never_silent() {
    let foreign = NodeId { dev: 1, ino: 500 };
    let mut events = vec![write(foreign, b"store bytes")];
    events.extend(publish_trace(
        Some(SyncKind::Barrier),
        Some(SyncKind::Barrier),
        false,
    ));
    events.push(Event::Sync {
        node: foreign,
        kind: SyncKind::FullFlush,
    });
    let error = check_view(&initial(), &events, &Options::default(), |_, _| Ok(())).unwrap_err();
    assert!(error.to_string().contains("outside the image"), "{error}");

    let options = Options {
        ignore_foreign: true,
        ..Options::default()
    };
    let report = check_view(&initial(), &events, &options, |view, info| {
        if info.complete
            && view.get(b"data")
                != Some(Entry::File {
                    data: NEW,
                    mode: 0o600,
                })
        {
            return Err("the foreign full flush did not drain the drive".to_owned());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(report.foreign, 1);
    assert!(report.summary(&events).contains("foreign=1"));
    assert!(report.passed(), "{}", report.summary(&events));
}

/// A traced symlink is an entry like any other: it persists only with its
/// directory, and reads back literally.
#[test]
fn symlinks_are_namespace_entries() {
    let events = vec![
        Event::Symlink {
            dir: ROOT,
            name: b"link".to_vec(),
            node: n(40),
            target: b"keep".to_vec(),
        },
        Event::Sync {
            node: ROOT,
            kind: SyncKind::Fsync,
        },
    ];
    let mut outcomes = BTreeSet::new();
    let report = check_view(&initial(), &events, &Options::default(), |view, info| {
        let link = view.get(b"link");
        if info.complete && link != Some(Entry::Symlink { target: b"keep" }) {
            return Err(format!("synced symlink lost: {link:?}"));
        }
        outcomes.insert(link.is_some());
        Ok(())
    })
    .unwrap();
    assert!(report.passed());
    assert_eq!(
        outcomes,
        [false, true].into(),
        "an unsynced symlink may be lost"
    );
}

/// Review probe P1: a trace whose Create reuses an inode identity the image
/// (or an earlier event) already names is refused, not conflated.
#[test]
fn inode_reuse_in_a_trace_is_refused() {
    let events = vec![
        Event::Unlink {
            dir: ROOT,
            name: b"keep".to_vec(),
        },
        Event::Create {
            dir: Some(ROOT),
            name: Some(b"fresh".to_vec()),
            node: KEEP_NODE,
            mode: 0o644,
        },
        write(KEEP_NODE, NEW),
        Event::Sync {
            node: KEEP_NODE,
            kind: SyncKind::Fsync,
        },
        Event::Sync {
            node: ROOT,
            kind: SyncKind::Fsync,
        },
    ];
    let error = check(&initial(), &events, &Options::default(), |_, _| Ok(())).unwrap_err();
    assert!(error.to_string().contains("inode reuse"), "{error}");

    let twice = vec![
        Event::Mkdir {
            dir: ROOT,
            name: b"a".to_vec(),
            node: n(20),
            mode: 0o755,
        },
        Event::Mkdir {
            dir: ROOT,
            name: b"b".to_vec(),
            node: n(20),
            mode: 0o755,
        },
    ];
    assert!(check(&initial(), &twice, &Options::default(), |_, _| Ok(())).is_err());
}

const OTHER: NodeId = n(5);

fn with_other() -> Image {
    initial().with_file(b"other", OTHER, b"")
}

/// `(keep, other)` file contents of one crash state.
type Pair = (Vec<u8>, Vec<u8>);

/// Collect `(keep, other)` contents of every state, and of the complete
/// states separately.
fn contents(events: &[Event], options: &Options) -> (Vec<Pair>, Vec<Pair>) {
    let mut complete = Vec::new();
    let mut any = Vec::new();
    let report = check(&with_other(), events, options, |root, info| {
        let keep = std::fs::read(root.join("keep")).map_err(|error| error.to_string())?;
        let other = std::fs::read(root.join("other")).map_err(|error| error.to_string())?;
        if info.complete {
            complete.push((keep.clone(), other.clone()));
        }
        any.push((keep, other));
        Ok(())
    })
    .unwrap();
    assert!(report.passed(), "{}", report.summary(events));
    (complete, any)
}

/// Review M3 (drain rule): `F_FULLFSYNC` on Y drains only what was already
/// sent to the drive. An unsynced write to X stays optional; once X was
/// kicked (sent) before the flush, the flush makes it durable.
#[test]
fn a_full_flush_drains_only_what_was_sent() {
    let unsent = vec![
        write(KEEP_NODE, b"XXXX"),
        Event::Sync {
            node: LEDGER,
            kind: SyncKind::FullFlush,
        },
    ];
    let (complete, _) = contents(&unsent, &Options::default());
    let keeps: BTreeSet<Vec<u8>> = complete.into_iter().map(|(keep, _)| keep).collect();
    assert_eq!(
        keeps,
        [KEEP.to_vec(), b"XXXX-bytes".to_vec()].into(),
        "an unsent write must survive or vanish after another file's full flush"
    );

    let sent = vec![
        write(KEEP_NODE, b"XXXX"),
        Event::Sync {
            node: KEEP_NODE,
            kind: SyncKind::Kick,
        },
        Event::Sync {
            node: LEDGER,
            kind: SyncKind::FullFlush,
        },
    ];
    let (complete, _) = contents(&sent, &Options::default());
    let keeps: BTreeSet<Vec<u8>> = complete.into_iter().map(|(keep, _)| keep).collect();
    assert_eq!(
        keeps,
        [b"XXXX-bytes".to_vec()].into(),
        "a sent write is drained"
    );
}

/// Review M4 (device-wide barrier): the barrier on Y orders only mutations
/// already sent before it. With the X write unsent, a state holding the later
/// Z write but not the X write is reachable; with X sent first, it is not.
#[test]
fn a_device_barrier_orders_only_sent_mutations() {
    let z_without_x = |events: &[Event]| {
        let (_, any) = contents(events, &Options::default());
        any.iter()
            .any(|(keep, other)| keep == KEEP && other == b"Z")
    };
    let unsent = vec![
        write(KEEP_NODE, b"XXXX"),
        Event::Sync {
            node: LEDGER,
            kind: SyncKind::Barrier,
        },
        write(OTHER, b"Z"),
    ];
    assert!(
        z_without_x(&unsent),
        "an unsent X write is not ordered by Y's barrier"
    );

    let sent = vec![
        write(KEEP_NODE, b"XXXX"),
        Event::Sync {
            node: KEEP_NODE,
            kind: SyncKind::Kick,
        },
        Event::Sync {
            node: LEDGER,
            kind: SyncKind::Barrier,
        },
        write(OTHER, b"Z"),
    ];
    assert!(!z_without_x(&sent), "a sent X write is ordered before Z");
}

/// #74 review, D2 (checker mutant CM1): a full flush drains only the drive
/// holding the file it names. A sent write on drive 1 stays optional after a
/// full flush on drive 2; the same flush on drive 1 makes it durable.
#[test]
fn a_full_flush_drains_only_its_own_drive() {
    let elsewhere = NodeId { dev: 2, ino: 77 };
    let trace = |flushed: NodeId| {
        vec![
            write(KEEP_NODE, b"XXXX"),
            Event::Sync {
                node: KEEP_NODE,
                kind: SyncKind::Kick,
            },
            Event::Sync {
                node: flushed,
                kind: SyncKind::FullFlush,
            },
        ]
    };
    let keeps = |events: &[Event]| -> BTreeSet<Vec<u8>> {
        contents(events, &Options::default())
            .0
            .into_iter()
            .map(|(keep, _)| keep)
            .collect()
    };
    assert_eq!(
        keeps(&trace(elsewhere)),
        [KEEP.to_vec(), b"XXXX-bytes".to_vec()].into(),
        "a full flush on another drive leaves this drive's write volatile"
    );
    assert_eq!(keeps(&trace(LEDGER)), [b"XXXX-bytes".to_vec()].into());
}

/// #74 review, D2 (checker mutant CM2): a device-wide barrier orders only
/// I/O on its own drive, on both sides of the barrier. `far` sits on drive 2;
/// `keep` and `other` on drive 1.
///
/// - X sent on drive 1, a barrier on drive 2, then Z on drive 1: Z may
///   persist without X.
/// - X sent on drive 1, a barrier on drive 2, then Z on drive 2: the barrier
///   orders nothing sent on drive 1, so Z may persist without X.
/// - Y sent on drive 2, a barrier on drive 2, then Z on drive 1: the barrier
///   orders nothing issued on drive 1, so Z may persist without Y.
#[test]
fn a_device_barrier_orders_only_its_own_drive() {
    const FAR: NodeId = NodeId { dev: 2, ino: 6 };
    let image = with_other().with_file(b"far", FAR, b"");
    let states = |events: &[Event]| -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let mut seen = Vec::new();
        let report = check(&image, events, &Options::default(), |root, _| {
            let read = |name: &str| std::fs::read(root.join(name)).map_err(|e| e.to_string());
            seen.push((read("keep")?, read("other")?, read("far")?));
            Ok(())
        })
        .unwrap();
        assert!(report.passed(), "{}", report.summary(events));
        seen
    };
    let sent_then_barrier = |sent: NodeId, data: &[u8], after: Event| {
        vec![
            write(sent, data),
            Event::Sync {
                node: sent,
                kind: SyncKind::Kick,
            },
            Event::Sync {
                node: FAR,
                kind: SyncKind::Barrier,
            },
            after,
        ]
    };

    let same_side = states(&sent_then_barrier(KEEP_NODE, b"XXXX", write(OTHER, b"Z")));
    assert!(
        same_side
            .iter()
            .any(|(keep, other, _)| keep == KEEP && other == b"Z"),
        "a barrier on another drive does not order this drive's writes"
    );
    let earlier_elsewhere = states(&sent_then_barrier(KEEP_NODE, b"XXXX", write(FAR, b"Z")));
    assert!(
        earlier_elsewhere
            .iter()
            .any(|(keep, _, far)| keep == KEEP && far == b"Z"),
        "a barrier orders only mutations sent on its own drive"
    );
    let later_elsewhere = states(&sent_then_barrier(FAR, b"Y", write(OTHER, b"Z")));
    assert!(
        later_elsewhere
            .iter()
            .any(|(_, other, far)| far.is_empty() && other == b"Z"),
        "a barrier orders only mutations issued on its own drive"
    );
}

/// Review M5 (a sync counts only once it completed): create tmp, write,
/// rename, then fsync the file, then fsync the directory. A crash between the
/// rename and the file fsync can publish `data` without its bytes, so this
/// protocol must fail, at crash point 3.
#[test]
fn syncing_the_file_after_the_rename_is_too_late() {
    let events = vec![
        Event::Create {
            dir: Some(ROOT),
            name: Some(b"tmp".to_vec()),
            node: TMP,
            mode: 0o600,
        },
        write(TMP, NEW),
        Event::Rename {
            node: TMP,
            from_dir: ROOT,
            from: b"tmp".to_vec(),
            to_dir: ROOT,
            to: b"data".to_vec(),
        },
        Event::Sync {
            node: TMP,
            kind: SyncKind::Fsync,
        },
        Event::Sync {
            node: ROOT,
            kind: SyncKind::Fsync,
        },
    ];
    for options in [Options::default(), strict()] {
        let report = run(&events, &options);
        assert!(!report.passed());
        assert!(report.violations.iter().any(
            |violation| violation.crash_point == 3 && violation.message.contains("wrong bytes")
        ));
    }
}

#[test]
fn partial_traces_and_bad_options_are_refused() {
    let untraced = vec![Event::Untraced {
        call: "pwrite",
        error: "fstat failed".to_owned(),
    }];
    assert!(check(&initial(), &untraced, &Options::default(), |_, _| Ok(())).is_err());
    let options = Options {
        exhaustive_limit: MAX_EXHAUSTIVE + 1,
        ..Options::default()
    };
    assert!(check(&initial(), &[], &options, |_, _| Ok(())).is_err());
}

/// `View::walk` lists every reachable entry, parents first, and
/// `View::node` finds an inode only while some directory names it.
#[test]
fn the_view_walks_the_tree_and_finds_named_inodes() {
    let dir = n(50);
    let events = vec![
        Event::Mkdir {
            dir: ROOT,
            name: b"d".to_vec(),
            node: dir,
            mode: 0o755,
        },
        Event::Create {
            dir: Some(dir),
            name: Some(b"f".to_vec()),
            node: n(51),
            mode: 0o644,
        },
        write(n(51), b"inner"),
        Event::Sync {
            node: n(51),
            kind: SyncKind::Fsync,
        },
        Event::Sync {
            node: dir,
            kind: SyncKind::Fsync,
        },
        Event::Sync {
            node: ROOT,
            kind: SyncKind::Fsync,
        },
    ];
    let mut complete_walks = Vec::new();
    check_view(&initial(), &events, &Options::default(), |view, info| {
        if info.complete {
            complete_walks.push(
                view.walk()
                    .into_iter()
                    .map(|(path, _)| String::from_utf8(path).unwrap())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(
                view.node(n(51)),
                Some(Entry::File {
                    data: b"inner",
                    mode: 0o644
                })
            );
        }
        if view.get(b"d").is_none() {
            assert!(
                !view.names(n(51)),
                "an unnamed directory hides its children"
            );
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(complete_walks, vec![vec!["d", "d/f", "keep", "ledger"]]);
}

/// The same protocols through the real syscall wrappers, checked from the
/// recorded trace. A file is published with `io::publish_noreplace`, the
/// path the stores use: an exclusive rename, or with the rename-unsupported
/// hook its counted `linkat` + `unlinkat` fallback (R-N119).
#[cfg(feature = "io-trace")]
mod recorded {
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::AsFd as _;

    use super::*;
    use crate::io::trace::recorder::Recorder;
    use crate::io::{publish_noreplace, sys, Published};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Seal {
        None,
        Barrier,
        Full,
    }

    fn seal(fd: impl std::os::fd::AsFd, how: Seal, dir: bool) {
        match (how, dir) {
            (Seal::None, _) => {}
            (Seal::Barrier, false) => sys::barrier(fd).unwrap(),
            (Seal::Barrier, true) => sys::barrier_dir(fd).unwrap(),
            (Seal::Full, _) => sys::full_flush(fd).unwrap(),
        }
    }

    /// Run the publish protocol in a fresh directory and return the pre-trace
    /// image, the recorded events and how the publish was carried out.
    /// `fallback` forces the link fallback.
    fn record(
        file: Seal,
        dir: Seal,
        drain: bool,
        fallback: bool,
    ) -> (Image, Vec<Event>, Published) {
        let scratch = tempfile::TempDir::new().unwrap();
        fs::write(scratch.path().join("keep"), KEEP).unwrap();
        fs::write(scratch.path().join("ledger"), b"").unwrap();
        let root = fs::File::from(sys::open_root(scratch.path()).unwrap());
        let ledger =
            sys::openat_beneath(&root, Path::new("ledger"), crate::io::OpenMode::Read).unwrap();
        let image = Image::scan(scratch.path()).unwrap();
        let recorder = Recorder::new();
        let published;
        {
            let _attached = recorder.attach();
            let temp = CString::new("tmp").unwrap();
            let data = CString::new("data").unwrap();
            let fd = sys::create_excl_at(root.as_fd(), &temp, 0o600).unwrap();
            sys::pwrite_all(&fd, NEW, 0).unwrap();
            seal(&fd, file, false);
            crate::io::force_rename_unsupported(fallback);
            let outcome = publish_noreplace(&root, &temp, &data);
            crate::io::force_rename_unsupported(false);
            published = outcome.unwrap();
            seal(&root, dir, true);
            if drain {
                sys::full_flush(&ledger).unwrap();
            }
        }
        (image, recorder.take(), published)
    }

    fn verdict(image: &Image, events: &[Event], scope: BarrierScope) -> Report {
        let options = Options {
            barrier_scope: scope,
            ..Options::default()
        };
        let report = check(image, events, &options, publish_invariant).unwrap();
        eprintln!("{}", report.summary(events));
        assert!(report.bounded.is_empty());
        report
    }

    const SCOPES: [BarrierScope; 2] = [BarrierScope::Device, BarrierScope::Object];

    #[test]
    fn recorded_publish_passes_and_its_broken_variants_fail() {
        for fallback in [false, true] {
            let expected = if fallback {
                Published::Linked
            } else {
                Published::Renamed
            };
            for scope in SCOPES {
                let (image, events, published) = record(Seal::Full, Seal::Full, false, fallback);
                assert_eq!(published, expected);
                assert!(verdict(&image, &events, scope).passed());

                let (image, events, _) = record(Seal::None, Seal::Full, false, fallback);
                assert!(
                    !verdict(&image, &events, scope).passed(),
                    "no file sync must fail (fallback={fallback})"
                );

                let (image, events, _) = record(Seal::Full, Seal::None, false, fallback);
                assert!(
                    !verdict(&image, &events, scope).passed(),
                    "no dir sync must fail (fallback={fallback})"
                );
            }
        }
    }

    /// The W3 group protocol through the real wrappers. It passes under the
    /// default device-wide model (R-N103). Under the strict per-object option
    /// it fails on Darwin, where `sys::barrier` is `F_BARRIERFSYNC` (ordering
    /// only), and passes on Linux, where `sys::barrier` is `fdatasync`
    /// (durable).
    #[test]
    fn recorded_group_protocol() {
        let (image, events, _) = record(Seal::Barrier, Seal::Barrier, true, false);
        assert!(verdict(&image, &events, Options::default().barrier_scope).passed());
        let strict = verdict(&image, &events, BarrierScope::Object);
        assert_eq!(strict.passed(), !cfg!(target_vendor = "apple"));
    }
}
