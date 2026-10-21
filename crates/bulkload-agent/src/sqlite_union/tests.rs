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
            host: "neo".to_owned(),
            source: format!("neo:{}", self.source.display()),
            source_db: self.source.as_os_str().as_bytes().to_vec(),
            source_clock: None,
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
        let homes = crate::consolidate::Ledger::open(&self.state).unwrap();
        let ledger = Ledger::new(
            homes.connection(),
            "neo",
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
    // Sized to finish well inside the 30 s wall on a loaded host: the wall
    // releases the snapshot (BUDGET_EXCEEDED), which is not this test's
    // subject.
    for at in 0..1_000_i64 {
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
    // The writer never waited out its busy timeout: every commit went in,
    // and no union batch held the write lock that long. (Its own fsync is
    // in `longest` too, which on a host at load 100 is seconds, so the
    // bound is the timeout, not a latency.)
    assert!(longest < super::dest::BUSY, "writer waited {longest:?}");
    assert!(stats.longest_batch < super::dest::BUSY, "{stats:?}");
    assert!(stats.batches > 10, "{stats:?}");
    let dest = Connection::open(&fixture.dest).unwrap();
    assert_eq!(count(&dest, "threads"), 1_000 + commits as i64);
    assert_eq!(count(&dest, "logs"), 2_000 + commits as i64);
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
fn an_absent_destination_store_is_created_by_the_row_union() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(fixture.dest.parent().unwrap()).unwrap();
    let source = store(&fixture.source, "");
    thread(&source, "A", "/Users/jess/git/lab", "neo A", 1_000);
    log(&source, 1, "neo");
    drop(source);

    let created = fixture.pass(false);
    assert_eq!(
        super::action(&created),
        Some(crate::consolidate::Action::Created)
    );
    let created = created.unwrap();
    assert!(created.created, "{created:?}");
    // The store, its sidecars, nothing else: no temporary, no copy.
    assert_eq!(beside(&fixture.dest), ["state_5.sqlite"]);
    let dest = Connection::open(&fixture.dest).unwrap();
    let mode: String = dest
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(
        rows(&dest, "SELECT cwd FROM threads"),
        ["/home/jess/git/lab"]
    );
    assert_eq!(count(&dest, "logs"), 1);
    // The migrations came in as rows, and the schema is the source's,
    // trigger and index included.
    assert_eq!(count(&dest, "_sqlx_migrations"), 1);
    let schema = |conn: &Connection| {
        rows(
            conn,
            "SELECT type || ' ' || name || ' ' || coalesce(sql, '') FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
    };
    let source = Connection::open(&fixture.source).unwrap();
    assert_eq!(schema(&dest), schema(&source));
    drop(source);
    // A rerun of the unchanged store reads nothing.
    let rerun = fixture.pass(false).unwrap();
    assert!(rerun.unchanged && !rerun.created, "{rerun:?}");
}

#[test]
fn a_corrupt_source_store_is_held_and_nothing_is_written() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    drop(source);
    std::fs::write(&fixture.source, vec![0x5a_u8; 8192]).unwrap();
    let held = fixture.pass(false);
    assert_eq!(
        held.as_ref().unwrap_err(),
        &BulkloadRefusal::SqliteUnionSourceUnreadable
    );
    assert_eq!(
        super::action(&held),
        Some(crate::consolidate::Action::Held(
            "SQLITE_UNION_SOURCE_UNREADABLE"
        ))
    );
    assert_eq!(count(&dest, "threads"), 0);
    assert!(fixture
        .ledger(|ledger| ledger.identity().unwrap())
        .is_none());
}

const HISTORY: &str = "
    CREATE TABLE thread_items (thread_id TEXT NOT NULL, turn_id TEXT NOT NULL,
        item_id TEXT NOT NULL, rollout_ordinal INTEGER NOT NULL, item_json TEXT NOT NULL,
        PRIMARY KEY (thread_id, turn_id, item_id));
    CREATE TABLE thread_history_projection_state (thread_id TEXT PRIMARY KEY,
        next_rollout_byte_offset INTEGER NOT NULL);
";

fn history(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .unwrap();
    conn.execute_batch(HISTORY).unwrap();
    conn
}

/// An item; its rollout ordinal is its number (`i7` is 7).
fn item(conn: &Connection, thread: &str, item: &str, json: &str) {
    let ordinal: i64 = item.trim_start_matches('i').parse().unwrap();
    conn.execute(
        "INSERT INTO thread_items VALUES (?1, 't1', ?2, ?3, ?4)",
        params![thread, item, ordinal, json],
    )
    .unwrap();
}

impl Fixture {
    fn history_paths(&self) -> (PathBuf, PathBuf) {
        (
            self.source.with_file_name("thread_history_1.sqlite"),
            self.dest.with_file_name("thread_history_1.sqlite"),
        )
    }

    /// The history store's pass: its threads' clock is the state store.
    fn history_pass(&self) -> crate::Result<UnionStats> {
        let (source, dest) = self.history_paths();
        pass(&Options {
            source: format!("neo:{}", source.display()),
            source_db: source.as_os_str().as_bytes().to_vec(),
            source_clock: Some(self.source.as_os_str().as_bytes().to_vec()),
            dest,
            ..self.options(false)
        })
    }
}

#[test]
fn a_threads_rows_in_another_store_follow_its_later_side_as_one() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    // T is later on neo, U on sting.
    thread(&source, "T", "/Users/jess/git/a", "neo T", 2_000);
    thread(&dest, "T", "/home/jess/git/a", "sting T", 1_000);
    thread(&source, "U", "/Users/jess/git/b", "neo U", 1_000);
    thread(&dest, "U", "/home/jess/git/b", "sting U", 3_000);
    drop(source);
    let (source_history, dest_history) = fixture.history_paths();
    let neo = history(&source_history);
    let sting = history(&dest_history);
    item(&neo, "T", "i1", "neo T i1 v2");
    item(&neo, "T", "i2", "neo T i2");
    item(&neo, "U", "i1", "neo U i1");
    item(&neo, "U", "i9", "neo U only");
    neo.execute_batch("INSERT INTO thread_history_projection_state VALUES ('T', 200), ('U', 50)")
        .unwrap();
    item(&sting, "T", "i1", "sting T i1 v1");
    item(&sting, "T", "i3", "sting T only");
    item(&sting, "U", "i1", "sting U i1");
    sting
        .execute_batch("INSERT INTO thread_history_projection_state VALUES ('T', 100), ('U', 300)")
        .unwrap();
    drop(neo);

    fixture.pass(false).unwrap();
    let merged = fixture.history_pass().unwrap();
    assert_eq!(merged.fk_violations, 0);
    // T: neo's thread supplies T's rows; sting's own extra item stays.
    assert_eq!(
        rows(
            &sting,
            "SELECT item_id || ' ' || item_json FROM thread_items WHERE thread_id = 'T' ORDER BY item_id"
        ),
        ["i1 neo T i1 v2", "i2 neo T i2", "i3 sting T only"]
    );
    // U: sting's thread stands for the key both hold; neo's extra goes in
    // (SPEC rule 4, the result contains both), and its differing i1 is
    // ledgered.
    assert_eq!(
        rows(
            &sting,
            "SELECT item_id || ' ' || item_json FROM thread_items WHERE thread_id = 'U' ORDER BY item_id"
        ),
        ["i1 sting U i1", "i9 neo U only"]
    );
    assert_eq!(
        rows(
            &sting,
            "SELECT thread_id || ' ' || next_rollout_byte_offset FROM thread_history_projection_state ORDER BY thread_id"
        ),
        ["T 200", "U 300"]
    );
    let homes = crate::consolidate::Ledger::open(&fixture.state).unwrap();
    let ledger = Ledger::new(
        homes.connection(),
        "neo",
        &format!("neo:{}", source_history.display()),
        &dest_history.to_string_lossy(),
    )
    .unwrap();
    let entries = ledger.entries().unwrap();
    let reasons: Vec<(String, String, String)> = entries
        .iter()
        .map(|(table, key, kept, reason, _)| {
            (table.clone(), format!("{kept} {reason}"), key.clone())
        })
        .collect();
    for (table, word, key) in [
        ("thread_items", "source thread-newer", "\"item_id\":\"i1\""),
        ("thread_items", "dest thread-older", "\"item_id\":\"i1\""),
        (
            "thread_history_projection_state",
            "source thread-newer",
            "\"T\"",
        ),
        (
            "thread_history_projection_state",
            "dest thread-older",
            "\"U\"",
        ),
    ] {
        assert!(
            reasons
                .iter()
                .any(|(t, w, k)| t == table && w == word && k.contains(key)),
            "{table} {word} {key} not in {reasons:?}"
        );
    }
}

#[test]
fn a_changed_thread_has_all_its_items_read_again_and_no_other() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    for (id, at) in [("T", 1_000), ("U", 1_001), ("V", 1_002)] {
        thread(&source, id, "/Users/jess/git/a", "neo", at);
    }
    drop(source);
    drop(dest);
    let (source_history, dest_history) = fixture.history_paths();
    let neo = history(&source_history);
    drop(history(&dest_history));
    for id in ["T", "U", "V"] {
        item(&neo, id, "i1", "v1");
        item(&neo, id, "i2", "v1");
    }
    drop(neo);
    fixture.pass(false).unwrap();
    assert_eq!(fixture.history_pass().unwrap().inserted, 6);

    // T changes on neo: its thread row, and an old item in place (its
    // rowid unchanged, thread_items has no time of its own).
    let source = Connection::open(&fixture.source).unwrap();
    source
        .execute(
            "UPDATE threads SET title = 'later', updated_at_ms = 9000 WHERE id = 'T'",
            [],
        )
        .unwrap();
    drop(source);
    let neo = Connection::open(&source_history).unwrap();
    neo.execute(
        "UPDATE thread_items SET item_json = 'v2' WHERE thread_id = 'T' AND item_id = 'i1'",
        [],
    )
    .unwrap();
    drop(neo);
    fixture.pass(false).unwrap();
    let rerun = fixture.history_pass().unwrap();
    // T's two items, and V's two (V sits at the old mark, which `>=`
    // reads again, adopted); nothing of U.
    assert_eq!(rerun.received, 4, "{rerun:?}");
    assert_eq!(rerun.replaced, 1, "{rerun:?}");
    assert_eq!(rerun.adopted, 3, "{rerun:?}");
    let sting = Connection::open(&dest_history).unwrap();
    assert_eq!(
        rows(
            &sting,
            "SELECT item_json FROM thread_items WHERE thread_id = 'T' AND item_id = 'i1'"
        ),
        ["v2"]
    );
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
    let ledger_file = fixture.state.join("consolidate.sqlite");
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
    fixture.ledger(|ledger| {
        ledger
            .before_commit(&[], Some(("logs", &pending)), &[])
            .unwrap();
    });
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
    fixture.ledger(|ledger| {
        ledger
            .before_commit(&[], Some(("logs", &missing)), &[])
            .unwrap();
    });
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

#[test]
fn a_diverged_thread_keeps_both_sides_extras_and_a_colliding_one_holds_it() {
    let fixture = fixture();
    let (source, dest) = fixture.stores();
    // D diverged: both hosts continued it, sting's later. H too, but neo's
    // extra reuses an ordinal sting used for another item.
    thread(&source, "D", "/Users/jess/git/a", "neo D", 2_000);
    thread(&dest, "D", "/home/jess/git/a", "sting D", 3_000);
    thread(&source, "H", "/Users/jess/git/a", "neo H", 2_000);
    thread(&dest, "H", "/home/jess/git/a", "sting H", 3_000);
    drop(source);
    let (source_history, dest_history) = fixture.history_paths();
    let neo = history(&source_history);
    let sting = history(&dest_history);
    item(&neo, "D", "i1", "shared");
    item(&neo, "D", "i2", "neo extra");
    item(&sting, "D", "i1", "shared");
    item(&sting, "D", "i3", "sting extra");
    item(&neo, "H", "i1", "shared");
    item(&sting, "H", "i1", "shared");
    item(&sting, "H", "i2", "sting extra");
    // neo's own second item, at sting's ordinal 2 under another id.
    neo.execute(
        "INSERT INTO thread_items VALUES ('H', 't1', 'n2', 2, 'neo extra')",
        [],
    )
    .unwrap();
    drop(neo);
    fixture.pass(false).unwrap();
    let merged = fixture.history_pass().unwrap();
    assert_eq!(
        rows(
            &sting,
            "SELECT item_id || ' ' || item_json FROM thread_items WHERE thread_id = 'D' ORDER BY item_id"
        ),
        ["i1 shared", "i2 neo extra", "i3 sting extra"]
    );
    // H is held, typed and listed; nothing of neo's written for it.
    assert_eq!(merged.held_threads, [b"H".to_vec()], "{merged:?}");
    assert_eq!(
        rows(
            &sting,
            "SELECT item_id FROM thread_items WHERE thread_id = 'H' ORDER BY item_id"
        ),
        ["i1", "i2"]
    );
    let homes = crate::consolidate::Ledger::open(&fixture.state).unwrap();
    let ledger = Ledger::new(
        homes.connection(),
        "neo",
        &format!("neo:{}", source_history.display()),
        &dest_history.to_string_lossy(),
    )
    .unwrap();
    assert!(ledger
        .entries()
        .unwrap()
        .iter()
        .any(|(_, key, _, reason, row)| reason == "thread-held"
            && key.contains("n2")
            && row.contains("neo extra")));
    assert_eq!(
        ledger.winners().unwrap().get(b"H".as_slice()),
        Some(&super::ledger::Winner::Held)
    );
}

#[test]
fn a_sessions_skipped_seats_become_stores_state_first_with_their_clock() {
    let routed = super::routed(&[
        b".codex/thread_history_1.sqlite".to_vec(),
        b".codex/logs_2.sqlite-wal".to_vec(),
        b".codex/state_5.sqlite-wal".to_vec(),
        b".codex/state_5.sqlite".to_vec(),
        b".codex/logs_2.sqlite".to_vec(),
        b".codex/state_5.sqlite-shm".to_vec(),
    ]);
    let stores: Vec<&[u8]> = routed
        .iter()
        .map(|routed| routed.store.as_slice())
        .collect();
    assert_eq!(
        stores,
        [
            b".codex/state_5.sqlite".as_slice(),
            b".codex/logs_2.sqlite",
            b".codex/thread_history_1.sqlite"
        ]
    );
    assert_eq!(routed[0].clock, None);
    assert_eq!(
        routed[0].sidecars,
        [
            b".codex/state_5.sqlite-wal".to_vec(),
            b".codex/state_5.sqlite-shm".to_vec()
        ]
    );
    assert_eq!(
        routed[1].clock.as_deref(),
        Some(b".codex/state_5.sqlite".as_slice())
    );
    assert_eq!(routed[1].sidecars, [b".codex/logs_2.sqlite-wal".to_vec()]);
    assert_eq!(
        routed[2].clock.as_deref(),
        Some(b".codex/state_5.sqlite".as_slice())
    );

    // Folded into the session's record: accounted, held typed, or refused.
    let mut stats = crate::transfer::TransferStats::default();
    super::fold(&mut stats, &routed[0], &Ok(UnionStats::default()));
    super::fold(
        &mut stats,
        &routed[1],
        &Err(BulkloadRefusal::SqliteSchemaMismatch),
    );
    super::fold(
        &mut stats,
        &routed[2],
        &Err(BulkloadRefusal::HomesConsolidateRaced),
    );
    assert_eq!(
        stats.consolidated,
        [
            (b".codex/state_5.sqlite".to_vec(), "consolidate".to_owned()),
            (
                b".codex/state_5.sqlite-wal".to_vec(),
                "consolidate".to_owned()
            ),
            (
                b".codex/state_5.sqlite-shm".to_vec(),
                "consolidate".to_owned()
            ),
        ]
    );
    assert_eq!(
        stats.held,
        [
            (b".codex/logs_2.sqlite".to_vec(), "SQLITE_SCHEMA_MISMATCH"),
            (
                b".codex/logs_2.sqlite-wal".to_vec(),
                "SQLITE_SCHEMA_MISMATCH"
            ),
        ]
    );
    assert!(stats.refusals.contains(&(
        b".codex/thread_history_1.sqlite".to_vec(),
        "HOMES_CONSOLIDATE_RACED".to_owned()
    )));
}

#[test]
fn a_consolidating_copy_routes_each_store_to_the_row_union_state_first() {
    let dir = tempfile::tempdir().unwrap();
    let (source_root, dest_root) = (dir.path().join("neo"), dir.path().join("sting"));
    let (source_state, dest_state) = (dir.path().join("neo-state"), dir.path().join("sting-state"));
    std::fs::create_dir_all(source_root.join(".codex")).unwrap();
    std::fs::create_dir_all(dest_root.join(".codex")).unwrap();
    let neo = store(&source_root.join(".codex/state_5.sqlite"), "");
    thread(&neo, "A", "/Users/jess/git/lab", "neo A", 2_000);
    drop(neo);
    let neo_history = history(&source_root.join(".codex/thread_history_1.sqlite"));
    item(&neo_history, "A", "i1", "neo A i1");
    drop(neo_history);
    let sting = store(&dest_root.join(".codex/state_5.sqlite"), "");
    thread(&sting, "C", "/home/jess/git/x", "sting C", 1_000);
    drop(sting);

    let settings = crate::transfer::Consolidation {
        maps: crate::consolidate::Maps::new([(
            "/Users/jess/git".to_owned(),
            "/home/jess/git".to_owned(),
        )]),
        ledger: dest_state.join("consolidate"),
        plan_only: false,
    };
    let root = dest_root.canonicalize().unwrap();
    crate::transfer::set_consolidate_for(&root, Some(Some(settings.clone())));
    let copied = crate::transfer::copy_kind(
        &source_root,
        &root,
        &source_state,
        &dest_state,
        crate::transfer::RootKind::Tree,
    );
    crate::transfer::set_consolidate_for(&root, None);
    let mut stats = copied.unwrap();
    assert!(!stats.sqlite_routed.is_empty(), "{stats:?}");
    let lines = super::union_routed(
        &mut stats,
        &settings,
        (&source_root, &root),
        (None, &|source: &Path| format!("local:{}", source.display())),
        super::local,
    );
    assert_eq!(
        lines,
        [
            "sqlite-union-store .codex/state_5.sqlite consolidate",
            "sqlite-union-store .codex/thread_history_1.sqlite created",
        ]
    );
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert!(stats
        .consolidated
        .contains(&(b".codex/state_5.sqlite".to_vec(), "consolidate".to_owned())));
    let sting = Connection::open(root.join(".codex/state_5.sqlite")).unwrap();
    assert_eq!(
        rows(&sting, "SELECT id || ' ' || cwd FROM threads ORDER BY id"),
        ["A /home/jess/git/lab", "C /home/jess/git/x"]
    );
    let sting_history = Connection::open(root.join(".codex/thread_history_1.sqlite")).unwrap();
    assert_eq!(
        rows(&sting_history, "SELECT item_json FROM thread_items"),
        ["neo A i1"]
    );
    // The live stores and nothing beside them.
    let mut names: Vec<String> = std::fs::read_dir(root.join(".codex"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with("-wal") && !name.ends_with("-shm"))
        .collect();
    names.sort();
    assert_eq!(names, ["state_5.sqlite", "thread_history_1.sqlite"]);
}
