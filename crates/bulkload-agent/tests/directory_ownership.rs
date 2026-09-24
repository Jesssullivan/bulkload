//! PR #60 review regressions (R-N102, TIN-4546): directory ownership and
//! temporary custody. Ported from the reviewer's adversarial tests at 009a280;
//! each one asserted a hazard there and asserts its absence here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bulkload_agent::freshness::NullCache;
use bulkload_agent::materialize::Destination;
use bulkload_agent::transfer::{copy, TransferStats};
use bulkload_agent::transfer_store::Store;
use bulkload_agent::walk::{walk, WalkOptions};
use bulkload_proto::{BulkloadRefusal, RowSchema};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-pr60-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for sub in ["source", "destination"] {
            std::fs::create_dir_all(base.join(sub)).unwrap();
        }
        Self(std::fs::canonicalize(base).unwrap())
    }

    fn state(&self) -> PathBuf {
        self.0.join("destination-state")
    }

    fn copy(&self) -> TransferStats {
        copy(
            &self.0.join("source"),
            &self.0.join("destination"),
            &self.0.join("source-state"),
            &self.state(),
        )
        .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn private_directory(path: &Path) {
    std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn nested_row(scratch: &Scratch, row_mode: u32) -> RowSchema {
    std::fs::create_dir(scratch.0.join("source/nested")).unwrap();
    let mut row = walk(&WalkOptions::new(scratch.0.join("source")), &mut NullCache)
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.rel_path == b"nested")
        .unwrap();
    row.mode = 0o40_000 | row_mode;
    row
}

fn key(row: &RowSchema) -> Vec<u8> {
    postcard::to_stdvec(&(b"authority".as_slice(), &row.rel_path)).unwrap()
}

/// Bind `row`'s record to a live directory that is not the one at its path:
/// the strongest stale record a crash can leave behind.
fn stale_record(scratch: &Scratch, store: &Store, row: &RowSchema, row_mode: u32) {
    let elsewhere = scratch.0.join("elsewhere");
    private_directory(&elsewhere);
    let metadata = std::fs::metadata(&elsewhere).unwrap();
    store
        .record_directory_created(&key(row), metadata.dev(), metadata.ino(), row_mode)
        .unwrap();
}

/// Was `refutes_intent_adopts_foreign_nonempty_private_directory`. A user's
/// own 0700 directory holding a key file, at a path with a stale record, is
/// refused and left untouched: no path-only record exists to adopt it.
#[test]
fn foreign_nonempty_private_directory_is_refused_and_untouched() {
    let scratch = Scratch::new("foreign");
    let row = nested_row(&scratch, 0o755);
    let destination = scratch.0.join("destination");
    let store = Store::open(&scratch.state()).unwrap();
    stale_record(&scratch, &store, &row, 0o755);
    let foreign = destination.join("nested");
    private_directory(&foreign);
    std::fs::write(foreign.join("id_ed25519"), b"user secret").unwrap();

    let mut resumed = Destination::open(&destination, &store).unwrap();
    assert!(matches!(
        resumed.directory(&row, &store, b"authority"),
        Err(BulkloadRefusal::GitDestinationOccupied)
    ));
    resumed.finish_directories(&store).unwrap();
    assert_eq!(mode(&foreign), 0o700, "foreign private directory widened");
    assert_eq!(
        std::fs::read(foreign.join("id_ed25519")).unwrap(),
        b"user secret"
    );
    assert_eq!(store.directory_record(&key(&row)).unwrap(), None);
}

/// A symlink at the path is never followed or adopted, even with a record
/// bound to the link target's own inode.
#[test]
fn symlink_at_directory_path_is_not_followed() {
    let scratch = Scratch::new("symlink");
    let row = nested_row(&scratch, 0o755);
    let destination = scratch.0.join("destination");
    let outside = scratch.0.join("outside");
    private_directory(&outside);
    std::os::unix::fs::symlink(&outside, destination.join("nested")).unwrap();
    let store = Store::open(&scratch.state()).unwrap();
    let target = std::fs::metadata(&outside).unwrap();
    store
        .record_directory_created(&key(&row), target.dev(), target.ino(), 0o755)
        .unwrap();
    let mut resumed = Destination::open(&destination, &store).unwrap();
    assert!(resumed.directory(&row, &store, b"authority").is_err());
    resumed.finish_directories(&store).unwrap();
    assert_eq!(mode(&outside), 0o700);
}

/// Was `refutes_stale_intent_survives_and_adopts_later` (F3). A record that
/// does not own the directory at its path is cleared on first sight, so a
/// later chmod to 0700 is never adopted and re-widened.
#[test]
fn stale_record_is_cleared_and_never_adopts_later() {
    let scratch = Scratch::new("stale");
    let row = nested_row(&scratch, 0o755);
    let destination = scratch.0.join("destination");
    let store = Store::open(&scratch.state()).unwrap();
    stale_record(&scratch, &store, &row, 0o755);
    let foreign = destination.join("nested");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut first = Destination::open(&destination, &store).unwrap();
    first.directory(&row, &store, b"authority").unwrap();
    first.finish_directories(&store).unwrap();
    assert_eq!(store.directory_record(&key(&row)).unwrap(), None);

    std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut later = Destination::open(&destination, &store).unwrap();
    assert!(matches!(
        later.directory(&row, &store, b"authority"),
        Err(BulkloadRefusal::GitDestinationOccupied)
    ));
    later.finish_directories(&store).unwrap();
    assert_eq!(mode(&foreign), 0o700, "a user's later chmod was reverted");
}

/// Pre-existing, not a regression: a source mode change between a crash and
/// the resume fails closed at 0700 (the record names the old mode, so it is
/// cleared and never adopts). Converging on the new mode is a follow-up.
#[test]
fn mode_change_between_crash_and_resume_fails_closed() {
    let scratch = Scratch::new("wedge");
    let row = nested_row(&scratch, 0o555);
    let destination = scratch.0.join("destination");
    let store = Store::open(&scratch.state()).unwrap();
    {
        let mut first = Destination::open(&destination, &store).unwrap();
        first.directory(&row, &store, b"authority").unwrap();
        // Crash: finish_directories never runs.
    }
    let mut changed = row.clone();
    changed.mode = 0o40_750;
    let mut resumed = Destination::open(&destination, &store).unwrap();
    assert!(matches!(
        resumed.directory(&changed, &store, b"authority"),
        Err(BulkloadRefusal::GitDestinationOccupied)
    ));
    assert_eq!(mode(&destination.join("nested")), 0o700);
    assert_eq!(store.directory_record(&key(&row)).unwrap(), None);
}

/// Was `refutes_untagged_grammar_user_file_silently_dropped_by_transfer`
/// (F2). Untagged names are payload and are carried; a tagged temporary in
/// the source is not carried but is reported in the receiver's stats.
#[test]
fn untagged_names_are_carried_and_tagged_exclusions_are_reported() {
    let scratch = Scratch::new("walk");
    let source = scratch.0.join("source");
    std::fs::write(source.join(".bulkload-2026-09"), b"september notes").unwrap();
    std::fs::write(source.join(".bulkload-2026-9"), b"untagged grammar").unwrap();
    std::fs::write(source.join(".bulkload-00112233aabbccdd-4-5"), b"orphan").unwrap();
    std::fs::write(source.join("keep"), b"k").unwrap();
    let stats = scratch.copy();
    let destination = scratch.0.join("destination");
    assert!(destination.join("keep").exists());
    assert_eq!(
        std::fs::read(destination.join(".bulkload-2026-09")).unwrap(),
        b"september notes"
    );
    assert_eq!(
        std::fs::read(destination.join(".bulkload-2026-9")).unwrap(),
        b"untagged grammar"
    );
    assert!(!destination.join(".bulkload-00112233aabbccdd-4-5").exists());
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        stats.source_engine_temporaries,
        vec![b".bulkload-00112233aabbccdd-4-5".to_vec()]
    );
    assert_eq!(stats.completed, 3);
}

/// The sweep never touches a user file whose other name is in the untagged
/// grammar: that name is recorded as left, and both names survive.
#[test]
fn sweep_of_user_hardlink_keeps_user_name() {
    let scratch = Scratch::new("hardlink");
    std::fs::write(scratch.0.join("source/a"), b"a").unwrap();
    scratch.copy();
    let destination = scratch.0.join("destination");
    std::fs::write(destination.join("user-file"), b"user").unwrap();
    std::fs::hard_link(
        destination.join("user-file"),
        destination.join(".bulkload-1-1"),
    )
    .unwrap();
    let stats = scratch.copy();
    assert_eq!(stats.temporaries_removed, 0);
    assert_eq!(stats.temporaries_left, vec![b".bulkload-1-1".to_vec()]);
    assert_eq!(
        std::fs::read(destination.join("user-file")).unwrap(),
        b"user"
    );
    assert_eq!(
        std::fs::metadata(destination.join("user-file"))
            .unwrap()
            .nlink(),
        2
    );
}
