//! P75 SQLITE-SHM-EXCEPTION: a provider snapshot's only source write is its
//! counted wal-index (S2; OI-1003-Q36, extending OI-1003-Q16; P34 family).
//!
//! OI-1003-Q36: a backup-API read of a WAL-mode source database may create or
//! touch its `<db>-shm` wal-index. The main database and its `-wal` stay
//! byte-identical, the effect is counted (`source_wal_index_touched`) and
//! recorded in S2 evidence, and no other source write occurs.
//!
//! "Touch" is read widely. A WAL-aware connection opens the wal-index
//! read-write, maps it shared and takes `fcntl` locks on it even when it
//! leaves every byte as it was, so the counter is 1 for every WAL-aware
//! snapshot that leaves a `-shm` beside its source, changed or not.
//!
//! Over generated WAL-mode source databases (page size, committed rows,
//! frames still in the `-wal`), in five shapes that cover a wal-index present
//! or absent and a live writer present or absent:
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
//! - leaves every entry of the source directory other than `<db>-shm` as it
//!   was: the main file and `-wal` byte-identical with the same identity,
//!   size, mode and timestamps (mtime and ctime), and no entry created or
//!   removed;
//! - adds to `source_wal_index_touched` the value the shape fixes, an oracle
//!   that does not share the implementation's observation: 1 for the three
//!   shapes with a `-wal` (`LiveWithShm`, `CrashedWithShm`, `CrashedNoShm`),
//!   whose read is WAL-aware, on the first snapshot and again on the second,
//!   whether or not the `-shm` bytes moved; 0 for the quiescent shapes
//!   (`LiveIdleNoShm`, `CleanClosed`), which leave no `-shm` at all;
//! - leaves a `-shm` beside the source exactly when it counted one.
//!
//! After the last round it takes no lock that outlives its bounded shared
//! read: a writer commits with a zero busy timeout (the live writer itself
//! where there is one).
//!
//! The live writer is a child process (this test binary re-run as
//! [`p75_writer_child`]) so that its POSIX locks are its own: the snapshot's
//! plain reads of the source's files would otherwise drop the locks of an
//! in-process connection. The child holds its connection until its stdin
//! closes, then ends on its own.
//!
//! **Corpus.** CI runs one PINNED row per shape and 12 fixed-seed cases
//! (`prop_config`, mirrored from `test_support` as `tests/git_estimate_dag.rs`
//! does); `BULKLOAD_PROPTEST_DEEP=1` runs 240 random-seed cases. The counter
//! is process scope, so every snapshot in this binary runs inside the one
//! test that reads it; [`the_counters_line_reports_the_wal_index`] runs the
//! `snapshot` verb as its own process and reads its `counters` line. Each
//! round prints a `p75 shape=<shape> round=<n> ...
//! source_wal_index_touched=<n> shm=<created|changed|same|absent>` line
//! (`--nocapture`); `same` is a counted read that left the bytes alone.

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
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::provider_sqlite::snapshot;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use rusqlite::{Connection, OpenFlags};

// ---------------------------------------------------------------------------
// Corpus configuration
// ---------------------------------------------------------------------------

/// `test_support::CI_SEED`, mirrored: every CI run draws the same cases.
const CI_SEED: u64 = 0x0B01_C0AD_2026_1003;

/// `test_support::DEEP`, mirrored: the switch for the deep local tier.
const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";

/// `test_support::prop_config`, mirrored for an integration test.
fn prop_config(cases: u32) -> Config {
    let deep = std::env::var_os(DEEP).is_some_and(|value| value == "1");
    Config {
        cases: if deep {
            cases.saturating_mul(20)
        } else {
            cases
        },
        rng_seed: if deep {
            RngSeed::Random
        } else {
            RngSeed::Fixed(CI_SEED)
        },
        failure_persistence: None,
        ..Config::default()
    }
}

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

/// Consecutive snapshots of one source. The second pins the counter for a
/// read that finds the wal-index already as it needs it.
const ROUNDS: usize = 2;

/// Snapshot one generated source [`ROUNDS`] times and assert P75 on each.
fn check(case: &Case) {
    let root = tempfile::Builder::new()
        .prefix("bulkload-p75-")
        .tempdir()
        .unwrap();
    let source = build(root.path(), case);
    let out = private_dir(&root.path().join("out"));
    let shm_name = sidecar(Path::new(source.database.file_name().unwrap()), "-shm");
    let shape = case.shape;
    let expected: Vec<Vec<u8>> = case.base.iter().chain(&case.pending).cloned().collect();
    // Fixed by the shape, not observed: a source with a `-wal` is read
    // WAL-aware, which opens, maps and locks the wal-index every time.
    let counted_reads = u64::from(shape.wal());

    let mut before = scan(&source.dir);
    for round in 0..ROUNDS {
        let output = out.join(format!("snapshot-{round}.sqlite"));
        let counted = Counters::snapshot();
        let outcome = snapshot(&source.database, &output, 10_000);
        let touched = Counters::snapshot()
            .since(counted)
            .get(Counter::SourceWalIndexTouched);
        let after = scan(&source.dir);

        assert!(outcome.is_ok(), "{shape:?} round {round}: {outcome:?}");
        assert!(
            captured(&output) == expected,
            "{shape:?} round {round}: captured rows"
        );

        // No source write but the wal-index.
        for name in before.keys().chain(after.keys()) {
            if name.as_os_str() == shm_name.as_os_str() {
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

        // The counter is every WAL-aware read that left a wal-index.
        let shm = match (
            before.get(shm_name.as_os_str()),
            after.get(shm_name.as_os_str()),
        ) {
            (None, Some(_)) => "created",
            (Some(old), Some(new)) if old != new => "changed",
            (Some(_), Some(_)) => "same",
            (_, None) => "absent",
        };
        println!(
            "p75 shape={shape:?} round={round} page_size={} base={} pending={} \
             source_wal_index_touched={touched} shm={shm}",
            case.page_size,
            case.base.len(),
            case.pending.len()
        );
        assert_eq!(
            touched, counted_reads,
            "{shape:?} round {round}: source_wal_index_touched (shm {shm})"
        );
        assert_eq!(
            shm != "absent",
            counted_reads == 1,
            "{shape:?} round {round}: a `-shm` sits beside the source exactly when one is counted"
        );
        if round == 0 && shape == Shape::CrashedNoShm {
            assert_eq!(shm, "created", "{shape:?} creates the wal-index");
        }
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

/// P75 over the PINNED rows, then over generated sources. One test, so every
/// in-process snapshot (and the process-scope counter) is this test's own.
#[test]
fn p75_a_snapshot_writes_only_the_counted_wal_index() {
    for case in pinned() {
        check(&case);
    }
    TestRunner::new(prop_config(12))
        .run(&case(), |case| {
            check(&case);
            Ok(())
        })
        .unwrap();
}

/// The counter reaches the `snapshot` verb's `counters` line: 1 for a source
/// whose wal-index the read creates, 0 for a quiescent one it reads
/// immutable.
#[test]
fn the_counters_line_reports_the_wal_index() {
    for (shape, expected) in [(Shape::CrashedNoShm, 1), (Shape::CleanClosed, 0)] {
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
        let output = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
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
        let field = format!("source_wal_index_touched={expected}");
        assert!(
            line.split(' ').any(|pair| pair == field),
            "{shape:?}: {field} not in {line}"
        );
    }
}
