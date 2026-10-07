//! P75 SQLITE-SHM-EXCEPTION: a provider snapshot's only source writes are its
//! counted wal-index and its counted empty `-wal` (S2; OI-1003-Q36 and
//! OI-1003-Q72, extending OI-1003-Q16; P34 family).
//!
//! OI-1003-Q36: a backup-API read of a WAL-mode source database may create or
//! touch its `<db>-shm` wal-index. The effect is counted
//! (`source_wal_index_touched`) and recorded in S2 evidence.
//!
//! OI-1003-Q72 extends that to the empty `-wal`: a read-only, WAL-aware
//! snapshot of a WAL-mode source that has no `-wal` may create an empty one
//! beside it. That is allowed only where no `-wal` existed, the file is zero
//! bytes, and it is counted (`source_wal_created`). The main database file
//! stays byte-identical, a `-wal` that existed stays byte-identical, and no
//! other source write occurs. Every source is read WAL-aware under the
//! backup API's shared lock; none is read `immutable=1`.
//!
//! "Touch" is read widely. A WAL-aware connection opens the wal-index
//! read-write, maps it shared and takes `fcntl` locks on it even when it
//! leaves every byte as it was, so `source_wal_index_touched` is 1 for every
//! snapshot of a WAL-mode source, changed or not.
//!
//! Over generated WAL-mode source databases (page size, committed rows,
//! frames still in the `-wal`), in five shapes that cover a wal-index present
//! or absent, a `-wal` present or absent and a live writer present or absent:
//!
//! | shape | live writer | `-shm` before | `-wal` before |
//! |---|---|---|---|
//! | `LiveWithShm` | yes, it has read and written | yes | yes, with any frames |
//! | `LiveIdleNoShm` | yes, opened but not yet read | no | no |
//! | `CrashedWithShm` | no: a writer's files as it left them | yes | yes, with any frames |
//! | `CrashedNoShm` | no, and the wal-index is gone | no | yes, with any frames |
//! | `CleanClosed` | no: checkpointed and closed | no | no |
//!
//! [`check`] snapshots every case twice in a row ([`ROUNDS`]) and asserts on
//! each round that `provider_sqlite::snapshot`:
//! - succeeds and captures exactly the committed rows;
//! - leaves every entry of the source directory other than `<db>-shm` and a
//!   newly created `<db>-wal` as it was: the main file, and a `-wal` that
//!   existed, byte-identical with the same identity, size, mode and
//!   timestamps (mtime and ctime), and no other entry created or removed;
//! - where the source had no `-wal` (`LiveIdleNoShm`, `CleanClosed`, first
//!   snapshot), leaves a `-wal` that is a regular file of exactly 0 bytes;
//!   the second snapshot finds that file and leaves it as it is;
//! - adds to both counters the values the shape and the round fix, an oracle
//!   that does not share the implementation's observation:
//!   `source_wal_index_touched` 1 on every snapshot of every shape;
//!   `source_wal_created` 1 on the first snapshot of the two shapes with no
//!   `-wal` and 0 everywhere else, so each created `-wal` is counted once;
//! - leaves a `-shm` beside the source on every round.
//!
//! After the last round it takes no lock that outlives its bounded shared
//! read: a writer commits with a zero busy timeout (the live writer itself
//! where there is one).
//!
//! The live writer is a child process (this test binary re-run as
//! [`p75_writer_child`]) so that its POSIX locks are its own: this test's
//! plain reads of the source's files would otherwise drop the locks of an
//! in-process connection. The child holds its connection until its stdin
//! closes, then ends on its own.
//!
//! **As root (OI-1003-Q76).** Opened as root, `SQLite` re-applies the
//! database's ownership to its `-wal` (`fchown`), which moves an existing
//! `-wal`'s ctime while its size, mtime and bytes stay: a source metadata
//! write no ruling admits, and the property above fails on it (CI runs as
//! root). So the provider refuses to read a source as root, and with an
//! effective uid of 0 each test here has two legs:
//!
//! - **the refusal** ([`refused_as_root`], [`cli_refused_as_root`]): over the
//!   PINNED rows, `snapshot` returns `SQLITE_SOURCE_AS_ROOT`, writes no
//!   output, adds 0 to both counters, and leaves the source directory as it
//!   was: every entry byte- and metadata-identical (ctime included), with no
//!   `-shm` and no `-wal` created;
//! - **the property as an ordinary user** ([`unprivileged_leg`]): the same
//!   test is re-run in a child process that drops to uid and gid
//!   [`UNPRIVILEGED`] (`nobody`), so a root run keeps the whole of P75. The
//!   child runs copies of this test binary and of the agent from a scratch
//!   directory under `TMPDIR` that it owns, so the build tree need not be
//!   readable by `nobody`. When the drop cannot be made (a user namespace
//!   that maps only uid 0, a sandbox without the capability, a `TMPDIR`
//!   `nobody` cannot reach), [`p75_unprivileged_probe`] fails first, the leg
//!   is skipped, and a `p75 root:` line on stderr says so and why: that run
//!   then proves the refusal only.
//!
//! **Corpus.** CI runs two PINNED rows per shape and 12 seeded cases through
//! the shared `test_support::prop_config` (OI-1003-Q7, OI-1003-Q78; compiled
//! in as `tests/git_estimate_dag.rs` does), which also owns the deep tier's
//! case count (`BULKLOAD_PROPTEST_DEEP=1`: 240 cases). The counters
//! are process scope, so every snapshot in this binary runs inside the one
//! test that reads them; [`the_counters_line_reports_both_exceptions`] runs
//! the `snapshot` verb as its own process and reads its `counters` line. Each
//! round prints a `p75 shape=<shape> round=<n> ...
//! source_wal_index_touched=<n> shm=<created|changed|same|absent>
//! source_wal_created=<n> wal=<created|same|absent>` line (`--nocapture`);
//! `shm=same` is a counted read that left the bytes alone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::provider_sqlite::snapshot;
use bulkload_agent::BulkloadRefusal;
use proptest::prelude::*;
use proptest::test_runner::TestRunner;
use rusqlite::{Connection, OpenFlags};

// ---------------------------------------------------------------------------
// Corpus configuration
// ---------------------------------------------------------------------------

/// The shared property-test configuration, compiled in from the library's
/// `test_support`, so the seed and the deep switch cannot drift.
#[path = "../src/test_support.rs"]
mod test_support;

// ---------------------------------------------------------------------------
// The live writer: this binary, re-run as one test
// ---------------------------------------------------------------------------

/// Names the database the writer child opens; unset, the child test is a
/// no-op.
const WRITER: &str = "BULKLOAD_P75_WRITER";

/// Marks the writer child's protocol lines, apart from the harness's own.
const TAG: &str = "p75> ";

/// The writer child. Opens the database named by `BULKLOAD_P75_WRITER` with
/// a zero busy timeout, prints `ready`, then runs each stdin line as one SQL
/// batch and answers `ok` or `err <message>`. At the end of stdin it closes
/// its connection, as a provider process does when it exits cleanly.
#[test]
fn p75_writer_child() {
    let Some(path) = std::env::var_os(WRITER) else {
        return;
    };
    let connection = Connection::open(PathBuf::from(path)).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    println!("{TAG}ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        match connection.execute_batch(&line) {
            Ok(()) => println!("{TAG}ok"),
            Err(error) => println!("{TAG}err {error}"),
        }
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// A writer child and its pipes.
struct Writer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Writer {
    fn start(database: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "p75_writer_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WRITER, database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut writer = Self {
            child,
            stdin,
            stdout,
        };
        assert_eq!(writer.reply(), "ready");
        writer
    }

    /// The child's next protocol line.
    fn reply(&mut self) -> String {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert_ne!(read, 0, "the writer child ended early");
            // The harness prints `test p75_writer_child ... ` with no line
            // end before the child's first line, so the tag may sit mid-line.
            if let Some((_, reply)) = line.trim_end().split_once(TAG) {
                return reply.to_owned();
            }
        }
    }

    /// Run one SQL batch (one line) in the child; its answer.
    fn run(&mut self, sql: &str) -> String {
        assert!(!sql.contains('\n'));
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{sql}").unwrap();
        stdin.flush().unwrap();
        self.reply()
    }

    /// Run one SQL batch that must succeed.
    fn must(&mut self, sql: &str) {
        let reply = self.run(sql);
        assert_eq!(reply, "ok", "{sql}");
    }

    /// Close the child's stdin and wait for it to close its connection and
    /// exit on its own.
    fn finish(mut self) {
        self.close();
    }

    fn close(&mut self) {
        if self.stdin.take().is_some() {
            let status = self.child.wait().unwrap();
            assert!(status.success(), "writer child exited {status}");
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // On an assertion failure: closing stdin is the child's cue to end.
        if self.stdin.take().is_some() {
            let _ = self.child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Generated sources
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    LiveWithShm,
    LiveIdleNoShm,
    CrashedWithShm,
    CrashedNoShm,
    CleanClosed,
}

const SHAPES: [Shape; 5] = [
    Shape::LiveWithShm,
    Shape::LiveIdleNoShm,
    Shape::CrashedWithShm,
    Shape::CrashedNoShm,
    Shape::CleanClosed,
];

impl Shape {
    const fn live(self) -> bool {
        matches!(self, Self::LiveWithShm | Self::LiveIdleNoShm)
    }

    const fn shm(self) -> bool {
        matches!(self, Self::LiveWithShm | Self::CrashedWithShm)
    }

    const fn wal(self) -> bool {
        !matches!(self, Self::LiveIdleNoShm | Self::CleanClosed)
    }
}

#[derive(Clone, Debug)]
struct Case {
    shape: Shape,
    page_size: u32,
    /// Rows checkpointed into the main file.
    base: Vec<Vec<u8>>,
    /// Rows committed after the checkpoint, one transaction each: frames in
    /// the `-wal` (checkpointed by the close in the quiescent shapes).
    pending: Vec<Vec<u8>>,
}

fn case() -> impl Strategy<Value = Case> {
    let payload = || prop::collection::vec(any::<u8>(), 0..1500);
    (
        prop::sample::select(SHAPES.to_vec()),
        prop::sample::select(vec![1024_u32, 4096]),
        prop::collection::vec(payload(), 1..24),
        prop::collection::vec(payload(), 0..12),
    )
        .prop_map(|(shape, page_size, base, pending)| Case {
            shape,
            page_size,
            base,
            pending,
        })
}

/// The PINNED rows: every shape, with and without frames in the `-wal`.
fn pinned() -> Vec<Case> {
    let rows = |count: usize, width: usize| -> Vec<Vec<u8>> {
        (0..count)
            .map(|row| {
                (0..width)
                    .map(|byte| u8::try_from((row * 31 + byte) % 251).unwrap())
                    .collect()
            })
            .collect()
    };
    let mut cases = Vec::new();
    for shape in SHAPES {
        for pending in [0, 5] {
            cases.push(Case {
                shape,
                page_size: 4096,
                base: rows(12, 700),
                pending: rows(pending, 1300),
            });
        }
    }
    cases
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2 + 3);
    text.push_str("x'");
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text.push('\'');
    text
}

fn insert(rows: &[Vec<u8>]) -> String {
    let mut sql = String::from("BEGIN;");
    for row in rows {
        let _ = write!(sql, " INSERT INTO t(payload) VALUES ({});", hex(row));
    }
    sql.push_str(" COMMIT;");
    sql
}

fn private_dir(path: &Path) -> PathBuf {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_path_buf()
}

fn sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A built source: the database, its directory, and the live writer if the
/// shape has one.
struct Source {
    dir: PathBuf,
    database: PathBuf,
    live: Option<Writer>,
}

/// Build `case` under `root`.
fn build(root: &Path, case: &Case) -> Source {
    let built = private_dir(&root.join("built"));
    let database = built.join("state.sqlite");
    let mut writer = Writer::start(&database);
    writer.must(&format!(
        "PRAGMA page_size={}; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
         CREATE TABLE t(id INTEGER PRIMARY KEY, payload BLOB NOT NULL);",
        case.page_size
    ));
    writer.must(&insert(&case.base));
    writer.must("PRAGMA wal_checkpoint(TRUNCATE);");
    for row in &case.pending {
        writer.must(&insert(std::slice::from_ref(row)));
    }
    let source = match case.shape {
        Shape::LiveWithShm => Source {
            dir: built,
            database,
            live: Some(writer),
        },
        Shape::LiveIdleNoShm => {
            writer.finish();
            let idle = Writer::start(&database);
            Source {
                dir: built,
                database,
                live: Some(idle),
            }
        }
        Shape::CrashedWithShm | Shape::CrashedNoShm => {
            // The writer's files as they are while it holds them: what a
            // crash leaves. Copied by this process, which holds no lock.
            let crashed = private_dir(&root.join("crashed"));
            let copy = crashed.join("state.sqlite");
            let mut suffixes = vec!["", "-wal"];
            if case.shape.shm() {
                suffixes.push("-shm");
            }
            for suffix in suffixes {
                fs::copy(sidecar(&database, suffix), sidecar(&copy, suffix)).unwrap();
            }
            writer.finish();
            Source {
                dir: crashed,
                database: copy,
                live: None,
            }
        }
        Shape::CleanClosed => {
            writer.finish();
            Source {
                dir: built,
                database,
                live: None,
            }
        }
    };
    // The fixture is the shape it claims to be.
    let present = |suffix| sidecar(&source.database, suffix).exists();
    assert_eq!(present("-shm"), case.shape.shm(), "{:?} -shm", case.shape);
    assert_eq!(present("-wal"), case.shape.wal(), "{:?} -wal", case.shape);
    assert_eq!(source.live.is_some(), case.shape.live());
    source
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

/// One directory entry as a plain read sees it.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    bytes: Vec<u8>,
}

fn scan(dir: &Path) -> BTreeMap<OsString, Entry> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        let bytes = if metadata.is_file() {
            fs::read(entry.path()).unwrap()
        } else {
            Vec::new()
        };
        entries.insert(
            entry.file_name(),
            Entry {
                dev: metadata.dev(),
                ino: metadata.ino(),
                mode: metadata.mode(),
                size: metadata.size(),
                mtime: (metadata.mtime(), metadata.mtime_nsec()),
                ctime: (metadata.ctime(), metadata.ctime_nsec()),
                bytes,
            },
        );
    }
    entries
}

/// The payloads of a snapshot, in row order.
fn captured(output: &Path) -> Vec<Vec<u8>> {
    let connection = Connection::open_with_flags(output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete", "a snapshot is one portable file");
    let mut statement = connection
        .prepare("SELECT payload FROM t ORDER BY id")
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(statement);
    rows
}

/// What a snapshot did to the source's `-wal`, asserting the two things it
/// may do (OI-1003-Q72): leave an existing one byte-identical, or create a
/// regular file of zero bytes where none was.
fn wal_outcome(what: &str, before: Option<&Entry>, after: Option<&Entry>) -> &'static str {
    match (before, after) {
        (None, Some(new)) => {
            assert!(
                new.mode & 0o170_000 == 0o100_000 && new.size == 0 && new.bytes.is_empty(),
                "{what}: the created `-wal` is not an empty regular file: mode {:o}, {} bytes",
                new.mode,
                new.size
            );
            "created"
        }
        (Some(old), Some(new)) => {
            assert!(
                old == new,
                "{what}: the `-wal` changed: {:?} -> {:?}",
                (old.size, old.mtime, old.ctime),
                (new.size, new.mtime, new.ctime)
            );
            "same"
        }
        (Some(_), None) => panic!("{what}: the `-wal` was removed"),
        (None, None) => "absent",
    }
}

/// What a snapshot did to the source's `-shm`. Any of these is allowed
/// (OI-1003-Q36); the name is for the trace line and the counter's oracle.
fn shm_outcome(before: Option<&Entry>, after: Option<&Entry>) -> &'static str {
    match (before, after) {
        (None, Some(_)) => "created",
        (Some(old), Some(new)) if old != new => "changed",
        (Some(_), Some(_)) => "same",
        (_, None) => "absent",
    }
}

/// Consecutive snapshots of one source. The second pins the counters for a
/// read that finds the wal-index, and the `-wal`, already there.
const ROUNDS: usize = 2;

/// Snapshot one generated source [`ROUNDS`] times and assert P75 on each.
fn check(case: &Case) {
    let root = tempfile::Builder::new()
        .prefix("bulkload-p75-")
        .tempdir()
        .unwrap();
    let source = build(root.path(), case);
    let out = private_dir(&root.path().join("out"));
    let file_name = Path::new(source.database.file_name().unwrap());
    let shm_name = sidecar(file_name, "-shm");
    let wal_name = sidecar(file_name, "-wal");
    let shape = case.shape;
    let expected: Vec<Vec<u8>> = case.base.iter().chain(&case.pending).cloned().collect();

    let mut before = scan(&source.dir);
    for round in 0..ROUNDS {
        // Fixed by the shape and the round, not observed. Every source is
        // WAL-mode and read WAL-aware, which opens, maps and locks the
        // wal-index every time. Only the first read of a source with no
        // `-wal` creates one.
        let index_reads = 1_u64;
        let wal_creations = u64::from(round == 0 && !shape.wal());

        let output = out.join(format!("snapshot-{round}.sqlite"));
        let counted = Counters::snapshot();
        let outcome = snapshot(&source.database, &output, 10_000);
        let delta = Counters::snapshot().since(counted);
        let touched = delta.get(Counter::SourceWalIndexTouched);
        let created = delta.get(Counter::SourceWalCreated);
        let after = scan(&source.dir);

        assert!(outcome.is_ok(), "{shape:?} round {round}: {outcome:?}");
        assert!(
            captured(&output) == expected,
            "{shape:?} round {round}: captured rows"
        );

        let wal = wal_outcome(
            &format!("{shape:?} round {round}"),
            before.get(wal_name.as_os_str()),
            after.get(wal_name.as_os_str()),
        );

        // No other source write: the main file and every other entry keep
        // their bytes, identity, size, mode and timestamps.
        for name in before.keys().chain(after.keys()) {
            if name.as_os_str() == shm_name.as_os_str() || name.as_os_str() == wal_name.as_os_str()
            {
                continue;
            }
            assert!(
                before.get(name) == after.get(name),
                "{shape:?} round {round}: {} changed: {:?} -> {:?}",
                name.display(),
                before
                    .get(name)
                    .map(|entry| (entry.size, entry.mtime, entry.ctime)),
                after
                    .get(name)
                    .map(|entry| (entry.size, entry.mtime, entry.ctime)),
            );
        }

        let shm = shm_outcome(
            before.get(shm_name.as_os_str()),
            after.get(shm_name.as_os_str()),
        );
        println!(
            "p75 shape={shape:?} round={round} page_size={} base={} pending={} \
             source_wal_index_touched={touched} shm={shm} \
             source_wal_created={created} wal={wal}",
            case.page_size,
            case.base.len(),
            case.pending.len()
        );
        assert_eq!(
            touched, index_reads,
            "{shape:?} round {round}: source_wal_index_touched (shm {shm})"
        );
        assert_ne!(
            shm, "absent",
            "{shape:?} round {round}: a WAL-aware read leaves a `-shm` beside its source"
        );
        if round == 0 && !shape.shm() {
            assert_eq!(shm, "created", "{shape:?} creates the wal-index");
        }
        assert_eq!(
            created, wal_creations,
            "{shape:?} round {round}: source_wal_created (wal {wal})"
        );
        assert_eq!(
            wal == "created",
            wal_creations == 1,
            "{shape:?} round {round}: a `-wal` is created exactly where the source had none"
        );
        assert_ne!(
            wal, "absent",
            "{shape:?} round {round}: a WAL-aware read leaves a `-wal` beside its source"
        );
        before = after;
    }

    // No lock outlives the read: a writer commits at once.
    let row = insert(&[b"after".to_vec()]);
    if let Some(mut live) = source.live {
        assert_eq!(live.run(&row), "ok", "{shape:?}: the live writer commits");
        live.finish();
    } else {
        let mut writer = Writer::start(&source.database);
        assert_eq!(writer.run(&row), "ok", "{shape:?}: a new writer commits");
        writer.finish();
    }
}

// ---------------------------------------------------------------------------
// As root (OI-1003-Q76)
// ---------------------------------------------------------------------------

/// The uid and gid the unprivileged leg drops to: `nobody` on Linux.
const UNPRIVILEGED: u32 = 65_534;

/// Set for [`p75_unprivileged_probe`]; unset, that test is a no-op.
const PROBE: &str = "BULKLOAD_P75_PROBE";

/// Names the agent binary for the unprivileged leg's child: a copy it can
/// reach. Unset, the agent is the one cargo built for this test.
const AGENT: &str = "BULKLOAD_P75_AGENT";

fn agent() -> PathBuf {
    std::env::var_os(AGENT).map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_bulkload-agent")),
        PathBuf::from,
    )
}

/// The uid this process creates files as: its effective uid.
fn effective_uid() -> u32 {
    let made = tempfile::Builder::new()
        .prefix("bulkload-p75-uid-")
        .tempdir()
        .unwrap();
    fs::metadata(made.path()).unwrap().uid()
}

/// A line on the real stderr, which the harness does not capture: a CI log
/// shows which legs a root run proved.
fn note(line: &str) {
    let _ = writeln!(std::io::stderr(), "p75 root: {line}");
}

/// What changed between two scans, for an assertion message.
fn differences(before: &BTreeMap<OsString, Entry>, after: &BTreeMap<OsString, Entry>) -> String {
    let mut text = String::new();
    let brief = |entry: Option<&Entry>| {
        entry.map(|entry| (entry.ino, entry.mode, entry.size, entry.mtime, entry.ctime))
    };
    let names: std::collections::BTreeSet<&OsString> = before.keys().chain(after.keys()).collect();
    for name in names {
        if before.get(name) != after.get(name) {
            let _ = write!(
                text,
                "\n  {}: {:?} -> {:?}",
                name.display(),
                brief(before.get(name)),
                brief(after.get(name))
            );
        }
    }
    text
}

/// The refusal leg: as root, a snapshot of every PINNED source is refused
/// with the typed refusal before the source is opened. Nothing in the source
/// directory is created, removed or changed (no `-shm`, no `-wal`, no ctime
/// moved), no output is written and neither counter moves.
fn refused_as_root() {
    for case in pinned() {
        let root = tempfile::Builder::new()
            .prefix("bulkload-p75-root-")
            .tempdir()
            .unwrap();
        let source = build(root.path(), &case);
        let out = private_dir(&root.path().join("out"));
        let output = out.join("snapshot.sqlite");
        let shape = case.shape;

        let before = scan(&source.dir);
        let counted = Counters::snapshot();
        let outcome = snapshot(&source.database, &output, 10_000);
        let delta = Counters::snapshot().since(counted);
        let after = scan(&source.dir);

        // The directory first: were the uid check gone, this names the
        // write root makes (the `-wal`'s ctime, or a sidecar it created).
        assert!(
            before == after,
            "{shape:?}: a snapshot as root changed its source directory:{}",
            differences(&before, &after)
        );
        assert_eq!(
            outcome,
            Err(BulkloadRefusal::SqliteSourceAsRoot),
            "{shape:?}: a snapshot as root is refused (OI-1003-Q76)"
        );
        assert!(!output.exists(), "{shape:?}: a refused snapshot wrote");
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0, "{shape:?}: out");
        assert_eq!(delta.get(Counter::SourceWalIndexTouched), 0, "{shape:?}");
        assert_eq!(delta.get(Counter::SourceWalCreated), 0, "{shape:?}");
        println!(
            "p75 shape={shape:?} pending={} euid=0 refused=SQLITE_SOURCE_AS_ROOT source=unchanged",
            case.pending.len()
        );
        if let Some(live) = source.live {
            live.finish();
        }
    }
}

/// The CLI's refusal leg: as root the `snapshot` verb fails with the typed
/// refusal on stderr, writes no output and leaves its source directory as
/// it was.
fn cli_refused_as_root() {
    for shape in [Shape::CrashedNoShm, Shape::CleanClosed] {
        let root = tempfile::Builder::new()
            .prefix("bulkload-p75-cli-root-")
            .tempdir()
            .unwrap();
        let case = Case {
            shape,
            page_size: 4096,
            base: vec![vec![7; 300]; 3],
            pending: vec![vec![9; 300]; 2],
        };
        let source = build(root.path(), &case);
        let out = private_dir(&root.path().join("out"));
        let before = scan(&source.dir);
        let output = Command::new(agent())
            .arg("snapshot")
            .arg(&source.database)
            .arg(out.join("snapshot.sqlite"))
            .output()
            .unwrap();
        let after = scan(&source.dir);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            before == after,
            "{shape:?}: the snapshot verb as root changed its source directory:{}",
            differences(&before, &after)
        );
        assert!(!output.status.success(), "{shape:?}: {stderr}");
        assert!(
            stderr
                .lines()
                .any(|line| line.ends_with("refused: SQLITE_SOURCE_AS_ROOT")),
            "{shape:?}: no SQLITE_SOURCE_AS_ROOT refusal in {stderr}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("snapshot complete"),
            "{shape:?}: a refused verb reported a snapshot"
        );
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0, "{shape:?}: out");
        // A counters line, where the refused verb prints one, reports no
        // source write.
        for line in stderr.lines().filter(|line| line.starts_with("counters ")) {
            for field in ["source_wal_index_touched=0", "source_wal_created=0"] {
                assert!(
                    line.split(' ').any(|pair| pair == field),
                    "{shape:?}: {field} not in {line}"
                );
            }
        }
    }
}

/// The probe the unprivileged leg runs first, as the dropped uid: this
/// process is not root, can create files under its `TMPDIR` and can start
/// the agent binary. Without `BULKLOAD_P75_PROBE` it is a no-op.
#[test]
fn p75_unprivileged_probe() {
    if std::env::var_os(PROBE).is_none() {
        return;
    }
    assert_ne!(effective_uid(), 0, "the probe runs after the drop");
    Command::new(agent())
        .arg("help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

/// The copy of this test binary in `scratch`, run for one test as uid and
/// gid [`UNPRIVILEGED`] with `scratch` as its `TMPDIR` and the copied agent.
fn dropped(test: &str, scratch: &Path) -> Command {
    let mut command = Command::new(scratch.join("p75-test"));
    command
        .args([test, "--exact", "--test-threads=1"])
        .env("TMPDIR", scratch)
        .env(AGENT, scratch.join("bulkload-agent"))
        .env_remove(WRITER)
        .env_remove(PROBE)
        .stdin(Stdio::null())
        .uid(UNPRIVILEGED)
        .gid(UNPRIVILEGED);
    command
}

/// The unprivileged leg of a root run: re-run `test` in a child process that
/// has dropped to [`UNPRIVILEGED`], where it is the whole property again.
/// The probe decides whether the drop can be made here; if it cannot, the
/// leg is skipped and the reason is written to stderr.
fn unprivileged_leg(test: &str) {
    let scratch = tempfile::Builder::new()
        .prefix("bulkload-p75-drop-")
        .tempdir()
        .unwrap();
    let skipped = |why: String| {
        note(&format!(
            "{test}: unprivileged leg SKIPPED ({why}); this run proved the refusal only"
        ));
    };
    if let Err(error) =
        std::os::unix::fs::chown(scratch.path(), Some(UNPRIVILEGED), Some(UNPRIVILEGED))
    {
        return skipped(format!("chown of the scratch directory: {error}"));
    }
    // Copies keep the mode of the originals: readable and runnable by all.
    fs::copy(
        std::env::current_exe().unwrap(),
        scratch.path().join("p75-test"),
    )
    .unwrap();
    fs::copy(agent(), scratch.path().join("bulkload-agent")).unwrap();
    match dropped("p75_unprivileged_probe", scratch.path())
        .env(PROBE, "1")
        .output()
    {
        Err(error) => return skipped(format!("dropping to uid {UNPRIVILEGED}: {error}")),
        Ok(probe) if !probe.status.success() => {
            return skipped(format!(
                "the probe as uid {UNPRIVILEGED} exited {}",
                probe.status
            ));
        }
        Ok(_) => {}
    }
    let run = dropped(test, scratch.path()).output().unwrap();
    assert!(
        run.status.success(),
        "{test} as uid {UNPRIVILEGED} exited {}:\n{}\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1 passed") && stdout.contains("0 failed"),
        "{test} as uid {UNPRIVILEGED} did not run exactly one test:\n{stdout}"
    );
    note(&format!(
        "{test}: refusal proved as root; full property passed as uid {UNPRIVILEGED}"
    ));
}

/// P75 over the PINNED rows, then over generated sources. One test, so every
/// in-process snapshot (and the process-scope counters) is this test's own.
/// As root: the refusal, then this test again as an ordinary user.
#[test]
fn p75_a_snapshot_writes_only_its_counted_sidecars() {
    if effective_uid() == 0 {
        refused_as_root();
        unprivileged_leg("p75_a_snapshot_writes_only_its_counted_sidecars");
        return;
    }
    for case in pinned() {
        check(&case);
    }
    TestRunner::new(test_support::prop_config(12))
        .run(&case(), |case| {
            check(&case);
            Ok(())
        })
        .unwrap();
}

/// Both counters reach the `snapshot` verb's `counters` line. A source with a
/// `-wal` and no wal-index: the read creates the `-shm` only. A checkpointed,
/// closed source: the read creates the `-shm` and the empty `-wal`.
/// As root: the verb's refusal, then this test again as an ordinary user.
#[test]
fn the_counters_line_reports_both_exceptions() {
    if effective_uid() == 0 {
        cli_refused_as_root();
        unprivileged_leg("the_counters_line_reports_both_exceptions");
        return;
    }
    for (shape, index, wal) in [(Shape::CrashedNoShm, 1, 0), (Shape::CleanClosed, 1, 1)] {
        let root = tempfile::Builder::new()
            .prefix("bulkload-p75-cli-")
            .tempdir()
            .unwrap();
        let case = Case {
            shape,
            page_size: 4096,
            base: vec![vec![7; 300]; 3],
            pending: vec![vec![9; 300]; 2],
        };
        let source = build(root.path(), &case);
        let out = private_dir(&root.path().join("out"));
        let output = Command::new(agent())
            .arg("snapshot")
            .arg(&source.database)
            .arg(out.join("snapshot.sqlite"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{shape:?}: {stderr}");
        let line = stderr
            .lines()
            .find(|line| line.starts_with("counters "))
            .unwrap_or_else(|| panic!("{shape:?}: no counters line in {stderr}"));
        for field in [
            format!("source_wal_index_touched={index}"),
            format!("source_wal_created={wal}"),
        ] {
            assert!(
                line.split(' ').any(|pair| pair == field),
                "{shape:?}: {field} not in {line}"
            );
        }
        let created = sidecar(&source.database, "-wal");
        assert_eq!(
            fs::metadata(&created).unwrap().len() == 0,
            wal == 1,
            "{shape:?}: the `-wal` is empty exactly where the snapshot created it"
        );
    }
}
