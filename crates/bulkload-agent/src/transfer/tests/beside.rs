//! `--beside SUFFIX` on `pull` and `copy` (plan of record 2026-10-19, Step C
//! gaps C1 and C2; SPEC R2.2 and C10: a lossless union into a live
//! destination that never clobbers it).
//!
//! An existing destination file bulkload did not write and whose bytes are
//! not the seat's lands beside, at `<path><SUFFIX>`, accounted
//! `applied-beside`; a byte-identical file is still adopted; a differing
//! symlink is created beside; a database with a live sidecar beside it lands
//! its snapshot beside; a verified byte prefix is still extended, and a
//! prefix whose straddling chunk differs lands whole beside; a beside path
//! holding something of its own is refused; without the flag every
//! differing file is refused `DESTINATION_OCCUPIED` exactly as before; and
//! an unchanged rerun reuses the beside output with 0 source bytes (S3).

use super::*;
use rusqlite::Connection;

const SUFFIX: &[u8] = b".neo-20261019";

/// Both sides quiet past the racy window.
fn settle(corpus: &Corpus) {
    settle_racy_window(&corpus.base.join("source")).unwrap();
    settle_racy_window(&corpus.base.join("destination")).unwrap();
}

/// A corpus whose destination root runs with `--beside SUFFIX`.
struct Beside {
    corpus: Corpus,
    root: PathBuf,
}

impl Beside {
    fn new() -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        set_beside(&root, Some(Some(SUFFIX.to_vec())));
        Self { corpus, root }
    }

    fn source(&self) -> PathBuf {
        self.corpus.base.join("source")
    }

    fn destination(&self) -> PathBuf {
        self.corpus.base.join("destination")
    }

    fn run(&self) -> TransferStats {
        self.corpus.run().unwrap()
    }

    fn snapshot(&self) -> TransferStats {
        copy_with(
            &self.source(),
            &self.destination(),
            &self.corpus.base.join("source-state"),
            &self.corpus.base.join("destination-state"),
            SqliteMode::Snapshot,
        )
        .unwrap()
    }

    /// Run once with the flag off for this root.
    fn run_without(&self) -> TransferStats {
        set_beside(&self.root, Some(None));
        let stats = self.corpus.run().unwrap();
        set_beside(&self.root, Some(Some(SUFFIX.to_vec())));
        stats
    }
}

impl Drop for Beside {
    fn drop(&mut self) {
        set_beside(&self.root, None);
        let source = std::fs::canonicalize(self.source()).unwrap_or_default();
        assume_uid(&source, None);
    }
}

fn beside_name(name: &str) -> String {
    format!("{name}{}", String::from_utf8_lossy(SUFFIX))
}

fn applied(stats: &TransferStats) -> Vec<(String, String)> {
    stats
        .applied_beside
        .iter()
        .map(|(path, beside)| {
            (
                String::from_utf8_lossy(path).into_owned(),
                String::from_utf8_lossy(beside).into_owned(),
            )
        })
        .collect()
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_differing_file_lands_beside_and_an_identical_one_is_adopted() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let ours = noise(301, 300_000);
    let theirs = noise(302, 200_000);
    let same = noise(303, 250_000);
    std::fs::write(source.join("settings.json"), &ours).unwrap();
    std::fs::write(destination.join("settings.json"), &theirs).unwrap();
    std::fs::write(source.join("same.json"), &same).unwrap();
    std::fs::write(destination.join("same.json"), &same).unwrap();
    std::fs::write(source.join("fresh.json"), b"new here\n").unwrap();
    settle(&beside.corpus);
    let live = std::fs::metadata(destination.join("settings.json")).unwrap();

    let stats = beside.run();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        applied(&stats),
        [("settings.json".to_owned(), beside_name("settings.json"))]
    );
    // The live file is untouched, byte for byte and by inode.
    assert_eq!(
        std::fs::read(destination.join("settings.json")).unwrap(),
        theirs
    );
    assert_eq!(
        std::fs::metadata(destination.join("settings.json"))
            .unwrap()
            .ino(),
        live.ino()
    );
    assert_eq!(
        std::fs::read(destination.join(beside_name("settings.json"))).unwrap(),
        ours
    );
    // The identical file is adopted in place: no beside copy of it.
    assert!(!destination.join(beside_name("same.json")).exists());
    assert_eq!(
        std::fs::read(destination.join("fresh.json")).unwrap(),
        b"new here\n"
    );
    assert_eq!(
        entries(&destination),
        [
            "fresh.json",
            "same.json",
            "settings.json",
            &beside_name("settings.json")
        ]
    );
    assert_eq!(
        stats.completed, 3,
        "beside, adopted and fresh all completed"
    );

    // An unchanged rerun reuses the beside output under its own row: 0
    // source bytes, nothing received, still accounted applied-beside (S3).
    let warm = beside.run();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!((warm.source_bytes_read, warm.bytes_received), (0, 0));
    assert_eq!(
        applied(&warm),
        [("settings.json".to_owned(), beside_name("settings.json"))]
    );
    assert_eq!(warm.reused, 3);

    // The seat changes on the source: the beside output is this store's own
    // and is superseded, the live file still untouched.
    let newer = noise(304, 310_000);
    std::fs::write(source.join("settings.json"), &newer).unwrap();
    settle(&beside.corpus);
    let again = beside.run();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        std::fs::read(destination.join(beside_name("settings.json"))).unwrap(),
        newer
    );
    assert_eq!(
        std::fs::read(destination.join("settings.json")).unwrap(),
        theirs
    );
    assert_eq!(
        applied(&again),
        [("settings.json".to_owned(), beside_name("settings.json"))]
    );
}

#[test]
fn without_the_flag_a_differing_file_is_still_refused() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    std::fs::write(source.join("settings.json"), noise(311, 100_000)).unwrap();
    let theirs = noise(312, 90_000);
    std::fs::write(destination.join("settings.json"), &theirs).unwrap();
    settle(&beside.corpus);
    let stats = beside.run_without();
    assert_eq!(
        stats.refusals,
        [(b"settings.json".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert!(stats.applied_beside.is_empty());
    assert_eq!(entries(&destination), ["settings.json"]);
    assert_eq!(
        std::fs::read(destination.join("settings.json")).unwrap(),
        theirs
    );
}

#[test]
fn a_differing_symlink_is_created_beside_and_a_same_one_adopted() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    std::os::unix::fs::symlink("skills/real", source.join("link")).unwrap();
    std::os::unix::fs::symlink("elsewhere", destination.join("link")).unwrap();
    std::os::unix::fs::symlink("same-target", source.join("same")).unwrap();
    std::os::unix::fs::symlink("same-target", destination.join("same")).unwrap();
    let stats = beside.run();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(applied(&stats), [("link".to_owned(), beside_name("link"))]);
    assert_eq!(
        std::fs::read_link(destination.join("link")).unwrap(),
        Path::new("elsewhere")
    );
    assert_eq!(
        std::fs::read_link(destination.join(beside_name("link"))).unwrap(),
        Path::new("skills/real")
    );
    assert!(!destination.join(beside_name("same")).exists());
    // A rerun adopts the beside link it made.
    let again = beside.run();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(applied(&again), applied(&stats));
}

/// A database with a `-wal` and `-shm` beside it at the destination (the
/// live Codex index, C2): its snapshot lands at `<db><SUFFIX>`, sound and in
/// journal mode DELETE, with the live database and its sidecars untouched.
#[test]
fn a_database_with_live_sidecars_lands_its_snapshot_beside() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    assume_uid(
        &std::fs::canonicalize(&source).unwrap(),
        Some(AssumeUid::Unprivileged),
    );
    let build = |path: &Path, rows: u64| {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE threads(id INTEGER PRIMARY KEY, cwd TEXT NOT NULL);",
            )
            .unwrap();
        for row in 0..rows {
            connection
                .execute(
                    "INSERT INTO threads(cwd) VALUES (?1)",
                    [format!("/Users/jess/git/lane-{row}")],
                )
                .unwrap();
        }
        connection.close().map_err(|(_, error)| error).unwrap();
    };
    build(&source.join("state_5.sqlite"), 5);
    build(&destination.join("state_5.sqlite"), 2);
    std::fs::write(destination.join("state_5.sqlite-wal"), b"").unwrap();
    std::fs::write(destination.join("state_5.sqlite-shm"), b"").unwrap();
    let live = std::fs::read(destination.join("state_5.sqlite")).unwrap();
    settle(&beside.corpus);

    let stats = beside.snapshot();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        applied(&stats),
        [("state_5.sqlite".to_owned(), beside_name("state_5.sqlite"))]
    );
    assert_eq!(stats.sqlite_snapshots, [b"state_5.sqlite".to_vec()]);
    assert_eq!(
        std::fs::read(destination.join("state_5.sqlite")).unwrap(),
        live,
        "the live index is untouched"
    );
    assert!(destination.join("state_5.sqlite-wal").exists());
    assert!(destination.join("state_5.sqlite-shm").exists());
    let landed = destination.join(beside_name("state_5.sqlite"));
    let connection = Connection::open_with_flags(
        format!("file:{}?immutable=1", landed.display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM threads", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 5);
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    assert!(!destination.join(beside_name("state_5.sqlite-wal")).exists());

    // Without the flag the same layout refuses at Decide, nothing read.
    let refused = {
        set_beside(&beside.root, Some(None));
        let stats = beside.snapshot();
        set_beside(&beside.root, Some(Some(SUFFIX.to_vec())));
        stats
    };
    assert_eq!(
        refused.refusals,
        [(
            b"state_5.sqlite".to_vec(),
            "DESTINATION_OCCUPIED".to_owned()
        )]
    );
    assert_eq!(refused.source_bytes_read, 0);

    // With it, the rerun reuses the beside snapshot: nothing read.
    let warm = beside.snapshot();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!(warm.source_bytes_read, 0);
    assert_eq!(applied(&warm), applied(&stats));
}

/// M2r2 A-103 stays: a verified byte prefix of an append-only file is
/// extended in place, not landed beside; a file whose straddling chunk
/// differs (not a prefix after all) lands whole beside instead of being
/// refused, from the proven prefix and the staged tail.
#[test]
fn a_grown_prefix_is_extended_and_a_false_prefix_lands_whole_beside() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let grown = noise(321, 3 << 20);
    let prior = (2 << 20) + 12_345;
    let false_prefix = noise(322, 1 << 20);
    let held = 400_000;
    // Landed once, the store lost (A-103's finding), then the sources grow.
    std::fs::write(source.join("history.jsonl"), &grown[..prior]).unwrap();
    std::fs::write(source.join("other.jsonl"), &false_prefix[..held]).unwrap();
    assert!(beside.run().refusals.is_empty());
    std::fs::remove_dir_all(beside.corpus.base.join("destination-state")).unwrap();
    std::fs::write(source.join("history.jsonl"), &grown).unwrap();
    std::fs::write(source.join("other.jsonl"), &false_prefix).unwrap();
    // The destination's `other.jsonl` differs in its last held byte: past
    // every whole chunk, inside the straddling one.
    let mut theirs = false_prefix[..held].to_vec();
    theirs[held - 1] ^= 1;
    std::fs::write(destination.join("other.jsonl"), &theirs).unwrap();
    settle(&beside.corpus);
    let inode = std::fs::metadata(destination.join("history.jsonl"))
        .unwrap()
        .ino();

    let stats = beside.run();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        std::fs::read(destination.join("history.jsonl")).unwrap(),
        grown
    );
    assert_eq!(
        std::fs::metadata(destination.join("history.jsonl"))
            .unwrap()
            .ino(),
        inode,
        "extended in place"
    );
    assert_eq!(
        stats.prefix_extended.len(),
        1,
        "{:?}",
        stats.prefix_extended
    );
    assert!(!destination.join(beside_name("history.jsonl")).exists());
    assert_eq!(
        applied(&stats),
        [("other.jsonl".to_owned(), beside_name("other.jsonl"))]
    );
    assert_eq!(
        std::fs::read(destination.join("other.jsonl")).unwrap(),
        theirs
    );
    assert_eq!(
        std::fs::read(destination.join(beside_name("other.jsonl"))).unwrap(),
        false_prefix
    );
    let warm = beside.run();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!((warm.source_bytes_read, warm.bytes_received), (0, 0));
}

/// The beside path is never taken from anyone: a foreign file there with
/// other bytes refuses the entry `DESTINATION_OCCUPIED`; one holding the
/// seat's bytes is adopted.
#[test]
fn a_foreign_file_at_the_beside_path_is_refused_or_adopted_never_replaced() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let ours = noise(331, 120_000);
    std::fs::write(source.join("a.json"), &ours).unwrap();
    std::fs::write(destination.join("a.json"), noise(332, 100_000)).unwrap();
    let foreign = noise(333, 50_000);
    std::fs::write(destination.join(beside_name("a.json")), &foreign).unwrap();
    std::fs::write(source.join("b.json"), &ours).unwrap();
    std::fs::write(destination.join("b.json"), noise(334, 100_000)).unwrap();
    std::fs::write(destination.join(beside_name("b.json")), &ours).unwrap();
    settle(&beside.corpus);
    let stats = beside.run();
    assert_eq!(
        stats.refusals,
        [(b"a.json".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(
        std::fs::read(destination.join(beside_name("a.json"))).unwrap(),
        foreign,
        "a foreign beside file is never replaced"
    );
    assert_eq!(
        applied(&stats),
        [("b.json".to_owned(), beside_name("b.json"))]
    );
    assert_eq!(
        std::fs::read_dir(&destination).unwrap().count(),
        4,
        "no staged file is left"
    );
}

/// The session's transfer record names each beside placement as an
/// `applied-beside` entry with its beside path.
#[test]
fn the_transfer_record_names_applied_beside_entries() {
    let mut stats = TransferStats::default();
    stats
        .applied_beside
        .push((b"settings.json".to_vec(), b"settings.json.neo".to_vec()));
    stats
        .refusals
        .push((b"kept".to_vec(), "DESTINATION_OCCUPIED".to_owned()));
    let record = crate::transfer_outcome::TransferRecord::ended(
        "pull",
        "neo:/Users/jess/.claude",
        Path::new("/home/jess/.claude"),
        &Ok(stats),
        "transfer::pull",
    );
    let beside: Vec<_> = record
        .entries
        .iter()
        .filter_map(|entry| match &entry.outcome {
            crate::transfer_outcome::EntryOutcome::AppliedBeside { beside } => {
                Some((entry.rel_path.clone(), beside.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        beside,
        [(b"settings.json".to_vec(), b"settings.json.neo".to_vec())]
    );
    assert_eq!(record.entries.len(), 2);
    let decoded = crate::transfer_outcome::decode(&record.encode().unwrap()).unwrap();
    assert_eq!(decoded, record, "round-trips with the new variant");
}

/// A suffix is one name component.
#[test]
fn a_suffix_is_one_name_component() {
    assert!(beside_suffix_allowed(b".neo-20261019"));
    assert!(beside_suffix_allowed(b"-neo"));
    for bad in [&b""[..], b"/x", b"a/b", b".", b"..", b"a\0b"] {
        assert!(!beside_suffix_allowed(bad), "{bad:?}");
        assert_eq!(
            set_beside_suffix(Some(bad.to_vec())).err(),
            Some(BulkloadRefusal::FieldDomainViolation),
            "{bad:?}"
        );
    }
    set_beside_suffix(None).unwrap();
}
