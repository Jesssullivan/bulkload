//! Superseding publish (WP0(d), OI-1003-Q18, #187), at the materializer.
//!
//! - an output this store wrote, untouched, is replaced by a changed seat's
//!   new bytes, and its old rows leave with it;
//! - a file this store has no matching row for is never one to supersede:
//!   another writer's file, this store's output changed since, or a row
//!   under another authority;
//! - a file that takes the output's place between the last look and the
//!   exchange is exchanged back, never removed;
//! - a file system without an exchange refuses and keeps the old output
//!   with its old row;
//! - what a crash leaves is settled by the next sweep: the old output still
//!   in place gets its rows back, this store's displaced output is removed
//!   and the staged file at the leaf keeps an ownership row,
//!   and a displaced file of anyone else is exchanged back, or kept aside
//!   and reported when the leaf no longer holds the staged file as staged
//!   (another file, or the staged one written since).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Write as _;
use std::path::PathBuf;

use super::*;
use crate::io::durable::GroupSink as _;
use crate::transfer_store::{row_key, PublisherSide};

const AUTHORITY: &[u8] = b"authority";
const OLD: &[u8] = b"the old output's bytes";
const NEW: &[u8] = b"the changed seat's new bytes, longer";
const FOREIGN: &[u8] = b"someone else's file";
const NEWER: &[u8] = b"the seat changed again before the next run";

struct Fixture {
    base: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// A destination whose `file` is this store's own output of `OLD`, with
    /// its row committed.
    fn published() -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-supersede-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        for directory in ["source", "destination"] {
            std::fs::create_dir_all(base.join(directory)).unwrap();
        }
        let fixture = Self { base };
        let row = fixture.seat(OLD);
        let target = fixture.target();
        let staged = Self::staged(&target, &row, OLD);
        let mut sink = fixture.sink();
        sink.commit(vec![Publication::Staged {
            staged,
            record: Self::record(&row),
        }]);
        assert_eq!(sink.finish(), [(b"file".to_vec(), Ok(()))]);
        fixture
    }

    fn destination(&self) -> PathBuf {
        self.base.join("destination")
    }

    fn leaf(&self) -> PathBuf {
        self.destination().join("file")
    }

    fn store(&self) -> Store {
        Store::open(&self.base.join("state")).unwrap()
    }

    fn target(&self) -> Destination {
        Destination::open(&self.destination(), &self.store()).unwrap()
    }

    fn sink(&self) -> PublishSink {
        PublishSink::new(
            self.store()
                .into_publisher(PublisherSide::Destination)
                .unwrap(),
        )
        .unwrap()
    }

    /// Write the source seat and walk its row.
    fn seat(&self, bytes: &[u8]) -> RowSchema {
        let source = self.base.join("source");
        std::fs::write(source.join("file"), bytes).unwrap();
        crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap()
    }

    fn staged(target: &Destination, row: &RowSchema, bytes: &[u8]) -> StagedFile {
        let staged = target.stage(row).unwrap();
        (&**staged.file()).write_all(bytes).unwrap();
        staged
    }

    fn record(row: &RowSchema) -> PendingOutput {
        PendingOutput {
            key: row_key(AUTHORITY, row).unwrap(),
            rel_path: b"file".to_vec(),
            size: row.size,
            racy: false,
            hints: Vec::new(),
        }
    }

    /// What `owned_output` says of the file at the leaf for `row`.
    fn owned(&self, authority: &[u8], row: &RowSchema) -> Option<OwnedOutput> {
        let file = File::open(self.leaf()).unwrap();
        owned_output(&self.store(), authority, row, &file).unwrap()
    }

    /// Queue `NEW` as a superseding publish of the output and commit its
    /// group; returns the group's report.
    fn supersede(&self) -> Vec<(Vec<u8>, Result<()>)> {
        let row = self.seat(NEW);
        let owned = self.owned(AUTHORITY, &row).unwrap();
        let target = self.target();
        let staged = Self::staged(&target, &row, NEW);
        let mut sink = self.sink();
        sink.commit(vec![Publication::Superseding {
            staged,
            record: Self::record(&row),
            owned,
        }]);
        sink.finish()
    }

    /// A superseding publish stopped where a crash would stop it: prepared,
    /// its intent committed, and with `exchanged` its exchange done. Returns
    /// the temporary's path.
    fn interrupted(&self, exchanged: bool, before_exchange: impl FnOnce()) -> PathBuf {
        let row = self.seat(NEW);
        let owned = self.owned(AUTHORITY, &row).unwrap();
        let target = self.target();
        let pending = Self::staged(&target, &row, NEW)
            .prepare_supersede(b"file", owned)
            .unwrap();
        let publisher = self
            .store()
            .into_publisher(PublisherSide::Destination)
            .unwrap();
        publisher
            .begin_supersedes(std::slice::from_ref(&pending.intent))
            .unwrap();
        before_exchange();
        if exchanged {
            crate::io::exchange(
                &pending.staged.parent,
                &pending.staged.temporary,
                &pending.staged.leaf,
            )
            .unwrap();
        }
        self.destination().join(
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(
                pending.staged.temporary.as_bytes(),
            ),
        )
    }

    /// Sweep the destination root as the next session does.
    fn sweep(&self) -> (Sweep, usize) {
        let store = self.store();
        let mut target = Destination::open(&self.destination(), &store).unwrap();
        target.sweep_root(&store).unwrap();
        (target.swept().clone(), target.salvaged())
    }

    fn rows(&self) -> usize {
        self.store().output_rows(AUTHORITY, b"file").unwrap().len()
    }

    fn intents(&self) -> usize {
        self.store().supersede_intents(b"").unwrap().len()
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.destination())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A file of another writer takes the leaf's place: a new inode.
    fn replace_leaf(&self, bytes: &[u8]) {
        let aside = self.base.join("aside");
        std::fs::write(&aside, bytes).unwrap();
        std::fs::rename(&aside, self.leaf()).unwrap();
    }
}

#[test]
fn a_superseding_publish_replaces_this_stores_own_output() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    assert_eq!(old.len(), 1);
    let before = counters::Counters::snapshot();

    assert_eq!(fixture.supersede(), [(b"file".to_vec(), Ok(()))]);

    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEW);
    assert_eq!(fixture.names(), ["file"], "the displaced output is removed");
    let rows = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    assert_eq!(rows.len(), 1, "the old row is dropped with the new one in");
    assert_ne!(rows, old);
    let identity = StatIdentity::from_metadata(&std::fs::metadata(fixture.leaf()).unwrap());
    assert_eq!(rows[0].1, identity_bytes(&identity).unwrap());
    assert_eq!(fixture.intents(), 0, "the intent is settled by the commit");
    let moved = counters::Counters::snapshot().since(before);
    assert!(moved.get(Counter::OutputsSuperseded) >= 1);
    // The new output is this store's own in turn.
    assert!(fixture.owned(AUTHORITY, &fixture.seat(OLD)).is_some());
}

/// WP0(d): only an output whose identity is this store's own row is one to
/// supersede. Without this check a rerun would replace anyone's file.
#[test]
fn a_file_this_store_does_not_own_is_never_one_to_supersede() {
    let fixture = Fixture::published();
    let row = fixture.seat(NEW);
    assert!(fixture.owned(AUTHORITY, &row).is_some(), "its own output");
    assert!(
        fixture.owned(b"another authority", &row).is_none(),
        "rows under another authority prove nothing here"
    );

    // Another writer's file at the path: a new inode.
    fixture.replace_leaf(FOREIGN);
    assert!(fixture.owned(AUTHORITY, &row).is_none());

    // This store's own output, rewritten in place since its row.
    let fixture = Fixture::published();
    let row = fixture.seat(NEW);
    let recorded = std::fs::metadata(fixture.leaf())
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(fixture.leaf(), FOREIGN).unwrap();
    assert!(fixture.owned(AUTHORITY, &row).is_none());
    // Even with its mtime put back: the ctime moved.
    File::options()
        .write(true)
        .open(fixture.leaf())
        .unwrap()
        .set_modified(recorded)
        .unwrap();
    assert!(fixture.owned(AUTHORITY, &row).is_none());

    // A file at a path this store published nothing at.
    std::fs::write(fixture.destination().join("other"), OLD).unwrap();
    let file = File::open(fixture.destination().join("other")).unwrap();
    let mut other = row;
    other.rel_path = b"other".to_vec();
    assert!(owned_output(&fixture.store(), AUTHORITY, &other, &file)
        .unwrap()
        .is_none());
}

/// A publish queued against this store's own output finds another file at
/// the leaf when its group commits: refused before anything is recorded.
#[test]
fn an_output_replaced_before_its_group_commits_is_refused() {
    let fixture = Fixture::published();
    let row = fixture.seat(NEW);
    let owned = fixture.owned(AUTHORITY, &row).unwrap();
    let target = fixture.target();
    let staged = Fixture::staged(&target, &row, NEW);
    fixture.replace_leaf(FOREIGN);
    let mut sink = fixture.sink();
    sink.commit(vec![Publication::Superseding {
        staged,
        record: Fixture::record(&row),
        owned,
    }]);
    assert_eq!(
        sink.finish(),
        [(b"file".to_vec(), Err(BulkloadRefusal::DestinationOccupied))]
    );
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), FOREIGN);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(fixture.intents(), 0);
}

/// The window the exchange design exists for (`MC_wp0d_check_rename`): a
/// third party's file lands between the last look and the exchange. It is
/// exchanged back, and the entry refused.
#[test]
fn a_file_displaced_by_the_exchange_is_put_back() {
    let fixture = Fixture::published();
    let (aside, leaf) = (fixture.base.join("aside"), fixture.leaf());
    std::fs::write(&aside, FOREIGN).unwrap();
    let _hook = set_before_exchange(&fixture.destination(), b"file", move || {
        // Another process, so not this thread's traced calls.
        let _ = std::fs::rename(&aside, &leaf);
    })
    .unwrap();
    let before = counters::Counters::snapshot();

    assert_eq!(
        fixture.supersede(),
        [(b"file".to_vec(), Err(BulkloadRefusal::DestinationOccupied))]
    );

    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), FOREIGN);
    assert_eq!(fixture.names(), ["file"], "the staged file is removed");
    assert_eq!(fixture.intents(), 0);
    assert_eq!(fixture.rows(), 0, "no row vouches for the other file");
    let moved = counters::Counters::snapshot().since(before);
    assert!(moved.get(Counter::SupersedeRestored) >= 1);
}

/// The same window, with the output rewritten in place (the same inode, a
/// later mtime): seen after the exchange, and put back.
#[test]
fn an_output_rewritten_before_the_exchange_is_put_back() {
    let fixture = Fixture::published();
    let leaf = fixture.leaf();
    let _hook = set_before_exchange(&fixture.destination(), b"file", move || {
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        let mut file = File::options().write(true).open(&leaf).unwrap();
        file.write_all(FOREIGN).unwrap();
        file.set_modified(later).unwrap();
    })
    .unwrap();
    assert_eq!(
        fixture.supersede(),
        [(b"file".to_vec(), Err(BulkloadRefusal::DestinationOccupied))]
    );
    assert!(std::fs::read(fixture.leaf()).unwrap().starts_with(FOREIGN));
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(fixture.intents(), 0);
    assert_eq!(fixture.rows(), 0);
}

/// Without an atomic exchange the publish refuses, under its own code, and
/// the old output keeps its old row: no-clobber holds, and nothing is lost.
/// The plan's probe answers the same question before anything is staged
/// (`transfer::tests::a_changed_seat_without_an_exchange_…`); this is the
/// group commit's own answer.
#[test]
fn a_file_system_without_an_exchange_keeps_the_old_output_and_its_row() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    crate::io::force_rename_unsupported(true);
    let report = fixture.supersede();
    crate::io::force_rename_unsupported(false);
    assert_eq!(
        report,
        [(
            b"file".to_vec(),
            Err(BulkloadRefusal::DestinationExchangeUnsupported)
        )]
    );
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), OLD);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(
        fixture.store().output_rows(AUTHORITY, b"file").unwrap(),
        old
    );
    assert_eq!(fixture.intents(), 0);
}

/// A group whose intent cannot be recorded exchanges nothing.
#[test]
fn a_failed_intent_commit_exchanges_nothing() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    let root = fixture.store().root().to_path_buf();
    crate::transfer_store::fail_output_commits(&root, true);
    let report = fixture.supersede();
    crate::transfer_store::fail_output_commits(&root, false);
    assert_eq!(
        report,
        [(
            b"file".to_vec(),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        )]
    );
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), OLD);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(
        fixture.store().output_rows(AUTHORITY, b"file").unwrap(),
        old
    );
    assert_eq!(fixture.intents(), 0);
}

/// Crash after the intent's commit, before the exchange: the old output is
/// still in place, so the sweep gives it its rows back and takes the staged
/// file as this store's orphan.
#[test]
fn a_crash_before_the_exchange_leaves_the_old_output_with_its_old_row() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    let temporary = fixture.interrupted(false, || ());
    assert!(temporary.exists());
    assert_eq!(fixture.rows(), 0, "the rows left with the intent's commit");
    assert_eq!(fixture.intents(), 1);

    let (swept, salvaged) = fixture.sweep();

    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), OLD);
    assert_eq!(
        fixture.store().output_rows(AUTHORITY, b"file").unwrap(),
        old
    );
    assert_eq!(fixture.intents(), 0);
    assert_eq!(salvaged, 1, "the staged file is an orphan of this store");
    assert!(swept.left.is_empty(), "{:?}", swept.left);
}

/// Crash after the exchange, before the displaced output is removed: the
/// sweep removes this store's own displaced output and drops its rows. The
/// new file at the leaf is the one the record says this store staged, so it
/// gets an ownership row and no reuse row: adopted like any unrowed output
/// while its seat is unchanged, and still this store's own to supersede
/// when the seat changes again before a run adopts it (#187 review).
#[test]
fn a_crash_after_the_exchange_leaves_the_new_output_owned_with_no_stale_row() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    let temporary = fixture.interrupted(true, || ());
    assert_eq!(std::fs::read(&temporary).unwrap(), OLD);
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEW);

    let (swept, salvaged) = fixture.sweep();

    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEW);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(swept.removed, 1);
    assert_eq!(salvaged, 0);
    assert_eq!(fixture.intents(), 0);
    let rows = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    let found = StatIdentity::from_metadata(&std::fs::metadata(fixture.leaf()).unwrap());
    assert_eq!(
        rows,
        [(
            crate::transfer_store::path_prefix(AUTHORITY, b"file").unwrap(),
            crate::transfer_store::identity_bytes(&found).unwrap()
        )],
        "the path's ownership row alone"
    );
    for (key, _) in &old {
        assert!(!fixture.store().output_matches(key, &found).unwrap());
    }
    let store = fixture.store();
    let reuse_key = row_key(AUTHORITY, &fixture.seat(NEW)).unwrap();
    assert!(
        !store.output_matches(&reuse_key, &found).unwrap(),
        "an ownership row is never a reuse key"
    );

    // The seat changes once more before any run adopted the new file: it
    // is this store's own, so it is superseded, not refused for ever.
    let row = fixture.seat(NEWER);
    let owned = fixture.owned(AUTHORITY, &row).unwrap();
    let target = fixture.target();
    let staged = Fixture::staged(&target, &row, NEWER);
    let mut sink = fixture.sink();
    sink.commit(vec![Publication::Superseding {
        staged,
        record: Fixture::record(&row),
        owned,
    }]);
    assert_eq!(sink.finish(), [(b"file".to_vec(), Ok(()))]);
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEWER);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(
        fixture.rows(),
        1,
        "the ownership row left with the old file"
    );
    assert_eq!(fixture.intents(), 0);
}

/// The record proves only the file this store staged. One written since
/// the exchange (another size or mtime) gets no ownership row: it is
/// adopted if its bytes verify, and never replaced.
#[test]
fn a_new_output_written_after_the_exchange_is_not_owned_by_the_sweep() {
    let fixture = Fixture::published();
    let temporary = fixture.interrupted(true, || ());
    assert_eq!(std::fs::read(&temporary).unwrap(), OLD);
    std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.leaf())
        .unwrap()
        .write_all(b" and a third party's line")
        .unwrap();

    let (swept, _) = fixture.sweep();

    assert_eq!(swept.removed, 1, "this store's displaced output");
    assert!(std::fs::read(fixture.leaf()).unwrap().starts_with(NEW));
    assert_eq!(fixture.rows(), 0, "no row vouches for a file written since");
    assert_eq!(fixture.intents(), 0);
    assert!(fixture.owned(AUTHORITY, &fixture.seat(NEW)).is_none());
}

/// `MC_neg_sweep_displaced`: a crash between the exchange and the check of
/// what it displaced leaves another writer's file under a temporary name.
/// The sweep exchanges it back; it never removes it like a temporary.
#[test]
fn a_displaced_file_of_another_writer_is_restored_by_the_sweep() {
    let fixture = Fixture::published();
    let temporary = fixture.interrupted(true, || fixture.replace_leaf(FOREIGN));
    assert_eq!(std::fs::read(&temporary).unwrap(), FOREIGN);
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEW);

    let (swept, salvaged) = fixture.sweep();

    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), FOREIGN);
    assert_eq!(fixture.names(), ["file"]);
    assert_eq!(swept.removed, 1, "the staged file, after the exchange back");
    assert!(swept.left.is_empty());
    assert_eq!(salvaged, 0);
    assert_eq!(fixture.rows(), 0);
    assert_eq!(fixture.intents(), 0);
}

/// The displaced file is exchanged back only over the staged file as it was
/// staged. A crash between the exchange and its displaced check leaves the
/// new file at the leaf; written since (an application's commit after a
/// reboot, the normal case for a `SQLite` snapshot, OI-1003-Q146), by size or
/// by mtime alone, it is no longer the file this store staged: exchanging
/// back and unlinking the staged name would delete those writes. Both stay
/// where they are, the displaced file kept aside and reported with its
/// record, as when the leaf holds another file.
#[test]
fn a_displaced_file_is_not_put_back_over_writes_to_the_new_output() {
    for grown in [true, false] {
        let fixture = Fixture::published();
        let temporary = fixture.interrupted(true, || fixture.replace_leaf(FOREIGN));
        assert_eq!(std::fs::read(&temporary).unwrap(), FOREIGN);
        let mut file = File::options().write(true).open(fixture.leaf()).unwrap();
        if grown {
            file.set_len(NEW.len() as u64 + 16).unwrap();
        } else {
            file.write_all(b"THE").unwrap();
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
                .unwrap();
        }
        drop(file);
        let written = std::fs::read(fixture.leaf()).unwrap();
        let name = temporary.file_name().unwrap().as_encoded_bytes().to_vec();

        let (swept, salvaged) = fixture.sweep();

        assert_eq!(
            std::fs::read(fixture.leaf()).unwrap(),
            written,
            "grown={grown}: the writes made to the new file are kept"
        );
        assert_eq!(std::fs::read(&temporary).unwrap(), FOREIGN, "grown={grown}");
        assert_eq!(swept.left, std::slice::from_ref(&name), "grown={grown}");
        assert_eq!(swept.removed, 0, "grown={grown}");
        assert_eq!(salvaged, 0, "grown={grown}");
        assert_eq!(fixture.intents(), 1, "grown={grown}: its record is kept");
    }
}

/// The displaced file cannot be exchanged back once the leaf holds
/// something else: it stays under its temporary name, reported on every
/// run and never removed, and nothing at the leaf is touched.
#[test]
fn a_displaced_file_that_cannot_go_back_is_kept_aside_and_reported() {
    let fixture = Fixture::published();
    let temporary = fixture.interrupted(true, || fixture.replace_leaf(FOREIGN));
    fixture.replace_leaf(b"a third file");
    let name = temporary.file_name().unwrap().as_encoded_bytes().to_vec();

    for _ in 0..2 {
        let (swept, salvaged) = fixture.sweep();
        assert_eq!(swept.left, std::slice::from_ref(&name));
        assert_eq!(swept.removed, 0);
        assert_eq!(salvaged, 0);
        assert_eq!(std::fs::read(&temporary).unwrap(), FOREIGN);
        assert_eq!(std::fs::read(fixture.leaf()).unwrap(), b"a third file");
        assert_eq!(fixture.intents(), 1, "its record keeps it out of the sweep");
    }
}

/// A crash that lost the exchange but kept the unlink that followed it (the
/// checker's persistence model allows it): the staged name is gone and the
/// old output is in place, so it gets its rows back.
#[test]
fn an_intent_with_no_temporary_restores_the_old_row_or_none() {
    let fixture = Fixture::published();
    let old = fixture.store().output_rows(AUTHORITY, b"file").unwrap();
    let temporary = fixture.interrupted(false, || ());
    std::fs::remove_file(&temporary).unwrap();
    fixture.sweep();
    assert_eq!(
        fixture.store().output_rows(AUTHORITY, b"file").unwrap(),
        old
    );
    assert_eq!(fixture.intents(), 0);

    // The exchange and the unlink both kept: the new file, with its
    // ownership row and no other.
    let fixture = Fixture::published();
    let temporary = fixture.interrupted(true, || ());
    std::fs::remove_file(&temporary).unwrap();
    fixture.sweep();
    assert_eq!(std::fs::read(fixture.leaf()).unwrap(), NEW);
    assert_eq!(
        fixture.store().output_rows(AUTHORITY, b"file").unwrap(),
        [(
            crate::transfer_store::path_prefix(AUTHORITY, b"file").unwrap(),
            crate::transfer_store::identity_bytes(&StatIdentity::from_metadata(
                &std::fs::metadata(fixture.leaf()).unwrap()
            ))
            .unwrap()
        )]
    );
    assert_eq!(fixture.intents(), 0);
}
