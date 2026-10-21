//! The append-log union through the homes' class interface (operator
//! ruling 2026-10-20): a transcript continued on both hosts, `history.jsonl`,
//! a live writer, a rerun.

use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

use super::super::tests::set_hook;
use super::super::{union_of, Action, Class, Ledger, Live, Maps, Outcome, Source};
use super::{by_uuid, plan};
use crate::BulkloadRefusal;

/// Claude transcript records: two shared, then each host's continuation of
/// the same parent.
const A: &str = "{\"uuid\":\"a\",\"parentUuid\":null,\"text\":\"start\"}\n";
const B: &str = "{\"uuid\":\"b\",\"parentUuid\":\"a\",\"text\":\"shared\"}\n";
const NEO: &str = "{\"uuid\":\"n\",\"parentUuid\":\"b\",\"text\":\"on neo\"}\n";
const NEO2: &str = "{\"uuid\":\"n2\",\"parentUuid\":\"n\",\"text\":\"neo again\"}\n";
const NEO3: &str = "{\"uuid\":\"n3\",\"parentUuid\":\"n2\",\"text\":\"neo, after a pull\"}\n";
const STING: &str = "{\"uuid\":\"s\",\"parentUuid\":\"b\",\"text\":\"on sting\"}\n";
const STING2: &str = "{\"uuid\":\"s2\",\"parentUuid\":\"s\",\"text\":\"sting again\"}\n";

const TRANSCRIPT: &str = ".claude/projects/-home-jess-git-x/s.jsonl";

fn cat(parts: &[&str]) -> Vec<u8> {
    parts.concat().into_bytes()
}

/// A ledgered union: file, action, source records, live extras, output
/// digest.
type Ledgered = (Vec<u8>, String, i64, i64, Vec<u8>);

struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ledger: Ledger,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let ledger = Ledger::open(&root.join("state")).unwrap();
        Self {
            _dir: dir,
            root,
            ledger,
        }
    }

    fn live(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Union `source` into the file at `rel` through the class dispatch,
    /// staged as the receive stages it: a private file in the live file's
    /// directory, removed after.
    fn union(&self, rel: &str, source: &[u8]) -> crate::Result<Outcome> {
        let path = self.root.join(rel);
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let staged = parent.join(".bulkload-staged-source");
        fs::write(&staged, source).unwrap();
        let file = File::open(&staged).unwrap();
        let dir = File::open(parent).unwrap();
        let maps = Maps::new(Vec::new());
        let union = union_of(Class::of(rel.as_bytes(), false)).expect("the append-log union");
        let outcome = union(
            &Live {
                dir: &dir,
                name: path.file_name().unwrap().as_bytes(),
                path: &path,
                rel: rel.as_bytes(),
            },
            &Source {
                file: &file,
                path: &staged,
                size: source.len() as u64,
                mtime_ns: 0,
                maps: &maps,
            },
            &self.ledger,
        );
        fs::remove_file(&staged).unwrap();
        outcome
    }

    /// The names beside the file at `rel` (itself included).
    fn names(&self, rel: &str) -> Vec<String> {
        let parent = self.root.join(rel);
        let mut names: Vec<String> = fs::read_dir(parent.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The ledgered unions: file, action, source records, live extras,
    /// output digest.
    fn ledgered(&self) -> Vec<Ledgered> {
        let conn = self.ledger.connection();
        let Ok(mut statement) = conn.prepare(
            "SELECT file, action, source_records, live_extra, output_blake3 FROM append_log_union ORDER BY rowid",
        ) else {
            return Vec::new();
        };
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        set_hook(None);
        set_append_hook(None);
    }
}

thread_local! {
    static BEFORE_APPEND: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

/// Runs between the extension's check and its write.
pub(super) fn before_append(_file: &File) {
    BEFORE_APPEND.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

fn set_append_hook(hook: Option<Box<dyn FnMut()>>) {
    BEFORE_APPEND.with(|slot| *slot.borrow_mut() = hook);
}

/// Each answer of the union, nothing read or written: equal, covered by
/// bytes and by records, extended, consolidated, mid-line.
#[test]
fn each_union_answer_is_decided_before_anything_is_written() {
    let shared = cat(&[A, B]);
    assert_eq!(
        plan(&shared, &shared, true).unwrap().action,
        Action::Adopted
    );
    let covered = plan(&cat(&[A, B, STING]), &shared, true).unwrap();
    assert_eq!((covered.action, covered.bytes), (Action::Covered, None));
    // Every record held, in another order: covered, nothing rewritten.
    let reordered = plan(&cat(&[A, STING, B]), &shared, true).unwrap();
    assert_eq!(reordered.action, Action::Covered);
    let extended = plan(&cat(&[A]), &shared, true).unwrap();
    assert_eq!(extended.action, Action::Extended);
    assert_eq!(extended.bytes, Some(shared));
    let union = plan(&cat(&[A, B, STING]), &cat(&[A, B, NEO]), true).unwrap();
    assert_eq!(union.action, Action::Consolidated);
    assert_eq!(union.bytes, Some(cat(&[A, B, NEO, STING])));
    assert_eq!((union.source_records, union.live_extra), (3, 1));
    // A writer at either side's last line: not merged.
    let mut at_it = cat(&[A, B]);
    at_it.extend_from_slice(b"{\"uuid\":\"s\"");
    assert_eq!(plan(&at_it, &cat(&[A, B, NEO]), true), None);
    let mut source_at_it = cat(&[A, B]);
    source_at_it.extend_from_slice(b"{\"uuid\":\"n\"");
    assert_eq!(plan(&cat(&[A, B, STING]), &source_at_it, true), None);
    // The live file holding the source's bytes, its last line finished:
    // covered.
    assert_eq!(
        plan(&cat(&[A, B, STING]), &at_it, true).unwrap().action,
        Action::Covered
    );
    assert_eq!(Action::Consolidated.word(), "consolidate");
    assert_eq!(
        Action::Held("x").refusal(),
        Some(BulkloadRefusal::HomesConsolidateUnsupported)
    );
}

/// The `--plan-only` row: keyed as the union keys it, a writer at the last
/// line held.
#[test]
fn the_plan_row_keys_by_path_and_holds_a_writer_at_it() {
    use super::plan_at;
    let union = plan_at(
        TRANSCRIPT.as_bytes(),
        &cat(&[A, B, STING]),
        &cat(&[A, B, NEO]),
    );
    assert_eq!(union.action, Action::Consolidated);
    assert_eq!(
        union.bytes.map(|bytes| bytes.len()),
        Some(cat(&[A, B, NEO, STING]).len())
    );
    // A record held under the same uuid, edited: covered by identity.
    let mut edited = cat(&[A, B]);
    edited.extend_from_slice(b"{\"uuid\":\"b\",\"text\":\"edited\"}\n");
    assert_eq!(
        plan_at(TRANSCRIPT.as_bytes(), &edited, &cat(&[A, B])).action,
        Action::Covered
    );
    let mut at_it = cat(&[A]);
    at_it.extend_from_slice(b"{\"uuid\"");
    assert_eq!(
        plan_at(b".codex/history.jsonl", &at_it, &cat(&[A, B])).action,
        Action::Held("append-log: ends inside a line")
    );
}

#[test]
fn transcripts_are_keyed_by_uuid_and_other_logs_by_line() {
    assert!(by_uuid(TRANSCRIPT.as_bytes()));
    assert!(by_uuid(b"projects/k/s/subagents/agent-1.jsonl"));
    assert!(by_uuid(b"home/jess/.claude/projects/k/s.jsonl"));
    assert!(!by_uuid(b".claude/history.jsonl"));
    assert!(!by_uuid(b".codex/sessions/2026/10/20/rollout-x.jsonl"));
    assert!(!by_uuid(b"x.claude/projects/k/s.jsonl"));
}

/// The ruling's case: one session continued on both hosts is one file
/// holding both branches, ledgered; a rerun writes nothing; a session that
/// grows on either host after it is unioned again.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a write needs the open-file check, Linux only"
)]
fn a_transcript_continued_on_both_hosts_is_one_file_holding_both_branches() {
    let home = Home::new();
    let path = home.live(TRANSCRIPT, &cat(&[A, B, STING]));
    let before = fs::metadata(&path).unwrap().ino();
    let outcome = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(outcome.recorded, 1);
    let union = cat(&[A, B, NEO, STING]);
    assert_eq!(
        fs::read(&path).unwrap(),
        union,
        "neo's bytes, then sting's branch"
    );
    assert_eq!(outcome.written, union.len() as u64);
    assert_ne!(
        fs::metadata(&path).unwrap().ino(),
        before,
        "traded in whole"
    );
    assert_eq!(
        home.names(TRANSCRIPT),
        ["s.jsonl"],
        "no beside, no private name"
    );
    let ledgered = home.ledgered();
    assert_eq!(ledgered.len(), 1);
    assert_eq!(ledgered[0].0, TRANSCRIPT.as_bytes());
    assert_eq!(
        (ledgered[0].1.as_str(), ledgered[0].2, ledgered[0].3),
        ("consolidate", 3, 1)
    );
    assert_eq!(ledgered[0].4, blake3::hash(&union).as_bytes());

    // Rerun with the same source: covered, nothing written or ledgered.
    let inode = fs::metadata(&path).unwrap().ino();
    let again = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(
        (again.action, again.written, again.recorded),
        (Action::Covered, 0, 0)
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(home.ledgered().len(), 1);

    // Sting writes on: still covered. Neo writes on: unioned again, each
    // record once.
    fs::write(&path, cat(&[A, B, NEO, STING, STING2])).unwrap();
    let covered = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(covered.action, Action::Covered);
    let grown = home.union(TRANSCRIPT, &cat(&[A, B, NEO, NEO2])).unwrap();
    assert_eq!(grown.action, Action::Consolidated);
    assert_eq!(
        fs::read(&path).unwrap(),
        cat(&[A, B, NEO, NEO2, STING, STING2])
    );
    assert_eq!(home.ledgered().len(), 2);
    // A live prefix of the source's is extended to it, in place: the same
    // inode, only the tail written.
    let inode = fs::metadata(&path).unwrap().ino();
    let extended = home
        .union(TRANSCRIPT, &cat(&[A, B, NEO, NEO2, STING, STING2, NEO3]))
        .unwrap();
    assert_eq!(extended.action, Action::Extended);
    assert_eq!(extended.written, NEO3.len() as u64);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode, "in place");
    assert_eq!(
        fs::read(&path).unwrap(),
        cat(&[A, B, NEO, NEO2, STING, STING2, NEO3])
    );
    assert_eq!(home.ledgered()[2].1, "extend");
}

/// A writer that appends between the extension's check and its write: its
/// bytes are kept (`O_APPEND` never writes over them), the extension is
/// raced, and the next pass merges again.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a write needs the open-file check, Linux only"
)]
fn an_append_racing_the_extension_is_never_written_over() {
    use std::io::Write as _;
    let home = Home::new();
    let path = home.live(TRANSCRIPT, &cat(&[A, B]));
    let writer = path.clone();
    set_append_hook(Some(Box::new(move || {
        let mut file = File::options().append(true).open(&writer).unwrap();
        file.write_all(STING.as_bytes()).unwrap();
    })));
    assert_eq!(
        home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap_err(),
        BulkloadRefusal::HomesConsolidateRaced
    );
    set_append_hook(None);
    assert_eq!(
        fs::read(&path).unwrap(),
        cat(&[A, B, STING, NEO]),
        "nothing lost"
    );
    // The next pass: covered (every record held), nothing rewritten.
    let inode = fs::metadata(&path).unwrap().ino();
    let again = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(again.action, Action::Covered);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
}

/// `history.jsonl` (no record `uuid`): the union by exact line, a line held
/// more often on the live side kept that many more times.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a write needs the open-file check, Linux only"
)]
fn history_is_the_union_of_both_hosts_lines() {
    const L1: &str = "{\"session_id\":\"x\",\"ts\":1,\"text\":\"ls\"}\n";
    const LNEO: &str = "{\"session_id\":\"x\",\"ts\":2,\"text\":\"on neo\"}\n";
    const LSTING: &str = "{\"session_id\":\"y\",\"ts\":2,\"text\":\"on sting\"}\n";
    const AGAIN: &str = "{\"session_id\":\"y\",\"ts\":3,\"text\":\"again\"}\n";
    let rel = ".codex/history.jsonl";
    let home = Home::new();
    let path = home.live(rel, &cat(&[L1, LSTING, AGAIN, AGAIN]));
    let outcome = home.union(rel, &cat(&[L1, LNEO, AGAIN])).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(
        fs::read(&path).unwrap(),
        cat(&[L1, LNEO, AGAIN, LSTING, AGAIN]),
        "each line of either host, the source's first"
    );
    assert_eq!(home.ledgered()[0].3, 2);
    let again = home.union(rel, &cat(&[L1, LNEO, AGAIN])).unwrap();
    assert_eq!(again.action, Action::Covered);
}

/// A writer that appends while the union is written: its line is merged on
/// the next read, never lost to the file traded out.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a write needs the open-file check, Linux only"
)]
fn a_line_appended_mid_union_is_merged_again_never_lost() {
    use std::io::Write as _;
    let home = Home::new();
    let path = home.live(TRANSCRIPT, &cat(&[A, B, STING]));
    let writer = path.clone();
    let mut fired = false;
    set_hook(Some(Box::new(move || {
        if !fired {
            fired = true;
            // Claude Code's own append, by path.
            let mut file = File::options().append(true).open(&writer).unwrap();
            file.write_all(STING2.as_bytes()).unwrap();
        }
    })));
    let outcome = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(fs::read(&path).unwrap(), cat(&[A, B, NEO, STING, STING2]));
    assert_eq!(home.names(TRANSCRIPT), ["s.jsonl"]);
    assert_eq!(home.ledgered().len(), 1, "the union written once");
}

/// A live file a process holds open, or ending inside a line: raced,
/// retryable, nothing written; once the writer is gone, unioned.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "the open-file check is Linux only")]
fn a_log_held_open_or_ending_mid_line_is_raced_and_untouched() {
    let home = Home::new();
    let live = cat(&[A, B, STING]);
    let path = home.live(TRANSCRIPT, &live);
    // A Codex-style writer: the file held open by a session.
    let mut child = std::process::Command::new("sleep")
        .arg("3")
        .stdin(File::open(&path).unwrap())
        .spawn()
        .unwrap();
    let mut refused = None;
    for _ in 0..200 {
        let found = crate::procs::inside(&[path.as_path()]).unwrap();
        if found.is_some_and(|pids| !pids.is_empty()) {
            refused = Some(home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap_err());
            break;
        }
        std::thread::yield_now();
    }
    child.wait().unwrap();
    assert_eq!(refused, Some(BulkloadRefusal::HomesConsolidateRaced));
    assert_eq!(fs::read(&path).unwrap(), live, "nothing written");
    assert!(home.ledgered().is_empty());
    // A writer at the last line.
    let mut at_it = live;
    at_it.extend_from_slice(b"{\"uuid\":\"s2\"");
    fs::write(&path, &at_it).unwrap();
    assert_eq!(
        home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap_err(),
        BulkloadRefusal::HomesConsolidateRaced
    );
    assert_eq!(fs::read(&path).unwrap(), at_it);
    // The line finished and the writer gone: unioned.
    fs::write(&path, cat(&[A, B, STING, STING2])).unwrap();
    let outcome = home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(fs::read(&path).unwrap(), cat(&[A, B, NEO, STING, STING2]));
}

/// A link at the live name is never followed; an absent log is created.
#[test]
fn a_link_is_never_followed_and_an_absent_log_is_created() {
    let home = Home::new();
    let elsewhere = home.live("elsewhere.jsonl", &cat(&[A]));
    let path = home.root.join(TRANSCRIPT);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    assert_eq!(
        home.union(TRANSCRIPT, &cat(&[A, B])).unwrap_err(),
        BulkloadRefusal::DestinationKindMismatch
    );
    assert_eq!(fs::read(&elsewhere).unwrap(), cat(&[A]));
    fs::remove_file(&path).unwrap();
    let created = home.union(TRANSCRIPT, &cat(&[A, B])).unwrap();
    assert_eq!(created.action, Action::Created);
    assert_eq!(fs::read(&path).unwrap(), cat(&[A, B]));
    assert!(home.ledgered().is_empty(), "a creation is no union");
}

/// Where no process can be seen, nothing is written.
#[test]
#[cfg(not(target_os = "linux"))]
fn where_open_files_cannot_be_seen_nothing_is_written() {
    let home = Home::new();
    let live = cat(&[A, B, STING]);
    let path = home.live(TRANSCRIPT, &live);
    assert_eq!(
        home.union(TRANSCRIPT, &cat(&[A, B, NEO])).unwrap_err(),
        BulkloadRefusal::HomesConsolidateRaced
    );
    assert_eq!(fs::read(&path).unwrap(), live);
}
