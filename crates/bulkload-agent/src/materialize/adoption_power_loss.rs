//! Power-loss regressions for the directory resume paths.
//!
//! A resume adopts a directory a crashed run left behind, then commits
//! outputs inside it. Whatever unsealed directory entry the crash left must
//! be sealed before any record that depends on it commits, or a power loss
//! can keep the record and lose the entry.
//!
//! - #74 review B1 (R-N119): a crash at `directory.after_fallback_mkdir`
//!   leaves an intent record and a fresh 0700 directory whose `mkdirat`
//!   entry was never sealed. The resume binds the record, which must come
//!   after the seal, or a durable record can name an inode no directory holds
//!   (the N3 hazard, R-N102).
//! - #74 round 2 N1: a crash at `directory.after_rename` leaves a bound record
//!   and a directory whose rename into place was never sealed. The resume
//!   adopts it through the bound-record arm; an output committed inside it
//!   must not outlive the rename.
//!
//! - #169 (R25's strict reading, OI-1003-Q40, R-N58): a crash between an
//!   output's publish and its row commit leaves its bytes durable at the
//!   final path with no row. In every power-loss state of that crashed run,
//!   a resume must adopt a surviving output from its capture record without
//!   reading a source byte, and must complete a lost one.
//!
//! - S1 (OI-1003-Q107): a group of several outputs is sealed device-wide,
//!   one `syncfs` before its renames and one after. No power-loss state may
//!   name an output whose bytes are not there. A superseding output (WP0(d))
//!   in the same group joins the first `syncfs`: its exchange never shows the
//!   new name before the new bytes are durable.
//!
//! Each crashed run is replayed with the real calls the engine makes before
//! its fault point. The resume then runs through `Destination::directory`, a
//! staged write, a `PublishSink` group commit, `finish_directories` and
//! `flush_session`, all under one process-wide recorder. `check_view`
//! enumerates every power-loss state of that resume.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use super::*;
use crate::io::crash_check::{check, check_view, Entry, Image, Options};
use crate::io::durable::GroupSink as _;
use crate::io::trace::recorder::Recorder;
#[cfg(target_os = "linux")]
use crate::io::trace::SyncKind;
use crate::io::trace::{CommitRecord, Event};
use crate::transfer_store::PublisherSide;

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "bulkload-adoption-{tag}-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(base.join("source")).unwrap();
    std::fs::create_dir_all(base.join("destination")).unwrap();
    base
}

fn rows(source: &Path) -> Vec<RowSchema> {
    crate::walk::walk(
        &crate::walk::WalkOptions::new(source.to_path_buf()),
        &mut crate::freshness::NullCache,
    )
    .unwrap()
    .rows
}

static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What a crashed run needs to replay its calls.
struct Crash<'a> {
    store: &'a Store,
    destination: &'a Path,
    key: &'a [u8],
    row: &'a RowSchema,
}

/// Replay `crash`, resume the copy of `d/f`, and check every power-loss
/// state of the resume: a committed output names a file holding its bytes,
/// and a bound directory record names a directory.
fn resume_after(tag: &str, crash: impl FnOnce(&Crash<'_>)) {
    let base = scratch(tag);
    let (source, destination, state) = (
        base.join("source"),
        base.join("destination"),
        base.join("state"),
    );
    std::fs::create_dir(source.join("d")).unwrap();
    std::fs::write(source.join("d/f"), b"payload").unwrap();
    std::fs::set_permissions(
        source.join("d"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let all = rows(&source);
    let row_d = all.iter().find(|row| row.rel_path == b"d").unwrap().clone();
    let row_f = all
        .iter()
        .find(|row| row.rel_path == b"d/f")
        .unwrap()
        .clone();
    let store = Store::open(&state).unwrap();
    let authority: &[u8] = b"adoption-authority";
    let key = postcard::to_stdvec(&(authority, &row_d.rel_path)).unwrap();

    let image = Image::scan(&destination).unwrap();
    let recorder = Recorder::new();
    // A process-wide recorder also records every other test thread with no
    // recorder of its own, so these proofs run one at a time.
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let _attached = recorder.attach_process();
        crash(&Crash {
            store: &store,
            destination: &destination,
            key: &key,
            row: &row_d,
        });
        // The resume.
        let mut target = Destination::open(&destination, &store).unwrap();
        target.directory(&row_d, &store, authority).unwrap();
        assert_eq!(target.directories.len(), 1, "the resume adopted d");
        let staged = target.stage(&row_f).unwrap();
        crate::io::sys::pwrite_all(&**staged.file(), b"payload", 0).unwrap();
        crate::io::sys::fchmod(&**staged.file(), row_f.mode & 0o7777).unwrap();
        let mut sink = PublishSink::new(
            Store::open(&state)
                .unwrap()
                .into_publisher(PublisherSide::Destination)
                .unwrap(),
        )
        .unwrap();
        sink.commit(vec![Publication::Staged {
            staged,
            record: PendingOutput {
                key: b"output-key".to_vec(),
                rel_path: b"d/f".to_vec(),
                size: 7,
                racy: false,
                hints: Vec::new(),
            },
        }]);
        let report = sink.finish();
        assert!(
            report.iter().all(|(_, outcome)| outcome.is_ok()),
            "{report:?}"
        );
        target.finish_directories(&store).unwrap();
        target.flush_session().unwrap();
    }
    let events = recorder.take();
    let options = Options {
        ignore_foreign: true,
        accept_bounded: true,
        ..Options::default()
    };
    let report = check_view(&image, &events, &options, |view, info| {
        for commit in &info.commits {
            let Event::Commit { records, .. } = &events[*commit] else {
                continue;
            };
            for record in records {
                match record {
                    CommitRecord::Output { rel_path } => {
                        if !matches!(view.get(rel_path), Some(Entry::File { data, .. }) if data == b"payload")
                        {
                            return Err(format!(
                                "committed output {} is {:?}",
                                String::from_utf8_lossy(rel_path),
                                view.get(rel_path)
                            ));
                        }
                    }
                    CommitRecord::DirectoryCreated {
                        node: Some(node), ..
                    } if !matches!(view.node(*node), Some(Entry::Dir { .. })) => {
                        return Err(format!("bound directory record {node:?} is not named"));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        report.passed(),
        "a committed record outlives its directory entry on power loss:\n{}",
        report.summary(&events)
    );
}

/// #74 review B1: the crashed run is `fallback_directory` up to its fault
/// point (the intent commit, then `mkdirat`).
#[test]
fn an_adopted_fallback_directory_is_sealed_before_its_record_binds() {
    resume_after("seal", |crash| {
        crash
            .store
            .record_directory_created(crash.key, INTENT.0, INTENT.1, crash.row.mode & 0o7777)
            .unwrap();
        let root = File::from(crate::io::sys::open_dir_path_nofollow(crash.destination).unwrap());
        crate::io::sys::mkdirat(&root, c"d", 0o700).unwrap();
    });
}

/// #74 round 2 N1 (probe P4): the crashed run is the rename path of
/// `directory` up to `directory.after_rename` (a sealed temporary, its bound
/// record, then the rename into place, never sealed).
#[test]
fn a_directory_adopted_by_its_bound_record_is_sealed_before_outputs_commit() {
    resume_after("rename", |crash| {
        let crashed = Destination::open(crash.destination, crash.store).unwrap();
        let root = File::from(crate::io::sys::open_dir_path_nofollow(crash.destination).unwrap());
        let temporary = crashed.temporary(Some(DIRECTORY_MARK)).unwrap();
        crate::io::sys::mkdirat(&root, &temporary, 0o700).unwrap();
        let metadata = open_dir(&root, &temporary).unwrap().metadata().unwrap();
        crashed.seal_entry(&root).unwrap();
        crash
            .store
            .record_directory_created(
                crash.key,
                metadata.dev(),
                metadata.ino(),
                crash.row.mode & 0o7777,
            )
            .unwrap();
        crate::io::rename_exclusive(&root, &temporary, c"d").unwrap();
    });
}

/// #169 (R25 strict, OI-1003-Q40, R-N58): the crashed run is a publish up to
/// its row commit: a staged file written, its capture record and mode set,
/// sealed, renamed into place and its directory sealed, and no commit. The
/// checker builds every power-loss state of that run, and each one is
/// resumed by a real `copy` into its own materialized destination, from a
/// source store of its own (a new authority and no ledger row: the crashed
/// run never got `Held`, and the record's key holds no authority). A state
/// whose output survived at the final path with its bytes must be adopted
/// from its record with 0 source bytes read; any other state is completed by
/// reading the seat once. Both kinds must occur.
#[test]
fn an_unrowed_output_is_adopted_without_source_reads() {
    const SIZE: usize = 150_000;
    let base = scratch("unrowed");
    let (source, destination) = (base.join("source"), base.join("destination"));
    let destination_state = base.join("state");
    let payload: Vec<u8> = (0..SIZE)
        .map(|at| u8::try_from((at * 7 + at / 251) % 256).unwrap())
        .collect();
    std::fs::write(source.join("f"), &payload).unwrap();
    let row = rows(&source)
        .into_iter()
        .find(|row| row.rel_path == b"f")
        .unwrap();
    let record = crate::transfer::unrowed::CaptureRecord {
        key: crate::transfer::unrowed::record_key(&row).unwrap(),
        root: bulkload_proto::frame::manifest_root(&chunk_specs(&payload)),
        size: SIZE as u64,
    };

    let image = Image::scan(&destination).unwrap();
    let recorder = Recorder::new();
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let _attached = recorder.attach_process();
        let store = Store::open(&destination_state).unwrap();
        let target = Destination::open(&destination, &store).unwrap();
        let staged = target.stage(&row).unwrap();
        crate::io::sys::pwrite_all(&**staged.file(), &payload, 0).unwrap();
        crate::transfer::unrowed::write_record(staged.file(), &record);
        crate::io::sys::fchmod(&**staged.file(), row.mode & 0o7777).unwrap();
        let (_, parent) = staged.publish().unwrap();
        crate::io::durable::seal_dir(&parent).unwrap();
        // The crash: the group's row commit never runs.
    }
    let events = recorder.take();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SetCaptureRecord { .. })),
        "the crashed run wrote no capture record: {events:?}"
    );
    let (mut adopted, mut completed, mut serial) = (0_u32, 0_u32, 0_u32);
    let options = Options {
        ignore_foreign: true,
        accept_bounded: true,
        ..Options::default()
    };
    let report = check(&image, &events, &options, |state, _| {
        serial += 1;
        let survived = std::fs::read(state.join("f")).ok().as_deref() == Some(payload.as_slice());
        let stats = crate::transfer::copy(
            &source,
            state,
            &base.join(format!("resume-source-state-{serial}")),
            &base.join(format!("resume-state-{serial}")),
        )
        .map_err(|refusal| format!("resume refused: {refusal:?}"))?;
        if !stats.refusals.is_empty() {
            return Err(format!("resume refused {:?}", stats.refusals));
        }
        if std::fs::read(state.join("f")).ok().as_deref() != Some(payload.as_slice()) {
            return Err("the resume did not complete f".to_owned());
        }
        if survived {
            if (stats.source_bytes_read, stats.unrowed_adopted) != (0, 1) {
                return Err(format!(
                    "a durable unrowed output was read again: {} source bytes, {} adopted",
                    stats.source_bytes_read, stats.unrowed_adopted
                ));
            }
            adopted += 1;
        } else {
            if stats.source_bytes_read != SIZE as u64 {
                return Err(format!(
                    "a lost output was read {} bytes, not once",
                    stats.source_bytes_read
                ));
            }
            completed += 1;
        }
        Ok(())
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        report.passed(),
        "a resume re-read a durable unrowed output:\n{}",
        report.summary(&events)
    );
    assert!(
        adopted > 0 && completed > 0,
        "states adopted {adopted}, completed {completed}"
    );
}

/// The chunks a capture of `data` sends: `FastCDC` with the engine's bounds,
/// BLAKE3 per chunk.
fn chunk_specs(data: &[u8]) -> Vec<bulkload_proto::frame::ChunkSpec> {
    fastcdc::v2020::FastCDC::new(
        data,
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    )
    .map(|chunk| bulkload_proto::frame::ChunkSpec {
        digest: blake3::hash(&data[chunk.offset..chunk.offset + chunk.length]).into(),
        size: chunk.length as u64,
    })
    .collect()
}

/// The outputs of the batched group proof.
#[cfg(target_os = "linux")]
const GROUP: [(&str, &[u8]); 3] = [
    ("a", b"alpha-bytes"),
    ("b", b"bravo"),
    ("c", b"charlie-bytes!"),
];

/// The output the batched group proof supersedes (WP0(d)): its path, the
/// bytes this store published there first, and the changed seat's bytes.
#[cfg(target_os = "linux")]
const SUPERSEDED: (&str, &[u8], &[u8]) = ("d", b"delta-old", b"delta-new-and-longer");

/// The authority the superseded output's rows are committed under.
#[cfg(target_os = "linux")]
const SUPERSEDE_AUTHORITY: &[u8] = b"batched-authority";

/// S1 (OI-1003-Q107): commit three new outputs and one superseding publish
/// (WP0(d)) in one group, which Linux group mode seals device-wide. In every
/// power-loss state of that commit, each new final name is absent or holds
/// exactly its output's bytes, the superseded path holds the old output or
/// the new one whole, a committed new row is never beside the old bytes, and
/// every committed output is present. The superseding temporary takes no
/// flush of its own: the group's first `syncfs` seals it before its exchange
/// (each device seal's `fsync` of its own descriptor aside).
///
/// Linux only: elsewhere no group is batched, so there is nothing to prove,
/// and `just resume-power-loss` expects this proof on Linux alone. On Linux
/// it never passes without running (OI-1003-Q107 review): a kernel below
/// the `syncfs` floor fails it. The recorded group's file system is taken as
/// tmpfs (`force_fs_magic`), so the proof runs whatever file system holds
/// the test's scratch directory; the allowlist has its own tests.
#[cfg(target_os = "linux")]
#[test]
fn a_batched_group_names_no_output_before_its_data_is_durable() {
    assert!(
        crate::io::durable::batched(GROUP.len() + 1),
        "Linux below the syncfs floor {:?}: the batched proof cannot run here",
        crate::io::durable::SYNCFS_REPORTS_ERRORS_SINCE
    );
    let base = scratch("batched");
    let (source, destination, state) = (
        base.join("source"),
        base.join("destination"),
        base.join("state"),
    );
    let (superseded, old, new) = SUPERSEDED;
    // Held from the first destination change: another proof's process-wide
    // recorder would trace this one's setup.
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // This store's own output of `old`, published and committed before the
    // recorded group (a single-file group: the per-file path).
    std::fs::write(source.join(superseded), old).unwrap();
    {
        let row = rows(&source)
            .into_iter()
            .find(|row| row.rel_path == superseded.as_bytes())
            .unwrap();
        let store = Store::open(&state).unwrap();
        let target = Destination::open(&destination, &store).unwrap();
        let staged = target.stage(&row).unwrap();
        crate::io::sys::pwrite_all(&**staged.file(), old, 0).unwrap();
        let mut sink = PublishSink::new(
            Store::open(&state)
                .unwrap()
                .into_publisher(PublisherSide::Destination)
                .unwrap(),
        )
        .unwrap();
        sink.commit(vec![Publication::Staged {
            staged,
            record: PendingOutput {
                key: crate::transfer_store::row_key(SUPERSEDE_AUTHORITY, &row).unwrap(),
                rel_path: row.rel_path.clone(),
                size: row.size,
                racy: false,
                hints: Vec::new(),
            },
        }]);
        assert!(sink.finish().iter().all(|(_, outcome)| outcome.is_ok()));
    }
    for (name, data) in GROUP {
        std::fs::write(source.join(name), data).unwrap();
    }
    std::fs::write(source.join(superseded), new).unwrap();
    let all = rows(&source);
    let store = Store::open(&state).unwrap();
    let changed = all
        .iter()
        .find(|row| row.rel_path == superseded.as_bytes())
        .unwrap();
    let owned = owned_output(
        &store,
        SUPERSEDE_AUTHORITY,
        changed,
        &File::open(destination.join(superseded)).unwrap(),
    )
    .unwrap()
    .unwrap();
    let image = Image::scan(&destination).unwrap();
    let recorder = Recorder::new();
    {
        let _attached = recorder.attach_process();
        let target = Destination::open(&destination, &store).unwrap();
        let mut publications = Vec::new();
        for (name, data) in GROUP {
            let row = all
                .iter()
                .find(|row| row.rel_path == name.as_bytes())
                .unwrap();
            let staged = target.stage(row).unwrap();
            crate::io::sys::pwrite_all(&**staged.file(), data, 0).unwrap();
            crate::io::durable::start_writeback(staged.file());
            publications.push(Publication::Staged {
                staged,
                record: PendingOutput {
                    key: name.as_bytes().to_vec(),
                    rel_path: name.as_bytes().to_vec(),
                    size: row.size,
                    racy: false,
                    hints: Vec::new(),
                },
            });
        }
        let staged = target.stage(changed).unwrap();
        crate::io::sys::pwrite_all(&**staged.file(), new, 0).unwrap();
        crate::io::durable::start_writeback(staged.file());
        publications.push(Publication::Superseding {
            staged,
            record: PendingOutput {
                key: crate::transfer_store::row_key(SUPERSEDE_AUTHORITY, changed).unwrap(),
                rel_path: changed.rel_path.clone(),
                size: changed.size,
                racy: false,
                hints: Vec::new(),
            },
            owned,
        });
        let mut sink = PublishSink::new(
            Store::open(&state)
                .unwrap()
                .into_publisher(PublisherSide::Destination)
                .unwrap(),
        )
        .unwrap();
        crate::io::durable::force_fs_magic(Some(crate::io::durable::TMPFS_MAGIC));
        sink.commit(publications);
        crate::io::durable::force_fs_magic(None);
        let report = sink.finish();
        assert!(
            report.iter().all(|(_, outcome)| outcome.is_ok()),
            "{report:?}"
        );
    }
    let events = recorder.take();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                Event::Sync {
                    kind: SyncKind::FsSync,
                    ..
                }
            ))
            .count(),
        2,
        "one syncfs before the renames and the exchange, and one after"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Exchange { .. })),
        "the superseding output was exchanged in"
    );
    // The only per-object flushes are the device seals' own `fsync`s, each
    // of the descriptor its `syncfs` ran on and recorded just before that
    // seal's `FsSync` (#210 review, xfs idle log).
    for (at, event) in events.iter().enumerate() {
        if let Event::Sync {
            kind: SyncKind::DataSync | SyncKind::Fsync,
            node,
        } = event
        {
            assert!(
                matches!(
                    events.get(at + 1),
                    Some(Event::Sync { kind: SyncKind::FsSync, node: sealed }) if sealed == node
                ),
                "no output of the batched group took a flush of its own: {event:?}"
            );
        }
    }
    let options = Options {
        ignore_foreign: true,
        accept_bounded: true,
        ..Options::default()
    };
    let report = check_view(&image, &events, &options, |view, info| {
        for (name, data) in GROUP {
            match view.get(name.as_bytes()) {
                None => {}
                Some(Entry::File { data: held, .. }) if held == data => {}
                other => return Err(format!("output {name} is {other:?}")),
            }
        }
        let held = match view.get(superseded.as_bytes()) {
            Some(Entry::File { data: held, .. }) if held == old || held == new => held,
            other => return Err(format!("superseded output is {other:?}")),
        };
        for commit in &info.commits {
            let Event::Commit { records, .. } = &events[*commit] else {
                continue;
            };
            for record in records {
                if let CommitRecord::Output { rel_path } = record {
                    if view.get(rel_path).is_none() {
                        return Err(format!(
                            "committed output {} is not named",
                            String::from_utf8_lossy(rel_path)
                        ));
                    }
                    if rel_path == superseded.as_bytes() && held != new {
                        return Err("the new row is committed beside the old bytes".into());
                    }
                }
            }
        }
        Ok(())
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        report.passed(),
        "a batched group named an output its bytes did not reach:\n{}",
        report.summary(&events)
    );
}

// ---------------------------------------------------------------------------
// Directory batching (OI-1003-Q143 item 2, #217 review)
// ---------------------------------------------------------------------------

/// The directories of the batching proofs: three siblings under the root,
/// each holding one file.
const SIBLINGS: [&str; 3] = ["a", "b", "c"];

/// The authority the batching proofs key their directory records under.
const BATCH_AUTHORITY: &[u8] = b"batch-authority";

/// The directory a record's key names: its key is `(authority, rel)`.
fn record_path(key: &[u8]) -> Vec<u8> {
    postcard::from_bytes::<(Vec<u8>, Vec<u8>)>(key)
        .map(|(_, rel)| rel)
        .unwrap_or_default()
}

fn is_temporary_leaf(rel: &[u8]) -> bool {
    rel.rsplit(|byte| *byte == b'/')
        .next()
        .is_some_and(|leaf| leaf.starts_with(TEMPORARY_PREFIX))
}

fn parent_path(rel: &[u8]) -> &[u8] {
    rel.iter()
        .rposition(|byte| *byte == b'/')
        .map_or(&[][..], |end| &rel[..end])
}

/// The directory-record invariants of one crash state, replayed from the
/// commits that completed (#217 review, findings 1, 9 and 10):
///
/// - a record bound to an inode names it at the record's own path, or under
///   a temporary name directly inside the record's parent, never inside
///   another directory's temporary (an orphan no sweep can remove);
/// - a completed directory has its final mode (`modes`);
/// - every directory at a final name that the pre-trace image did not hold
///   is owned: bound by its record to that very inode, covered by an
///   intent, or completed;
/// - every committed output holds `payload` under its path.
fn directory_records_hold(
    events: &[Event],
    view: crate::io::crash_check::View<'_>,
    info: &crate::io::crash_check::StateInfo,
    before: &std::collections::BTreeSet<Vec<u8>>,
    modes: &std::collections::BTreeMap<Vec<u8>, u32>,
    payload: &[u8],
) -> std::result::Result<(), String> {
    let name = |rel: &[u8]| String::from_utf8_lossy(rel).into_owned();
    let mut bound: std::collections::BTreeMap<Vec<u8>, Option<crate::io::NodeId>> =
        std::collections::BTreeMap::new();
    let mut completed = std::collections::BTreeSet::new();
    for commit in &info.commits {
        let Event::Commit { records, .. } = &events[*commit] else {
            continue;
        };
        for record in records {
            match record {
                CommitRecord::DirectoryCreated { key, node, .. } => {
                    completed.remove(&record_path(key));
                    bound.insert(record_path(key), *node);
                }
                CommitRecord::DirectoryComplete { key } => {
                    let rel = record_path(key);
                    bound.remove(&rel);
                    match view.get(&rel) {
                        Some(Entry::Dir { mode }) if modes.get(&rel) == Some(&mode) => {}
                        other => {
                            return Err(format!("completed directory {} is {other:?}", name(&rel)))
                        }
                    }
                    completed.insert(rel);
                }
                CommitRecord::DirectoryCleared { key } => {
                    bound.remove(&record_path(key));
                }
                CommitRecord::Output { rel_path } if !matches!(view.get(rel_path), Some(Entry::File { data, .. }) if data == payload) =>
                {
                    return Err(format!(
                        "committed output {} is {:?}",
                        name(rel_path),
                        view.get(rel_path)
                    ));
                }
                _ => {}
            }
        }
    }
    for (rel, node) in &bound {
        let Some(node) = node else {
            continue;
        };
        let paths = view.paths_of(*node);
        if paths.is_empty() {
            return Err(format!("recorded directory {} names no entry", name(rel)));
        }
        for path in paths {
            let own = path == *rel
                || (parent_path(&path) == parent_path(rel) && is_temporary_leaf(&path));
            if !own {
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
                .any(|part| part.starts_with(TEMPORARY_PREFIX))
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

/// One batching proof's tree: [`SIBLINGS`], 0755, each with `f`.
struct Siblings {
    base: PathBuf,
    destination: PathBuf,
    state: PathBuf,
    directories: Vec<RowSchema>,
    files: Vec<RowSchema>,
}

const PAYLOAD: &[u8] = b"batched-payload";

fn siblings(tag: &str) -> Siblings {
    let base = scratch(tag);
    let source = base.join("source");
    for directory in SIBLINGS {
        std::fs::create_dir(source.join(directory)).unwrap();
        std::fs::write(source.join(directory).join("f"), PAYLOAD).unwrap();
        std::fs::set_permissions(
            source.join(directory),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }
    let all = rows(&source);
    Siblings {
        destination: base.join("destination"),
        state: base.join("state"),
        directories: all
            .iter()
            .filter(|row| row.kind == bulkload_proto::FileKind::Directory)
            .cloned()
            .collect(),
        files: all
            .iter()
            .filter(|row| row.kind == bulkload_proto::FileKind::Regular)
            .cloned()
            .collect(),
        base,
    }
}

fn batch_key(row: &RowSchema) -> Vec<u8> {
    postcard::to_stdvec(&(BATCH_AUTHORITY, &row.rel_path)).unwrap()
}

/// A crashed level: every sibling made under a tagged temporary, the root
/// sealed, and one commit binding every record; `renamed` of them renamed
/// into place, never sealed. Returns the destination it used.
fn crashed_level(tree: &Siblings, store: &Store, renamed: usize) {
    let crashed = Destination::open(&tree.destination, store).unwrap();
    let root = File::from(crate::io::sys::open_dir_path_nofollow(&tree.destination).unwrap());
    let mut made = Vec::new();
    for row in &tree.directories {
        let temporary = crashed.temporary(Some(DIRECTORY_MARK)).unwrap();
        crate::io::sys::mkdirat(&root, &temporary, 0o700).unwrap();
        let metadata = open_dir(&root, &temporary).unwrap().metadata().unwrap();
        made.push((row, temporary, metadata));
    }
    crashed.seal_entry(&root).unwrap();
    let keys: Vec<Vec<u8>> = made.iter().map(|(row, ..)| batch_key(row)).collect();
    let records: Vec<(&[u8], PendingDirectory)> = made
        .iter()
        .zip(&keys)
        .map(|((row, _, metadata), key)| {
            (
                key.as_slice(),
                PendingDirectory {
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                    mode: row.mode & 0o7777,
                },
            )
        })
        .collect();
    store.record_directories_created(&records).unwrap();
    for (row, temporary, _) in made.iter().take(renamed) {
        let leaf = cstring(&row.rel_path).unwrap();
        crate::io::rename_exclusive(&root, temporary, &leaf).unwrap();
    }
}

/// The resume of a batching proof: the root swept, each sibling decided as
/// the receiving thread decides it (an existing one at once, which adopts
/// it and seals the root; the new ones as one batch), every file published
/// through a group commit, then the directories finished and the session
/// flushed.
fn resume_siblings(tree: &Siblings, store: &Store) {
    let mut target = Destination::open(&tree.destination, store).unwrap();
    assert!(target.batch() > 1, "the resume batches new directories");
    target.sweep_root(store).unwrap();
    let mut new = Vec::new();
    for row in &tree.directories {
        if target.directory_is_new(row) {
            new.push(row);
        } else {
            target.directory(row, store, BATCH_AUTHORITY).unwrap();
        }
    }
    for outcome in target.create_directories(&new, store, BATCH_AUTHORITY) {
        outcome.unwrap();
    }
    let mut sink = PublishSink::new(
        Store::open(&tree.state)
            .unwrap()
            .into_publisher(PublisherSide::Destination)
            .unwrap(),
    )
    .unwrap();
    let mut group = Vec::new();
    for row in &tree.files {
        let staged = target.stage(row).unwrap();
        crate::io::sys::pwrite_all(&**staged.file(), PAYLOAD, 0).unwrap();
        crate::io::sys::fchmod(&**staged.file(), row.mode & 0o7777).unwrap();
        group.push(Publication::Staged {
            staged,
            record: PendingOutput {
                key: [b"out-".as_slice(), &row.rel_path].concat(),
                rel_path: row.rel_path.clone(),
                size: PAYLOAD.len() as u64,
                racy: false,
                hints: Vec::new(),
            },
        });
    }
    sink.commit(group);
    let report = sink.finish();
    assert!(
        report.iter().all(|(_, outcome)| outcome.is_ok()),
        "{report:?}"
    );
    target.finish_directories(store).unwrap();
    target.flush_session().unwrap();
}

/// Record `crash` and then the resume under one process-wide recorder, and
/// check every power-loss state of the whole trace.
fn check_siblings(
    tag: &str,
    crash: impl FnOnce(&Siblings, &Store),
) -> (Vec<Event>, crate::io::crash_check::Report) {
    let tree = siblings(tag);
    let image = Image::scan(&tree.destination).unwrap();
    let recorder = Recorder::new();
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let _attached = recorder.attach_process();
        let store = Store::open(&tree.state).unwrap();
        crash(&tree, &store);
        resume_siblings(&tree, &store);
    }
    let events = recorder.take();
    let modes = tree
        .directories
        .iter()
        .map(|row| (row.rel_path.clone(), row.mode & 0o7777))
        .collect();
    let options = Options {
        ignore_foreign: true,
        accept_bounded: true,
        ..Options::default()
    };
    let before = std::collections::BTreeSet::new();
    let report = check_view(&image, &events, &options, |view, info| {
        directory_records_hold(&events, view, info, &before, &modes, PAYLOAD)
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&tree.base);
    (events, report)
}

/// A crash after a level's one commit, before any rename: the resume's
/// sweep removes every temporary and clears every record bound to one (each
/// record cleared before its temporary goes, #74 N3), and creates the level
/// again. No committed record ever names an unnamed inode, and no directory
/// at a final name lacks its record.
#[test]
fn a_level_crashed_after_its_commit_is_swept_and_its_records_cleared() {
    let (events, report) = check_siblings("level-commit", |tree, store| {
        crashed_level(tree, store, 0);
    });
    assert!(
        report.passed(),
        "a crashed level's records outlive their temporaries:\n{}",
        report.summary(&events)
    );
    let cleared = events
        .iter()
        .filter_map(|event| match event {
            Event::Commit { records, .. } => Some(
                records
                    .iter()
                    .filter(|record| matches!(record, CommitRecord::DirectoryCleared { .. }))
                    .count(),
            ),
            _ => None,
        })
        .sum::<usize>();
    assert_eq!(cleared, SIBLINGS.len(), "every crashed record was cleared");
}

/// A crash after two of a level's three renames, its parent unsealed: in
/// every power-loss state a surviving rename is adopted, its parent sealed
/// before any output inside it commits; a lost one is swept and created
/// again.
#[test]
fn a_level_crashed_after_some_renames_adopts_the_renamed_and_makes_the_rest() {
    let (events, report) = check_siblings("level-renames", |tree, store| {
        crashed_level(tree, store, 2);
    });
    assert!(
        report.passed(),
        "an output committed inside a directory whose rename a power loss can take:\n{}",
        report.summary(&events)
    );
}

/// A crash in the finish of a batch, after two of three directories had
/// their final mode applied and sealed: the resume adopts all three and
/// completes them in one commit, after every seal.
#[test]
fn a_finish_crashed_after_some_seals_completes_every_directory_in_one_commit() {
    let (events, report) = check_siblings("finish", |tree, store| {
        let mut target = Destination::open(&tree.destination, store).unwrap();
        let rows: Vec<&RowSchema> = tree.directories.iter().collect();
        for outcome in target.create_directories(&rows, store, BATCH_AUTHORITY) {
            outcome.unwrap();
        }
        for pending in target.directories.iter().rev().take(2) {
            let (parent, leaf) = target.parent(&pending.path).unwrap();
            let directory = open_dir(&parent, &leaf).unwrap();
            crate::io::sys::fchmod(&directory, pending.mode).unwrap();
            target.seal_entry(&directory).unwrap();
        }
        // The crash: the completion commit never runs.
    });
    assert!(
        report.passed(),
        "a directory completed before its final mode was durable:\n{}",
        report.summary(&events)
    );
    let completions: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            Event::Commit { records, .. } => {
                let completed = records
                    .iter()
                    .filter(|record| matches!(record, CommitRecord::DirectoryComplete { .. }))
                    .count();
                (completed > 0).then_some(completed)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        completions,
        [SIBLINGS.len()],
        "one commit completes the batch"
    );
}

/// #217 review, finding 1: a batch is open (`q/x`, new), then comes `y`, an
/// existing directory whose crashed run's rename into place was never
/// sealed (its record matches), then `y/z`, new. `y/z` must not join the
/// batch: its parent's decision (the adoption's seal of the root) comes
/// first, so `z` is never made inside a `y` a power loss can still turn
/// back into a temporary. The same copy with `y/z` admitted (the design the
/// review refuted) must fail the same invariant.
#[test]
fn a_batch_waits_for_a_held_adoptable_directory_before_making_inside_it() {
    let run = |tag: &str, admit_under_held: bool| {
        let base = scratch(tag);
        let (source, destination) = (base.join("source"), base.join("destination"));
        let (source_state, destination_state) =
            (base.join("source-state"), base.join("destination-state"));
        for directory in ["q", "q/x", "y", "y/z"] {
            std::fs::create_dir(source.join(directory)).unwrap();
            std::fs::set_permissions(
                source.join(directory),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(source.join("y/z/f"), PAYLOAD).unwrap();
        crate::transfer::settle_racy_window(&source).unwrap();
        let canonical_destination = std::fs::canonicalize(&destination).unwrap();
        crate::materialize::set_directory_batch(&canonical_destination, Some(64));
        if admit_under_held {
            crate::transfer::ADMIT_UNDER_HELD
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(canonical_destination.clone());
        }
        // The output authority `receive` will key `y`'s record under.
        let source_store = Store::open(&source_state).unwrap();
        let source_root = std::fs::canonicalize(&source).unwrap();
        let source_meta = std::fs::metadata(&source_root).unwrap();
        let start = postcard::to_stdvec(&(
            source_store.authority().unwrap(),
            source_root.as_os_str().as_encoded_bytes(),
            source_meta.dev(),
            source_meta.ino(),
        ))
        .unwrap();
        drop(source_store);
        let destination_meta = std::fs::metadata(&canonical_destination).unwrap();
        let authority = postcard::to_stdvec(&(
            &start,
            canonical_destination.as_os_str().as_encoded_bytes(),
            destination_meta.dev(),
            destination_meta.ino(),
        ))
        .unwrap();
        let key = postcard::to_stdvec(&(&authority, b"y".as_slice())).unwrap();
        // `q` is already there (a third party's, with the source's mode),
        // so the batch's one member beside `y/z` is made in `q`, and no seal
        // of the root comes between `y/z`'s record and `y`'s adoption.
        std::fs::create_dir(destination.join("q")).unwrap();
        std::fs::set_permissions(
            destination.join("q"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let image = Image::scan(&destination).unwrap();
        let recorder = Recorder::new();
        let _alone = ALONE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stats = {
            let _attached = recorder.attach_process();
            // The crashed run: `y` made, sealed, bound and renamed into
            // place, the rename never sealed (`directory.after_rename`).
            let store = Store::open(&destination_state).unwrap();
            let crashed = Destination::open(&destination, &store).unwrap();
            let root = File::from(crate::io::sys::open_dir_path_nofollow(&destination).unwrap());
            let temporary = crashed.temporary(Some(DIRECTORY_MARK)).unwrap();
            crate::io::sys::mkdirat(&root, &temporary, 0o700).unwrap();
            let metadata = open_dir(&root, &temporary).unwrap().metadata().unwrap();
            crashed.seal_entry(&root).unwrap();
            store
                .record_directory_created(&key, metadata.dev(), metadata.ino(), 0o755)
                .unwrap();
            crate::io::rename_exclusive(&root, &temporary, c"y").unwrap();
            drop((crashed, store));
            crate::transfer::copy(&source, &destination, &source_state, &destination_state).unwrap()
        };
        crate::materialize::set_directory_batch(&canonical_destination, None);
        crate::transfer::ADMIT_UNDER_HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|root| *root != canonical_destination);
        assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
        let events = recorder.take();
        let modes = [b"q/x".as_slice(), b"y", b"y/z"]
            .into_iter()
            .map(|rel| (rel.to_vec(), 0o755))
            .collect();
        let options = Options {
            ignore_foreign: true,
            accept_bounded: true,
            ..Options::default()
        };
        let before = std::collections::BTreeSet::from([b"q".to_vec()]);
        let report = check_view(&image, &events, &options, |view, info| {
            directory_records_hold(&events, view, info, &before, &modes, PAYLOAD)
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&base);
        (events, report)
    };
    let (events, report) = run("held-parent", false);
    assert!(
        report.passed(),
        "a directory made inside a held directory before its adoption:\n{}",
        report.summary(&events)
    );
    let (events, report) = run("held-parent-admitted", true);
    assert!(
        report.violations.iter().any(|violation| violation
            .message
            .contains("inside a directory not yet named")),
        "admitting y/z under the held y must be caught:\n{}",
        report.summary(&events)
    );
}
