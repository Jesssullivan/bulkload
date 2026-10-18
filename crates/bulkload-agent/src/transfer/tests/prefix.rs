//! M2r2 A-103: an existing destination file with no owning record that
//! holds a verified byte prefix of its source (an append-only transcript
//! that grew after a landing whose store was lost) is extended in place by
//! the tail, ledgered and counted; every other occupied file is refused
//! `DESTINATION_OCCUPIED` exactly as before.

use super::*;

/// The live-writer hook is process-wide: these rows run one at a time.
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

/// Both sides quiet past the racy window: the source's seats are recorded,
/// and the destination's files are settled, which the extension requires.
fn settle(corpus: &Corpus) {
    settle_racy_window(&corpus.base.join("source")).unwrap();
    settle_racy_window(&corpus.base.join("destination")).unwrap();
}

fn occupied(names: &[&str]) -> Vec<(Vec<u8>, String)> {
    let mut out: Vec<(Vec<u8>, String)> = names
        .iter()
        .map(|name| (name.as_bytes().to_vec(), "DESTINATION_OCCUPIED".to_owned()))
        .collect();
    out.sort();
    out
}

fn sorted(mut refusals: Vec<(Vec<u8>, String)>) -> Vec<(Vec<u8>, String)> {
    refusals.sort();
    refusals
}

/// A copy of `held` of each `(name, held)`, its store lost, each source
/// grown to `full`, settled.
fn landed_then_grown(corpus: &Corpus, files: &[(&str, &[u8])], full: &[u8]) {
    for (name, held) in files {
        std::fs::write(corpus.base.join("source").join(name), held).unwrap();
    }
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan(corpus);
    for (name, _) in files {
        std::fs::write(corpus.base.join("source").join(name), full).unwrap();
    }
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
    settle(&corpus);
    let inode = std::fs::metadata(destination.join("t.jsonl"))
        .unwrap()
        .ino();
    let rerun = corpus.run().unwrap();
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
    // The process-wide counters (`dest_prefix_extended`,
    // `write_dest_prefix_append_bytes`, `read_dest_verify_existing_bytes`)
    // are proven where nothing else runs beside the session: the
    // cross-process row of `tests/loopback_handoff.rs` (serialized) and
    // `tests/prefix_extend_cli.rs` (the binary's own counters line). Lib
    // tests run in parallel, so a delta here would count other rows' reads.
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

/// Every occupied file the extension does not try, or whose proof fails,
/// keeps today's refusal, untouched: a changed byte in a whole chunk, a
/// hard link, another mode, an empty or a longer file, a prefix inside the
/// first chunk, a file this user cannot write (review A4), a path outside
/// the default append-only scope, and a prefix with the extension off.
#[test]
fn anything_but_a_proven_prefix_is_refused_as_before() {
    let _serial = serial();
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let full = noise(72, 1 << 20);
    let names = [
        "changed.jsonl",
        "linked.jsonl",
        "moded.jsonl",
        "empty.jsonl",
        "longer.jsonl",
        "readonly.jsonl",
        "plain.bin",
    ];
    let mut files: Vec<(&str, &[u8])> = names.iter().map(|name| (*name, &full[..HELD])).collect();
    files.push(("tiny.jsonl", &full[..1000]));
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    landed_then_grown(&corpus, &files, &full);
    let mut changed = full[..HELD].to_vec();
    changed[500] ^= 1;
    std::fs::write(destination.join("changed.jsonl"), &changed).unwrap();
    std::fs::hard_link(
        destination.join("linked.jsonl"),
        corpus.base.join("linked-elsewhere"),
    )
    .unwrap();
    std::fs::set_permissions(
        destination.join("moded.jsonl"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(destination.join("empty.jsonl"), b"").unwrap();
    std::fs::write(source.join("longer.jsonl"), &full[..HELD - 1]).unwrap();
    // Read-only on both sides: the same mode, and not writable here.
    for side in [&source, &destination] {
        std::fs::set_permissions(
            side.join("readonly.jsonl"),
            std::fs::Permissions::from_mode(0o444),
        )
        .unwrap();
    }
    settle(&corpus);
    let held: BTreeMap<String, Vec<u8>> = std::fs::read_dir(&destination)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    let rerun = corpus.run().unwrap();
    let mut all: Vec<&str> = names.to_vec();
    all.push("tiny.jsonl");
    // Root writes a 0444 file: it is extended then.
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        all.retain(|name| *name != "readonly.jsonl");
    }
    assert_eq!(sorted(rerun.refusals), occupied(&all));
    for name in &all {
        assert_eq!(
            &std::fs::read(destination.join(name)).unwrap(),
            &held[*name],
            "{name}"
        );
    }

    // `all` scope: the plain file is extended; the extension off: a true
    // prefix is refused as it was before A-103.
    for (scope, extended) in [(PrefixExtend::All, true), (PrefixExtend::Off, false)] {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        landed_then_grown(&corpus, &[("t.bin", &full[..HELD])], &full);
        settle(&corpus);
        set_prefix_extend(&root, Some(scope));
        let rerun = corpus.run();
        set_prefix_extend(&root, None);
        let rerun = rerun.unwrap();
        let now = std::fs::read(corpus.base.join("destination/t.bin")).unwrap();
        if extended {
            assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
            assert_eq!(now, full);
        } else {
            assert_eq!(rerun.refusals, occupied(&["t.bin"]));
            assert_eq!(now, &full[..HELD]);
        }
    }
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
    let destination = corpus.base.join("destination");
    let root = std::fs::canonicalize(&destination).unwrap();
    let full = noise(73, 1 << 20);
    landed_then_grown(
        &corpus,
        &[
            ("grown.jsonl", &full[..HELD]),
            ("other.jsonl", &full[..HELD]),
        ],
        &full,
    );
    let mut other = full[..HELD].to_vec();
    other[7] ^= 1;
    std::fs::write(destination.join("other.jsonl"), &other).unwrap();
    settle(&corpus);
    // Before A-103 (the extension off): both refused and remembered.
    set_prefix_extend(&root, Some(PrefixExtend::Off));
    let before = corpus.run();
    let remembered = corpus.run();
    set_prefix_extend(&root, None);
    assert_eq!(
        sorted(before.unwrap().refusals),
        occupied(&["grown.jsonl", "other.jsonl"])
    );
    assert_eq!(remembered.unwrap().source_bytes_read, 0, "remembered");
    // With it: the grown file is proved and extended; the other's proof
    // fails and is remembered `NotPrefix`.
    let with = corpus.run().unwrap();
    assert_eq!(with.refusals, occupied(&["other.jsonl"]));
    assert_eq!(
        std::fs::read(destination.join("grown.jsonl")).unwrap(),
        full
    );
    assert_eq!(with.prefix_extended.len(), 1);
    let again = corpus.run().unwrap();
    assert_eq!(again.refusals, occupied(&["other.jsonl"]));
    assert_eq!(again.source_bytes_read, 0, "the failed proof is remembered");
    assert_eq!(
        std::fs::read(destination.join("other.jsonl")).unwrap(),
        other
    );
}

/// Review B3 and A3: a file whose whole chunks are the seat's but whose
/// last, partial chunk differs is not a prefix. The head comparison refuses
/// it before a byte is appended, the refusal is remembered `NotPrefix`, and
/// an unchanged rerun neither reads the seat nor receives its tail again.
#[test]
fn a_differing_partial_chunk_is_refused_and_remembered() {
    let _serial = serial();
    let corpus = Corpus::new();
    let destination = corpus.base.join("destination");
    let full = noise(75, 1 << 20);
    landed_then_grown(&corpus, &[("t.jsonl", &full[..HELD])], &full);
    let mut held = full[..HELD].to_vec();
    // The last byte held: past every whole chunk, inside the straddling one.
    held[HELD - 1] ^= 1;
    std::fs::write(destination.join("t.jsonl"), &held).unwrap();
    settle(&corpus);
    let first = corpus.run().unwrap();
    assert_eq!(first.refusals, occupied(&["t.jsonl"]));
    assert!(first.prefix_extended.is_empty());
    assert!(first.bytes_received > 0, "the tail was asked for");
    assert_eq!(std::fs::read(destination.join("t.jsonl")).unwrap(), held);
    let again = corpus.run().unwrap();
    assert_eq!(again.refusals, occupied(&["t.jsonl"]));
    assert_eq!(
        (again.source_bytes_read, again.bytes_received),
        (0, 0),
        "remembered NotPrefix"
    );
}

/// Review A1: a live writer appending while the tail is written. The
/// extension's next piece sees the file end past its own last byte and
/// stops, refused; the writer's bytes are kept where it wrote them, never
/// written over.
#[test]
fn a_live_writer_appending_mid_extension_is_never_overwritten() {
    let _serial = serial();
    let corpus = Corpus::new();
    let destination = corpus.base.join("destination");
    let full = noise(76, 4 << 20);
    let prior = 1 << 20;
    landed_then_grown(&corpus, &[("t.jsonl", &full[..prior])], &full);
    settle(&corpus);
    let path = destination.join("t.jsonl");
    let foreign = b"{\"written\":\"by a live session\"}\n".to_vec();
    let line = foreign.clone();
    let writer = path.clone();
    let mut fired = false;
    set_append_hook(Some(Box::new(move |_| {
        if !fired {
            fired = true;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&writer)
                .unwrap();
            file.write_all(&line).unwrap();
        }
    })));
    let rerun = corpus.run();
    set_append_hook(None);
    let rerun = rerun.unwrap();
    assert_eq!(rerun.refusals, occupied(&["t.jsonl"]));
    assert!(rerun.prefix_extended.is_empty());
    let now = std::fs::read(&path).unwrap();
    // The old prefix and the first appended piece, then the writer's line,
    // whole and in place.
    let piece = prior + (1 << 20);
    assert_eq!(&now[..piece], &full[..piece]);
    assert_eq!(&now[piece..], foreign.as_slice());
}

/// Review A2: a file written within the racy window (a live session's) is
/// not extended: refused as before, not remembered, and extended once it
/// is quiet.
#[test]
fn a_file_written_just_now_is_extended_only_once_it_is_quiet() {
    let _serial = serial();
    let corpus = Corpus::new();
    let destination = corpus.base.join("destination");
    let full = noise(77, 1 << 20);
    landed_then_grown(&corpus, &[("t.jsonl", &full[..HELD])], &full);
    settle_racy_window(&corpus.base.join("source")).unwrap();
    // Touched just now at the destination, its bytes unchanged, and judged
    // against a clock inside its racy window (unit tests otherwise read a
    // clock an hour ahead).
    std::fs::write(destination.join("t.jsonl"), &full[..HELD]).unwrap();
    let clock = PinnedDestinationClock::at(
        &corpus,
        stamp_ns(&destination.join("t.jsonl")) + 500_000_000,
    );
    let busy = corpus.run().unwrap();
    drop(clock);
    assert_eq!(busy.refusals, occupied(&["t.jsonl"]));
    assert_eq!(
        std::fs::read(destination.join("t.jsonl")).unwrap(),
        &full[..HELD]
    );
    settle(&corpus);
    let quiet = corpus.run().unwrap();
    assert!(quiet.refusals.is_empty(), "{:?}", quiet.refusals);
    assert_eq!(std::fs::read(destination.join("t.jsonl")).unwrap(), full);
}
