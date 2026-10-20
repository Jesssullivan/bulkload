#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};

use super::ledger::{Ledger, Pending};
use super::{consolidate, serve_rows, Options, UnionStats};
use crate::BulkloadRefusal;

/// A Codex-like store: natural keys with versions and foreign keys, a
/// cascade child, a secondary `UNIQUE`, two `AUTOINCREMENT` tables, a
/// trigger, and sqlx's migration table.
const SCHEMA: &str = "
    CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL,
        installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL,
        checksum BLOB NOT NULL, execution_time BIGINT NOT NULL);
    CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, updated_at_ms INTEGER NOT NULL);
    CREATE TABLE thread_sections (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
        created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, cwd TEXT NOT NULL,
        title TEXT NOT NULL, updated_at_ms INTEGER,
        project_id TEXT REFERENCES projects(id),
        thread_section_id TEXT REFERENCES thread_sections(id));
    CREATE INDEX threads_updated_at_ms ON threads(updated_at_ms);
    CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL, position INTEGER NOT NULL,
        name TEXT NOT NULL, PRIMARY KEY (thread_id, position),
        FOREIGN KEY (thread_id) REFERENCES threads(id) ON DELETE CASCADE);
    CREATE TABLE thread_attachments (id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        kind TEXT NOT NULL, identity_key TEXT NOT NULL, payload TEXT NOT NULL,
        UNIQUE (thread_id, kind, identity_key));
    CREATE TABLE logs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL,
        level TEXT NOT NULL, target TEXT NOT NULL, thread_id TEXT);
    CREATE TABLE queued_thread_revisions (revision INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id TEXT NOT NULL UNIQUE);
    CREATE TRIGGER threads_updated_at_ms_after_insert AFTER INSERT ON threads
        WHEN NEW.updated_at_ms IS NULL
        BEGIN UPDATE threads SET updated_at_ms = NEW.updated_at * 1000 WHERE id = NEW.id; END;
    INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
        VALUES (1, 'threads', 1, x'0102', 5);
";

fn store(path: &Path, extra: &str) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(extra).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    conn
}

fn thread(conn: &Connection, id: &str, cwd: &str, title: &str, updated_ms: i64) {
    conn.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, cwd, title, updated_at_ms)
         VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
        params![
            id,
            format!("/Users/jess/.codex/sessions/2026/10/20/rollout-{id}.jsonl"),
            updated_ms / 1000,
            cwd,
            title,
            updated_ms
        ],
    )
    .unwrap();
}

fn log(conn: &Connection, ts: i64, target: &str) {
    conn.execute(
        "INSERT INTO logs (ts, level, target) VALUES (?1, 'INFO', ?2)",
        params![ts, target],
    )
    .unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    source: PathBuf,
    dest: PathBuf,
    state: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    Fixture {
        source: dir.path().join("neo").join("state_5.sqlite"),
        dest: dir.path().join("sting").join("state_5.sqlite"),
        state,
        _dir: dir,
    }
}

impl Fixture {
    fn stores(&self) -> (Connection, Connection) {
        std::fs::create_dir_all(self.source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(self.dest.parent().unwrap()).unwrap();
        (store(&self.source, ""), store(&self.dest, ""))
    }

    fn options(&self, full: bool) -> Options {
        Options {
            source: format!("neo:{}", self.source.display()),
            source_db: self.source.as_os_str().as_bytes().to_vec(),
            dest: self.dest.clone(),
            state: self.state.clone(),
            maps: super::maps(&[
                (b"/Users/jess/git".to_vec(), b"/home/jess/git".to_vec()),
                (
                    b"/Users/jess/.codex".to_vec(),
                    b"/home/jess/.codex".to_vec(),
                ),
            ]),
            full,
        }
    }

    fn pass(&self, full: bool) -> crate::Result<UnionStats> {
        pass(&self.options(full))
    }

    fn ledger<T>(&self, read: impl FnOnce(&Ledger<'_>) -> T) -> T {
        let conn = super::ledger::open_store(&self.state).unwrap();
        let ledger = Ledger::new(
            &conn,
            &format!("neo:{}", self.source.display()),
            &self.dest.to_string_lossy(),
        )
        .unwrap();
        read(&ledger)
    }
}

/// Both halves over a socket pair, the source on its own thread, as
/// `sqlite-union --local` runs them.
fn pass(options: &Options) -> crate::Result<UnionStats> {
    let (source, mut dest) = UnixStream::pair().unwrap();
    let mut source_in = source.try_clone().unwrap();
    let mut source_out = source;
    let served = std::thread::spawn(move || serve_rows(&mut source_in, &mut source_out));
    let mut dest_in = dest.try_clone().unwrap();
    let merged = consolidate(&mut dest_in, &mut dest, options);
    let _ = dest.shutdown(std::net::Shutdown::Both);
    let _ = served.join().unwrap();
    merged
}

fn rows(conn: &Connection, query: &str) -> Vec<String> {
    let mut statement = conn.prepare(query).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Every file beside the destination store: the store, its live sidecars,
/// and nothing else (no beside copy, no stage).
fn beside(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with("-wal") && !name.ends_with("-shm"))
        .collect();
    names.sort();
    names
}

#[test]
fn disjoint_threads_are_merged_into_the_live_store_respelled_and_never_beside() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    source
        .execute("INSERT INTO projects VALUES ('p1', 'neo project', 10)", [])
        .unwrap();
    thread(&source, "A", "/Users/jess/git/lab", "neo A", 1_000);
    thread(&source, "B", "/Users/jess/git/bulkload/sub", "neo B", 2_000);
    source
        .execute("UPDATE threads SET project_id = 'p1' WHERE id = 'A'", [])
        .unwrap();
    source
        .execute(
            "INSERT INTO thread_dynamic_tools VALUES ('A', 0, 'shell')",
            [],
        )
        .unwrap();
    log(&source, 1, "neo.one");
    log(&source, 2, "neo.two");
    log(&source, 3, "neo.three");
    source
        .execute(
            "INSERT INTO queued_thread_revisions (thread_id) VALUES ('A')",
            [],
        )
        .unwrap();
    thread(&dest, "C", "/home/jess/git/tummycrypt", "sting C", 1_500);
    dest.execute(
        "INSERT INTO thread_dynamic_tools VALUES ('C', 0, 'shell')",
        [],
    )
    .unwrap();
    log(&dest, 1, "sting.one");
    log(&dest, 2, "sting.two");
    dest.execute(
        "INSERT INTO queued_thread_revisions (thread_id) VALUES ('C')",
        [],
    )
    .unwrap();
    drop(source);

    let stats = fixture.pass(false).unwrap();
    assert!(!stats.unchanged);
    assert_eq!(stats.kept + stats.refused + stats.replaced, 0, "{stats:?}");
    assert_eq!(stats.fk_violations, 0);
    assert_eq!(
        rows(
            &dest,
            "SELECT id || ' ' || cwd || ' ' || rollout_path FROM threads ORDER BY id"
        ),
        [
            "A /home/jess/git/lab /home/jess/.codex/sessions/2026/10/20/rollout-A.jsonl",
            "B /home/jess/git/bulkload/sub /home/jess/.codex/sessions/2026/10/20/rollout-B.jsonl",
            "C /home/jess/git/tummycrypt /Users/jess/.codex/sessions/2026/10/20/rollout-C.jsonl",
        ]
    );
    assert_eq!(stats.respelled, 4);
    assert_eq!(
        rows(
            &dest,
            "SELECT thread_id FROM thread_dynamic_tools ORDER BY thread_id"
        ),
        ["A", "C"]
    );
    assert_eq!(
        rows(&dest, "SELECT project_id FROM threads WHERE id = 'A'"),
        ["p1"]
    );
    // Logs get new keys here, every one once.
    assert_eq!(
        rows(&dest, "SELECT target FROM logs ORDER BY id"),
        ["sting.one", "sting.two", "neo.one", "neo.two", "neo.three"]
    );
    assert_eq!(
        rows(
            &dest,
            "SELECT thread_id FROM queued_thread_revisions ORDER BY thread_id"
        ),
        ["A", "C"]
    );
    // sqlx's table is compared, never merged.
    assert_eq!(count(&dest, "_sqlx_migrations"), 1);
    assert_eq!(beside(&fixture.dest), ["state_5.sqlite"]);
    assert!(fixture
        .ledger(|ledger| ledger.entries().unwrap())
        .is_empty());
}

#[test]
fn conflicting_thread_rows_keep_the_later_version_and_ledger_the_other() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    // T1: the source's is later; T2: the destination's; T3: the same
    // version, different titles.
    thread(&source, "T1", "/Users/jess/git/a", "neo T1", 2_000);
    thread(&dest, "T1", "/home/jess/git/a", "sting T1", 1_000);
    thread(&source, "T2", "/Users/jess/git/b", "neo T2", 2_000);
    thread(&dest, "T2", "/home/jess/git/b", "sting T2", 3_000);
    thread(&source, "T3", "/Users/jess/git/c", "neo T3", 5_000);
    thread(&dest, "T3", "/home/jess/git/c", "sting T3", 5_000);
    // T1's child the source does not have: an UPDATE, never a delete and
    // insert, so the cascade never takes it.
    dest.execute(
        "INSERT INTO thread_dynamic_tools VALUES ('T1', 0, 'sting tool')",
        [],
    )
    .unwrap();
    // A secondary UNIQUE the destination already fills with other content.
    source
        .execute(
            "INSERT INTO thread_attachments VALUES ('n1', 'T3', 'file', 'k', 'neo payload')",
            [],
        )
        .unwrap();
    dest.execute(
        "INSERT INTO thread_attachments VALUES ('s1', 'T3', 'file', 'k', 'sting payload')",
        [],
    )
    .unwrap();
    drop(source);

    let stats = fixture.pass(false).unwrap();
    assert_eq!(stats.replaced, 1, "{stats:?}");
    assert_eq!(stats.kept, 2, "{stats:?}");
    assert_eq!(stats.refused, 1, "{stats:?}");
    assert_eq!(
        rows(&dest, "SELECT id || ' ' || title FROM threads ORDER BY id"),
        ["T1 neo T1", "T2 sting T2", "T3 sting T3"]
    );
    assert_eq!(
        rows(
            &dest,
            "SELECT name FROM thread_dynamic_tools WHERE thread_id = 'T1'"
        ),
        ["sting tool"]
    );
    assert_eq!(
        rows(&dest, "SELECT payload FROM thread_attachments"),
        ["sting payload"]
    );
    let entries = fixture.ledger(|ledger| ledger.entries().unwrap());
    let find = |key: &str, kept: &str| {
        entries
            .iter()
            .find(|(table, k, who, _, _)| table == "threads" && k == key && who == kept)
            .unwrap_or_else(|| panic!("no {key} {kept} in {entries:?}"))
            .clone()
    };
    let (_, _, _, reason, row) = find("{\"id\":\"T1\"}", "source");
    assert_eq!(reason, "newer");
    assert!(row.contains("\"title\":\"sting T1\""), "{row}");
    let (_, _, _, reason, row) = find("{\"id\":\"T2\"}", "dest");
    assert_eq!(reason, "older");
    assert!(row.contains("\"title\":\"neo T2\""), "{row}");
    assert!(
        row.contains("\"cwd\":\"/home/jess/git/b\""),
        "respelled: {row}"
    );
    let (_, _, _, reason, _) = find("{\"id\":\"T3\"}", "dest");
    assert_eq!(reason, "same-version");
    assert!(entries.iter().any(
        |(table, _, kept, reason, row)| table == "thread_attachments"
            && kept == "dest"
            && reason.starts_with("constraint-")
            && row.contains("neo payload")
    ));
    let integrity: String = dest
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn a_live_destination_writer_keeps_committing_throughout_the_merge() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    source.execute_batch("BEGIN").unwrap();
    for at in 0..3_000_i64 {
        thread(
            &source,
            &format!("n{at:05}"),
            "/Users/jess/git/lab",
            "neo",
            1_000 + at,
        );
        log(&source, at, "neo");
        log(&source, at, "neo again");
    }
    source.execute_batch("COMMIT").unwrap();
    drop(source);
    drop(dest);

    // The destination's own writer, as a running Codex: its own
    // connection, a busy timeout, one short transaction per commit.
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let done = Arc::clone(&done);
        let path = fixture.dest.clone();
        std::thread::spawn(move || {
            let conn = Connection::open(&path).unwrap();
            conn.busy_timeout(Duration::from_secs(5)).unwrap();
            let (mut commits, mut longest) = (0_u64, Duration::ZERO);
            let mut failures = Vec::new();
            while !done.load(Ordering::Relaxed) || commits < 20 {
                let started = Instant::now();
                let outcome = conn.execute_batch(&format!(
                    "BEGIN IMMEDIATE;
                     INSERT INTO threads (id, rollout_path, created_at, updated_at, cwd, title)
                         VALUES ('w{commits:06}', '/home/jess/.codex/w', 1, 1, '/home/jess/git/w', 'sting');
                     INSERT INTO logs (ts, level, target) VALUES ({commits}, 'INFO', 'sting');
                     COMMIT;"
                ));
                if let Err(error) = outcome {
                    let _ = conn.execute_batch("ROLLBACK");
                    failures.push(error.to_string());
                }
                longest = longest.max(started.elapsed());
                commits += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            (commits, longest, failures)
        })
    };
    let stats = fixture.pass(false);
    done.store(true, Ordering::Relaxed);
    let (commits, longest, failures) = writer.join().unwrap();
    let stats = stats.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(commits >= 20);
    // The writer waited at most about one short batch (plus its fsync, on a
    // loaded host) for each commit.
    assert!(
        longest < Duration::from_secs(3),
        "writer waited {longest:?}"
    );
    assert!(stats.longest_batch < Duration::from_secs(2), "{stats:?}");
    assert!(stats.batches > 10, "{stats:?}");
    let dest = Connection::open(&fixture.dest).unwrap();
    assert_eq!(count(&dest, "threads"), 3_000 + commits as i64);
    assert_eq!(count(&dest, "logs"), 6_000 + commits as i64);
    let integrity: String = dest
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn a_rerun_reads_nothing_when_unchanged_and_only_changed_rows_after() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    for at in 0..50_i64 {
        thread(
            &source,
            &format!("n{at:02}"),
            "/Users/jess/git/lab",
            "neo",
            1_000 + at,
        );
        log(&source, at, "neo");
    }
    drop(source);
    let first = fixture.pass(false).unwrap();
    assert_eq!(first.inserted, 100, "{first:?}");

    let second = fixture.pass(false).unwrap();
    assert!(second.unchanged, "{second:?}");
    assert_eq!(second.received, 0);
    assert_eq!(second.batches, 0);

    // One thread changes and one log is written on the source.
    let source = Connection::open(&fixture.source).unwrap();
    source
        .execute(
            "UPDATE threads SET title = 'neo, later', updated_at_ms = 9000 WHERE id = 'n07'",
            [],
        )
        .unwrap();
    log(&source, 99, "neo, later");
    drop(source);
    let third = fixture.pass(false).unwrap();
    assert!(!third.unchanged);
    // The changed thread, the thread at the old time mark (read again,
    // adopted), and the new log.
    assert!(third.received <= 3, "{third:?}");
    assert_eq!(third.replaced, 1, "{third:?}");
    assert_eq!(third.inserted, 1, "{third:?}");
    assert_eq!(count(&dest, "logs"), 51);
    assert_eq!(
        rows(&dest, "SELECT title FROM threads WHERE id = 'n07'"),
        ["neo, later"]
    );

    // --full reads every natural-key row again, writes nothing, and never
    // reads a surrogate row twice.
    let full = fixture.pass(true).unwrap();
    assert_eq!(full.received, 50, "{full:?}");
    assert_eq!(full.adopted, 50);
    assert_eq!(full.inserted, 0);
    assert_eq!(count(&dest, "logs"), 51);
    assert_eq!(count(&dest, "threads"), 50);
}

#[test]
fn a_schema_mismatch_is_refused_and_nothing_is_written() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(fixture.dest.parent().unwrap()).unwrap();
    let source = store(
        &fixture.source,
        "ALTER TABLE threads ADD COLUMN extra TEXT;",
    );
    let dest = store(&fixture.dest, "");
    thread(&source, "A", "/Users/jess/git/lab", "neo A", 1_000);
    log(&source, 1, "neo");
    drop(source);
    let before: i64 = dest
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        fixture.pass(false).unwrap_err(),
        BulkloadRefusal::SqliteSchemaMismatch
    );
    assert_eq!(count(&dest, "threads"), 0);
    assert_eq!(count(&dest, "logs"), 0);
    let after: i64 = dest
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        after, before,
        "another connection committed to the destination"
    );
    assert!(fixture
        .ledger(|ledger| ledger.identity().unwrap())
        .is_none());
    assert!(fixture
        .ledger(|ledger| ledger.entries().unwrap())
        .is_empty());
}

#[test]
fn an_absent_destination_store_is_refused_and_never_created() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(fixture.dest.parent().unwrap()).unwrap();
    drop(store(&fixture.source, ""));
    assert_eq!(
        fixture.pass(false).unwrap_err(),
        BulkloadRefusal::SqliteUnionTargetAbsent
    );
    assert!(beside(&fixture.dest).is_empty());
}

#[test]
fn a_source_not_in_wal_mode_is_refused() {
    let fixture = fixture();
    let (source, _dest) = fixture.stores();
    source
        .query_row("PRAGMA journal_mode=DELETE", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    drop(source);
    assert_eq!(
        fixture.pass(false).unwrap_err(),
        BulkloadRefusal::SqliteUnionSourceNotWal
    );
}

#[test]
fn a_surrogate_batch_found_committed_after_an_interruption_is_never_inserted_again() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    for at in 0..5_i64 {
        log(&source, at, "neo");
    }
    drop(source);
    fixture.pass(false).unwrap();
    assert_eq!(count(&dest, "logs"), 5);

    // As if the pass had stopped between the destination's commit and the
    // ledger's confirmation: no mark, and the batch still pending.
    let inserted: Vec<Vec<bulkload_proto::rows::Value>> = {
        let mut statement = dest
            .prepare("SELECT ts, level, target, thread_id FROM logs ORDER BY rowid")
            .unwrap();
        statement
            .query_map([], |row| {
                (0..4)
                    .map(|at| row.get_ref(at).map(super::value_of))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let (low, high): (i64, i64) = dest
        .query_row("SELECT min(rowid), max(rowid) FROM logs", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    let ledger_file = fixture.state.join(super::ledger::FILE);
    Connection::open(&ledger_file)
        .unwrap()
        .execute(
            "UPDATE row_union_marks SET after_rowid = NULL WHERE tbl = 'logs'",
            [],
        )
        .unwrap();
    let pending = Pending {
        to_rowid: 5,
        low,
        high,
        digest: super::dest::digest(&inserted),
    };
    fixture.ledger(|ledger| ledger.before_commit(&[], Some(("logs", &pending))).unwrap());
    // The store changes, so the rerun reads.
    let source = Connection::open(&fixture.source).unwrap();
    log(&source, 6, "neo, later");
    drop(source);

    let rerun = fixture.pass(false).unwrap();
    assert_eq!(rerun.inserted, 1, "{rerun:?}");
    assert_eq!(count(&dest, "logs"), 6);
    assert!(fixture
        .ledger(|ledger| ledger.pending("logs").unwrap())
        .is_none());

    // A pending batch whose rows are not there (its commit never
    // happened) is dropped and the marks stand: nothing is inserted twice.
    let missing = Pending {
        to_rowid: 7,
        low: 1000,
        high: 1005,
        digest: [7; 32],
    };
    fixture.ledger(|ledger| ledger.before_commit(&[], Some(("logs", &missing))).unwrap());
    let source = Connection::open(&fixture.source).unwrap();
    log(&source, 7, "neo, latest");
    drop(source);
    let after = fixture.pass(false).unwrap();
    assert!(fixture
        .ledger(|ledger| ledger.pending("logs").unwrap())
        .is_none());
    assert_eq!(after.inserted, 1, "{after:?}");
    assert_eq!(count(&dest, "logs"), 7);
}
