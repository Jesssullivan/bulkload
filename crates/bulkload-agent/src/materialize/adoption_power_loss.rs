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
