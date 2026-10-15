//! #218, OI-1003-Q146: a crash at each step of a `SQLite` snapshot's
//! superseding exchange (WP0(d)'s points, `materialize`'s module docs).
//!
//! Each scenario copies a closed WAL-mode store and a DELETE-mode store with
//! `--sqlite=snapshot`, commits one transaction to each on the source, and
//! crashes the rerun that supersedes their outputs at one point (the child
//! ends itself with `_exit`; nothing here sends a signal, R-N11). It asserts:
//!
//! - **old or new, whole.** At the crash every final name holds a whole
//!   snapshot, old or new: `integrity_check` ok, journal mode DELETE, its
//!   rows the old store's or the new one's, never another state; no `-wal`,
//!   `-journal` or `-shm` beside it; only `.bulkload-*` temporaries extra;
//! - **no row beside other bytes.** Every output row names a file with
//!   exactly its recorded identity;
//! - **the crash landed where it says.** The points that stage or displace a
//!   file leave a temporary, and `supersede.after_exchange` leaves the old
//!   snapshot displaced under one;
//! - **a rerun finishes.** The resume converges with no refusal, every leaf
//!   holds the new snapshot, no temporary survives (the sweep removed
//!   exactly the crash's), and a further run reuses both stores with 0
//!   bytes read and 0 received.
//!
//! **As root** (PR CI runs this gate as root) a `snapshot`-mode session is
//! refused (OI-1003-Q76), so each row re-runs itself, crash child included,
//! in a copy of this test binary dropped to uid and gid [`UNPRIVILEGED`]
//! (`nobody`), with a scratch `TMPDIR` given to that uid, as
//! `tests/sqlite_wal_index.rs` and `tests/sqlite_carry.rs` do. The row passes
//! only when that child ran exactly the row and it passed. A root run that
//! cannot make the drop (a user namespace mapping uid 0 alone, `unshare -r`)
//! fails the row: it is never a silent pass (R-N122).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bulkload_agent::fault::Point;
use bulkload_agent::transfer::{copy_with, TransferStats};
use bulkload_proto::frame::SqliteMode;
use rusqlite::{Connection, OpenFlags};

use super::{
    crash_child, digest, is_temporary, relative, tree, Scenario, Scratch, CHILD_ENV, NEXT,
};

/// The uid and gid a root run re-runs each row as: `nobody` on Linux.
const UNPRIVILEGED: u32 = 65_534;

/// Set in a crash child: its `copy` runs in `snapshot` mode.
pub const SQLITE_CHILD_ENV: &str = "BULKLOAD_W7_CHILD_SQLITE";

/// The stores of the fixture: name and WAL mode.
const STORES: [(&str, bool); 2] = [("wal.db", true), ("delete.db", false)];

/// Rows each store holds before the change; the change adds one.
const ROWS: u64 = 40;

/// A crash in the rerun that supersedes a changed store's snapshot output.
#[derive(Clone, Copy)]
pub struct SqliteSupersede;

impl Scenario for SqliteSupersede {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth);
    }

    fn run_named(self, point: Point, nth: u64, test: &str) {
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            as_unprivileged(test);
        } else {
            scenario(point, nth);
        }
    }
}

/// Removes a root run's scratch directory, binary copy included.
struct Dropped(PathBuf);

impl Drop for Dropped {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A temporary root that [`UNPRIVILEGED`] can reach: every ancestor must be
/// searchable by others, or exec of the binary copy fails EACCES. CI's
/// TMPDIR sits under the runner's home, which is not, so `TMPDIR` is tried
/// first and then the world-searchable system roots. None reachable fails the
/// row: a root run that cannot drop proves nothing.
fn searchable_temp_root(test: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let searchable = |root: &Path| {
        root.is_absolute()
            && root.ancestors().all(|dir| {
                fs::metadata(dir)
                    .is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o001 != 0)
            })
    };
    [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/shm"),
        PathBuf::from("/var/tmp"),
    ]
    .into_iter()
    .find(|root| searchable(root))
    .unwrap_or_else(|| {
        panic!(
            "{test}: as root this row runs as uid {UNPRIVILEGED}, and no temporary root \
             is searchable by that uid; a root run that cannot drop proves nothing, so it fails"
        )
    })
}

/// The root leg (module docs): run the row `test` in a copy of this binary
/// as [`UNPRIVILEGED`], and require that it ran exactly that row and passed.
fn as_unprivileged(test: &str) {
    let scratch = Dropped(searchable_temp_root(test).join(format!(
        "bulkload-w7-sqlite-drop-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    fs::create_dir(&scratch.0).unwrap();
    std::os::unix::fs::chown(&scratch.0, Some(UNPRIVILEGED), Some(UNPRIVILEGED)).unwrap_or_else(
        |error| {
            panic!(
                "{test}: as root this row runs as uid {UNPRIVILEGED}, and its scratch \
                 directory cannot be given to that uid: {error}"
            )
        },
    );
    // The copy keeps the original's mode: readable and runnable by all.
    let binary = scratch.0.join("w7-test");
    fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    let run = Command::new(&binary)
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env("TMPDIR", &scratch.0)
        .current_dir(&scratch.0)
        .env_remove(CHILD_ENV)
        .env_remove(SQLITE_CHILD_ENV)
        .stdin(Stdio::null())
        .uid(UNPRIVILEGED)
        .gid(UNPRIVILEGED)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "{test}: as root this row runs as uid {UNPRIVILEGED}, and the drop failed \
                 ({error}); a root run that cannot drop proves nothing, so it fails"
            )
        });
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{test} as uid {UNPRIVILEGED} exited {}:\n{stdout}\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed"),
        "{test} as uid {UNPRIVILEGED} did not run exactly this row:\n{stdout}"
    );
    for line in stdout
        .lines()
        .filter(|line| line.contains("sqlite superseding: "))
    {
        println!("{line} uid={UNPRIVILEGED}");
    }
}

fn build(path: &Path, wal: bool) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA page_size=1024;
             PRAGMA journal_mode={};
             CREATE TABLE ledger(k INTEGER PRIMARY KEY, v BLOB NOT NULL);",
            if wal { "WAL" } else { "DELETE" }
        ))
        .unwrap();
    for _ in 0..ROWS {
        commit(&connection);
    }
    connection.close().map_err(|(_, error)| error).unwrap();
}

fn commit(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO ledger(k, v) SELECT coalesce(max(k), 0) + 1, randomblob(200) FROM ledger;",
        )
        .unwrap();
}

/// A published snapshot's rows, after checking it is whole: `integrity_check`
/// ok, journal mode DELETE, rows `1..=k`, and no sidecar beside it.
fn whole(label: &str, path: &Path) -> u64 {
    let uri = format!("file:{}?immutable=1", path.display());
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap_or_else(|error| panic!("{label}: {path:?} does not open: {error}"));
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{label}: {path:?}: {error}"));
    assert_eq!(check, "ok", "{label}: {path:?} is not whole");
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete", "{label}: {path:?}");
    let (count, top): (u64, u64) = connection
        .query_row(
            "SELECT count(*), coalesce(max(k), 0) FROM ledger",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, top, "{label}: {path:?} rows 1..=k");
    for suffix in ["-wal", "-journal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        assert!(
            fs::symlink_metadata(&name).is_err(),
            "{label}: {name:?} beside a published snapshot"
        );
    }
    count
}

fn run(scratch: &Scratch) -> TransferStats {
    copy_with(
        &scratch.source(),
        &scratch.destination(),
        &scratch.source_state(),
        &scratch.destination_state(),
        SqliteMode::Snapshot,
    )
    .unwrap()
}

/// A recorded stat identity: `(dev, ino, size, mtime_ns, ctime_ns)`.
type Identity = (u64, u64, u64, i128, i128);

/// Every output row of the destination store: its path, and the identity
/// it records. Read from a copy of the database, as `committed` does; the
/// path is the key's second field whatever the key's kind (a row key, a
/// snapshot key or an ownership row).
fn output_rows(scratch: &Scratch) -> Vec<(Vec<u8>, Identity)> {
    let database = scratch.destination_state().join("transfer.sqlite");
    let copy = scratch.base.join("inspect").join(format!(
        "outputs-{}.sqlite",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::copy(&database, &copy).unwrap();
    let wal = scratch.destination_state().join("transfer.sqlite-wal");
    let copied_wal = format!("{}-wal", copy.display());
    if wal.exists() {
        fs::copy(&wal, &copied_wal).unwrap();
    }
    let rows = {
        let connection = Connection::open(&copy).unwrap();
        let mut statement = connection.prepare("SELECT * FROM outputs").unwrap();
        let rows: Vec<_> = statement
            .query_map([], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .unwrap()
            .map(|row| {
                let (key, value) = row.unwrap();
                let (_, rest) = postcard::take_from_bytes::<Vec<u8>>(&key).unwrap();
                let (path, _) = postcard::take_from_bytes::<Vec<u8>>(rest).unwrap();
                (path, postcard::from_bytes(&value).unwrap())
            })
            .collect();
        rows
    };
    for leftover in [
        copy.display().to_string(),
        copied_wal,
        format!("{}-shm", copy.display()),
    ] {
        let _ = fs::remove_file(leftover);
    }
    rows
}

/// The crash state's checks (module docs). Returns the temporaries.
fn assert_old_or_new(label: &str, scratch: &Scratch, old: &BTreeMap<&str, [u8; 32]>) -> usize {
    use std::os::unix::fs::MetadataExt as _;
    let mut temporaries = 0;
    let mut leaves = Vec::new();
    for (path, metadata) in tree(&scratch.destination()) {
        if is_temporary(&path) {
            assert!(metadata.is_file(), "{label}: a temporary that is no file");
            temporaries += 1;
            continue;
        }
        let name = String::from_utf8(path.clone()).unwrap();
        assert!(
            STORES.iter().any(|(store, _)| *store == name),
            "{label}: unexpected leaf {name}"
        );
        let rows = whole(label, &scratch.destination().join(&name));
        assert!(
            rows == ROWS || rows == ROWS + 1,
            "{label}: {name} holds {rows} rows: neither the old snapshot nor the new"
        );
        if rows == ROWS {
            assert_eq!(
                Some(&digest(&scratch.destination().join(&name))),
                old.get(name.as_str()),
                "{label}: {name} is the old state but not the old output"
            );
        }
        leaves.push(name);
    }
    assert_eq!(
        leaves.len(),
        STORES.len(),
        "{label}: a store's leaf is missing"
    );
    for (path, recorded) in output_rows(scratch) {
        let target = scratch.destination().join(relative(&path));
        let metadata = fs::symlink_metadata(&target)
            .unwrap_or_else(|error| panic!("{label}: a row names {target:?}: {error}"));
        let observed = (
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        );
        assert_eq!(
            observed, recorded,
            "{label}: a row sits beside a file it does not describe: {target:?}"
        );
    }
    temporaries
}

fn scenario(point: Point, nth: u64) {
    let label = format!("{}:{nth} sqlite superseding", point.name());
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "{label}: as root a snapshot-mode session is refused SQLITE_SOURCE_AS_ROOT \
         (OI-1003-Q76); the row runs through `as_unprivileged`"
    );
    let scratch = Scratch::new(&format!(
        "{}-sqlite-superseding",
        point.name().replace('.', "-")
    ));
    for (name, wal) in STORES {
        build(&scratch.source().join(name), wal);
    }
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();
    let first = run(&scratch);
    assert!(first.refusals.is_empty(), "{label}: {:?}", first.refusals);
    let mut old = BTreeMap::new();
    for (name, _) in STORES {
        let output = scratch.destination().join(name);
        assert_eq!(whole(&label, &output), ROWS, "{label}: first copy");
        old.insert(name, digest(&output));
        let connection = Connection::open(scratch.source().join(name)).unwrap();
        commit(&connection);
        connection.close().map_err(|(_, error)| error).unwrap();
    }
    bulkload_agent::transfer::settle_racy_window(&scratch.source()).unwrap();

    let spec = format!("{}:{nth}", point.name());
    let group = crash_child(&scratch, point, &spec, nth > 1, &[(SQLITE_CHILD_ENV, "1")]);
    let crash_temporaries = assert_old_or_new(&label, &scratch, &old);
    if matches!(
        point,
        Point::SupersedeAfterIntent
            | Point::SupersedeAfterExchange
            | Point::MaterializeAfterTempSeal
            | Point::MaterializeAfterTempWrite
    ) {
        assert!(
            crash_temporaries >= 1,
            "{label}: this crash must leave a staged or a displaced file"
        );
    }
    if point == Point::SupersedeAfterExchange {
        let displaced = tree(&scratch.destination())
            .into_keys()
            .filter(|path| is_temporary(path))
            .filter(|path| {
                old.values()
                    .any(|held| *held == digest(&scratch.destination().join(relative(path))))
            })
            .count();
        assert!(displaced >= 1, "{label}: an old snapshot must be displaced");
    }

    let resumed = run(&scratch);
    assert!(
        resumed.refusals.is_empty(),
        "{label}: {:?}",
        resumed.refusals
    );
    assert_eq!(
        assert_old_or_new(&format!("{label} resumed"), &scratch, &BTreeMap::new()),
        0,
        "{label}: a temporary survived the resume"
    );
    for (name, _) in STORES {
        assert_eq!(
            whole(&label, &scratch.destination().join(name)),
            ROWS + 1,
            "{label}: {name} holds the new snapshot after the resume"
        );
    }
    assert_eq!(
        resumed.temporaries_removed, crash_temporaries as u64,
        "{label}: the sweep must remove exactly the crash's temporaries"
    );
    assert!(
        resumed.temporaries_left.is_empty(),
        "{label}: {:?} was left",
        resumed.temporaries_left
    );

    let again = run(&scratch);
    assert!(again.refusals.is_empty(), "{label}: {:?}", again.refusals);
    assert_eq!(again.source_bytes_read, 0, "{label}: a further run reads 0");
    assert_eq!(again.bytes_received, 0, "{label}: a further run receives 0");
    assert_eq!(again.reused, STORES.len() as u64, "{label}: all reused");
    println!(
        "{label}: temporaries_at_crash={crash_temporaries} resume_completed={} resume_reused={} \
         resume_unrowed_adopted={} resume_source_bytes={} resume_bytes_received={}{}",
        resumed.completed,
        resumed.reused,
        resumed.unrowed_adopted,
        resumed.source_bytes_read,
        resumed.bytes_received,
        group
            .map(|group| format!(" crash_{group}"))
            .unwrap_or_default(),
    );
}
