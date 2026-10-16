//! The folded directory finish ([`Destination::fold_finish`], OI-1003-Q154
//! lever 3) at the materializer: a finish that sets its final modes and
//! then rides no commit leaves every directory one the fallback finish and
//! the next run can still enter.
//!
//! Two ways a fold rides no commit, after its modes are set: the drain's
//! group is already empty when it arrives (the last output closed its group
//! at the file or byte bound, or the idle or age bound closed it first), so
//! the committer drops the finish and [`Destination::finish_directories`]
//! runs as before; or the last group refuses (a publish, its seal or its
//! store commit), so the session refuses and the next run adopts each
//! directory by its record. Either way every later step opens the directory
//! `O_RDONLY | O_DIRECTORY` (`open_dir`, and `parent` for each component),
//! which needs this user's read permission, and lists it in the adoption's
//! sweep. A final mode without owner read, write and search therefore never
//! folds (S1 fold review, 2026-10-16, finding 1).
//!
//! A source directory with such a mode is one this user does not own (its
//! owner could not list it); the walk reads it through its group or other
//! bits. These tests give the row that mode directly, which is what the
//! destination sees either way.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use super::*;

const AUTHORITY: &[u8] = b"fold-authority";

struct Fixture {
    base: PathBuf,
    destination: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(
            self.destination.join("p"),
            std::fs::Permissions::from_mode(0o700),
        );
        crate::materialize::set_directory_batch(&self.destination, None);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A destination and the rows of `p` (final mode `mode`) and `p/q` (0755),
/// in walk order, with batching on.
fn fixture(tag: &str, mode: u32) -> (Fixture, Store, Vec<RowSchema>) {
    let base = std::env::temp_dir().join(format!(
        "bulkload-fold-{tag}-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let source = base.join("source");
    std::fs::create_dir_all(source.join("p/q")).unwrap();
    std::fs::create_dir_all(base.join("destination")).unwrap();
    let destination = std::fs::canonicalize(base.join("destination")).unwrap();
    crate::materialize::set_directory_batch(&destination, Some(16));
    let mut rows: Vec<RowSchema> = crate::walk::walk(
        &crate::walk::WalkOptions::new(source),
        &mut crate::freshness::NullCache,
    )
    .unwrap()
    .rows
    .into_iter()
    .filter(|row| row.kind == bulkload_proto::FileKind::Directory)
    .collect();
    assert_eq!(
        rows.iter()
            .map(|row| row.rel_path.as_slice())
            .collect::<Vec<_>>(),
        [b"p".as_slice(), b"p/q".as_slice()]
    );
    rows[0].mode = (rows[0].mode & !0o7777) | mode;
    rows[1].mode = (rows[1].mode & !0o7777) | 0o755;
    let store = Store::open(&base.join("state")).unwrap();
    (Fixture { base, destination }, store, rows)
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().mode() & 0o7777
}

/// Create both directories, then offer the finish to a last group that
/// commits nothing: what the committer does with a fold when the drain's
/// group is empty or refuses. Returns whether the fold was offered.
fn fold_into_no_commit(target: &mut Destination, store: &Store, rows: &[RowSchema]) -> bool {
    for row in rows {
        target.directory(row, store, AUTHORITY).unwrap();
    }
    let Some(finish) = target.fold_finish() else {
        return false;
    };
    let outcome = finish.outcome();
    drop(finish);
    assert_eq!(target.folded_finish(&outcome), 0);
    true
}

/// Final modes the fold may set early: each keeps owner read, write and
/// search. Any other mode is finished after the last group, as before.
#[test]
fn only_a_final_mode_with_owner_read_write_and_search_folds() {
    for (mode, folds) in [
        (0o700, true),
        (0o755, true),
        (0o711, true),
        (0o311, false),
        (0o333, false),
        (0o355, false),
        (0o555, false),
        (0o600, false),
    ] {
        let (fixture, store, rows) = fixture("rule", mode);
        let mut target = Destination::open(&fixture.destination, &store).unwrap();
        assert_eq!(
            fold_into_no_commit(&mut target, &store, &rows),
            folds,
            "{mode:o}"
        );
        target.finish_directories(&store).unwrap();
        assert_eq!(mode_of(&fixture.destination.join("p")), mode, "{mode:o}");
    }
}

/// The drain's group was already empty, so the folded finish rode no
/// commit: the fallback finish still opens every directory, sets its final
/// mode and completes it.
#[test]
fn a_finish_that_rode_no_group_still_finishes_a_directory_without_owner_read() {
    if crate::io::sys::effective_uid() == 0 {
        return;
    }
    let (fixture, store, rows) = fixture("empty-group", 0o311);
    let mut target = Destination::open(&fixture.destination, &store).unwrap();
    fold_into_no_commit(&mut target, &store, &rows);
    target.finish_directories(&store).unwrap();
    target.flush_session().unwrap();
    assert_eq!(store.directory_records().unwrap(), 0);
    assert_eq!(mode_of(&fixture.destination.join("p/q")), 0o755);
    assert_eq!(mode_of(&fixture.destination.join("p")), 0o311);
}

/// The last group refused, so the session refused with every directory
/// pending: the next run adopts each by its record and finishes it.
#[test]
fn a_resume_after_a_refused_last_group_adopts_a_directory_without_owner_read() {
    if crate::io::sys::effective_uid() == 0 {
        return;
    }
    let (fixture, store, rows) = fixture("refused-group", 0o311);
    {
        let mut target = Destination::open(&fixture.destination, &store).unwrap();
        fold_into_no_commit(&mut target, &store, &rows);
    }
    assert_eq!(store.directory_records().unwrap(), 2);
    let mut target = Destination::open(&fixture.destination, &store).unwrap();
    for row in &rows {
        target.directory(row, &store, AUTHORITY).unwrap();
    }
    target.finish_directories(&store).unwrap();
    target.flush_session().unwrap();
    assert_eq!(store.directory_records().unwrap(), 0);
    assert_eq!(mode_of(&fixture.destination.join("p/q")), 0o755);
    assert_eq!(mode_of(&fixture.destination.join("p")), 0o311);
}
