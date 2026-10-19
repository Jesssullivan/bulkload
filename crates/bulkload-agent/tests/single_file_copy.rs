//! #232 through the binary: `copy --file` and `pull --file` carry one file
//! to the file DEST names, a regular file byte for byte and a symlink as a
//! link (never its target's bytes), with a tree's guarantees (adopted when
//! identical, `DESTINATION_OCCUPIED` when it differs, 0 source bytes on an
//! unchanged rerun, and a later tree pass of the parent reuses the rows);
//! a leaf that is missing or of another kind is a typed row that crosses
//! the wire; `copy` makes an absent DEST root (0755 under the umask, as
//! carry does) and never its parent, and removes it again when its own
//! setup refuses; a symlinked DEST is typed and never followed. And #276's
//! source half: `serve` makes every missing parent of its state root.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-single-file-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("source")).unwrap();
        std::fs::create_dir(base.join("destination")).unwrap();
        Self(std::fs::canonicalize(base).unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// `copy [--file] SOURCE DEST SOURCE_STATE DEST_STATE`, every path
    /// relative to the scratch.
    fn copy(&self, file: bool, source: &str, dest: &str, states: (&str, &str)) -> Ran {
        self.copy_with(&[], file, source, dest, states)
    }

    /// [`Scratch::copy`] with global flags before the verb.
    fn copy_with(
        &self,
        globals: &[&str],
        file: bool,
        source: &str,
        dest: &str,
        states: (&str, &str),
    ) -> Ran {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        command
            .arg("--min-free-percent=0")
            .args(globals)
            .arg("copy");
        if file {
            command.arg("--file");
        }
        command.args([
            self.path(source),
            self.path(dest),
            self.path(states.0),
            self.path(states.1),
        ]);
        let out = command.output().unwrap();
        Ran {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// `pull [--file] fakehost SOURCE DEST SOURCE_STATE DEST_STATE AGENT`
    /// through an `ssh` stand-in first on `PATH` that runs the remote
    /// command here: the real wire between two processes.
    fn pull(&self, file: bool, source: &str, dest: &str, states: (&str, &str)) -> Ran {
        let bin = self.path("fake-bin");
        if !bin.join("ssh").exists() {
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(
                bin.join("ssh"),
                "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nshift 2\nexec \"$@\"\n",
            )
            .unwrap();
            std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let mut path = std::ffi::OsString::from(&bin);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        command
            .arg("--min-free-percent=0")
            .arg("pull")
            .env("PATH", path);
        if file {
            command.arg("--file");
        }
        command.arg("fakehost").args([
            self.path(source),
            self.path(dest),
            self.path(states.0),
            self.path(states.1),
        ]);
        command.arg(env!("CARGO_BIN_EXE_bulkload-agent"));
        let out = command.output().unwrap();
        Ran {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A test may have locked a directory; open it again before removing.
        let _ = std::fs::set_permissions(
            self.0.join("locked"),
            std::fs::Permissions::from_mode(0o700),
        );
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn counter(output: &str, name: &str) -> u64 {
    output
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in {output}"))
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

fn settle(path: &Path) {
    bulkload_agent::transfer::settle_racy_window(path).unwrap();
}

#[test]
fn copy_file_lands_the_file_byte_exact_and_an_unchanged_rerun_reads_nothing() {
    let scratch = Scratch::new("lands");
    let bytes = noise(7, 300 * 1024);
    std::fs::write(scratch.path("source/a.txt"), &bytes).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(true, "source/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        bytes
    );
    // One file landed, nothing else in its directory.
    assert_eq!(names(&scratch.path("destination")), ["a.txt"]);
    assert_eq!(counter(&ran.stdout, "completed"), 1, "{}", ran.stdout);
    assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
    assert_eq!(
        counter(&ran.stdout, "source_bytes_read"),
        bytes.len() as u64,
        "{}",
        ran.stdout
    );
    // R25: the same file again costs no source read.
    settle(&scratch.path("destination"));
    let again = scratch.copy(true, "source/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(again.ok, "{}{}", again.stdout, again.stderr);
    assert_eq!(
        counter(&again.stdout, "source_bytes_read"),
        0,
        "{}",
        again.stdout
    );
    assert_eq!(counter(&again.stdout, "refusals"), 0, "{}", again.stdout);
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        bytes
    );
    // The session is recorded in DEST_STATE like a tree's (WP3 PR 4).
    assert!(scratch.path("ds/transfer.closure.d").is_dir());
}

#[test]
fn copy_file_lands_under_the_destination_name() {
    let scratch = Scratch::new("rename");
    let bytes = noise(11, 4096);
    std::fs::write(scratch.path("source/a.txt"), &bytes).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(true, "source/a.txt", "destination/b.txt", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        std::fs::read(scratch.path("destination/b.txt")).unwrap(),
        bytes
    );
    assert_eq!(names(&scratch.path("destination")), ["b.txt"]);
}

#[test]
fn copy_file_adopts_an_identical_file_and_refuses_a_differing_one() {
    let scratch = Scratch::new("adopt");
    let bytes = noise(3, 64 * 1024);
    std::fs::write(scratch.path("source/a.txt"), &bytes).unwrap();
    // Identical, and not bulkload's: adopted, nothing refused.
    std::fs::write(scratch.path("destination/a.txt"), &bytes).unwrap();
    settle(&scratch.path("source"));
    settle(&scratch.path("destination"));
    let ran = scratch.copy(true, "source/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        bytes
    );
    // Differing, and not bulkload's: preserved and refused, typed.
    let other = noise(5, 64 * 1024);
    std::fs::write(scratch.path("destination/c.txt"), &other).unwrap();
    settle(&scratch.path("destination"));
    let ran = scratch.copy(true, "source/a.txt", "destination/c.txt", ("ss2", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused c.txt: DESTINATION_OCCUPIED"),
        "{}",
        ran.stderr
    );
    assert_eq!(
        std::fs::read(scratch.path("destination/c.txt")).unwrap(),
        other
    );
}

#[test]
fn a_root_that_is_not_the_kind_the_verb_was_given_is_typed() {
    let scratch = Scratch::new("kind");
    std::fs::write(scratch.path("source/a.txt"), b"one file\n").unwrap();
    // `--file` named a directory: the walk's typed row, nothing entered.
    let ran = scratch.copy(true, "source", "destination/x", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused x: SOURCE_ROOT_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("destination/x").exists());
    // A tree verb named a regular file: the session's refusal.
    let ran = scratch.copy(false, "source/a.txt", "destination/y", ("ss2", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: SOURCE_ROOT_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    // The absent tree root was not made for a session the source refused.
    assert!(!scratch.path("destination/y").exists(), "{}", ran.stderr);
}

#[test]
fn a_missing_source_root_is_typed() {
    let scratch = Scratch::new("missing");
    // A missing `--file` leaf is the walk's typed row (review 2).
    let ran = scratch.copy(true, "source/none", "destination/none", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused none: SOURCE_VANISHED"),
        "{}",
        ran.stderr
    );
    // A missing tree root, or a missing `--file` parent, is the session's.
    for (file, source) in [(false, "source/none"), (true, "source/gone/none")] {
        let ran = scratch.copy(file, source, "destination/none", ("ss2", "ds2"));
        assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr.contains("refused: SOURCE_ROOT_MISSING"),
            "{}",
            ran.stderr
        );
    }
    assert_eq!(names(&scratch.path("destination")), Vec::<String>::new());
}

#[test]
fn an_unreadable_source_root_is_typed_with_its_errno() {
    let scratch = Scratch::new("unreadable");
    std::fs::create_dir(scratch.path("locked")).unwrap();
    std::fs::write(scratch.path("locked/a.txt"), b"behind a closed door\n").unwrap();
    std::fs::set_permissions(
        scratch.path("locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    // Root reads through any mode: nothing to prove there.
    if std::fs::read_dir(scratch.path("locked")).is_ok() {
        return;
    }
    let ran = scratch.copy(true, "locked/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr
            .contains("refused: SOURCE_ROOT_UNREADABLE (errno 13)"),
        "{}",
        ran.stderr
    );
}

#[test]
fn copy_makes_an_absent_destination_root_and_never_its_parent() {
    let scratch = Scratch::new("absent");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::create_dir(scratch.path("source/d")).unwrap();
    std::fs::write(scratch.path("source/d/b.txt"), b"b\n").unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(false, "source", "destination/new", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(scratch.path("destination/new").is_dir());
    // 0755 under the umask, as estate-carry-files makes its destinations.
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(scratch.path("as-carry"))
            .unwrap();
    }
    assert_eq!(
        mode(&scratch.path("destination/new")),
        mode(&scratch.path("as-carry"))
    );
    assert_eq!(
        std::fs::read(scratch.path("destination/new/d/b.txt")).unwrap(),
        b"b\n"
    );
    assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
    // A missing parent is the operator's: typed, nothing made.
    let ran = scratch.copy(false, "source", "destination/missing/new", ("ss2", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: DESTINATION_PARENT_MISSING"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("destination/missing").exists());
    // The same for a file's parent.
    let ran = scratch.copy(
        true,
        "source/a.txt",
        "destination/missing/a.txt",
        ("ss3", "ds3"),
    );
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: DESTINATION_PARENT_MISSING"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("destination/missing").exists());
}

#[test]
fn the_wrong_kind_of_node_at_the_destination_is_typed() {
    let scratch = Scratch::new("destkind");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::write(
        scratch.path("destination/taken"),
        b"a file where a tree lands\n",
    )
    .unwrap();
    // `--file` would land on a directory.
    let ran = scratch.copy(true, "source/a.txt", "destination", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: DESTINATION_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    // A tree would land on a regular file.
    let ran = scratch.copy(false, "source", "destination/taken", ("ss2", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: DESTINATION_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    assert_eq!(
        std::fs::read(scratch.path("destination/taken")).unwrap(),
        b"a file where a tree lands\n"
    );
    assert_eq!(names(&scratch.path("destination")), ["taken"]);
}

// #276 (b): `serve`, here `copy`'s in-process source half, makes every
// missing directory above its state root, 0700 each (the carry phase's
// `serve/carry/ITEM` is two levels below `serve/`); a missing chain that
// would lie inside the source refuses the overlap with nothing made.
#[test]
fn serve_makes_every_missing_parent_of_its_state_root() {
    let scratch = Scratch::new("stateparent");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::create_dir(scratch.path("states")).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(
        false,
        "source",
        "destination",
        ("states/serve/carry/item", "ds"),
    );
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    for level in ["states/serve", "states/serve/carry"] {
        assert!(scratch.path(level).is_dir(), "{level}");
        assert_eq!(mode(&scratch.path(level)), 0o700, "{level}");
    }
    assert!(scratch.path("states/serve/carry/item").is_dir());
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        b"a\n"
    );
    // Inside the source: S2 refuses before any level is made.
    let ran = scratch.copy(false, "source", "destination2", ("source/x/y/state", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: SNAPSHOT_ROOTS_OVERLAP"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("source/x").exists());
    assert!(!scratch.path("destination2").exists());
}

// #283 review 3: a symlink at DEST (to a directory, a file or nothing) is
// typed `DESTINATION_KIND_MISMATCH`, for a tree's root and for a regular
// file landing under `--file`, and nothing is written through it.
#[test]
fn a_symlinked_destination_is_typed_and_never_followed() {
    let scratch = Scratch::new("symlinkdest");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::create_dir(scratch.path("destination/real")).unwrap();
    std::fs::write(scratch.path("destination/real.txt"), b"theirs\n").unwrap();
    for (link, target) in [("ld", "real"), ("lf", "real.txt"), ("dangling", "nowhere")] {
        std::os::unix::fs::symlink(target, scratch.path(&format!("destination/{link}"))).unwrap();
    }
    settle(&scratch.path("source"));
    for link in ["ld", "lf", "dangling"] {
        let dest = format!("destination/{link}");
        let ran = scratch.copy(
            false,
            "source",
            &dest,
            (&format!("ss-t{link}"), &format!("ds-t{link}")),
        );
        assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr.contains("refused: DESTINATION_KIND_MISMATCH"),
            "{link}: {}",
            ran.stderr
        );
        let ran = scratch.copy(
            true,
            "source/a.txt",
            &dest,
            (&format!("ss-f{link}"), &format!("ds-f{link}")),
        );
        assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr
                .contains(&format!("refused {link}: DESTINATION_KIND_MISMATCH")),
            "{link}: {}",
            ran.stderr
        );
        assert!(std::fs::symlink_metadata(scratch.path(&dest))
            .unwrap()
            .file_type()
            .is_symlink());
    }
    assert_eq!(
        names(&scratch.path("destination/real")),
        Vec::<String>::new()
    );
    assert_eq!(
        std::fs::read(scratch.path("destination/real.txt")).unwrap(),
        b"theirs\n"
    );
    assert!(!scratch.path("destination/nowhere").exists());
}

// #218 under `--file` (#232): a live WAL store is keyed by its `-wal` too,
// though the single-seat walk lists nothing else. A commit that writes
// only the `-wal` (no checkpoint) changes the key, so the rerun takes a
// new snapshot and lands the new row; keyed by the main file alone, it
// would reuse the stale one.
#[test]
fn copy_file_keys_a_live_wal_store_by_its_wal() {
    use std::os::unix::fs::MetadataExt as _;
    let scratch = Scratch::new("walkey");
    // As root the snapshot mode refuses SQLITE_SOURCE_AS_ROOT (OI-1003-Q76).
    if std::fs::metadata(scratch.path("source")).unwrap().uid() == 0 {
        return;
    }
    let db = scratch.path("source/app.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE t(k INTEGER PRIMARY KEY, v BLOB NOT NULL);
             INSERT INTO t(v) VALUES (randomblob(64)), (randomblob(64));",
        )
        .unwrap();
    assert!(scratch.path("source/app.db-wal").is_file());
    let count = |path: &Path| -> i64 {
        let reader = rusqlite::Connection::open_with_flags(
            format!("file:{}?immutable=1", path.display()),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        reader
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap()
    };
    let main_before = std::fs::symlink_metadata(&db).unwrap();
    settle(&scratch.path("source"));
    let snapshot = ["--sqlite=snapshot"];
    let ran = scratch.copy_with(
        &snapshot,
        true,
        "source/app.db",
        "destination/app.db",
        ("ss", "ds"),
    );
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        counter(&ran.stdout, "sqlite_snapshots"),
        1,
        "{}",
        ran.stdout
    );
    assert_eq!(count(&scratch.path("destination/app.db")), 2);
    // Nothing but the database lands: its sidecars are never offered.
    assert_eq!(names(&scratch.path("destination")), ["app.db"]);
    writer
        .execute_batch("INSERT INTO t(v) VALUES (randomblob(64));")
        .unwrap();
    // The commit went to the `-wal` only.
    let main_after = std::fs::symlink_metadata(&db).unwrap();
    assert_eq!(
        (
            main_before.ino(),
            main_before.size(),
            main_before.mtime_nsec(),
            main_before.mtime()
        ),
        (
            main_after.ino(),
            main_after.size(),
            main_after.mtime_nsec(),
            main_after.mtime()
        )
    );
    settle(&scratch.path("source"));
    settle(&scratch.path("destination"));
    let ran = scratch.copy_with(
        &snapshot,
        true,
        "source/app.db",
        "destination/app.db",
        ("ss", "ds"),
    );
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        counter(&ran.stdout, "sqlite_snapshots"),
        1,
        "{}",
        ran.stdout
    );
    assert_eq!(count(&scratch.path("destination/app.db")), 3);
    drop(writer);
}

// #283 review 1: a `--file` leaf that is a symlink is carried as a
// symlink, its literal target byte for byte, never the bytes it points at
// (relative or absolute, inside or outside the source directory); the
// rerun adopts the identical link, and a differing link at DEST is
// `DESTINATION_OCCUPIED`, left as it was.
#[test]
fn copy_file_carries_a_symlink_as_a_link() {
    let scratch = Scratch::new("symlinksrc");
    std::fs::write(scratch.path("source/real.txt"), b"real\n").unwrap();
    std::fs::create_dir(scratch.path("outside")).unwrap();
    std::fs::write(scratch.path("outside/secret.txt"), b"outside\n").unwrap();
    let absolute = scratch.path("outside/secret.txt");
    std::os::unix::fs::symlink("real.txt", scratch.path("source/rel")).unwrap();
    std::os::unix::fs::symlink(&absolute, scratch.path("source/abs")).unwrap();
    std::os::unix::fs::symlink("elsewhere", scratch.path("destination/other")).unwrap();
    settle(&scratch.path("source"));
    for (leaf, target) in [
        ("rel", PathBuf::from("real.txt")),
        ("abs", absolute.clone()),
    ] {
        let source = format!("source/{leaf}");
        let dest = format!("destination/{leaf}");
        for pass in 0..2 {
            let ran = scratch.copy(true, &source, &dest, ("ss", "ds"));
            assert!(ran.ok, "{leaf} pass {pass}: {}{}", ran.stdout, ran.stderr);
            assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
            let landed = std::fs::symlink_metadata(scratch.path(&dest)).unwrap();
            assert!(landed.file_type().is_symlink(), "{leaf}");
            assert_eq!(std::fs::read_link(scratch.path(&dest)).unwrap(), target);
        }
    }
    // Nothing the links point at was carried.
    assert_eq!(names(&scratch.path("destination")), ["abs", "other", "rel"]);
    let ran = scratch.copy(true, "source/rel", "destination/other", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused other: DESTINATION_OCCUPIED"),
        "{}",
        ran.stderr
    );
    assert_eq!(
        std::fs::read_link(scratch.path("destination/other")).unwrap(),
        PathBuf::from("elsewhere")
    );
}

// #283 review 2, over the wire: through `pull --file`, a missing leaf and
// a directory leaf reach the destination's record as typed rows, never as
// `TRANSPORT_LOST`; a regular file and a symlink land as they do under
// `copy --file`.
#[test]
fn pull_file_types_its_leaf_over_the_wire() {
    let scratch = Scratch::new("pullfile");
    std::fs::write(scratch.path("source/a.txt"), b"over the wire\n").unwrap();
    std::os::unix::fs::symlink("a.txt", scratch.path("source/link")).unwrap();
    std::fs::create_dir(scratch.path("source/dir")).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.pull(true, "source/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        b"over the wire\n"
    );
    let ran = scratch.pull(true, "source/link", "destination/link", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        std::fs::read_link(scratch.path("destination/link")).unwrap(),
        PathBuf::from("a.txt")
    );
    for (leaf, code) in [
        ("none", "SOURCE_VANISHED"),
        ("dir", "SOURCE_ROOT_KIND_MISMATCH"),
    ] {
        let ran = scratch.pull(
            true,
            &format!("source/{leaf}"),
            &format!("destination/{leaf}"),
            (&format!("ss-{leaf}"), &format!("ds-{leaf}")),
        );
        let both = format!("{}{}", ran.stdout, ran.stderr);
        assert!(!ran.ok, "{both}");
        assert!(both.contains(&format!("refused {leaf}: {code}")), "{both}");
        assert!(!both.contains("TRANSPORT_LOST"), "{both}");
        assert!(!scratch.path(&format!("destination/{leaf}")).exists());
    }
}

// #283 review 4: a `--file` landing is keyed as a tree session of the
// source's parent keys it, so a later tree pass of that parent with the
// same states reuses it (0 bytes read), and a changed file supersedes its
// own output instead of being refused as someone else's.
#[test]
fn a_file_landing_is_reused_by_a_later_tree_pass_of_its_parent() {
    let scratch = Scratch::new("treeafterfile");
    std::fs::write(scratch.path("source/a.txt"), noise(21, 50_000)).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(true, "source/a.txt", "destination/a.txt", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    settle(&scratch.path("destination"));
    let ran = scratch.copy(false, "source", "destination", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        counter(&ran.stdout, "source_bytes_read"),
        0,
        "{}",
        ran.stdout
    );
    assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
    let changed = noise(22, 60_000);
    std::fs::write(scratch.path("source/a.txt"), &changed).unwrap();
    settle(&scratch.path("source"));
    settle(&scratch.path("destination"));
    let ran = scratch.copy(false, "source", "destination", ("ss", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(counter(&ran.stdout, "refusals"), 0, "{}", ran.stdout);
    assert_eq!(
        std::fs::read(scratch.path("destination/a.txt")).unwrap(),
        changed
    );
}

// #283 review 6: a root this side made is removed again, still empty, when
// its own setup then refuses (here a destination state another user could
// read, `PATH_ESCAPES_ROOT`); nothing is made at all when the state refuses
// first.
#[test]
fn a_destination_setup_refusal_leaves_no_empty_root() {
    let scratch = Scratch::new("setupfail");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::create_dir(scratch.path("ds-wide")).unwrap();
    std::fs::set_permissions(
        scratch.path("ds-wide"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(false, "source", "destination/new", ("ss", "ds-wide"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: PATH_ESCAPES_ROOT"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("destination/new").exists(), "{}", ran.stderr);
}
