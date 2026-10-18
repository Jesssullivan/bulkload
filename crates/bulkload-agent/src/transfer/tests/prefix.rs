//! M2r2 A-103: an existing destination file with no owning record that
//! holds a verified byte prefix of its source (an append-only transcript
//! that grew after a landing whose store was lost) is extended in place by
//! the tail, ledgered and counted; every other occupied file is refused
//! `DESTINATION_OCCUPIED` exactly as before.

use super::*;
use crate::counters::Counters;

/// The process-wide counters these rows measure: one row at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `prefix_extensions` rows of the destination store: path, prior size and
/// prior BLAKE3. Read from a copy of the database and its `-wal`.
fn ledger(state: &Path) -> Vec<(Vec<u8>, u64, Vec<u8>)> {
    let copy = state.with_extension("inspect");
    std::fs::create_dir_all(&copy).unwrap();
    let database = copy.join("transfer.sqlite");
    std::fs::copy(state.join("transfer.sqlite"), &database).unwrap();
    let wal = state.join("transfer.sqlite-wal");
    if wal.exists() {
        std::fs::copy(&wal, copy.join("transfer.sqlite-wal")).unwrap();
    }
    let rows = {
        let connection = rusqlite::Connection::open(&database).unwrap();
        let mut statement = connection
            .prepare("SELECT path, prior_size, prior_blake3 FROM prefix_extensions ORDER BY path")
            .unwrap();
        let rows: Vec<(Vec<u8>, u64, Vec<u8>)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        rows
    };
    std::fs::remove_dir_all(&copy).unwrap();
    rows
}

/// A held length past the first chunk (at most 256 KiB), inside a later one.
const HELD: usize = 400_000;

/// The landing is moved by hand (A-103's finding): its destination store is
/// gone, so nothing at the destination is owned.
fn orphan(corpus: &Corpus) {
    std::fs::remove_dir_all(corpus.base.join("destination-state")).unwrap();
}

#[test]
fn a_grown_prefix_is_extended_in_place_ledgered_and_counted() {
    let _serial = serial();
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let full = noise(71, 3 << 20);
    // Not on a chunk boundary: the straddling chunk's head is compared.
    let prior = (2 << 20) + 12_345;
    std::fs::write(source.join("t.jsonl"), &full[..prior]).unwrap();
    // A second file grown by a few bytes past several chunks.
    let small = noise(74, 600_000);
    std::fs::write(source.join("small.jsonl"), &small[..599_990]).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan(&corpus);
    std::fs::write(source.join("t.jsonl"), &full).unwrap();
    std::fs::write(source.join("small.jsonl"), &small).unwrap();
    settle_racy_window(&source).unwrap();
    let inode = std::fs::metadata(destination.join("t.jsonl"))
        .unwrap()
        .ino();
    let before = Counters::snapshot();
    let rerun = corpus.run().unwrap();
    let since = Counters::snapshot().since(before);
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!(std::fs::read(destination.join("t.jsonl")).unwrap(), full);
    assert_eq!(
        std::fs::read(destination.join("small.jsonl")).unwrap(),
        small
    );
    assert_eq!(
        std::fs::metadata(destination.join("t.jsonl"))
            .unwrap()
            .ino(),
        inode,
        "extended in place, never replaced"
    );
    let mut extended = rerun.prefix_extended.clone();
    extended.sort();
    assert_eq!(
        extended,
        vec![
            (
                b"small.jsonl".to_vec(),
                599_990,
                *blake3::hash(&small[..599_990]).as_bytes(),
                600_000
            ),
            (
                b"t.jsonl".to_vec(),
                prior as u64,
                *blake3::hash(&full[..prior]).as_bytes(),
                full.len() as u64
            ),
        ]
    );
    assert_eq!(since.get(Counter::DestPrefixExtended), 2);
    assert_eq!(
        since.get(Counter::DestPrefixAppendWrite),
        (full.len() - prior + 10) as u64
    );
    // The prefix proof reads each held byte once.
    assert_eq!(since.get(Counter::DestVerifyRead), (prior + 599_990) as u64);
    // Only the tail and the chunk it starts in cross: never the whole file.
    assert!(
        rerun.bytes_received
            < (full.len() - prior + 10) as u64 + 2 * u64::from(crate::hash::CDC_MAX_BYTES),
        "{} received",
        rerun.bytes_received
    );
    assert_eq!(
        ledger(&corpus.base.join("destination-state")),
        vec![
            (
                b"small.jsonl".to_vec(),
                599_990,
                blake3::hash(&small[..599_990]).as_bytes().to_vec()
            ),
            (
                b"t.jsonl".to_vec(),
                prior as u64,
                blake3::hash(&full[..prior]).as_bytes().to_vec()
            ),
        ]
    );
    // The extended file is this store's from then on: a further run reuses
    // it and reads nothing.
    let again = corpus.run().unwrap();
    assert!(again.refusals.is_empty());
    assert_eq!((again.reused, again.source_bytes_read), (2, 0));
    assert!(again.prefix_extended.is_empty());
    assert_eq!(
        std::fs::read_dir(&destination).unwrap().count(),
        2,
        "no staged tail is left"
    );
}

/// Every occupied file that is not a strict byte prefix of a regular file
/// with one link and the source's mode keeps today's refusal, untouched;
/// and with the extension off a prefix is refused too.
#[test]
fn anything_but_a_proven_prefix_is_refused_as_before() {
    let _serial = serial();
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let full = noise(72, 1 << 20);
    for name in ["changed", "linked", "moded", "empty", "longer"] {
        std::fs::write(source.join(name), &full[..HELD]).unwrap();
    }
    std::fs::write(source.join("tiny"), &full[..1000]).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan(&corpus);
    for name in ["changed", "linked", "moded", "empty", "longer", "tiny"] {
        std::fs::write(source.join(name), &full).unwrap();
    }
    // "tiny" holds a true prefix, but all of it inside the first chunk:
    // judging it would take that chunk over the wire, so it is refused as
    // before.
    // A changed byte inside the held prefix.
    let mut changed = full[..HELD].to_vec();
    changed[500] ^= 1;
    std::fs::write(destination.join("changed"), &changed).unwrap();
    // A second name for the same bytes.
    std::fs::hard_link(
        destination.join("linked"),
        corpus.base.join("linked-elsewhere"),
    )
    .unwrap();
    std::fs::set_permissions(
        destination.join("moded"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(destination.join("empty"), b"").unwrap();
    // Longer than the source: not a strict prefix.
    std::fs::write(source.join("longer"), &full[..HELD - 1]).unwrap();
    settle_racy_window(&source).unwrap();
    let held: BTreeMap<&str, Vec<u8>> = ["changed", "linked", "moded", "empty", "longer", "tiny"]
        .into_iter()
        .map(|name| (name, std::fs::read(destination.join(name)).unwrap()))
        .collect();
    let rerun = corpus.run().unwrap();
    let mut refused = rerun.refusals.clone();
    refused.sort();
    assert_eq!(
        refused,
        ["changed", "empty", "linked", "longer", "moded", "tiny"]
            .iter()
            .map(|name| (name.as_bytes().to_vec(), "DESTINATION_OCCUPIED".to_owned()))
            .collect::<Vec<_>>()
    );
    assert!(rerun.prefix_extended.is_empty());
    for (name, bytes) in &held {
        assert_eq!(
            &std::fs::read(destination.join(name)).unwrap(),
            bytes,
            "{name}"
        );
    }

    // The extension off: a true prefix is refused as it was before A-103.
    let off = Corpus::new();
    let root = std::fs::canonicalize(off.base.join("destination")).unwrap();
    std::fs::write(off.base.join("source/t"), &full[..HELD]).unwrap();
    assert!(off.run().unwrap().refusals.is_empty());
    orphan(&off);
    std::fs::write(off.base.join("source/t"), &full).unwrap();
    settle_racy_window(&off.base.join("source")).unwrap();
    set_prefix_extend(&root, Some(false));
    let rerun = off.run();
    set_prefix_extend(&root, None);
    let rerun = rerun.unwrap();
    assert_eq!(
        rerun.refusals,
        [(b"t".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(
        std::fs::read(off.base.join("destination/t")).unwrap(),
        &full[..HELD]
    );
}

/// R25 with the extension (#187 review's remembered refusal): a file
/// refused `DESTINATION_OCCUPIED` before the extension existed is proved
/// once more and extended; a file whose proof fails is remembered
/// `NotPrefix`, and an unchanged rerun refuses it with nothing read on
/// either side.
#[test]
fn a_refusal_remembered_before_the_extension_is_proved_once_more() {
    let _serial = serial();
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let root = std::fs::canonicalize(&destination).unwrap();
    let full = noise(73, 1 << 20);
    std::fs::write(source.join("grown"), &full[..HELD]).unwrap();
    std::fs::write(source.join("other"), &full[..HELD]).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan(&corpus);
    std::fs::write(source.join("grown"), &full).unwrap();
    std::fs::write(source.join("other"), &full).unwrap();
    let mut other = full[..HELD].to_vec();
    other[7] ^= 1;
    std::fs::write(destination.join("other"), &other).unwrap();
    settle_racy_window(&source).unwrap();
    settle_racy_window(&destination).unwrap();
    let occupied = |names: &[&str]| -> Vec<(Vec<u8>, String)> {
        names
            .iter()
            .map(|name| (name.as_bytes().to_vec(), "DESTINATION_OCCUPIED".to_owned()))
            .collect()
    };
    // Before A-103 (the extension off): both refused and remembered.
    set_prefix_extend(&root, Some(false));
    let before = corpus.run();
    let remembered = corpus.run();
    set_prefix_extend(&root, None);
    let mut before = before.unwrap().refusals;
    before.sort();
    assert_eq!(before, occupied(&["grown", "other"]));
    assert_eq!(remembered.unwrap().source_bytes_read, 0, "remembered");
    // With it: the grown file is proved and extended; the other's proof
    // fails and is remembered `NotPrefix`.
    let with = corpus.run().unwrap();
    assert_eq!(with.refusals, occupied(&["other"]));
    assert_eq!(std::fs::read(destination.join("grown")).unwrap(), full);
    assert_eq!(with.prefix_extended.len(), 1);
    let again = corpus.run().unwrap();
    assert_eq!(again.refusals, occupied(&["other"]));
    assert_eq!(again.source_bytes_read, 0, "the failed proof is remembered");
    assert_eq!(std::fs::read(destination.join("other")).unwrap(), other);
}
