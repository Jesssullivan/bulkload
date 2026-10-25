//! A pass after a source reboot ([`source_dev`]): macOS renumbers an APFS
//! volume's `st_dev` across a reboot, so the rows an earlier pass keyed
//! under the source root's old device are still this store's own. The
//! estate-20261022 incident (2026-10-23): pass 5, after neo rebooted
//! (`st_dev` 16777234 to 16777232), refused 981 of the 988 seats of its own
//! carry pull `DESTINATION_OCCUPIED` with no output superseded, where pass 4
//! had superseded its own 962.
//!
//! Each test lands a corpus, then rewrites the destination store's keys as a
//! build before the fix wrote them under the root's earlier device
//! ([`keyed_before`]): the rows on disk in the live move states are such
//! rows.

use super::*;

/// An output authority as `receive` composes it.
type OutputAuthority = (Vec<u8>, Vec<u8>, u64, u64);
/// A source authority as `serve` composes it: the source store's
/// authority, the root's path, device and inode.
type SourceAuthority = ([u8; 32], Vec<u8>, u64, u64);

/// `authority` with its source part rewritten by `remap`, and the root's
/// device before and after.
fn remapped(authority: &[u8], remap: &dyn Fn(&mut SourceAuthority)) -> (Vec<u8>, u64, u64) {
    let (source, destination, dev, ino): OutputAuthority =
        bulkload_proto::frame::decode_exact(authority).unwrap();
    let mut source: SourceAuthority = bulkload_proto::frame::decode_exact(&source).unwrap();
    let was = source.2;
    remap(&mut source);
    let now = source.2;
    let source = postcard::to_stdvec(&source).unwrap();
    (
        postcard::to_stdvec(&(source, destination, dev, ino)).unwrap(),
        was,
        now,
    )
}

/// Rewrite every key of the destination store at `state` as a pass whose
/// source authority `remap` gives would have written it; returns how many
/// keys moved. A seat on the root's device gets the root's other device
/// too, as every seat beneath an APFS root renumbers with it.
fn keyed_before(state: &Path, remap: &dyn Fn(&mut SourceAuthority)) -> usize {
    let connection = rusqlite::Connection::open(state.join("transfer.sqlite")).unwrap();
    let mut moved = 0;
    for table in [
        "outputs",
        "owned_outputs",
        "consolidated",
        "refused_outputs",
    ] {
        let keys: Vec<Vec<u8>> = connection
            .prepare(&format!("SELECT key FROM {table}"))
            .unwrap()
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|key| key.unwrap())
            .collect();
        for key in keys {
            // A key is postcard of `(authority, row)` (an ownership row's,
            // `(authority, rel_path)`): the two encodings in turn.
            let (authority, rest) = postcard::take_from_bytes::<Vec<u8>>(&key).unwrap();
            let (authority, was, now) = remapped(&authority, remap);
            let mut rekeyed = postcard::to_stdvec(&authority).unwrap();
            if table == "owned_outputs" {
                rekeyed.extend_from_slice(rest);
            } else {
                let (mut row, tail) = postcard::take_from_bytes::<RowSchema>(rest).unwrap();
                if row.dev == was {
                    row.dev = now;
                }
                rekeyed.extend(postcard::to_stdvec(&row).unwrap());
                rekeyed.extend_from_slice(tail);
            }
            moved += connection
                .execute(
                    &format!("UPDATE {table} SET key = ?1 WHERE key = ?2"),
                    (&rekeyed, &key),
                )
                .unwrap();
        }
    }
    moved
}

/// The root's device as it was before the reboot.
fn earlier_dev(source: &mut SourceAuthority) {
    source.2 = source.2.wrapping_add(2);
}

/// How many output rows the destination store holds.
fn outputs(corpus: &Corpus) -> u64 {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .row_counts()
        .unwrap()
        .1
}

fn refused_outputs(corpus: &Corpus) -> u64 {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .conn_count("refused_outputs")
        .unwrap()
}

/// A changed seat whose output an earlier pass wrote, before the source
/// root's device was renumbered, supersedes that output: its rows are this
/// store's own. The old row goes with the supersede, and the run after
/// reads nothing.
#[test]
fn a_rerun_after_a_source_root_device_change_supersedes_its_own_output() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(901, 400_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    let state = corpus.base.join("destination-state");
    assert_eq!(keyed_before(&state, &earlier_dev), 1);

    let changed = noise(902, 420_000);
    std::fs::write(&seat, &changed).unwrap();
    let rerun = corpus.run().unwrap();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!((rerun.reused, rerun.completed), (0, 1));
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        changed
    );
    assert_eq!(outputs(&corpus), 1, "the earlier pass's row is dropped");
    assert_eq!(
        std::fs::read_dir(corpus.base.join("destination"))
            .unwrap()
            .count(),
        1,
        "the displaced output is removed, nothing is kept beside it"
    );

    let warm = corpus.run().unwrap();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// An unchanged seat whose row an earlier pass wrote under the root's
/// earlier device is reused: the source reads nothing for it (SPEC rule 8).
#[test]
fn an_unchanged_seat_is_reused_across_a_source_root_device_change() {
    let corpus = Corpus::new();
    let bytes = noise(903, 300_000);
    std::fs::write(corpus.base.join("source/seat"), &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    keyed_before(&corpus.base.join("destination-state"), &earlier_dev);

    let rerun = corpus.run().unwrap();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!((rerun.reused, rerun.source_bytes_read), (1, 0));
    assert_eq!(rerun.bytes_received, 0);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        bytes
    );
}

/// The live incident, pass by pass: pass 4's rows are keyed under the
/// root's earlier device; pass 5, by a build that did not look under it,
/// refuses the changed seat `DESTINATION_OCCUPIED` and remembers that
/// refusal under the new device; the fixed build's next pass finds the file
/// is still exactly its own output and supersedes it, the remembered
/// refusal notwithstanding.
#[test]
fn a_refusal_remembered_by_a_pass_that_missed_its_own_rows_does_not_stand() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(904, 350_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    keyed_before(&corpus.base.join("destination-state"), &earlier_dev);
    let changed = noise(905, 360_000);
    std::fs::write(&seat, &changed).unwrap();

    crate::transfer::source_dev::off(true);
    let missed = corpus.run();
    crate::transfer::source_dev::off(false);
    let missed = missed.unwrap();
    assert_eq!(
        missed.refusals,
        [(b"seat".to_vec(), "DESTINATION_OCCUPIED".to_owned())],
        "a build without the fix refuses its own output"
    );
    assert_eq!(refused_outputs(&corpus), 1, "and remembers the refusal");

    let fixed = corpus.run().unwrap();
    assert!(fixed.refusals.is_empty(), "{:?}", fixed.refusals);
    assert_eq!(fixed.completed, 1);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        changed
    );
    let warm = corpus.run().unwrap();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// A file at the path that is not the one the earlier pass recorded (a
/// third party wrote it since) is refused and kept, exactly as before: only
/// where the rows are looked for widens, never what they must match.
#[test]
fn a_file_another_wrote_is_still_refused_across_a_source_root_device_change() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(906, 300_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    keyed_before(&corpus.base.join("destination-state"), &earlier_dev);

    let theirs = noise(907, 310_000);
    std::fs::write(corpus.base.join("destination/seat"), &theirs).unwrap();
    std::fs::write(&seat, noise(908, 320_000)).unwrap();
    let rerun = corpus.run().unwrap();
    assert_eq!(
        rerun.refusals,
        [(b"seat".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        theirs,
        "never overwritten"
    );
    // Refused again, not superseded, on the next run too.
    let again = corpus.run().unwrap();
    assert_eq!(
        again.refusals,
        [(b"seat".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        theirs
    );
}

/// Only the device is let go: rows keyed under another source root (here
/// another inode at the path, as a root removed and made again would have)
/// are not this session's, and the file at the path is refused.
#[test]
fn rows_of_another_source_root_are_never_taken_for_its_own() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(909, 300_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    keyed_before(
        &corpus.base.join("destination-state"),
        &|source: &mut SourceAuthority| {
            source.2 = source.2.wrapping_add(2);
            source.3 = source.3.wrapping_add(1);
        },
    );
    let ours = std::fs::read(corpus.base.join("destination/seat")).unwrap();
    std::fs::write(&seat, noise(910, 310_000)).unwrap();
    let rerun = corpus.run().unwrap();
    assert_eq!(
        rerun.refusals,
        [(b"seat".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        ours
    );
}

/// Repeated source reboots retain all earlier device aliases. A changed
/// file supersedes its own output, a stable file is never re-read and a
/// warm pass after each reboot reads nothing (SPEC rules 6 and 8).
#[test]
fn successive_source_reboots_keep_reuse_and_supersede_ownership() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    std::fs::write(source.join("stable"), noise(911, 310_000)).unwrap();
    std::fs::write(source.join("changing"), noise(912, 320_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    let state = corpus.base.join("destination-state");
    for reboot in 1_u64..=3 {
        keyed_before(&state, &|authority| {
            authority.2 = authority.2.wrapping_add(2 * reboot);
        });
        let changed = noise(
            912 + reboot,
            320_000 + usize::try_from(reboot).unwrap() * 1000,
        );
        std::fs::write(source.join("changing"), &changed).unwrap();
        let resumed = corpus.run().unwrap();
        assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
        assert_eq!(resumed.reused, 1, "the stable file survives every reboot");
        assert_eq!(resumed.completed, 1);
        assert!(resumed.source_bytes_read <= changed.len() as u64);
        assert_eq!(
            std::fs::read(corpus.base.join("destination/changing")).unwrap(),
            changed
        );
        assert_eq!(outputs(&corpus), 2, "no duplicate output records");
        let warm = corpus.run().unwrap();
        assert!(warm.refusals.is_empty());
        assert_eq!((warm.reused, warm.source_bytes_read), (2, 0));
        assert_eq!(
            std::fs::read_dir(corpus.base.join("destination"))
                .unwrap()
                .count(),
            2,
            "no beside copies after repeated source reboots"
        );
    }
}
