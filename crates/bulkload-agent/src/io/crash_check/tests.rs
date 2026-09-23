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

use std::path::Path;

use super::{check, BarrierScope, Image, Options, Report, StateInfo, MAX_EXHAUSTIVE};
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
    let mut seen_child_without_parent = false;
    // The root is never synced, so the mkdir may be lost; its child may
    // survive only together with it.
    let report = check(&initial(), &events, &Options::default(), |root, _| {
        if root.join("d/x").exists() && !root.join("d").exists() {
            seen_child_without_parent = true;
        }
        Ok(())
    })
    .unwrap();
    assert!(report.passed());
    assert!(!seen_child_without_parent);
    assert!(report.states >= 3);
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
    let report = check(&initial(), &events, &options, |_, _| Ok(())).unwrap();
    let summary = report.summary(&events);
    eprintln!("{summary}");
    assert!(!report.bounded.is_empty());
    let last = report.bounded.last().unwrap();
    assert_eq!(last.crash_point, 12);
    assert_eq!(last.optional, 12);
    assert_eq!(last.skipped, (1_u128 << 12) - last.explored as u128);
    // Prefix, required, 12 drop-one and 12 keep-one states, deduplicated.
    assert!(last.explored <= 2 + 2 * 12);
    assert!(summary.contains("bound: crash point 12 has 12 optional mutations"));
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

    std::os::unix::fs::symlink("top", dir.path().join("link")).unwrap();
    assert!(
        Image::scan(dir.path()).is_err(),
        "symlinks are not modelled"
    );
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

/// The same protocols through the real syscall wrappers, checked from the
/// recorded trace.
#[cfg(feature = "io-trace")]
mod recorded {
    use std::ffi::CString;
    use std::fs;

    use super::*;
    use crate::io::trace::recorder::Recorder;
    use crate::io::{sys, TempFile};

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
    /// image and the recorded events.
    fn record(
        base: Option<&Path>,
        file: Seal,
        dir: Seal,
        drain: bool,
        named: bool,
    ) -> (Image, Vec<Event>, bool) {
        let scratch = base
            .map_or_else(tempfile::TempDir::new, tempfile::TempDir::new_in)
            .unwrap();
        fs::write(scratch.path().join("keep"), KEEP).unwrap();
        fs::write(scratch.path().join("ledger"), b"").unwrap();
        let root = sys::open_root(scratch.path()).unwrap();
        let ledger =
            sys::openat_beneath(&root, Path::new("ledger"), crate::io::OpenMode::Read).unwrap();
        let image = Image::scan(scratch.path()).unwrap();
        let recorder = Recorder::new();
        let anonymous;
        {
            let _attached = recorder.attach();
            let data = CString::new("data").unwrap();
            if named {
                let (fd, name) = sys::create_temp_named(&root, 0o600).unwrap();
                sys::pwrite_all(&fd, NEW, 0).unwrap();
                seal(&fd, file, false);
                sys::rename_noreplace(&root, &name, &data).unwrap();
                anonymous = false;
            } else {
                let temp = TempFile::create(&root, 0o600).unwrap();
                anonymous = temp.is_anonymous();
                sys::pwrite_all(temp.fd(), NEW, 0).unwrap();
                seal(temp.fd(), file, false);
                temp.publish(&root, &data).unwrap();
            }
            seal(&root, dir, true);
            if drain {
                sys::full_flush(&ledger).unwrap();
            }
        }
        (image, recorder.take(), anonymous)
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
    fn recorded_named_publish_passes_and_its_broken_variants_fail() {
        for scope in SCOPES {
            let (image, events, _) = record(None, Seal::Full, Seal::Full, false, true);
            assert!(verdict(&image, &events, scope).passed());

            let (image, events, _) = record(None, Seal::None, Seal::Full, false, true);
            assert!(
                !verdict(&image, &events, scope).passed(),
                "no file sync must fail"
            );

            let (image, events, _) = record(None, Seal::Full, Seal::None, false, true);
            assert!(
                !verdict(&image, &events, scope).passed(),
                "no dir sync must fail"
            );
        }
    }

    #[test]
    fn recorded_temp_file_publish_passes_and_its_broken_variants_fail() {
        for scope in SCOPES {
            let (image, events, anonymous) = record(None, Seal::Full, Seal::Full, false, false);
            eprintln!("temp file anonymous={anonymous}");
            assert!(verdict(&image, &events, scope).passed());
            let (image, events, _) = record(None, Seal::None, Seal::Full, false, false);
            assert!(!verdict(&image, &events, scope).passed());
            let (image, events, _) = record(None, Seal::Full, Seal::None, false, false);
            assert!(!verdict(&image, &events, scope).passed());
        }
    }

    /// The W3 group protocol through the real wrappers. It passes under the
    /// default device-wide model (R-N103). Under the strict per-object option
    /// it fails on Darwin, where `sys::barrier` is `F_BARRIERFSYNC` (ordering
    /// only), and passes on Linux, where `sys::barrier` is `fdatasync`
    /// (durable).
    #[test]
    fn recorded_group_protocol() {
        let (image, events, _) = record(None, Seal::Barrier, Seal::Barrier, true, true);
        assert!(verdict(&image, &events, Options::default().barrier_scope).passed());
        let strict = verdict(&image, &events, BarrierScope::Object);
        assert_eq!(strict.passed(), !cfg!(target_vendor = "apple"));
    }

    /// Linux `O_TMPFILE` + `linkat(/proc/self/fd)` on tmpfs, where the kernel
    /// supports it: the correct protocol passes and the unsynced one fails.
    #[cfg(target_os = "linux")]
    #[test]
    fn recorded_o_tmpfile_publish_on_tmpfs() {
        let base = Path::new("/dev/shm");
        if tempfile::TempDir::new_in(base).is_err() {
            eprintln!("SKIP: /dev/shm is not writable here");
            return;
        }
        let (image, events, anonymous) =
            record(Some(base), Seal::Barrier, Seal::Full, false, false);
        assert!(anonymous);
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::Create { name: None, .. })));
        for scope in SCOPES {
            assert!(verdict(&image, &events, scope).passed());
        }
        let (image, events, _) = record(Some(base), Seal::None, Seal::Full, false, false);
        for scope in SCOPES {
            assert!(!verdict(&image, &events, scope).passed());
        }
    }
}
