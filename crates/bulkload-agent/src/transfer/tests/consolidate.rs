//! Consolidation of append logs in place (operator ruling 2026-10-20:
//! "neither WIN. they must be consolidated"; "no dupes, no duplicated
//! besides"): a session continued on both hosts lands as one file holding
//! both branches, `history.jsonl` as the union of its lines, a destination
//! writer appending during the consolidation is never lost, and an unchanged
//! rerun reads nothing. Never a beside copy.

use super::*;

const SUFFIX: &[u8] = b".neo-20261020";

struct Homes {
    corpus: Corpus,
    root: PathBuf,
}

impl Homes {
    fn new() -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        set_beside(&root, Some(Some(SUFFIX.to_vec())));
        set_consolidate_root(&root, Some(true));
        Self { corpus, root }
    }

    fn write(&self, side: &str, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.corpus.base.join(side).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn settle(&self) {
        settle_racy_window(&self.corpus.base.join("source")).unwrap();
        settle_racy_window(&self.corpus.base.join("destination")).unwrap();
    }
}

impl Drop for Homes {
    fn drop(&mut self) {
        set_beside(&self.root, None);
        set_consolidate_root(&self.root, None);
        set_consolidate_hook(&self.root, None);
    }
}

fn record(uuid: &str, parent: &str, text: &str) -> String {
    format!(
        "{{\"parentUuid\":\"{parent}\",\"uuid\":\"{uuid}\",\"timestamp\":\"2026-10-20T00:00:00Z\",\"message\":\"{text}\"}}\n"
    )
}

fn beside_of(path: &Path) -> PathBuf {
    let mut beside = path.as_os_str().to_owned();
    beside.push(std::ffi::OsStr::from_bytes(SUFFIX));
    PathBuf::from(beside)
}

/// One session id continued on both hosts: one file at the path, neo's
/// bytes then sting's branch; never a beside copy; ledgered; and the rerun
/// reads nothing.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "the process check proves nothing off Linux: every consolidation is unsettled"
)]
fn a_session_continued_on_both_hosts_is_one_file_with_both_branches() {
    let homes = Homes::new();
    let shared = format!("{}{}", record("a", "", "hi"), record("b", "a", "hello"));
    let neo = format!("{shared}{}", record("n1", "b", "on neo"));
    let sting_branch = format!(
        "{}{}",
        record("s1", "b", "on sting"),
        record("s2", "s1", "more")
    );
    let rel = "projects/-k/s.jsonl";
    homes.write("source", rel, neo.as_bytes());
    let live = homes.write(
        "destination",
        rel,
        format!("{shared}{sting_branch}").as_bytes(),
    );
    homes.settle();

    let first = homes.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(
        first.applied_beside.is_empty(),
        "{:?}",
        first.applied_beside
    );
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        format!("{neo}{sting_branch}"),
        "one file: neo's bytes, then sting's branch"
    );
    assert!(!beside_of(&live).exists(), "never a beside copy");
    assert_eq!(first.consolidated.len(), 1, "{:?}", first.consolidated);
    let done = &first.consolidated[0];
    assert_eq!(done.path, rel.as_bytes());
    assert_eq!((done.seat_records, done.live_extra), (3, 2));
    assert_eq!(
        done.output_blake3,
        *blake3::hash(format!("{neo}{sting_branch}").as_bytes()).as_bytes()
    );
    let ledger = crate::transfer_outcome::TransferRecord::ended(
        "copy",
        "source",
        &homes.root,
        &Ok(first),
        "transfer::copy",
    );
    assert!(ledger
        .entries
        .iter()
        .any(|entry| entry.rel_path == rel.as_bytes()
            && matches!(
                entry.outcome,
                crate::transfer_outcome::EntryOutcome::Consolidated { live_extra: 2, .. }
            )));

    homes.settle();
    let again = homes.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
    assert!(again.consolidated.is_empty());
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        format!("{neo}{sting_branch}")
    );
}

/// `history.jsonl` (no `uuid`): the union of its lines, counted.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "the process check proves nothing off Linux: every consolidation is unsettled"
)]
fn history_is_the_union_of_both_hosts_lines() {
    let homes = Homes::new();
    let neo = "{\"display\":\"a\"}\n{\"display\":\"neo\"}\n";
    let sting = "{\"display\":\"a\"}\n{\"display\":\"sting\"}\n";
    homes.write("source", "history.jsonl", neo.as_bytes());
    let live = homes.write("destination", "history.jsonl", sting.as_bytes());
    homes.settle();
    let stats = homes.corpus.run().unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        "{\"display\":\"a\"}\n{\"display\":\"neo\"}\n{\"display\":\"sting\"}\n"
    );
    assert!(!beside_of(&live).exists());
    assert_eq!(stats.consolidated.len(), 1);
}

/// A destination writer appends between the read and the exchange: its
/// line is never lost (nothing is replaced, the path is refused
/// `DESTINATION_UNSETTLED`), and the next pass merges it in.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "the process check proves nothing off Linux: every consolidation is unsettled"
)]
fn a_line_sting_appends_during_the_consolidation_is_never_lost() {
    fn append(live: &Path) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(live).unwrap();
        file.write_all(b"{\"uuid\":\"late\",\"message\":\"while merging\"}\n")
            .unwrap();
    }
    let homes = Homes::new();
    let shared = record("a", "", "hi");
    let neo = format!("{shared}{}", record("n1", "a", "on neo"));
    let sting = format!("{shared}{}", record("s1", "a", "on sting"));
    let rel = "projects/-k/t.jsonl";
    homes.write("source", rel, neo.as_bytes());
    let live = homes.write("destination", rel, sting.as_bytes());
    homes.settle();
    set_consolidate_hook(&homes.root, Some(append));
    let first = homes.corpus.run().unwrap();
    set_consolidate_hook(&homes.root, None);
    assert_eq!(
        first.refusals,
        [(rel.as_bytes().to_vec(), "DESTINATION_UNSETTLED".to_owned())]
    );
    assert!(first.consolidated.is_empty());
    let late = "{\"uuid\":\"late\",\"message\":\"while merging\"}\n";
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        format!("{sting}{late}"),
        "the live file as the writer left it"
    );
    assert!(!beside_of(&live).exists());

    homes.settle();
    let again = homes.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        format!("{neo}{}{late}", record("s1", "a", "on sting"))
    );
}

/// Operator ruling 2026-10-20 ("no dupes, no duplicated besides"), every
/// file class of the homes: a beside copy is written only for bytes the live
/// file neither is, nor is a prefix of (extended in place), nor holds
/// (covered, `dest-ahead`, the very bytes under another mode included).
#[test]
fn a_beside_is_written_only_for_bytes_the_live_file_lacks() {
    use std::os::unix::fs::PermissionsExt as _;
    let homes = Homes::new();
    // Settings the destination extended: covered.
    homes.write("source", "settings.json", b"{\"a\":1}\n");
    let covered = homes.write("destination", "settings.json", b"{\"a\":1}\n{\"b\":2}\n");
    // The same bytes under another mode: covered.
    let same = homes.write("source", "plugins/known.json", b"{\"p\":1}\n");
    let moded = homes.write("destination", "plugins/known.json", b"{\"p\":1}\n");
    std::fs::set_permissions(&same, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&moded, std::fs::Permissions::from_mode(0o600)).unwrap();
    // A live file that is a prefix of the source's: extended in place.
    homes.write("source", "plugins/grown.json", b"{\"g\":1}\n{\"g\":2}\n");
    let grown = homes.write("destination", "plugins/grown.json", b"{\"g\":1}\n");
    // Other bytes: the one beside copy.
    homes.write("source", "config.toml", b"model = \"neo\"\n");
    let other = homes.write("destination", "config.toml", b"model = \"sting\"\n");
    homes.settle();

    let stats = homes.corpus.run().unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    let mut ahead = stats.dest_ahead.clone();
    ahead.sort();
    assert_eq!(
        ahead,
        [b"plugins/known.json".to_vec(), b"settings.json".to_vec()]
    );
    assert_eq!(
        stats.applied_beside,
        [(
            b"config.toml".to_vec(),
            [&b"config.toml"[..], SUFFIX].concat()
        )]
    );
    assert_eq!(std::fs::read(&grown).unwrap(), b"{\"g\":1}\n{\"g\":2}\n");
    for path in [&covered, &moded, &grown] {
        assert!(!beside_of(path).exists(), "{path:?}");
    }
    assert!(beside_of(&other).exists());
}

/// An append log whose live path is another kind (a directory, a link
/// outside the archive roots) is refused typed, nothing landed beside it; a
/// link into an archive root is the re-seat's A-106 union and is landed for
/// it.
#[test]
fn a_log_whose_live_path_is_another_kind_is_refused_unless_an_archive_link() {
    let homes = Homes::new();
    let neo = record("a", "", "hi");
    let (dir_rel, link_rel, archived_rel) = (
        "projects/-k/dir.jsonl",
        "projects/-k/link.jsonl",
        "projects/-k/archived.jsonl",
    );
    for rel in [dir_rel, link_rel, archived_rel] {
        homes.write("source", rel, neo.as_bytes());
    }
    let dir = homes.root.join(dir_rel);
    std::fs::create_dir_all(&dir).unwrap();
    let elsewhere = homes.write("elsewhere", "other.jsonl", b"{\"x\":1}\n");
    std::os::unix::fs::symlink(&elsewhere, homes.root.join(link_rel)).unwrap();
    let archive = homes.corpus.base.join("archive");
    let target = homes.write("archive", "s.jsonl", b"{\"x\":2}\n");
    std::os::unix::fs::symlink(&target, homes.root.join(archived_rel)).unwrap();
    set_consolidate_archives(&[archive]);
    homes.settle();
    let stats = homes.corpus.run();
    set_consolidate_archives(&[]);
    let stats = stats.unwrap();
    let mut refused: Vec<(String, String)> = stats
        .refusals
        .iter()
        .map(|(path, code)| (String::from_utf8_lossy(path).into_owned(), code.clone()))
        .collect();
    refused.sort();
    assert_eq!(
        refused,
        [
            (dir_rel.to_owned(), "DESTINATION_KIND_MISMATCH".to_owned()),
            (link_rel.to_owned(), "DESTINATION_KIND_MISMATCH".to_owned()),
        ]
    );
    assert!(!beside_of(&dir).exists());
    assert!(!beside_of(&homes.root.join(link_rel)).exists());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"{\"x\":1}\n");
    assert_eq!(
        std::fs::read(beside_of(&homes.root.join(archived_rel))).unwrap(),
        neo.as_bytes(),
        "landed for the re-seat's union"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"x\":2}\n");
}
