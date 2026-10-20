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

// ---- review of #280 (2026-10-19), findings 1, 3, 4, 5 ----------------------

/// JSONL lines of about 100 bytes each, `count` of them, numbered from
/// `from`.
fn jsonl(from: usize, count: usize) -> Vec<u8> {
    (from..from + count)
        .flat_map(|index| {
            format!("{{\"n\":{index},\"text\":\"line {index} of a live rollout, padded to about a hundred bytes\"}}\n")
                .into_bytes()
        })
        .collect()
}

/// Finding 1, first reproduction: a live file adopted on pass 1 (identical)
/// is the destination's still; the seat changes on the source, and pass 2
/// lands the new bytes beside it, the live file untouched (same inode, same
/// bytes), never exchanged.
#[test]
fn an_adopted_live_file_is_never_replaced_when_its_seat_changes() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let original = noise(341, 4096);
    std::fs::write(source.join("config.toml"), &original).unwrap();
    std::fs::write(destination.join("config.toml"), &original).unwrap();
    settle(&beside.corpus);
    let first = beside.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(first.applied_beside.is_empty(), "adopted, not beside");
    let inode = std::fs::metadata(destination.join("config.toml"))
        .unwrap()
        .ino();

    let edited = noise(342, 5000);
    std::fs::write(source.join("config.toml"), &edited).unwrap();
    settle(&beside.corpus);
    let second = beside.run();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(
        std::fs::read(destination.join("config.toml")).unwrap(),
        original,
        "the live file keeps the destination's bytes"
    );
    assert_eq!(
        std::fs::metadata(destination.join("config.toml"))
            .unwrap()
            .ino(),
        inode,
        "never exchanged"
    );
    assert_eq!(
        std::fs::read(destination.join(beside_name("config.toml"))).unwrap(),
        edited
    );
    assert_eq!(
        applied(&second),
        [("config.toml".to_owned(), beside_name("config.toml"))]
    );
}

/// Finding 1, second reproduction: an append-only file bulkload published
/// on pass 1, held open for appending by a destination writer, grows on the
/// source; pass 2 extends it in place through `O_APPEND` (same inode, never
/// replaced), so the writer's next line lands in the live file after it.
#[test]
fn a_published_append_only_file_held_open_by_a_writer_is_extended_in_place() {
    use std::io::Write as _;
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let head = jsonl(0, 6000);
    std::fs::write(source.join("rollout.jsonl"), &head).unwrap();
    settle(&beside.corpus);
    let first = beside.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let live = destination.join("rollout.jsonl");
    assert_eq!(std::fs::read(&live).unwrap(), head);
    let inode = std::fs::metadata(&live).unwrap().ino();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&live)
        .unwrap();

    let mut grown = head;
    grown.extend_from_slice(&jsonl(6000, 2000));
    std::fs::write(source.join("rollout.jsonl"), &grown).unwrap();
    settle(&beside.corpus);
    let second = beside.run();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(
        second.prefix_extended.len(),
        1,
        "{:?}",
        second.prefix_extended
    );
    assert!(
        second.applied_beside.is_empty(),
        "{:?}",
        second.applied_beside
    );
    assert_eq!(
        std::fs::metadata(&live).unwrap().ino(),
        inode,
        "extended in place, never replaced"
    );
    writer.write_all(b"{\"dest\":\"next turn\"}\n").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let mut want = grown;
    want.extend_from_slice(b"{\"dest\":\"next turn\"}\n");
    assert_eq!(
        std::fs::read(&live).unwrap(),
        want,
        "the writer's line follows the extension, in the live file"
    );
}

/// Finding 3: a seat whose live file differs and whose beside path holds a
/// foreign file is refused, and the unchanged rerun refuses it again with 0
/// source bytes read and nothing received (S3, R25).
#[test]
fn a_refused_beside_landing_is_remembered_and_reads_nothing_again() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    std::fs::write(source.join("a.json"), noise(351, 120_000)).unwrap();
    std::fs::write(destination.join("a.json"), noise(352, 100_000)).unwrap();
    std::fs::write(destination.join(beside_name("a.json")), noise(353, 50_000)).unwrap();
    settle(&beside.corpus);
    let first = beside.run();
    assert_eq!(
        first.refusals,
        [(b"a.json".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert!(first.source_bytes_read > 0);
    let again = beside.run();
    assert_eq!(again.refusals, first.refusals);
    assert_eq!(
        (again.source_bytes_read, again.bytes_received),
        (0, 0),
        "remembered under both keys"
    );
    // The beside path freed: the next pass lands it.
    std::fs::remove_file(destination.join(beside_name("a.json"))).unwrap();
    let landed = beside.run();
    assert!(landed.refusals.is_empty(), "{:?}", landed.refusals);
    assert_eq!(
        applied(&landed),
        [("a.json".to_owned(), beside_name("a.json"))]
    );
}

/// Finding 4: the source holds `x` and `x<SUFFIX>`, and the destination's
/// `x` differs: one seat is refused `DESTINATION_OCCUPIED`, typed, the
/// session completes, and nothing is written twice under one name; a rerun
/// never replaces the source's own `x<SUFFIX>` with `x`'s bytes.
#[test]
fn a_beside_name_the_source_also_holds_is_a_typed_refusal_of_one_seat() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let ours = noise(361, 90_000);
    let suffixed = noise(362, 80_000);
    let theirs = noise(363, 70_000);
    std::fs::write(source.join("x"), &ours).unwrap();
    std::fs::write(source.join(beside_name("x")), &suffixed).unwrap();
    std::fs::write(destination.join("x"), &theirs).unwrap();
    settle(&beside.corpus);
    for pass in 0..2 {
        let stats = beside.run();
        assert_eq!(stats.refusals.len(), 1, "pass {pass}: {:?}", stats.refusals);
        assert_eq!(stats.refusals[0].1, "DESTINATION_OCCUPIED", "pass {pass}");
        assert_eq!(
            std::fs::read(destination.join("x")).unwrap(),
            theirs,
            "pass {pass}"
        );
        let held = std::fs::read(destination.join(beside_name("x"))).unwrap();
        assert!(held == suffixed || held == ours, "pass {pass}");
        if pass == 1 {
            assert_eq!(
                std::fs::read(destination.join(beside_name("x"))).unwrap(),
                held,
                "never replaced across passes"
            );
        }
        settle(&beside.corpus);
    }
}

/// Finding 5: a regular seat over a destination symlink (a Home Manager
/// `/nix/store` link, as sting's `~/.codex/AGENTS.md`) lands beside under
/// `--beside`, the link untouched; without the flag it is refused
/// `DESTINATION_OCCUPIED`, typed, never a bare `IO`. A directory over a
/// destination symlink (sting's `~/.claude/file-history/<uuid>` links into a
/// repair tree; second review, finding 5) lands beside too, at
/// `<dir><SUFFIX>/` with every entry under it, accounted `applied-beside`;
/// nothing is written through the link, and a rerun reads nothing.
#[test]
fn a_seat_over_a_destination_symlink_lands_beside_or_refuses_typed() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let store = beside.corpus.base.join("nix-store");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("agents.md"), b"home manager's\n").unwrap();
    std::fs::create_dir(store.join("dir")).unwrap();
    std::fs::write(store.join("dir/theirs.txt"), b"the repair tree's\n").unwrap();
    std::os::unix::fs::symlink(store.join("agents.md"), destination.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(store.join("dir"), destination.join("d")).unwrap();
    std::fs::write(source.join("AGENTS.md"), b"neo's agents\n").unwrap();
    std::fs::create_dir_all(source.join("d/deeper")).unwrap();
    std::fs::write(source.join("d/inner.txt"), b"inner\n").unwrap();
    std::fs::write(source.join("d/deeper/leaf.txt"), b"leaf\n").unwrap();
    settle(&beside.corpus);

    let without = beside.run_without();
    let mut codes: Vec<(String, String)> = without
        .refusals
        .iter()
        .map(|(path, code)| (String::from_utf8_lossy(path).into_owned(), code.clone()))
        .collect();
    codes.sort();
    assert!(
        codes.iter().all(|(_, code)| code == "DESTINATION_OCCUPIED"),
        "typed, never IO: {codes:?}"
    );
    assert!(codes.contains(&("AGENTS.md".to_owned(), "DESTINATION_OCCUPIED".to_owned())));
    assert!(codes.contains(&("d".to_owned(), "DESTINATION_OCCUPIED".to_owned())));

    settle(&beside.corpus);
    let with = beside.run();
    assert!(with.refusals.is_empty(), "{:?}", with.refusals);
    let mut placed = applied(&with);
    placed.sort();
    assert_eq!(
        placed,
        [
            ("AGENTS.md".to_owned(), beside_name("AGENTS.md")),
            ("d".to_owned(), beside_name("d")),
        ]
    );
    assert_eq!(
        std::fs::read(destination.join(beside_name("AGENTS.md"))).unwrap(),
        b"neo's agents\n"
    );
    let dir = destination.join(beside_name("d"));
    assert_eq!(std::fs::read(dir.join("inner.txt")).unwrap(), b"inner\n");
    assert_eq!(
        std::fs::read(dir.join("deeper/leaf.txt")).unwrap(),
        b"leaf\n"
    );
    for link in ["AGENTS.md", "d"] {
        assert!(std::fs::symlink_metadata(destination.join(link))
            .unwrap()
            .file_type()
            .is_symlink());
    }
    assert_eq!(
        std::fs::read(store.join("agents.md")).unwrap(),
        b"home manager's\n"
    );
    let mut theirs: Vec<String> = std::fs::read_dir(store.join("dir"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    theirs.sort();
    assert_eq!(theirs, ["theirs.txt"], "nothing written through the link");

    settle(&beside.corpus);
    let again = beside.run();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!((again.source_bytes_read, again.bytes_received), (0, 0));
    let mut placed_again = applied(&again);
    placed_again.sort();
    assert_eq!(placed_again, placed);
}

/// Second review of #280, N3: a beside output published from a racy
/// capture (a source written within the racy window, as a live
/// `history.jsonl` is) is still this seat's own: once the source changes,
/// the next pass supersedes it at the beside path, never refusing it as a
/// stranger's, and the live file stays untouched.
#[test]
fn a_racy_beside_output_is_still_this_seats_own() {
    let beside = Beside::new();
    let source = beside.source();
    let destination = beside.destination();
    let theirs = noise(371, 30_000);
    std::fs::write(destination.join("config.json"), &theirs).unwrap();
    let first_bytes = noise(372, 40_000);
    let seat = source.join("config.json");
    std::fs::write(&seat, &first_bytes).unwrap();
    settle_racy_window(&destination).unwrap();
    let racy = {
        let clock = PinnedClock::at(&beside.corpus, stamp_ns(&seat) + 1);
        let stats = beside.run();
        drop(clock);
        stats
    };
    assert!(racy.refusals.is_empty(), "{:?}", racy.refusals);
    assert_eq!(
        std::fs::read(destination.join(beside_name("config.json"))).unwrap(),
        first_bytes
    );

    let second_bytes = noise(373, 45_000);
    std::fs::write(&seat, &second_bytes).unwrap();
    settle(&beside.corpus);
    let next = beside.run();
    assert!(next.refusals.is_empty(), "{:?}", next.refusals);
    assert_eq!(
        std::fs::read(destination.join(beside_name("config.json"))).unwrap(),
        second_bytes,
        "the racy beside output was this seat's own, so it was updated"
    );
    assert_eq!(
        std::fs::read(destination.join("config.json")).unwrap(),
        theirs
    );
}

/// Independent review of #280, N1: a destination root swapped for a link
/// after the caller resolved it (sting's `~/.claude` relinked between the
/// pull's resolve and its open) is refused `PATH_ESCAPES_ROOT`, typed,
/// never a bare `IO`; nothing is written through the link.
#[test]
fn a_destination_root_swapped_for_a_link_is_a_typed_refusal() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/a.txt"), b"a\n").unwrap();
    let real = corpus.base.join("elsewhere");
    std::fs::create_dir(&real).unwrap();
    let store = Store::open(&corpus.base.join("destination-state")).unwrap();
    std::fs::remove_dir(corpus.base.join("destination")).unwrap();
    std::os::unix::fs::symlink(&real, corpus.base.join("destination")).unwrap();
    let refused = Destination::open(&corpus.base.join("destination"), &store)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(refused, BulkloadRefusal::PathEscapesRoot);
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}
