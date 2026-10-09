//! P80 to P83: `SQLite` snapshot seats (#218,
//! `docs/agent-notes/2026-10-08-sqlite-carry-design.md`).
//!
//! - **P80 SQLITE-CARRY-CONSISTENT** (G1, G3; S2): a live store, its writer a
//!   child process committing during the carry, arrives as one committed
//!   state, verified, in journal mode DELETE, with no sidecar beside it; the
//!   source's main file (and an existing `-wal`) keep their bytes; a WAL
//!   writer is never refused for busy. Pinned rows: a closed WAL store with
//!   no `-wal` (OI-1003-Q72), a hot journal, a store committed to between
//!   the backup and the chunking (review R12, deterministic), a busy WAL
//!   store under the stepped backup (D2), a symlinked source path (R9).
//! - **P81 SQLITE-CARRY-RESUME** (G5; S3, R25): an unchanged rerun takes no
//!   snapshot and reads 0 bytes; a closed WAL store is reused on its second
//!   run; a changed store among unchanged ones is the only one snapshotted;
//!   an unsettled capture keeps no reuse row; an unrowed output is adopted
//!   only under its settled `-wal` key (R1); `refuse` mode is v5's.
//! - **P82 SQLITE-CARRY-REFUSALS** (G6; S4, OI-1003-Q76, R4, R10): every
//!   corruption kind ends in the closed set, nothing is published, the slot
//!   is gone, and an unchanged rerun refuses from the record with 0 bytes;
//!   the session as root and a database another user owns are refused
//!   before anything is opened; a peer that sends a non-database as a
//!   snapshot is refused at the destination.
//! - **P83 SQLITE-CARRY-SIDECARS** (G2; R2, R5): over source and destination
//!   sidecar layouts, no sidecar byte is carried, a sidecar is covered only
//!   by its base's snapshot, and a destination sidecar refuses its path with
//!   nothing read on the source, until it is removed.
//!
//! The live writer is this test binary re-run as [`p80_writer_child`], so
//! its POSIX locks are its own (P75's pattern). Generated cases come from
//! fixed seeds, twenty times as many in the deep tier (OI-1003-Q7, Q78; no
//! fuzzing). Tests that read the process-scope `SQLite` counters hold
//! [`serial`].

use super::*;
use crate::counters::{Counter, Counters};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

/// Holds the tests that read process-scope `SQLite` counters apart.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A corpus whose source half runs as an ordinary user, whoever runs the
/// tests (CI runs them as root; the refusal is P82's and
/// `tests/sqlite_carry.rs`'s).
struct Sqlite {
    corpus: Corpus,
    root: PathBuf,
}

impl Sqlite {
    fn new() -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("source")).unwrap();
        assume_uid(&root, Some(AssumeUid::Unprivileged));
        Self { corpus, root }
    }

    fn source(&self) -> PathBuf {
        self.corpus.base.join("source")
    }

    fn destination(&self) -> PathBuf {
        self.corpus.base.join("destination")
    }

    fn run(&self, mode: SqliteMode) -> Result<TransferStats> {
        copy_with(
            &self.source(),
            &self.destination(),
            &self.corpus.base.join("source-state"),
            &self.corpus.base.join("destination-state"),
            mode,
        )
    }

    fn snapshot(&self) -> TransferStats {
        self.run(SqliteMode::Snapshot).unwrap()
    }

    /// The source store's remembered refusals.
    fn remembered(&self) -> u64 {
        Store::open(&self.corpus.base.join("source-state"))
            .unwrap()
            .refused_seats()
            .unwrap()
    }

    /// Whatever is left in the source's snapshot slot directory.
    fn slots(&self) -> usize {
        std::fs::read_dir(self.corpus.base.join("source-state").join(SLOT_DIR))
            .map_or(0, Iterator::count)
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        assume_uid(&self.root, None);
        set_after_snapshot(&self.root, None);
        set_before_backup(&self.root, None);
        set_capture_clock(&self.root, None);
        crate::provider_sqlite::set_carry_steps(&self.root, None);
        SLOT_OVERRIDE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(root, _)| *root != self.root);
        RETAIN_OVERRIDE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(root, _)| *root != self.root);
    }
}

/// How a generated store keeps its journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Journal {
    Wal,
    Delete,
}

/// Create a store: a ledger of rows `1..=rows`, each committed in its own
/// transaction that also moves one unit between two accounts, so the sum
/// is invariant and the rows say which transaction a state is.
fn build(path: &Path, journal: Journal, page_size: u32, rows: u64) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA page_size={page_size};
             PRAGMA journal_mode={};
             CREATE TABLE ledger(k INTEGER PRIMARY KEY, v BLOB NOT NULL);
             CREATE TABLE accounts(id INTEGER PRIMARY KEY, amount INTEGER NOT NULL);
             INSERT INTO accounts VALUES (1, 1000), (2, 0);",
            match journal {
                Journal::Wal => "WAL",
                Journal::Delete => "DELETE",
            }
        ))
        .unwrap();
    for _ in 0..rows {
        connection.execute_batch(COMMIT_ONE).unwrap();
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// One transaction: the next ledger row, and one unit moved.
const COMMIT_ONE: &str = "BEGIN IMMEDIATE;
    INSERT INTO ledger(k, v) SELECT coalesce(max(k), 0) + 1, randomblob(48) FROM ledger;
    UPDATE accounts SET amount = amount - 1 WHERE id = 1;
    UPDATE accounts SET amount = amount + 1 WHERE id = 2;
    COMMIT;";

/// What a published database holds: its ledger rows, and whether its
/// accounts are one transaction's state (sum and split agree with them).
fn published(path: &Path) -> Vec<u64> {
    let uri = format!("file:{}?immutable=1", path.display());
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok", "{}", path.display());
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        mode,
        "delete",
        "{}: published in DELETE mode",
        path.display()
    );
    let rows: Vec<u64> = connection
        .prepare("SELECT k FROM ledger ORDER BY k")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let (first, second): (i64, i64) = connection
        .query_row(
            "SELECT (SELECT amount FROM accounts WHERE id = 1), (SELECT amount FROM accounts WHERE id = 2)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let count = i64::try_from(rows.len()).unwrap();
    assert_eq!(
        first + second,
        1000,
        "{}: the sum is invariant",
        path.display()
    );
    assert_eq!(second, count, "{}: one transaction's state", path.display());
    assert_eq!(
        rows,
        (1..=rows.len() as u64).collect::<Vec<_>>(),
        "{}: rows 1..=k, never torn",
        path.display()
    );
    rows
}

/// The ledger rows of a source store, read in this process with a
/// read-only connection (only after the copy: no in-process connection may
/// share the source with the transfer's).
fn source_rows(path: &Path) -> u64 {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row("SELECT count(*) FROM ledger", [], |row| row.get(0))
        .unwrap()
}

/// Whether these tests really run as root (CI). The source half is told to
/// read as an ordinary user ([`AssumeUid::Unprivileged`]), but `SQLite`
/// itself still runs as root, and re-applies a database's owner to each
/// `-wal` it opens (OI-1003-Q76): the `-wal`'s ctime moves on every read, so
/// a capture beside an existing `-wal` never settles. The legs that depend
/// on a `-wal` keeping its identity across a read are proved as an ordinary
/// user (here, and in `tests/sqlite_carry.rs`'s dropped leg); as root they
/// say so on stderr and are skipped.
fn really_root(leg: &str) -> bool {
    let root = crate::io::sys::effective_uid() == 0;
    if root {
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            format!(
                "sqlite-carry root: {leg} skipped: as root SQLite re-owns the -wal it opens \
                 (OI-1003-Q76), so its identity moves on every read\n"
            )
            .as_bytes(),
        );
    }
    root
}

/// A session's refusals of `base`, apart from those of its sidecars, which
/// must each be refused by name exactly when `base` was refused (R2).
fn base_refusals(stats: &TransferStats, base: &[u8]) -> Vec<String> {
    let mut own = Vec::new();
    let mut sidecars = Vec::new();
    for (path, code) in &stats.refusals {
        if path == base {
            own.push(code.clone());
        } else {
            let sidecar = path
                .strip_prefix(base)
                .is_some_and(|suffix| crate::walk::SQLITE_SIDECAR_SUFFIXES.contains(&suffix));
            assert!(sidecar, "an unexpected refusal: {:?}", stats.refusals);
            assert_eq!(code, "SQLITE_STATE_CHANGED", "{:?}", stats.refusals);
            sidecars.push(path.clone());
        }
    }
    if own.is_empty() {
        assert!(sidecars.is_empty(), "{:?}", stats.refusals);
    }
    own
}

/// No `-wal`, `-shm` or `-journal` beside a destination path.
fn no_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        assert!(
            std::fs::symlink_metadata(&name).is_err(),
            "{}{suffix} beside a published snapshot",
            path.display()
        );
    }
}

/// Every entry of a directory: its bytes and stat identity.
fn scan(dir: &Path) -> BTreeMap<std::ffi::OsString, (Vec<u8>, StatIdentity)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let bytes = if meta.is_file() {
                std::fs::read(&path).unwrap()
            } else {
                Vec::new()
            };
            (
                entry.file_name(),
                (bytes, StatIdentity::from_metadata(&meta)),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The live writer: this binary, re-run as one test
// ---------------------------------------------------------------------------

/// Names the database the writer child opens; unset, the child is a no-op.
const WRITER: &str = "BULKLOAD_P80_WRITER";

/// Marks the writer child's protocol lines.
const TAG: &str = "p80> ";

/// The writer child. Opens the database named by [`WRITER`] with a zero
/// busy timeout and no automatic checkpoint, prints `ready`, then takes one
/// command per stdin line: `commit` (one [`COMMIT_ONE`]), `loop <ms>`
/// (commit every `ms` until the next command), `stop`, or any SQL batch.
/// Each commit prints `ack <k>` or `busy`. At the end of stdin it closes its
/// connection, as a provider process does when it exits cleanly.
#[test]
fn p80_writer_child() {
    let Some(path) = std::env::var_os(WRITER) else {
        return;
    };
    let connection = Connection::open(PathBuf::from(path)).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    connection
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let (lines, commands) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let commit = |connection: &Connection| -> std::result::Result<u64, rusqlite::Error> {
        let done = connection.execute_batch(COMMIT_ONE);
        if done.is_err() {
            let _ = connection.execute_batch("ROLLBACK");
        }
        done?;
        connection.query_row("SELECT max(k) FROM ledger", [], |row| row.get(0))
    };
    let say = |line: String| {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{TAG}{line}");
        let _ = stdout.flush();
    };
    say("ready".to_owned());
    let mut every: Option<Duration> = None;
    let mut acked = 0;
    loop {
        let next = match every {
            Some(period) => match commands.recv_timeout(period) {
                Ok(line) => Some(line),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match commands.recv() {
                Ok(line) => Some(line),
                Err(_) => break,
            },
        };
        match next.as_deref() {
            None | Some("commit") => match commit(&connection) {
                Ok(k) => {
                    acked = k;
                    say(format!("ack {k}"));
                }
                Err(_) => say("busy".to_owned()),
            },
            Some("stop") => {
                every = None;
                say(format!("stopped {acked}"));
            }
            Some(line) => {
                if let Some(ms) = line.strip_prefix("loop ") {
                    every = Some(Duration::from_millis(ms.parse().unwrap()));
                    say("looping".to_owned());
                } else {
                    match connection.execute_batch(line) {
                        Ok(()) => say("ok".to_owned()),
                        Err(error) => say(format!("err {error}")),
                    }
                }
            }
        }
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// What the parent has heard from a writer.
#[derive(Default)]
struct Heard {
    /// The last acknowledged commit's row.
    acked: u64,
    /// Commits refused for busy.
    busy: u64,
    /// Replies other than `ack` and `busy`, in order.
    replies: VecDeque<String>,
}

/// A writer child and what it said.
struct Writer {
    child: Child,
    stdin: Option<ChildStdin>,
    heard: Arc<(Mutex<Heard>, Condvar)>,
}

impl Writer {
    fn start(database: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "transfer::tests::sqlite_carry::p80_writer_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WRITER, database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let heard = Arc::new((Mutex::new(Heard::default()), Condvar::new()));
        {
            let heard = Arc::clone(&heard);
            std::thread::spawn(move || {
                for line in stdout.lines() {
                    let Ok(line) = line else { break };
                    // The first line follows libtest's own `test ... `.
                    let Some(said) = line.find(TAG).map(|at| &line[at + TAG.len()..]) else {
                        continue;
                    };
                    let (state, ready) = &*heard;
                    {
                        let mut state = state.lock().unwrap();
                        if let Some(k) = said.strip_prefix("ack ") {
                            state.acked = k.parse().unwrap();
                        } else if said == "busy" {
                            state.busy += 1;
                        } else {
                            state.replies.push_back(said.to_owned());
                        }
                    }
                    ready.notify_all();
                }
            });
        }
        let writer = Self {
            child,
            stdin,
            heard,
        };
        assert_eq!(writer.reply(), "ready");
        writer
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn reply(&self) -> String {
        let (state, ready) = &*self.heard;
        let mut state = state.lock().unwrap();
        loop {
            if let Some(reply) = state.replies.pop_front() {
                return reply;
            }
            let (next, timeout) = ready.wait_timeout(state, Duration::from_mins(1)).unwrap();
            assert!(!timeout.timed_out(), "the writer child went quiet");
            state = next;
        }
    }

    /// Commit one transaction now and wait for its answer: the row it
    /// committed.
    fn commit(&mut self) -> u64 {
        let before = self.acked();
        self.send("commit");
        let (state, ready) = &*self.heard;
        let mut state = state.lock().unwrap();
        while state.acked == before {
            let (next, timeout) = ready.wait_timeout(state, Duration::from_mins(1)).unwrap();
            assert!(!timeout.timed_out(), "no ack");
            state = next;
        }
        state.acked
    }

    fn acked(&self) -> u64 {
        self.heard.0.lock().unwrap().acked
    }

    fn busy(&self) -> u64 {
        self.heard.0.lock().unwrap().busy
    }

    fn sql(&mut self, batch: &str) {
        self.send(batch);
        assert_eq!(self.reply(), "ok", "{batch}");
    }

    fn start_loop(&mut self, every_ms: u64) {
        self.send(&format!("loop {every_ms}"));
        assert_eq!(self.reply(), "looping");
    }

    /// Stop committing; the last acknowledged row.
    fn stop(&mut self) -> u64 {
        self.send("stop");
        let reply = self.reply();
        reply
            .strip_prefix("stopped ")
            .unwrap_or_else(|| panic!("{reply}"))
            .parse()
            .unwrap()
    }

    /// Close the child's stdin and wait for it to close its connection.
    fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // A failed test still lets its child close and exit.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Seeds
// ---------------------------------------------------------------------------

/// The fixed seeds a property draws its cases from: these, and in the deep
/// tier nineteen more streams of them.
fn seeds(pinned: &[u64]) -> Vec<u64> {
    let rounds = if crate::test_support::deep() { 20 } else { 1 };
    (0..rounds)
        .flat_map(|round| {
            pinned
                .iter()
                .map(move |seed| seed.wrapping_add(round * 0x9E37_79B9))
        })
        .collect()
}

/// A tiny deterministic generator over a seed.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

// ---------------------------------------------------------------------------
// P80 SQLITE-CARRY-CONSISTENT
// ---------------------------------------------------------------------------

/// One generated P80 case.
#[derive(Debug)]
struct P80Case {
    journal: Journal,
    page_size: u32,
    rows: u64,
    every_ms: u64,
}

impl P80Case {
    fn generate(seed: u64) -> Self {
        let mut draw = Draw(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        Self {
            journal: if draw.below(2) == 0 {
                Journal::Wal
            } else {
                Journal::Delete
            },
            page_size: if draw.below(2) == 0 { 1024 } else { 4096 },
            rows: 1 + draw.below(40),
            every_ms: 2 + draw.below(19),
        }
    }
}

const P80_SEEDS: [u64; 6] = [1, 2, 3, 5, 8, 13];

/// P80: a live store, committed to by a child every 2 to 20 ms during the
/// carry, arrives as one committed state: rows `1..=k`, the sum invariant,
/// with `k` at least the commits acknowledged before the carry began (R12),
/// verified and in journal mode DELETE with no sidecar beside it. A capture
/// the stepped backup could not finish ends `BUDGET_EXCEEDED` or, for a
/// rollback-mode step that met the writer's lock, `SQLITE_STATE_CHANGED`,
/// with nothing published (D2). In WAL mode the writer is never refused for
/// busy, and the main file keeps its bytes and identity (no checkpoint).
#[test]
fn p80_a_live_store_arrives_as_one_committed_state() {
    let _serial = serial();
    let mut published_cases = 0;
    let cases: Vec<P80Case> = seeds(&P80_SEEDS)
        .into_iter()
        .map(P80Case::generate)
        .collect();
    for journal in [Journal::Wal, Journal::Delete] {
        assert!(
            cases.iter().any(|case| case.journal == journal),
            "the seeds draw {journal:?}"
        );
    }
    for case in &cases {
        let sqlite = Sqlite::new();
        // Two pages a step (review): every generated store takes several
        // steps, so a live commit can land between them.
        crate::provider_sqlite::set_carry_steps(&sqlite.root, Some((2, None)));
        let database = sqlite.source().join("store.db");
        build(&database, case.journal, case.page_size, case.rows);
        let mut writer = Writer::start(&database);
        writer.commit();
        writer.commit();
        let main_before = std::fs::read(&database).unwrap();
        let identity_before = StatIdentity::from_metadata(&std::fs::metadata(&database).unwrap());
        writer.start_loop(case.every_ms);
        let acked_before = writer.acked();
        let stats = sqlite.snapshot();
        let acked_after = writer.stop();
        let output = sqlite.destination().join("store.db");
        let refused = base_refusals(&stats, b"store.db");
        if refused.is_empty() {
            let rows = published(&output);
            let k = rows.len() as u64;
            assert!(
                (acked_before..=acked_after).contains(&k),
                "{case:?}: k {k} outside the commits [{acked_before}, {acked_after}]"
            );
            no_sidecars(&output);
            assert_eq!(stats.sqlite_snapshots, [b"store.db".to_vec()]);
            published_cases += 1;
        } else {
            assert_eq!(refused.len(), 1, "{case:?}: {:?}", stats.refusals);
            let code = &refused[0];
            assert!(
                code == "BUDGET_EXCEEDED"
                    || (case.journal == Journal::Delete && code == "SQLITE_STATE_CHANGED"),
                "{case:?}: {code}"
            );
            assert!(
                !output.exists(),
                "{case:?}: a refused store is not published"
            );
        }
        assert_eq!(sqlite.slots(), 0, "{case:?}: no slot outlives its entry");
        if case.journal == Journal::Wal {
            assert_eq!(writer.busy(), 0, "{case:?}: a WAL writer is never held up");
            assert_eq!(std::fs::read(&database).unwrap(), main_before, "{case:?}");
            assert_eq!(
                StatIdentity::from_metadata(&std::fs::metadata(&database).unwrap()),
                identity_before,
                "{case:?}: the main file keeps its identity"
            );
        }
        writer.finish();
    }
    assert!(published_cases > 0, "some live store was carried");
}

/// P80 pinned (OI-1003-Q72): a closed WAL store has no `-wal`; its read
/// creates an empty one, and the capture still settles, so the second run
/// reuses it with nothing opened (P81's second-run clause too).
#[test]
fn p80_a_closed_wal_store_is_carried_and_reused_on_its_second_run() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("closed.db");
    build(&database, Journal::Wal, 4096, 12);
    let wal = sqlite.source().join("closed.db-wal");
    assert!(!wal.exists(), "a closed WAL store keeps no -wal");
    let main = std::fs::read(&database).unwrap();
    let snapshots = Counters::snapshot();
    let first = sqlite.snapshot();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(published(&sqlite.destination().join("closed.db")).len(), 12);
    assert_eq!(std::fs::read(&database).unwrap(), main);
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len(),
        0,
        "the read's own -wal is empty"
    );
    let second = sqlite.snapshot();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(second.source_bytes_read, 0);
    assert_eq!(second.reused, 1);
    assert_eq!(second.bytes_received, 0);
    assert_eq!(
        Counters::snapshot()
            .since(snapshots)
            .get(Counter::SourceSqliteSnapshots),
        1,
        "the second run takes no snapshot"
    );
}

/// P80 pinned: a rollback-mode store with a hot journal cannot be read by
/// a read-only connection (it would have to roll the journal back). It is
/// refused `SQLITE_BACKUP_FAILED`, not remembered, nothing is published, and
/// its journal, a sidecar of a base that was not carried, is refused by
/// name.
#[test]
fn p80_a_hot_journal_is_refused_and_not_remembered() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let scratch = sqlite.corpus.base.join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let live = scratch.join("hot.db");
    build(&live, Journal::Delete, 1024, 0);
    {
        let connection = Connection::open(&live).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fill(v BLOB);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
                 INSERT INTO fill SELECT randomblob(900) FROM n;",
            )
            .unwrap();
    }
    let mut writer = Writer::start(&live);
    // A transaction that spills to the database file: its journal is hot
    // in any copy taken before it ends.
    writer.sql("PRAGMA cache_size=2; BEGIN IMMEDIATE; UPDATE fill SET v = randomblob(900);");
    std::fs::copy(&live, sqlite.source().join("hot.db")).unwrap();
    std::fs::copy(
        scratch.join("hot.db-journal"),
        sqlite.source().join("hot.db-journal"),
    )
    .unwrap();
    writer.sql("ROLLBACK;");
    writer.finish();
    for _ in 0..2 {
        let stats = sqlite.snapshot();
        let mut refusals = stats.refusals.clone();
        refusals.sort();
        assert_eq!(
            refusals,
            [
                (b"hot.db".to_vec(), "SQLITE_BACKUP_FAILED".to_owned()),
                (
                    b"hot.db-journal".to_vec(),
                    "SQLITE_STATE_CHANGED".to_owned()
                ),
            ]
        );
        assert!(!sqlite.destination().join("hot.db").exists());
        assert_eq!(
            stats.default_disposition(b"hot.db", "SQLITE_BACKUP_FAILED"),
            None,
            "a hot journal is not corruption: {:?}",
            stats.refusal_sqlite_codes
        );
        assert_eq!(sqlite.remembered(), 0, "a hot journal is not remembered");
        assert_eq!(sqlite.slots(), 0);
    }
}

/// P80 pinned (review R12): a commit that lands after the backup and before
/// the snapshot is chunked never reaches the carried bytes. A capture that
/// sent the live main file instead of its slot would carry it.
#[test]
fn p80_a_commit_after_the_backup_is_not_carried() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("store.db");
    build(&database, Journal::Delete, 1024, 5);
    let hooked = {
        let database = database.clone();
        Arc::new(move || {
            let connection = Connection::open(&database).unwrap();
            connection.execute_batch(COMMIT_ONE).unwrap();
            connection.close().map_err(|(_, error)| error).unwrap();
        })
    };
    set_after_snapshot(&sqlite.root, Some(hooked));
    let before = Counters::snapshot();
    let stats = sqlite.snapshot();
    set_after_snapshot(&sqlite.root, None);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        published(&sqlite.destination().join("store.db")).len(),
        5,
        "the carried bytes are the backup's"
    );
    assert_eq!(source_rows(&database), 6, "the source has the later commit");
    let delta = Counters::snapshot().since(before);
    assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 1);
    assert!(delta.get(Counter::SourceSnapshotRead) > 0);
    // The changed store is snapshotted again, and its own older output,
    // untouched and with no sidecar beside it, is superseded through the
    // exchange (OI-1003-Q146).
    let before = Counters::snapshot();
    let rerun = sqlite.snapshot();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::DestSqliteSuperseded),
        1
    );
    assert_eq!(published(&sqlite.destination().join("store.db")).len(), 6);
    no_sidecars(&sqlite.destination().join("store.db"));
}

/// P80 pinned (D2): a WAL store larger than a few steps, with a writer
/// committing every 2 ms, under the stepped backup: it is carried as one
/// committed state or refused `BUDGET_EXCEEDED` (counted restarts, not
/// remembered), and once the writer stops it is carried.
#[test]
fn p80_a_busy_wal_store_is_carried_or_refused_for_budget() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("busy.db");
    build(&database, Journal::Wal, 1024, 0);
    {
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fill(v BLOB);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
                 INSERT INTO fill SELECT randomblob(800) FROM n;",
            )
            .unwrap();
    }
    let mut writer = Writer::start(&database);
    writer.start_loop(2);
    let before = Counters::snapshot();
    let busy = sqlite.snapshot();
    let restarts = Counters::snapshot()
        .since(before)
        .get(Counter::SourceSqliteBackupRestarts);
    writer.stop();
    match base_refusals(&busy, b"busy.db").as_slice() {
        [] => {
            published(&sqlite.destination().join("busy.db"));
        }
        [code] => {
            assert_eq!(code, "BUDGET_EXCEEDED");
            assert!(restarts > 0, "a budget refusal comes from restarts");
            assert_eq!(sqlite.remembered(), 0, "a budget refusal is not remembered");
            let calm = sqlite.snapshot();
            assert!(calm.refusals.is_empty(), "{:?}", calm.refusals);
            published(&sqlite.destination().join("busy.db"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(writer.busy(), 0, "a WAL writer is never held up");
    writer.finish();
}

/// P80 pinned (review R9): a source named through a symlink is carried:
/// `serve` canonicalises the root, so `SQLITE_OPEN_NOFOLLOW` meets no link.
#[test]
fn p80_a_source_named_through_a_symlink_is_carried() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    build(&sqlite.source().join("store.db"), Journal::Wal, 4096, 3);
    let link = sqlite.corpus.base.join("link");
    std::os::unix::fs::symlink(sqlite.source(), &link).unwrap();
    let stats = copy_with(
        &link,
        &sqlite.destination(),
        &sqlite.corpus.base.join("source-state"),
        &sqlite.corpus.base.join("destination-state"),
        SqliteMode::Snapshot,
    )
    .unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(published(&sqlite.destination().join("store.db")).len(), 3);
}

// ---------------------------------------------------------------------------
// P81 SQLITE-CARRY-RESUME
// ---------------------------------------------------------------------------

/// P81: an unchanged rerun reads 0 content bytes, takes no snapshot and is
/// answered `Reuse`; a commit to one store of many makes the rerun take
/// exactly that store's snapshot.
#[test]
fn p81_an_unchanged_store_is_reused_and_only_a_changed_one_is_read() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let stores = ["a.db", "b.db", "c.db", "d.db"];
    for (index, name) in stores.iter().enumerate() {
        let journal = if index % 2 == 0 {
            Journal::Wal
        } else {
            Journal::Delete
        };
        build(&sqlite.source().join(name), journal, 1024, 3 + index as u64);
    }
    std::fs::write(sqlite.source().join("plain"), noise(5, 9000)).unwrap();
    let first = sqlite.snapshot();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(first.sqlite_snapshots.len(), 4);
    for _ in 0..2 {
        let before = Counters::snapshot();
        let warm = sqlite.snapshot();
        let delta = Counters::snapshot().since(before);
        assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
        assert_eq!(warm.source_bytes_read, 0);
        assert_eq!(warm.bytes_received, 0);
        assert_eq!(warm.reused, 5);
        assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 0);
        assert_eq!(delta.get(Counter::SourceSqliteBackupBytes), 0);
    }
    // A commit to one DELETE-mode store (its main file moves).
    let connection = Connection::open(sqlite.source().join("b.db")).unwrap();
    connection.execute_batch(COMMIT_ONE).unwrap();
    connection.close().map_err(|(_, error)| error).unwrap();
    let before = Counters::snapshot();
    let changed = sqlite.snapshot();
    let delta = Counters::snapshot().since(before);
    assert!(changed.refusals.is_empty(), "{:?}", changed.refusals);
    assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 1, "only b.db");
    // Its own older output is superseded in place (OI-1003-Q146).
    assert_eq!(delta.get(Counter::DestSqliteSuperseded), 1);
    assert_eq!(changed.reused, 4);
    assert_eq!(published(&sqlite.destination().join("b.db")).len(), 5);
}

/// P81: a capture stamped within the racy allowance is not settled: it is
/// published with an ownership row and no reuse row, so the next run takes
/// a snapshot again; that one, of the same bytes, is adopted, and the run
/// after it reads nothing.
#[test]
fn p81_an_unsettled_capture_keeps_no_reuse_row() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("store.db");
    build(&database, Journal::Delete, 1024, 4);
    let clock = PinnedClock::at(&sqlite.corpus, stamp_ns(&database) + 500_000_000);
    let racy = sqlite.snapshot();
    drop(clock);
    assert!(racy.refusals.is_empty(), "{:?}", racy.refusals);
    let before = Counters::snapshot();
    let again = sqlite.snapshot();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::SourceSqliteSnapshots),
        1,
        "an unsettled capture's store is read again"
    );
    assert_eq!(again.reused, 0);
    let settled = sqlite.snapshot();
    assert_eq!(settled.reused, 1);
    assert_eq!(settled.source_bytes_read, 0);
}

/// P81 (review R1): an output published by a group whose row commit failed
/// is adopted with no source read while its store is unchanged; after a WAL
/// commit with no checkpoint (its main file unchanged), its capture record
/// names another `-wal`, so it is never adopted as current: the store is
/// read again, and the stale output, which this store does not own, is
/// refused and kept.
#[test]
fn p81_an_unrowed_snapshot_is_adopted_only_under_its_settled_wal() {
    let _serial = serial();
    for commit in [false, true] {
        if !commit && really_root("p81 unrowed adoption") {
            continue;
        }
        let sqlite = Sqlite::new();
        let database = sqlite.source().join("live.db");
        build(&database, Journal::Wal, 4096, 4);
        let mut writer = Writer::start(&database);
        writer.commit();
        let _ = sqlite.run(SqliteMode::Snapshot).unwrap();
        // The same capture again, its row lost: unrowed, with its record.
        std::fs::remove_file(sqlite.destination().join("live.db")).unwrap();
        let store_root = destination_store_root(&sqlite.corpus);
        crate::transfer_store::fail_output_commits(&store_root, true);
        let failed = sqlite.run(SqliteMode::Snapshot);
        crate::transfer_store::fail_output_commits(&store_root, false);
        assert_eq!(
            base_refusals(&failed.unwrap(), b"live.db"),
            ["DESTINATION_SPACE_INSUFFICIENT"]
        );
        let unrowed = std::fs::read(sqlite.destination().join("live.db")).unwrap();
        let main = std::fs::read(&database).unwrap();
        if commit {
            writer.commit();
            assert_eq!(std::fs::read(&database).unwrap(), main, "no checkpoint");
        }
        let before = Counters::snapshot();
        let resumed = sqlite.snapshot();
        let snapshots = Counters::snapshot()
            .since(before)
            .get(Counter::SourceSqliteSnapshots);
        if commit {
            assert_eq!(snapshots, 1, "the changed store is read again");
            assert_eq!(resumed.unrowed_adopted, 0, "never adopted as current");
            assert_eq!(
                base_refusals(&resumed, b"live.db"),
                ["DESTINATION_OCCUPIED"]
            );
            assert_eq!(
                std::fs::read(sqlite.destination().join("live.db")).unwrap(),
                unrowed
            );
        } else {
            assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
            assert_eq!(snapshots, 0, "adopted with nothing opened");
            assert_eq!(resumed.unrowed_adopted, 1);
            assert_eq!(resumed.source_bytes_read, 0);
            assert_eq!(resumed.sqlite_snapshots, [b"live.db".to_vec()]);
        }
        writer.finish();
    }
}

/// P81 (R8): `refuse` mode is v5's: a database and its sidecars are refused
/// `SQLITE_STATE_CHANGED`, and an output a `snapshot`-mode run published is
/// not reused (its key is not a row key) but refused at the source by its
/// sniff, from the record on the next run.
#[test]
fn p81_refuse_mode_is_v5() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("store.db");
    build(&database, Journal::Delete, 1024, 2);
    std::fs::write(sqlite.source().join("store.db-journal"), [0_u8; 512]).unwrap();
    let refused = |stats: &TransferStats| {
        let mut refusals = stats.refusals.clone();
        refusals.sort();
        assert_eq!(
            refusals,
            [
                (b"store.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
                (
                    b"store.db-journal".to_vec(),
                    "SQLITE_STATE_CHANGED".to_owned()
                ),
            ]
        );
        assert_eq!(stats.source_bytes_read, 0);
    };
    refused(&sqlite.run(SqliteMode::Refuse).unwrap());
    assert!(!sqlite.destination().join("store.db").exists());
    let carried = sqlite.snapshot();
    assert!(carried.refusals.is_empty(), "{:?}", carried.refusals);
    assert_eq!(
        carried.sqlite_sidecars_covered,
        [b"store.db-journal".to_vec()]
    );
    // Back in `refuse` mode: v5's refusal, from the v5 record.
    refused(&sqlite.run(SqliteMode::Refuse).unwrap());
    assert_eq!(sqlite.remembered(), 1);
}

// ---------------------------------------------------------------------------
// P82 SQLITE-CARRY-REFUSALS
// ---------------------------------------------------------------------------

/// How P82 damages a store.
#[derive(Clone, Copy, Debug)]
enum Damage {
    /// An interior table page overwritten with garbage.
    InteriorPage,
    /// The header past the magic overwritten (its page size invalid).
    Header,
    /// An index page overwritten with garbage.
    IndexPage,
    /// The file cut to a few pages, its header still counting them all.
    Truncated,
}

/// P82: every corruption kind ends in the closed set
/// (`SQLITE_INTEGRITY_CHECK_FAILED`, `SQLITE_BACKUP_FAILED`), never `IO`
/// or `FRAME_CODEC`; nothing is published and no slot is left; an
/// unchanged rerun refuses from the record with 0 source bytes, and so does
/// the run after.
#[test]
fn p82_a_corrupt_store_is_refused_in_the_closed_set_and_remembered() {
    let _serial = serial();
    for damage in [
        Damage::InteriorPage,
        Damage::Header,
        Damage::IndexPage,
        Damage::Truncated,
    ] {
        let sqlite = Sqlite::new();
        let database = sqlite.source().join("rescue.db");
        build(&database, Journal::Delete, 1024, 0);
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE fill(k INTEGER PRIMARY KEY, v BLOB, w TEXT);
                     WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300)
                     INSERT INTO fill SELECT i, randomblob(300), hex(randomblob(40)) FROM n;
                     CREATE INDEX fill_w ON fill(w);",
                )
                .unwrap();
        }
        let mut bytes = std::fs::read(&database).unwrap();
        let page = |bytes: &mut Vec<u8>, number: usize| {
            for byte in &mut bytes[(number - 1) * 1024 + 8..number * 1024] {
                *byte = 0xA5;
            }
        };
        match damage {
            Damage::InteriorPage => page(&mut bytes, 5),
            Damage::Header => {
                for byte in &mut bytes[16..100] {
                    *byte = 0x5A;
                }
            }
            Damage::IndexPage => {
                let root: i64 = Connection::open(&database)
                    .unwrap()
                    .query_row(
                        "SELECT rootpage FROM sqlite_schema WHERE name = 'fill_w'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                page(&mut bytes, usize::try_from(root).unwrap());
            }
            Damage::Truncated => bytes.truncate(1024 * 6),
        }
        std::fs::write(&database, &bytes).unwrap();
        let mut codes = Vec::new();
        for round in 0..3 {
            let stats = sqlite.snapshot();
            assert_eq!(stats.refusals.len(), 1, "{damage:?}: {:?}", stats.refusals);
            let (path, code) = &stats.refusals[0];
            assert_eq!(path, b"rescue.db");
            assert!(
                code == "SQLITE_INTEGRITY_CHECK_FAILED" || code == "SQLITE_BACKUP_FAILED",
                "{damage:?}: {code}"
            );
            // OI-1003-Q148: corruption is abandoned by default, whichever
            // step met it, from the record as well (round > 0).
            assert_eq!(
                stats.default_disposition(path, code),
                Some("abandon"),
                "{damage:?} round {round}: {code} {:?}",
                stats.refusal_sqlite_codes
            );
            assert!(
                !sqlite.destination().join("rescue.db").exists(),
                "{damage:?}"
            );
            assert_eq!(sqlite.slots(), 0, "{damage:?}");
            if round > 0 {
                assert_eq!(stats.source_bytes_read, 0, "{damage:?}: from the record");
            }
            codes.push(code.clone());
        }
        assert!(codes.windows(2).all(|pair| pair[0] == pair[1]), "{codes:?}");
        assert_eq!(sqlite.remembered(), 1, "{damage:?}: remembered");
    }
}

/// P82 (R8): the v5 record a `refuse`-mode run leaves does not stop a
/// `snapshot`-mode run, and a later `refuse`-mode run honours it again.
#[test]
fn p82_a_v5_header_record_does_not_stop_a_snapshot() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    build(&sqlite.source().join("store.db"), Journal::Wal, 4096, 2);
    let refused = sqlite.run(SqliteMode::Refuse).unwrap();
    assert_eq!(
        refused.refusals,
        [(b"store.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned())]
    );
    assert_eq!(sqlite.remembered(), 1);
    let carried = sqlite.snapshot();
    assert!(carried.refusals.is_empty(), "{:?}", carried.refusals);
    published(&sqlite.destination().join("store.db"));
    // The snapshot's read left a wal-index and an empty `-wal` (Q36, Q72):
    // `refuse` mode refuses them by name, as v5 does, and the database from
    // its v5 record.
    let again = sqlite.run(SqliteMode::Refuse).unwrap();
    let mut refusals = again.refusals.clone();
    refusals.sort();
    assert_eq!(
        refusals,
        [
            (b"store.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
            (b"store.db-shm".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
            (b"store.db-wal".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
        ]
    );
    assert_eq!(again.source_bytes_read, 0);
}

/// P82 (OI-1003-Q76, R14): a `snapshot`-mode session as root is refused
/// before anything is opened or created on the source, through `copy`, whose
/// source half is `serve`; `refuse` mode as root is unchanged.
#[test]
fn p82_a_snapshot_session_as_root_is_refused_before_any_open() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    build(&sqlite.source().join("store.db"), Journal::Wal, 4096, 2);
    std::fs::write(sqlite.source().join("plain"), b"bytes").unwrap();
    assume_uid(&sqlite.root, Some(AssumeUid::Root));
    let before = scan(&sqlite.source());
    assert_eq!(
        sqlite.run(SqliteMode::Snapshot).unwrap_err(),
        BulkloadRefusal::SqliteSourceAsRoot
    );
    assert_eq!(scan(&sqlite.source()), before, "the source as it was");
    assert!(
        !sqlite.corpus.base.join("source-state").exists(),
        "no source store is created"
    );
    let refuse = sqlite.run(SqliteMode::Refuse).unwrap();
    assert_eq!(
        refuse.refusals,
        [(b"store.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned())]
    );
    assert_eq!(refuse.completed, 1);
}

/// P82 (R4): a database another user owns is refused
/// `SQLITE_SOURCE_NOT_OWNER` from the sniff's descriptor, before `SQLite`
/// opens it: no `-shm` or `-wal` is created beside it, and the rest of the
/// tree is carried.
#[test]
fn p82_another_users_database_is_refused_before_sqlite_opens_it() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    build(&sqlite.source().join("theirs.db"), Journal::Wal, 4096, 2);
    std::fs::write(sqlite.source().join("plain"), b"bytes").unwrap();
    assume_uid(
        &sqlite.root,
        Some(AssumeUid::Euid(
            crate::io::sys::effective_uid().wrapping_add(1),
        )),
    );
    let before = scan(&sqlite.source());
    let stats = sqlite.snapshot();
    assert_eq!(
        stats.refusals,
        [(b"theirs.db".to_vec(), "SQLITE_SOURCE_NOT_OWNER".to_owned())]
    );
    assert_eq!(scan(&sqlite.source()), before, "no sidecar created");
    assert_eq!(stats.completed, 1, "the plain file is carried");
}

/// P82: a peer that sends a non-database as a snapshot is refused
/// `SQLITE_INTEGRITY_CHECK_FAILED` at the destination, and nothing is
/// published: the destination verifies what it publishes (G3).
#[test]
#[allow(clippy::too_many_lines)] // One scripted peer, read top to bottom.
fn p82_a_non_database_sent_as_a_snapshot_is_refused_at_the_destination() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let (mut peer, ours) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut payload = b"SQLite format 3\0".to_vec();
    payload.extend(noise(9, 4096 - 16));
    let source = std::thread::spawn(move || {
        let Frame::Control(Control::Open { sqlite, .. }) = read_frame(&mut peer).unwrap() else {
            panic!("no Open");
        };
        assert_eq!(sqlite, SqliteMode::Snapshot);
        write_control(
            &mut peer,
            &Control::Start {
                authority: b"peer".to_vec(),
            },
        )
        .unwrap();
        let row = RowSchema {
            rel_path: b"fake.db".to_vec(),
            kind: FileKind::Regular,
            dev: 1,
            ino: 2,
            size: 4096,
            mtime_ns: 1,
            ctime_ns: 1,
            mode: 0o100_644,
            nlink: 1,
            link_target: None,
            blake3: None,
        };
        write_control(
            &mut peer,
            &Control::Entry {
                entry: 0,
                row,
                wal: None,
            },
        )
        .unwrap();
        write_control(&mut peer, &Control::WalkDone { entries: 1 }).unwrap();
        loop {
            match read_frame(&mut peer).unwrap() {
                Frame::Control(Control::Decide { decision, .. }) => {
                    assert_eq!(decision, Decision::Send);
                    break;
                }
                Frame::Control(Control::Credit { .. }) => {}
                other => panic!("{other:?}"),
            }
        }
        write_control(
            &mut peer,
            &Control::SqliteSnapshot {
                entry: 0,
                size: 4096,
                wal: None,
            },
        )
        .unwrap();
        let digest = *blake3::hash(&payload).as_bytes();
        write_data(
            &mut peer,
            &DataHeader {
                entry: 0,
                index: 0,
                size: 4096,
                offset: 0,
                digest,
            },
            &payload,
        )
        .unwrap();
        write_control(
            &mut peer,
            &Control::End {
                entry: 0,
                root: manifest_root(&[ChunkSpec { digest, size: 4096 }]),
                chunks: 1,
                size: 4096,
                racy: false,
            },
        )
        .unwrap();
        loop {
            match read_frame(&mut peer).unwrap() {
                Frame::Control(Control::Held { held, .. }) => {
                    assert!(!held);
                    break;
                }
                Frame::Control(Control::Credit { .. }) => {}
                other => panic!("{other:?}"),
            }
        }
        write_control(
            &mut peer,
            &Control::SourceDone {
                entries: 1,
                source_bytes_read: 0,
            },
        )
        .unwrap();
        peer
    });
    let mut input = ours.try_clone().unwrap();
    let mut output = ours;
    let stats = receive_with(
        &mut input,
        &mut output,
        Path::new("/peer/source"),
        Path::new("/peer/state"),
        &sqlite.destination(),
        &sqlite.corpus.base.join("destination-state"),
        SqliteMode::Snapshot,
    )
    .unwrap();
    drop(source.join().unwrap());
    assert_eq!(
        stats.refusals,
        [(
            b"fake.db".to_vec(),
            "SQLITE_INTEGRITY_CHECK_FAILED".to_owned()
        )]
    );
    assert_eq!(std::fs::read_dir(sqlite.destination()).unwrap().count(), 0);
}

/// P82: a snapshot frame in a `refuse`-mode session ends it: a peer that
/// claims a snapshot it was not asked for is refused, never believed.
#[test]
fn p82_a_snapshot_frame_in_refuse_mode_is_a_protocol_violation() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let (mut peer, ours) = std::os::unix::net::UnixStream::pair().unwrap();
    let source = std::thread::spawn(move || {
        let _ = read_frame(&mut peer).unwrap();
        write_control(
            &mut peer,
            &Control::Start {
                authority: b"peer".to_vec(),
            },
        )
        .unwrap();
        let _ = write_control(
            &mut peer,
            &Control::SqliteSidecar {
                rel_path: b"x-wal".to_vec(),
                database: b"x".to_vec(),
            },
        );
        peer
    });
    let mut input = ours.try_clone().unwrap();
    let mut output = ours;
    assert_eq!(
        receive_with(
            &mut input,
            &mut output,
            Path::new("/peer/source"),
            Path::new("/peer/state"),
            &sqlite.destination(),
            &sqlite.corpus.base.join("destination-state"),
            SqliteMode::Refuse,
        )
        .unwrap_err(),
        BulkloadRefusal::ProtocolStateViolation
    );
    drop(source.join().unwrap());
}

// ---------------------------------------------------------------------------
// P83 SQLITE-CARRY-SIDECARS
// ---------------------------------------------------------------------------

/// What lies at the destination before a P83 run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destined {
    Nothing,
    StaleWal,
    HotJournal,
    Shm,
    OwnOutput,
    Foreign,
}

/// The bytes a source sidecar holds, so a carried one would show.
fn marker(suffix: &str) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes.extend_from_slice(format!("SIDECAR-MARKER{suffix}").as_bytes());
    bytes.extend(noise(77, 4000));
    bytes
}

/// P83: over source layouts {base present, absent} x {`-wal`, `-shm`,
/// `-journal`, none} and destination layouts {nothing, a stale `-wal`, a
/// hot `-journal`, a `-shm`, the store's own output, a foreign file}: no
/// sidecar byte reaches the destination; a sidecar is covered only when its
/// base is held as a snapshot, refused by name otherwise; a snapshot is
/// published only where no sidecar sat beside its path, and a destination
/// sidecar refuses the path `DESTINATION_OCCUPIED` at its decision, with no
/// snapshot taken, until it is removed.
#[test]
#[allow(clippy::too_many_lines)] // One matrix, its expectations inline.
fn p83_sidecars_follow_their_base_and_are_never_carried() {
    let _serial = serial();
    let layouts = [
        Destined::Nothing,
        Destined::StaleWal,
        Destined::HotJournal,
        Destined::Shm,
        Destined::OwnOutput,
        Destined::Foreign,
    ];
    for base in [true, false] {
        for sidecar in [Some("-wal"), Some("-shm"), Some("-journal"), None] {
            if base && sidecar == Some("-wal") && really_root("p83 with a -wal beside the base") {
                continue;
            }
            for destined in layouts {
                let case = format!("base={base} sidecar={sidecar:?} destination={destined:?}");
                let sqlite = Sqlite::new();
                let database = sqlite.source().join("store.db");
                let output = sqlite.destination().join("store.db");
                if base {
                    build(&database, Journal::Delete, 1024, 3);
                }
                if destined == Destined::OwnOutput && base {
                    assert!(sqlite.snapshot().refusals.is_empty(), "{case}");
                }
                if let Some(suffix) = sidecar {
                    std::fs::write(
                        sqlite.source().join(format!("store.db{suffix}")),
                        marker(suffix),
                    )
                    .unwrap();
                }
                let stale = match destined {
                    Destined::StaleWal => Some(("store.db-wal", vec![7_u8; 4128])),
                    Destined::HotJournal => {
                        let mut hot = vec![0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
                        hot.extend(vec![1_u8; 1016]);
                        Some(("store.db-journal", hot))
                    }
                    Destined::Shm => Some(("store.db-shm", vec![0_u8; 32768])),
                    Destined::Foreign => Some(("store.db", b"someone else's".to_vec())),
                    Destined::Nothing | Destined::OwnOutput => None,
                };
                if let Some((name, bytes)) = &stale {
                    std::fs::write(sqlite.destination().join(name), bytes).unwrap();
                }
                let destination_before = scan(&sqlite.destination());
                let before = Counters::snapshot();
                let stats = sqlite.snapshot();
                let snapshots = Counters::snapshot()
                    .since(before)
                    .get(Counter::SourceSqliteSnapshots);
                // No sidecar byte is ever on the destination.
                for entry in std::fs::read_dir(sqlite.destination()).unwrap() {
                    let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                    assert!(
                        !bytes.windows(14).any(|window| window == b"SIDECAR-MARKER"),
                        "{case}: a sidecar's bytes were carried"
                    );
                }
                let sidecar_path = sidecar.map(|suffix| format!("store.db{suffix}").into_bytes());
                let mut refusals = stats.refusals.clone();
                refusals.sort();
                if !base {
                    // An orphan sidecar is offered and refused by name.
                    let expected: Vec<_> = sidecar_path
                        .iter()
                        .map(|path| (path.clone(), "SQLITE_STATE_CHANGED".to_owned()))
                        .collect();
                    if destined == Destined::Foreign {
                        assert!(output.exists(), "{case}: the foreign file stays");
                    }
                    assert_eq!(refusals, expected, "{case}");
                    assert_eq!(snapshots, 0, "{case}");
                    continue;
                }
                let blocked = matches!(
                    destined,
                    Destined::StaleWal | Destined::HotJournal | Destined::Shm
                );
                let mut expected = Vec::new();
                if blocked || destined == Destined::Foreign {
                    expected.push((b"store.db".to_vec(), "DESTINATION_OCCUPIED".to_owned()));
                    if let Some(path) = &sidecar_path {
                        expected.push((path.clone(), "SQLITE_STATE_CHANGED".to_owned()));
                    }
                    expected.sort();
                    assert_eq!(refusals, expected, "{case}");
                    assert_eq!(
                        scan(&sqlite.destination()),
                        destination_before,
                        "{case}: the destination as it was"
                    );
                    if blocked {
                        assert_eq!(snapshots, 0, "{case}: refused before any read");
                        assert_eq!(stats.source_bytes_read, 0, "{case}");
                        // Remove the sidecar: the next run converges.
                        let (name, _) = stale.as_ref().unwrap();
                        std::fs::remove_file(sqlite.destination().join(name)).unwrap();
                        let converged = sqlite.snapshot();
                        assert!(
                            converged.refusals.is_empty(),
                            "{case}: {:?}",
                            converged.refusals
                        );
                        published(&output);
                        no_sidecars(&output);
                    } else {
                        // Remembered: the unchanged rerun reads nothing.
                        let again = sqlite.snapshot();
                        assert_eq!(again.source_bytes_read, 0, "{case}");
                    }
                } else {
                    assert!(refusals.is_empty(), "{case}: {refusals:?}");
                    published(&output);
                    no_sidecars(&output);
                    assert_eq!(
                        stats.sqlite_sidecars_covered,
                        sidecar_path.iter().cloned().collect::<Vec<_>>(),
                        "{case}"
                    );
                    if destined == Destined::OwnOutput {
                        if sidecar == Some("-wal") {
                            // A `-wal` that appeared moves the seat's key:
                            // the store is read once, and its own output,
                            // the same bytes, is adopted.
                            assert_eq!(snapshots, 1, "{case}");
                            assert_eq!(stats.reused, 0, "{case}");
                        } else {
                            assert_eq!(stats.reused, 1, "{case}");
                            assert_eq!(snapshots, 0, "{case}");
                        }
                    }
                }
            }
        }
    }
}

/// P83 (R2, #218 review): a sidecar's coverage follows what its base turned
/// out to be. A store without the `SQLite` magic (an encrypted one) whose
/// `-wal` holds frames is refused `SQLITE_STATE_CHANGED`, never published
/// raw: its main file alone may miss committed transactions or hold a
/// half-done checkpoint. The refusal is remembered under its row and its
/// `-wal`'s identity, so an unchanged rerun reads nothing. Beside an empty
/// `-wal` (no frame) such a file is carried raw, and its `-wal` refused by
/// name; a plain `notes` is carried and the `notes-journal` beside it is
/// refused, as v5 does.
#[test]
fn p83_a_sidecar_of_a_base_that_is_not_a_database_is_refused() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let cipher = noise(31, 8192);
    std::fs::write(sqlite.source().join("signal.db"), &cipher).unwrap();
    std::fs::write(sqlite.source().join("signal.db-wal"), marker("-wal")).unwrap();
    let quiet = noise(37, 8192);
    std::fs::write(sqlite.source().join("quiet.db"), &quiet).unwrap();
    std::fs::write(sqlite.source().join("quiet.db-wal"), b"").unwrap();
    std::fs::write(sqlite.source().join("notes"), b"my notes").unwrap();
    std::fs::write(sqlite.source().join("notes-journal"), b"my journal").unwrap();
    let expected = [
        (b"notes-journal".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
        (b"quiet.db-wal".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
        (b"signal.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
        (b"signal.db-wal".to_vec(), "SQLITE_STATE_CHANGED".to_owned()),
    ];
    let stats = sqlite.snapshot();
    let mut refusals = stats.refusals.clone();
    refusals.sort();
    assert_eq!(refusals, expected);
    assert!(stats.sqlite_sidecars_covered.is_empty());
    assert!(
        !sqlite.destination().join("signal.db").exists(),
        "a store with a live -wal is never published raw"
    );
    assert_eq!(
        std::fs::read(sqlite.destination().join("quiet.db")).unwrap(),
        quiet
    );
    assert_eq!(
        std::fs::read(sqlite.destination().join("notes")).unwrap(),
        b"my notes"
    );
    assert!(!sqlite.destination().join("signal.db-wal").exists());
    assert_eq!(sqlite.remembered(), 1, "signal.db, under its -wal key");
    // Unchanged: the raw files are reused, signal.db is refused from its
    // record with nothing opened, and the sidecars are refused again.
    let before = Counters::snapshot();
    let again = sqlite.snapshot();
    let mut refusals = again.refusals.clone();
    refusals.sort();
    assert_eq!(refusals, expected);
    assert_eq!(again.reused, 2);
    assert_eq!(again.source_bytes_read, 0);
    assert_eq!(
        Counters::snapshot().since(before).get(Counter::SourceSniff),
        0,
        "refused from the record, not sniffed again"
    );
    // A new frame in the -wal moves its identity: sniffed and refused again.
    let mut grown = marker("-wal");
    grown.extend(noise(41, 4096));
    std::fs::write(sqlite.source().join("signal.db-wal"), grown).unwrap();
    let moved = sqlite.snapshot();
    assert!(moved
        .refusals
        .contains(&(b"signal.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned())));
    assert!(!sqlite.destination().join("signal.db").exists());
}

/// The live sidecars of a WAL store with a writer are covered by its
/// snapshot (P83's live leg): the `-wal` and `-shm` are reported, never
/// carried, and the store is published without them.
#[test]
fn p83_a_live_stores_sidecars_are_covered_by_its_snapshot() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("live.db");
    build(&database, Journal::Wal, 4096, 6);
    let mut writer = Writer::start(&database);
    writer.commit();
    let stats = sqlite.snapshot();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    let mut covered = stats.sqlite_sidecars_covered;
    covered.sort();
    assert_eq!(covered, [b"live.db-shm".to_vec(), b"live.db-wal".to_vec()]);
    assert_eq!(published(&sqlite.destination().join("live.db")).len(), 7);
    no_sidecars(&sqlite.destination().join("live.db"));
    if really_root("p83 live warm rerun") {
        writer.finish();
        return;
    }
    // The writer is idle: the rerun reuses the store under its -wal key.
    let warm = sqlite.snapshot();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!(warm.reused, 1);
    assert_eq!(warm.source_bytes_read, 0);
    writer.finish();
}

// ---------------------------------------------------------------------------
// #218 review, round 2
// ---------------------------------------------------------------------------

/// A store of `rows` ledger rows plus `fill` blobs of 800 bytes, so it spans
/// many 1 KiB pages.
fn build_large(path: &Path, journal: Journal, rows: u64, fill: u32) {
    build(path, journal, 1024, rows);
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE fill(v BLOB);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {fill})
             INSERT INTO fill SELECT randomblob(800) FROM n;"
        ))
        .unwrap();
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// P80 pinned (review): a commit that lands between two steps of the
/// backup, in either journal mode, restarts it from page 1 (counted), and
/// the carried store is one committed state that includes the commit: the
/// restart path, with the read lock released between steps (Q16), is
/// exercised deterministically, not left to a writer's timing.
#[test]
fn p80_a_commit_between_steps_restarts_the_backup_and_arrives_whole() {
    let _serial = serial();
    for journal in [Journal::Wal, Journal::Delete] {
        let sqlite = Sqlite::new();
        let database = sqlite.source().join("store.db");
        build_large(&database, journal, 10, 60);
        let writer = Arc::new(Mutex::new(Writer::start(&database)));
        let acked_before = writer.lock().unwrap().commit();
        let hook: crate::provider_sqlite::StepHook = {
            let writer = Arc::clone(&writer);
            Arc::new(move |step, done| {
                if step == 2 && !done {
                    writer.lock().unwrap().commit();
                }
            })
        };
        crate::provider_sqlite::set_carry_steps(&sqlite.root, Some((4, Some(hook))));
        let before = Counters::snapshot();
        let stats = sqlite.snapshot();
        let delta = Counters::snapshot().since(before);
        crate::provider_sqlite::set_carry_steps(&sqlite.root, None);
        assert!(
            stats.refusals.is_empty(),
            "{journal:?}: {:?}",
            stats.refusals
        );
        let acked_after = writer.lock().unwrap().acked();
        assert_eq!(acked_after, acked_before + 1, "{journal:?}");
        assert!(
            delta.get(Counter::SourceSqliteBackupRestarts) > 0,
            "{journal:?}: the commit between steps restarted the backup"
        );
        let rows = published(&sqlite.destination().join("store.db"));
        assert_eq!(
            rows.len() as u64,
            acked_after,
            "{journal:?}: the restarted backup carries the commit"
        );
        assert_eq!(writer.lock().unwrap().busy(), 0, "{journal:?}");
        let writer = Arc::into_inner(writer).unwrap().into_inner().unwrap();
        writer.finish();
    }
}

/// #218 review: a sniffed database's descriptor is closed only under the
/// backup lock. Closing any descriptor of an inode drops every POSIX lock
/// this process holds on it, so a sniff closed while another seat's backup
/// (a hard link of the same store) holds `SHARED` would let a writer
/// checkpoint under that backup.
#[test]
fn a_sniffed_database_is_closed_under_the_backup_lock() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("a.db");
    build(&database, Journal::Wal, 4096, 3);
    std::fs::hard_link(&database, sqlite.source().join("b.db")).unwrap();
    build(&sqlite.source().join("c.db"), Journal::Delete, 1024, 2);
    let before = SNIFF_CLOSED_UNLOCKED.load(Ordering::Relaxed);
    let stats = sqlite.snapshot();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(stats.sqlite_snapshots.len(), 3);
    assert_eq!(
        SNIFF_CLOSED_UNLOCKED.load(Ordering::Relaxed) - before,
        0,
        "every sniffed database closed under the backup lock"
    );
}

/// P82 (review): a store whose index entry no longer matches its row (the
/// same number of entries, one holding a stale value) passes `quick_check`,
/// which does not compare index content with table content, but not
/// `integrity_check`. The source checks its snapshot as the
/// destination does, so it is refused `SQLITE_INTEGRITY_CHECK_FAILED` at the
/// source, remembered, and an unchanged rerun reads nothing and sends
/// nothing.
#[test]
fn p82_an_index_out_of_step_with_its_table_is_refused_at_the_source_and_remembered() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("index.db");
    {
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE fill(k INTEGER PRIMARY KEY, w TEXT);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
                 INSERT INTO fill SELECT i, hex(randomblob(20)) FROM n;
                 CREATE INDEX fill_w ON fill(w);",
            )
            .unwrap();
        let (root, sql): (i64, String) = connection
            .query_row(
                "SELECT rootpage, sql FROM sqlite_schema WHERE name = 'fill_w'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        // Hide the index, change a row behind its back, then restore it: its
        // b-tree keeps the row's old value.
        connection
            .execute_batch(
                "PRAGMA writable_schema=ON;
                 DELETE FROM sqlite_schema WHERE name = 'fill_w';
                 PRAGMA writable_schema=OFF;",
            )
            .unwrap();
        connection.close().map_err(|(_, error)| error).unwrap();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("UPDATE fill SET w = 'stale' WHERE k = 7;")
            .unwrap();
        connection.execute("PRAGMA writable_schema=ON", []).unwrap();
        connection
            .execute(
                "INSERT INTO sqlite_schema VALUES ('index', 'fill_w', 'fill', ?1, ?2)",
                rusqlite::params![root, sql],
            )
            .unwrap();
        connection.close().map_err(|(_, error)| error).unwrap();
        let connection = Connection::open(&database).unwrap();
        let quick: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(quick, "ok", "the damage passes quick_check");
        let full: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_ne!(full, "ok", "and fails integrity_check");
    }
    for round in 0..3 {
        let stats = sqlite.snapshot();
        assert_eq!(
            stats.refusals,
            [(
                b"index.db".to_vec(),
                "SQLITE_INTEGRITY_CHECK_FAILED".to_owned()
            )],
            "round {round}"
        );
        assert!(!sqlite.destination().join("index.db").exists());
        assert_eq!(stats.bytes_received, 0, "round {round}: nothing sent");
        if round > 0 {
            assert_eq!(stats.source_bytes_read, 0, "round {round}: from the record");
        }
    }
    assert_eq!(sqlite.remembered(), 1);
}

/// #218 review: a store larger than what is left of the in-memory retention
/// budget is still offered as a manifest when the destination asks for one
/// (the slot budget governs a snapshot's manifest, design 5.6): an existing
/// output of the same bytes is adopted with no content on the wire.
#[test]
fn a_snapshot_manifest_is_not_bounded_by_the_memory_budget() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("store.db");
    build_large(&database, Journal::Delete, 4, 120);
    let size = std::fs::metadata(&database).unwrap().len();
    // An unsettled first capture: published, no reuse row.
    let clock = PinnedClock::at(&sqlite.corpus, stamp_ns(&database) + 500_000_000);
    let racy = sqlite.snapshot();
    drop(clock);
    assert!(racy.refusals.is_empty(), "{:?}", racy.refusals);
    RETAIN_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((sqlite.root.clone(), size / 4));
    let again = sqlite.snapshot();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.bytes_received, 0,
        "the destination filled every chunk from the output it holds"
    );
    assert_eq!(published(&sqlite.destination().join("store.db")).len(), 4);
}

/// P81 (review): a WAL store whose last commit sits only in its `-wal` (the
/// main file unchanged, no checkpoint) is not reused after that commit: its
/// key holds the `-wal`'s identity. The rerun snapshots it again and its
/// older output is superseded through the exchange (OI-1003-Q146).
#[test]
fn p81_a_commit_only_in_the_wal_is_not_reused() {
    let _serial = serial();
    if really_root("p81 wal-only commit") {
        return;
    }
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("live.db");
    build(&database, Journal::Wal, 4096, 4);
    let mut writer = Writer::start(&database);
    writer.commit();
    let first = sqlite.snapshot();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let warm = sqlite.snapshot();
    assert!(warm.refusals.is_empty(), "{:?}", warm.refusals);
    assert_eq!(warm.reused, 1);
    assert_eq!(warm.source_bytes_read, 0);
    let main = std::fs::read(&database).unwrap();
    let identity = StatIdentity::from_metadata(&std::fs::metadata(&database).unwrap());
    let k = writer.commit();
    assert_eq!(std::fs::read(&database).unwrap(), main, "no checkpoint");
    assert_eq!(
        StatIdentity::from_metadata(&std::fs::metadata(&database).unwrap()),
        identity,
        "the main file did not move"
    );
    let before = Counters::snapshot();
    let changed = sqlite.snapshot();
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::SourceSqliteSnapshots),
        1,
        "the store is read again"
    );
    assert_eq!(changed.reused, 0);
    assert!(changed.refusals.is_empty(), "{:?}", changed.refusals);
    assert_eq!(
        published(&sqlite.destination().join("live.db")).len() as u64,
        k
    );
    writer.finish();
}

/// P81 (review): a commit that lands after the backup's last step and
/// before its post-stat moves the `-wal` the key would carry, but not the
/// snapshot: the capture is unsettled, so no reuse row vouches for it and
/// the next run reads the store again.
#[test]
fn p81_a_commit_after_the_last_step_leaves_the_capture_unsettled() {
    let _serial = serial();
    if really_root("p81 commit after the last step") {
        return;
    }
    let sqlite = Sqlite::new();
    let database = sqlite.source().join("live.db");
    build(&database, Journal::Wal, 4096, 4);
    let writer = Arc::new(Mutex::new(Writer::start(&database)));
    writer.lock().unwrap().commit();
    let hook: crate::provider_sqlite::StepHook = {
        let writer = Arc::clone(&writer);
        Arc::new(move |_, done| {
            if done {
                writer.lock().unwrap().commit();
            }
        })
    };
    crate::provider_sqlite::set_carry_steps(&sqlite.root, Some((128, Some(hook))));
    let before = Counters::snapshot();
    let first = sqlite.snapshot();
    crate::provider_sqlite::set_carry_steps(&sqlite.root, None);
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::SourceSqliteUnsettled),
        1
    );
    let before = Counters::snapshot();
    let again = sqlite.snapshot();
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::SourceSqliteSnapshots),
        1,
        "an unsettled capture is never reused"
    );
    assert_eq!(again.reused, 0);
    let writer = Arc::into_inner(writer).unwrap().into_inner().unwrap();
    writer.finish();
}

/// `wal_settled` and `stamps_racy` over every pair (review).
#[test]
fn wal_settled_judges_every_pair() {
    let (started, now) = (100 * RACY_GRANULARITY_NS, 101 * RACY_GRANULARITY_NS);
    let old = started - 2 * RACY_GRANULARITY_NS;
    let id = |size, stamp| SidecarId {
        dev: 1,
        ino: 2,
        size,
        mtime_ns: stamp,
        ctime_ns: stamp,
    };
    let present = |size, stamp| WalStat::Present(id(size, stamp));
    assert!(!stamps_racy(old, old, started, now));
    assert!(stamps_racy(started - 1, old, started, now));
    assert!(stamps_racy(
        old,
        started - RACY_GRANULARITY_NS,
        started,
        now
    ));
    assert!(stamps_racy(old, now + 1, started, now));
    let cases = [
        (WalStat::Absent, WalStat::Absent, true),
        (WalStat::Absent, present(0, started), true),
        (WalStat::Absent, present(32, old), false),
        (present(4096, old), present(4096, old), true),
        (present(4096, started), present(4096, started), false),
        (present(0, started), present(0, started), true),
        (present(4096, old), present(8192, old), false),
        (present(4096, old), WalStat::Absent, false),
        (present(4096, old), WalStat::Unknown, false),
        (WalStat::Unknown, WalStat::Unknown, false),
        (WalStat::Absent, WalStat::Unknown, false),
    ];
    for (before, after, settled) in cases {
        assert_eq!(
            wal_settled(before, after, started, now),
            settled,
            "{before:?} -> {after:?}"
        );
    }
}

/// #218 review: every snapshot slot, streamed or retained, is reserved
/// against the slot budget before its backup, so the slots alive at once
/// never pass it, whatever the capture threads do; a store that waits for
/// room is carried once a slot is freed.
#[test]
fn snapshot_slots_alive_at_once_stay_within_the_slot_budget() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let names = ["a.db", "b.db", "c.db", "d.db"];
    for name in names {
        build_large(&sqlite.source().join(name), Journal::Delete, 3, 90);
    }
    let size = std::fs::metadata(sqlite.source().join("a.db"))
        .unwrap()
        .len();
    let budget = size * 3 / 2;
    SLOT_OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((sqlite.root.clone(), budget));
    let slots = sqlite.corpus.base.join("source-state").join(SLOT_DIR);
    let peak = Arc::new(AtomicU64::new(0));
    let hook: CaptureHook = {
        let peak = Arc::clone(&peak);
        Arc::new(move || {
            // Hold this slot a while, so another capture thread's slot, if
            // it is allowed one, is alive beside it.
            let started = Instant::now();
            loop {
                let alive: u64 = std::fs::read_dir(&slots).map_or(0, |entries| {
                    entries
                        .filter_map(|entry| entry.ok()?.metadata().ok())
                        .map(|meta| meta.len())
                        .sum()
                });
                peak.fetch_max(alive, Ordering::Relaxed);
                if started.elapsed() > Duration::from_millis(1500) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    set_after_snapshot(&sqlite.root, Some(hook));
    let stats = sqlite.snapshot();
    set_after_snapshot(&sqlite.root, None);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(stats.sqlite_snapshots.len(), 4);
    let peak = peak.load(Ordering::Relaxed);
    assert!(peak > 0, "the hook saw a slot");
    assert!(
        peak <= budget,
        "slots alive at once: {peak} bytes, budget {budget}"
    );
    assert_eq!(sqlite.slots(), 0);
}

/// #218 review (R9): a directory swapped for a symlink after the seat was
/// sniffed, before `SQLite` opens it by its path, is never followed:
/// `SQLITE_OPEN_NOFOLLOW` refuses a symlink at any component of the path
/// (`SQLITE_CANTOPEN_SYMLINK`), so the database outside the root is never
/// read and nothing is published.
#[test]
fn a_directory_swapped_for_a_symlink_before_the_backup_is_not_followed() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    let inner = sqlite.source().join("a");
    std::fs::create_dir(&inner).unwrap();
    build(&inner.join("x.db"), Journal::Delete, 1024, 3);
    let outside = sqlite.corpus.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    build(&outside.join("x.db"), Journal::Delete, 1024, 9);
    let hook: CaptureHook = {
        let moved = sqlite.source().join("a.moved");
        Arc::new(move || {
            std::fs::rename(&inner, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &inner).unwrap();
        })
    };
    set_before_backup(&sqlite.root, Some(hook));
    let stats = sqlite.snapshot();
    set_before_backup(&sqlite.root, None);
    assert_eq!(base_refusals(&stats, b"a/x.db"), ["PATH_ESCAPES_ROOT"]);
    assert!(!sqlite.destination().join("a").join("x.db").exists());
    assert_eq!(sqlite.slots(), 0);
}

// ---------------------------------------------------------------------------
// OI-1003-Q146: a changed store supersedes its own landed output
// ---------------------------------------------------------------------------

/// One committed transaction on a closed store, from this process.
fn commit_one(database: &Path) {
    let connection = Connection::open(database).unwrap();
    connection.execute_batch(COMMIT_ONE).unwrap();
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// The engine temporaries (`.bulkload-*`) left in a destination directory.
fn temporaries(dir: &Path) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.as_bytes().starts_with(b".bulkload-"))
        .collect()
}

/// OI-1003-Q146: a changed store, WAL mode (its commit only in the `-wal`,
/// the main file unchanged) and DELETE mode (the main file rewritten),
/// supersedes the output this store landed for it through the WP0(d)
/// exchange: the path then holds the new snapshot, `integrity_check` ok, in
/// journal mode DELETE, on a new inode, with no sidecar and no temporary
/// beside it; and the unchanged rerun after it reuses the store with 0
/// bytes read, 0 received and no snapshot taken.
#[test]
fn q146_a_changed_store_supersedes_its_own_output_through_the_exchange() {
    let _serial = serial();
    for journal in [Journal::Wal, Journal::Delete] {
        if journal == Journal::Wal && really_root("q146 WAL supersede") {
            continue;
        }
        let case = format!("{journal:?}");
        let sqlite = Sqlite::new();
        let database = sqlite.source().join("store.db");
        let output = sqlite.destination().join("store.db");
        build(&database, journal, 4096, 4);
        let mut writer = (journal == Journal::Wal).then(|| {
            let mut writer = Writer::start(&database);
            writer.commit();
            writer
        });
        let first = sqlite.snapshot();
        assert!(first.refusals.is_empty(), "{case}: {:?}", first.refusals);
        let old = std::fs::read(&output).unwrap();
        let old_rows = published(&output).len() as u64;
        let old_ino = std::fs::metadata(&output).unwrap().ino();
        let main = std::fs::read(&database).unwrap();
        let expected = writer.as_mut().map_or_else(
            || {
                commit_one(&database);
                old_rows + 1
            },
            |writer| {
                let k = writer.commit();
                assert_eq!(
                    std::fs::read(&database).unwrap(),
                    main,
                    "{case}: no checkpoint"
                );
                k
            },
        );
        let before = Counters::snapshot();
        let changed = sqlite.snapshot();
        let delta = Counters::snapshot().since(before);
        assert!(
            changed.refusals.is_empty(),
            "{case}: {:?}",
            changed.refusals
        );
        assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 1, "{case}");
        assert_eq!(delta.get(Counter::DestSqliteSuperseded), 1, "{case}");
        assert_eq!(delta.get(Counter::OutputsSuperseded), 1, "{case}");
        assert_eq!(delta.get(Counter::DestSqliteSidecarRefused), 0, "{case}");
        assert_eq!(published(&output).len() as u64, expected, "{case}");
        assert_ne!(std::fs::read(&output).unwrap(), old, "{case}");
        assert_ne!(
            std::fs::metadata(&output).unwrap().ino(),
            old_ino,
            "{case}: the new snapshot was exchanged in"
        );
        no_sidecars(&output);
        assert!(
            temporaries(&sqlite.destination()).is_empty(),
            "{case}: {:?}",
            temporaries(&sqlite.destination())
        );
        // Unchanged since: reused with nothing read, sent or snapshotted.
        let before = Counters::snapshot();
        let warm = sqlite.snapshot();
        let delta = Counters::snapshot().since(before);
        assert!(warm.refusals.is_empty(), "{case}: {:?}", warm.refusals);
        assert_eq!(warm.reused, 1, "{case}");
        assert_eq!(warm.source_bytes_read, 0, "{case}");
        assert_eq!(warm.bytes_received, 0, "{case}");
        assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 0, "{case}");
        if let Some(writer) = writer {
            writer.finish();
        }
    }
}

/// OI-1003-Q146: only an output this store landed, untouched since, is
/// superseded. Three changed stores: `a.db`'s output is untouched and is
/// superseded; `b.db`'s was touched (its mtime set) and `c.db`'s written by
/// an application at the destination, so neither ownership proof holds:
/// both are refused `DESTINATION_OCCUPIED` and left byte-identical, and the
/// unchanged rerun refuses them again from the record with nothing read.
#[test]
fn q146_a_touched_output_is_refused_and_kept_while_an_untouched_one_is_superseded() {
    let _serial = serial();
    let sqlite = Sqlite::new();
    for name in ["a.db", "b.db", "c.db"] {
        build(&sqlite.source().join(name), Journal::Delete, 1024, 3);
    }
    let first = sqlite.snapshot();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    for name in ["a.db", "b.db", "c.db"] {
        commit_one(&sqlite.source().join(name));
    }
    let touched = std::fs::OpenOptions::new()
        .write(true)
        .open(sqlite.destination().join("b.db"))
        .unwrap();
    touched
        .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
    drop(touched);
    {
        let app = Connection::open(sqlite.destination().join("c.db")).unwrap();
        app.execute_batch("CREATE TABLE app_owned(x); INSERT INTO app_owned VALUES (1);")
            .unwrap();
        app.close().map_err(|(_, error)| error).unwrap();
    }
    let kept: BTreeMap<_, _> = ["b.db", "c.db"]
        .into_iter()
        .map(|name| {
            let path = sqlite.destination().join(name);
            let identity = StatIdentity::from_metadata(&std::fs::metadata(&path).unwrap());
            (name, (std::fs::read(&path).unwrap(), identity))
        })
        .collect();
    let before = Counters::snapshot();
    let stats = sqlite.snapshot();
    let delta = Counters::snapshot().since(before);
    let mut refusals = stats.refusals;
    refusals.sort();
    assert_eq!(
        refusals,
        [
            (b"b.db".to_vec(), "DESTINATION_OCCUPIED".to_owned()),
            (b"c.db".to_vec(), "DESTINATION_OCCUPIED".to_owned()),
        ]
    );
    assert_eq!(delta.get(Counter::DestSqliteSuperseded), 1, "a.db only");
    assert_eq!(published(&sqlite.destination().join("a.db")).len(), 4);
    for (name, (bytes, identity)) in &kept {
        let path = sqlite.destination().join(name);
        assert_eq!(&std::fs::read(&path).unwrap(), bytes, "{name} kept");
        assert_eq!(
            &StatIdentity::from_metadata(&std::fs::metadata(&path).unwrap()),
            identity,
            "{name} untouched by the refusal"
        );
    }
    assert!(temporaries(&sqlite.destination()).is_empty());
    let before = Counters::snapshot();
    let again = sqlite.snapshot();
    let delta = Counters::snapshot().since(before);
    let mut refusals = again.refusals.clone();
    refusals.sort();
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    assert_eq!(again.reused, 1);
    assert_eq!(again.source_bytes_read, 0, "remembered: nothing read");
    assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 0);
    for (name, (bytes, _)) in &kept {
        assert_eq!(
            &std::fs::read(sqlite.destination().join(name)).unwrap(),
            bytes
        );
    }
}

/// OI-1003-Q146: the destination sidecar check runs at Decide and again
/// immediately before the exchange. For a `-wal` and a `-journal`:
///
/// - one beside the old output at Decide refuses the changed store
///   `DESTINATION_OCCUPIED` with no snapshot taken (not remembered);
/// - one appearing after Decide, between the exchange's last identity look
///   and the exchange itself (the committer's test hook), refuses it too:
///   nothing is exchanged, the old output stays byte- and
///   identity-identical with its rows, and no temporary is left;
///
/// and once the sidecar is removed the rerun supersedes it (the old
/// output's rows came back, so it is still this store's own).
#[test]
fn q146_a_sidecar_at_decide_or_before_the_exchange_refuses_and_keeps_the_old_output() {
    let _serial = serial();
    for suffix in ["-wal", "-journal"] {
        for late in [false, true] {
            let case = format!("{suffix} late={late}");
            let sqlite = Sqlite::new();
            let database = sqlite.source().join("store.db");
            let output = sqlite.destination().join("store.db");
            build(&database, Journal::Delete, 1024, 3);
            let first = sqlite.snapshot();
            assert!(first.refusals.is_empty(), "{case}: {:?}", first.refusals);
            commit_one(&database);
            let old = std::fs::read(&output).unwrap();
            let identity = StatIdentity::from_metadata(&std::fs::metadata(&output).unwrap());
            let sidecar = sqlite.destination().join(format!("store.db{suffix}"));
            let hook = if late {
                let sidecar = sidecar.clone();
                Some(
                    crate::materialize::set_before_exchange(
                        &sqlite.destination(),
                        b"store.db",
                        move || std::fs::write(&sidecar, b"a third party's sidecar").unwrap(),
                    )
                    .unwrap(),
                )
            } else {
                std::fs::write(&sidecar, b"a third party's sidecar").unwrap();
                None
            };
            let before = Counters::snapshot();
            let stats = sqlite.snapshot();
            let delta = Counters::snapshot().since(before);
            drop(hook);
            assert_eq!(
                stats.refusals,
                [(b"store.db".to_vec(), "DESTINATION_OCCUPIED".to_owned())],
                "{case}"
            );
            assert_eq!(std::fs::read(&output).unwrap(), old, "{case}: kept");
            assert_eq!(
                StatIdentity::from_metadata(&std::fs::metadata(&output).unwrap()),
                identity,
                "{case}: the old output, untouched"
            );
            assert_eq!(delta.get(Counter::DestSqliteSuperseded), 0, "{case}");
            if late {
                assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 1, "{case}");
                assert_eq!(delta.get(Counter::DestSqliteSidecarRefused), 1, "{case}");
            } else {
                assert_eq!(
                    delta.get(Counter::SourceSqliteSnapshots),
                    0,
                    "{case}: refused at Decide, nothing read"
                );
                assert_eq!(stats.source_bytes_read, 0, "{case}");
            }
            assert!(
                temporaries(&sqlite.destination()).is_empty(),
                "{case}: {:?}",
                temporaries(&sqlite.destination())
            );
            assert!(
                sidecar.exists(),
                "{case}: the sidecar is not ours to remove"
            );
            // Removed, the next run converges by the exchange.
            std::fs::remove_file(&sidecar).unwrap();
            let before = Counters::snapshot();
            let converged = sqlite.snapshot();
            let delta = Counters::snapshot().since(before);
            assert!(
                converged.refusals.is_empty(),
                "{case}: {:?}",
                converged.refusals
            );
            assert_eq!(delta.get(Counter::DestSqliteSuperseded), 1, "{case}");
            assert_eq!(published(&output).len(), 4, "{case}");
            no_sidecars(&output);
        }
    }
}

/// #218: the fresh publish (a rename without replacement) makes the same
/// last look as the exchange. The destination path is free at Decide; a
/// `-wal` or `-journal` a third party drops there after the snapshot is
/// verified and before the rename (the committer's test hook) refuses the
/// store `DESTINATION_OCCUPIED`: nothing lands at the path (where a hot
/// journal could be rolled into the new snapshot), no temporary is left, and
/// once the sidecar is removed the rerun publishes it.
#[test]
fn a_sidecar_before_a_fresh_rename_refuses_and_publishes_nothing() {
    let _serial = serial();
    for suffix in ["-wal", "-journal"] {
        let sqlite = Sqlite::new();
        let database = sqlite.source().join("store.db");
        let output = sqlite.destination().join("store.db");
        build(&database, Journal::Delete, 1024, 3);
        std::fs::create_dir_all(sqlite.destination()).unwrap();
        let sidecar = sqlite.destination().join(format!("store.db{suffix}"));
        let hook = {
            let sidecar = sidecar.clone();
            crate::materialize::set_before_rename(&sqlite.destination(), b"store.db", move || {
                std::fs::write(&sidecar, b"a third party's sidecar").unwrap();
            })
            .unwrap()
        };
        let before = Counters::snapshot();
        let stats = sqlite.snapshot();
        let delta = Counters::snapshot().since(before);
        drop(hook);
        assert_eq!(
            stats.refusals,
            [(b"store.db".to_vec(), "DESTINATION_OCCUPIED".to_owned())],
            "{suffix}"
        );
        assert!(
            std::fs::symlink_metadata(&output).is_err(),
            "{suffix}: nothing published beside the sidecar"
        );
        assert!(
            temporaries(&sqlite.destination()).is_empty(),
            "{suffix}: {:?}",
            temporaries(&sqlite.destination())
        );
        assert_eq!(delta.get(Counter::SourceSqliteSnapshots), 1, "{suffix}");
        assert_eq!(delta.get(Counter::DestSqliteSidecarRefused), 1, "{suffix}");
        assert!(sidecar.exists(), "{suffix}: the sidecar is not ours");
        std::fs::remove_file(&sidecar).unwrap();
        let converged = sqlite.snapshot();
        assert!(
            converged.refusals.is_empty(),
            "{suffix}: {:?}",
            converged.refusals
        );
        assert_eq!(published(&output).len(), 3, "{suffix}");
        no_sidecars(&output);
    }
}

/// OI-1003-Q148: a store that fails `integrity_check` is refused
/// `SQLITE_INTEGRITY_CHECK_FAILED`, whose default S4 disposition, printed
/// beside the refusal until transfer refusals are closure-ledger rows
/// (WP3 PR 4), is `abandon`; so is a store whose corruption the backup step
/// met first (`SQLITE_BACKUP_FAILED` with primary code 11 or 26, extended
/// codes masked as R10 masks them). No other refusal has a default, a
/// backup that failed for another reason (busy, I/O, no code) included.
#[test]
fn q148_an_integrity_failure_defaults_to_abandon() {
    for refusal in [
        BulkloadRefusal::SqliteIntegrityCheckFailed,
        BulkloadRefusal::SqliteBackupFailed(Some(11)),
        BulkloadRefusal::SqliteBackupFailed(Some(26)),
        BulkloadRefusal::SqliteBackupFailed(Some(267)),
        BulkloadRefusal::SqliteBackupFailed(Some(779)),
    ] {
        assert_eq!(
            default_disposition(refusal.code(), refusal.sqlite_code()),
            Some("abandon"),
            "{refusal}"
        );
    }
    for refusal in [
        BulkloadRefusal::DestinationOccupied,
        BulkloadRefusal::SqliteStateChanged,
        BulkloadRefusal::BudgetExceeded,
        BulkloadRefusal::SqliteBackupFailed(Some(5)),
        BulkloadRefusal::SqliteBackupFailed(Some(10)),
        BulkloadRefusal::SqliteBackupFailed(Some(1032)),
        BulkloadRefusal::SqliteBackupFailed(None),
    ] {
        assert_eq!(
            default_disposition(refusal.code(), refusal.sqlite_code()),
            None,
            "{refusal}"
        );
    }
    // A `SQLite` code is read only beside `SQLITE_BACKUP_FAILED`.
    assert_eq!(default_disposition("DESTINATION_OCCUPIED", Some(11)), None);
}

/// OI-1003-Q148 across the wire: the backup's `SQLite` code crosses in the
/// refusal frame, so the receiver gives backup-detected corruption the
/// `abandon` default; a peer that attaches a `SQLite` code to any other
/// refusal violates the protocol.
#[test]
fn q148_the_backup_code_crosses_the_wire_and_only_with_its_refusal() {
    let _serial = serial();
    for (code, sqlite_code, accepted, disposition) in [
        ("SQLITE_BACKUP_FAILED", Some(11), true, Some("abandon")),
        ("SQLITE_BACKUP_FAILED", Some(5), true, None),
        ("SQLITE_INTEGRITY_CHECK_FAILED", Some(11), false, None),
    ] {
        let sqlite = Sqlite::new();
        let (mut peer, ours) = std::os::unix::net::UnixStream::pair().unwrap();
        let source = std::thread::spawn(move || {
            let _ = read_frame(&mut peer).unwrap();
            for control in [
                Control::Start {
                    authority: b"peer".to_vec(),
                },
                Control::Refused {
                    entry: None,
                    rel_path: b"store.db".to_vec(),
                    code: code.to_owned(),
                    sqlite_code,
                },
                Control::WalkDone { entries: 0 },
                Control::SourceDone {
                    entries: 0,
                    source_bytes_read: 0,
                },
            ] {
                let _ = write_control(&mut peer, &control);
            }
            peer
        });
        let mut input = ours.try_clone().unwrap();
        let mut output = ours;
        let received = receive_with(
            &mut input,
            &mut output,
            Path::new("/peer/source"),
            Path::new("/peer/state"),
            &sqlite.destination(),
            &sqlite.corpus.base.join("destination-state"),
            SqliteMode::Snapshot,
        );
        drop(source.join().unwrap());
        if accepted {
            let stats = received.unwrap();
            assert_eq!(stats.refusals, [(b"store.db".to_vec(), code.to_owned())]);
            assert_eq!(
                stats.default_disposition(b"store.db", code),
                disposition,
                "{code} {sqlite_code:?}"
            );
        } else {
            assert_eq!(
                received.unwrap_err(),
                BulkloadRefusal::ProtocolStateViolation,
                "{code} {sqlite_code:?}"
            );
        }
    }
}
