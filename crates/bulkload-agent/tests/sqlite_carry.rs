//! `SQLite` snapshot seats through the CLI (#218, P80 to P83's end-to-end
//! leg; `docs/agent-notes/2026-10-08-sqlite-carry-design.md`).
//!
//! `copy --sqlite=snapshot` is the verb gate (a) runs, and its source half
//! is `serve` in the same process, so this is `pull`'s path without ssh.
//! Over a tree with a live WAL store (a writer child holding it, frames in
//! its `-wal`), a closed WAL store (no `-wal`, OI-1003-Q72) and a
//! rollback-mode store:
//!
//! - every store arrives as a verified snapshot in journal mode DELETE, its
//!   rows the committed ones, with no sidecar beside it; the live store's
//!   `-wal` and `-shm` are covered, never carried;
//! - on the source, each main file and the existing `-wal` keep their bytes
//!   and identity, and the only new entries are the closed store's `-shm`
//!   and empty `-wal` (P75's footprint, OI-1003-Q36, Q72);
//! - the counters line reports the snapshots, their source bytes, the lock
//!   span and both S2 write counters, and the transfer line its mode;
//! - the warm rerun takes no snapshot, holds no lock, writes nothing beside
//!   the source and reads 0 bytes (S3, G5);
//! - `--sqlite=refuse`, the default, refuses every store as v5 does.
//!
//! **As root (OI-1003-Q76, R14):** `copy --sqlite=snapshot` refuses
//! `SQLITE_SOURCE_AS_ROOT` before anything beneath the source is opened, and
//! the source directory stays byte- and metadata-identical; then the whole
//! test runs again in a child dropped to uid and gid 65534, as P75 does, and
//! says so on stderr when the drop cannot be made.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Names the database the writer child opens; unset, the child is a no-op.
const WRITER: &str = "BULKLOAD_SQLITE_CARRY_WRITER";
/// Marks the writer child's protocol lines.
const TAG: &str = "carry> ";
/// The uid and gid the unprivileged leg drops to: `nobody` on Linux.
const UNPRIVILEGED: u32 = 65_534;
/// Set for [`sqlite_carry_unprivileged_probe`]; unset, it is a no-op.
const PROBE: &str = "BULKLOAD_SQLITE_CARRY_PROBE";
/// Names the agent for the unprivileged leg: a copy it can reach.
const AGENT: &str = "BULKLOAD_SQLITE_CARRY_AGENT";

fn agent() -> PathBuf {
    std::env::var_os(AGENT).map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_bulkload-agent")),
        PathBuf::from,
    )
}

/// The writer child: opens the database named by [`WRITER`] with a zero
/// busy timeout and no automatic checkpoint, prints `ready`, runs each
/// stdin line as one SQL batch (`ok` or `err`), and closes its connection
/// at the end of stdin.
#[test]
fn sqlite_carry_writer_child() {
    let Some(path) = std::env::var_os(WRITER) else {
        return;
    };
    let connection = Connection::open(PathBuf::from(path)).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    connection
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    println!("{TAG}ready");
    for line in std::io::stdin().lock().lines() {
        match connection.execute_batch(&line.unwrap()) {
            Ok(()) => println!("{TAG}ok"),
            Err(error) => println!("{TAG}err {error}"),
        }
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

struct Writer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Writer {
    fn start(database: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "sqlite_carry_writer_child",
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

    fn reply(&mut self) -> String {
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(self.stdout.read_line(&mut line).unwrap(), 0, "writer gone");
            if let Some(at) = line.find(TAG) {
                return line[at + TAG.len()..].trim_end().to_owned();
            }
        }
    }

    /// Run `batch` as one line, and wait for its answer.
    fn run(&mut self, batch: &str) -> String {
        let line = batch.replace('\n', " ");
        writeln!(self.stdin.as_mut().unwrap(), "{line}").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
        self.reply()
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

/// Every entry of a directory: bytes, identity and mode.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    bytes: Vec<u8>,
    ino: u64,
    size: u64,
    mode: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

fn scan(dir: &Path) -> BTreeMap<OsString, Entry> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            (
                entry.file_name(),
                Entry {
                    bytes: fs::read(entry.path()).unwrap_or_default(),
                    ino: meta.ino(),
                    size: meta.size(),
                    mode: meta.mode(),
                    mtime: (meta.mtime(), meta.mtime_nsec()),
                    ctime: (meta.ctime(), meta.ctime_nsec()),
                },
            )
        })
        .collect()
}

const COMMIT_ONE: &str = "BEGIN IMMEDIATE;
    INSERT INTO ledger(k, v) SELECT coalesce(max(k), 0) + 1, randomblob(48) FROM ledger;
    COMMIT;";

fn build(path: &Path, journal: &str, rows: u64) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode={journal};
             CREATE TABLE ledger(k INTEGER PRIMARY KEY, v BLOB NOT NULL);"
        ))
        .unwrap();
    for _ in 0..rows {
        connection.execute_batch(COMMIT_ONE).unwrap();
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

/// A published store's rows, read without any sidecar (`immutable=1`).
fn rows(path: &Path) -> i64 {
    let connection = Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    connection
        .query_row("SELECT count(*) FROM ledger", [], |row| row.get(0))
        .unwrap()
}

/// `key=value` fields of the line of `output` that starts with `prefix`.
fn fields(output: &str, prefix: &str) -> BTreeMap<String, String> {
    let line = output
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} line in {output}"));
    line.split(' ')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn copy(base: &Path, mode: &str) -> std::process::Output {
    Command::new(agent())
        .arg(format!("--sqlite={mode}"))
        .arg("copy")
        .arg(base.join("source"))
        .arg(base.join("destination"))
        .arg(base.join("source-state"))
        .arg(base.join("destination-state"))
        .output()
        .unwrap()
}

/// The uid this process creates files as.
fn effective_uid() -> u32 {
    let made = tempfile::Builder::new()
        .prefix("bulkload-carry-uid-")
        .tempdir()
        .unwrap();
    fs::metadata(made.path()).unwrap().uid()
}

/// A tree of three stores, a writer holding the live one.
fn corpus() -> (tempfile::TempDir, Writer) {
    let base = tempfile::Builder::new()
        .prefix("bulkload-sqlite-carry-")
        .tempdir()
        .unwrap();
    for dir in ["source", "destination"] {
        fs::create_dir(base.path().join(dir)).unwrap();
    }
    let source = base.path().join("source");
    build(&source.join("live.db"), "WAL", 20);
    build(&source.join("closed.db"), "WAL", 7);
    build(&source.join("rollback.db"), "DELETE", 5);
    let mut writer = Writer::start(&source.join("live.db"));
    for _ in 0..3 {
        assert_eq!(writer.run(COMMIT_ONE), "ok");
    }
    // Past the racy allowance, so the first copy's captures settle and the
    // warm rerun can show 0 (#86).
    bulkload_agent::transfer::settle_racy_window(&source).unwrap();
    (base, writer)
}

/// The refusal leg as root: the session is refused before anything beneath
/// the source is opened, and the source directory stays as it was.
fn refused_as_root() {
    let (base, writer) = corpus();
    let before = scan(&base.path().join("source"));
    let output = copy(base.path(), "snapshot");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        scan(&base.path().join("source")),
        before,
        "a snapshot session as root changed its source directory"
    );
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr
            .lines()
            .any(|line| line.ends_with("refused: SQLITE_SOURCE_AS_ROOT")),
        "{stderr}"
    );
    assert!(!base.path().join("source-state").exists());
    writer.finish();
}

/// `copy --sqlite=snapshot` end to end, then its warm rerun, then
/// `--sqlite=refuse`. As root: the refusal, then this test as `nobody`.
#[test]
#[allow(clippy::too_many_lines)] // One run, its warm rerun and refuse mode, in order.
fn a_tree_of_live_stores_is_carried_by_snapshot_and_reused_warm() {
    if effective_uid() == 0 {
        refused_as_root();
        unprivileged_leg("a_tree_of_live_stores_is_carried_by_snapshot_and_reused_warm");
        return;
    }
    let (base, writer) = corpus();
    let source = base.path().join("source");
    let destination = base.path().join("destination");
    let before = scan(&source);
    assert!(before.contains_key(&OsString::from("live.db-wal")));
    assert!(!before.contains_key(&OsString::from("closed.db-wal")));

    let output = copy(base.path(), "snapshot");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let transfer = fields(&stdout, "completed=");
    assert_eq!(transfer["sqlite_mode"], "snapshot");
    assert_eq!(transfer["sqlite_snapshots"], "3");
    assert_eq!(
        transfer["sqlite_sidecars_covered"], "2",
        "live.db's -wal, -shm"
    );
    assert_eq!(transfer["refusals"], "0");
    let counters = fields(&stdout, "counters ");
    assert_eq!(counters["source_sqlite_snapshots"], "3");
    assert_eq!(
        counters["source_wal_created"], "1",
        "closed.db's empty -wal"
    );
    assert_eq!(counters["read_source_file_bytes"], "0", "no live byte read");
    assert_ne!(counters["source_sqlite_backup_bytes"], "0");
    assert_ne!(counters["read_source_snapshot_bytes"], "0");
    assert_ne!(counters["source_sqlite_lock_ns"], "0");
    assert_eq!(counters["dest_sqlite_verified"], "3");
    for (name, count) in [("live.db", 23), ("closed.db", 7), ("rollback.db", 5)] {
        assert_eq!(rows(&destination.join(name)), count, "{name}");
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!destination.join(format!("{name}{suffix}")).exists());
        }
    }
    let after = scan(&source);
    for (name, entry) in &before {
        // The live writer's wal-index is the one file a read may write in
        // place (its read marks): OI-1003-Q36, counted, not compared.
        if name == "live.db-shm" {
            assert!(after.contains_key(name));
            continue;
        }
        assert_eq!(after.get(name), Some(entry), "{}", name.display());
    }
    let created: Vec<&OsString> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .collect();
    assert_eq!(created, ["closed.db-shm", "closed.db-wal"]);
    assert_eq!(after[&OsString::from("closed.db-wal")].size, 0);

    // Warm: nothing opened, nothing read, nothing written beside the source.
    let warm = copy(base.path(), "snapshot");
    let warm_stdout = String::from_utf8_lossy(&warm.stdout);
    assert!(warm.status.success(), "{warm_stdout}");
    let transfer = fields(&warm_stdout, "completed=");
    assert_eq!(transfer["reused"], "3");
    assert_eq!(transfer["source_bytes_read"], "0");
    assert_eq!(transfer["bytes_received"], "0");
    let counters = fields(&warm_stdout, "counters ");
    for field in [
        "source_sqlite_snapshots",
        "source_sqlite_backup_bytes",
        "source_sqlite_lock_ns",
        "source_wal_index_touched",
        "source_wal_created",
        "source_sniff_bytes",
    ] {
        assert_eq!(counters[field], "0", "{field} on the warm rerun");
    }
    let warm_after = scan(&source);
    for (name, entry) in &after {
        if name != "live.db-shm" {
            assert_eq!(warm_after.get(name), Some(entry), "{}", name.display());
        }
    }
    assert_eq!(
        warm_after.len(),
        after.len(),
        "the warm rerun creates nothing"
    );

    // The default, refuse: v5's refusals, and nothing published over them.
    let refuse = Command::new(agent())
        .arg("copy")
        .arg(&source)
        .arg(&destination)
        .arg(base.path().join("source-state"))
        .arg(base.path().join("destination-state"))
        .output()
        .unwrap();
    let refuse_stdout = String::from_utf8_lossy(&refuse.stdout);
    assert!(!refuse.status.success());
    let transfer = fields(&refuse_stdout, "completed=");
    assert_eq!(transfer["sqlite_mode"], "refuse");
    assert_eq!(transfer["sqlite_snapshots"], "0");
    assert_eq!(transfer["source_bytes_read"], "0");
    let stderr = String::from_utf8_lossy(&refuse.stderr);
    for name in [
        "live.db",
        "closed.db",
        "rollback.db",
        "live.db-wal",
        "closed.db-wal",
    ] {
        assert!(
            stderr.contains(&format!("refused {name}: SQLITE_STATE_CHANGED")),
            "{name}: {stderr}"
        );
    }
    writer.finish();
}

/// A note on the real stderr, which the harness does not capture.
fn note(line: &str) {
    let _ = writeln!(std::io::stderr(), "sqlite-carry root: {line}");
}

/// The probe the unprivileged leg runs first, as the dropped uid.
#[test]
fn sqlite_carry_unprivileged_probe() {
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

fn dropped(test: &str, scratch: &Path) -> Command {
    let mut command = Command::new(scratch.join("carry-test"));
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

/// The unprivileged leg of a root run, as P75's: the same test again in a
/// child dropped to [`UNPRIVILEGED`], skipped with a note when the drop
/// cannot be made.
fn unprivileged_leg(test: &str) {
    let scratch = tempfile::Builder::new()
        .prefix("bulkload-carry-drop-")
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
    fs::copy(
        std::env::current_exe().unwrap(),
        scratch.path().join("carry-test"),
    )
    .unwrap();
    fs::copy(agent(), scratch.path().join("bulkload-agent")).unwrap();
    match dropped("sqlite_carry_unprivileged_probe", scratch.path())
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
        "{test}: refusal proved as root; full test passed as uid {UNPRIVILEGED}"
    ));
}
