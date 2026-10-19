//! #232 through the binary: `copy --file` carries one regular file to the
//! file DEST names, byte for byte, with a tree's guarantees (adopted when
//! identical, `DESTINATION_OCCUPIED` when it differs, 0 source bytes on an
//! unchanged rerun); `copy` makes an absent DEST root (0700) and never its
//! parent; every root that is not what the verb was given is typed. And
//! #276's source half: `serve` (here `copy`'s in-process source) makes the
//! one missing level its state root sits in, and no more.

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
        let base =
            std::env::temp_dir().join(format!("bulkload-single-file-{name}-{}", std::process::id()));
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        command.arg("--min-free-percent=0").arg("copy");
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
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
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
    assert_eq!(std::fs::read(scratch.path("destination/a.txt")).unwrap(), bytes);
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
    assert_eq!(counter(&again.stdout, "source_bytes_read"), 0, "{}", again.stdout);
    assert_eq!(counter(&again.stdout, "refusals"), 0, "{}", again.stdout);
    assert_eq!(std::fs::read(scratch.path("destination/a.txt")).unwrap(), bytes);
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
    assert_eq!(std::fs::read(scratch.path("destination/b.txt")).unwrap(), bytes);
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
    assert_eq!(std::fs::read(scratch.path("destination/a.txt")).unwrap(), bytes);
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
    assert_eq!(std::fs::read(scratch.path("destination/c.txt")).unwrap(), other);
}

#[test]
fn a_root_that_is_not_the_kind_the_verb_was_given_is_typed() {
    let scratch = Scratch::new("kind");
    std::fs::write(scratch.path("source/a.txt"), b"one file\n").unwrap();
    // `--file` named a directory.
    let ran = scratch.copy(true, "source", "destination/x", ("ss", "ds"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: SOURCE_ROOT_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    assert!(!scratch.path("destination/x").exists());
    // A tree verb named a regular file.
    let ran = scratch.copy(false, "source/a.txt", "destination/y", ("ss2", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("refused: SOURCE_ROOT_KIND_MISMATCH"),
        "{}",
        ran.stderr
    );
    // The absent tree root was not made for a session that never ran.
    assert!(!scratch.path("destination/y").exists(), "{}", ran.stderr);
}

#[test]
fn a_missing_source_root_is_typed() {
    let scratch = Scratch::new("missing");
    for file in [true, false] {
        let ran = scratch.copy(file, "source/none", "destination/none", ("ss", "ds"));
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
    assert_eq!(mode(&scratch.path("destination/new")), 0o700);
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
    let ran = scratch.copy(true, "source/a.txt", "destination/missing/a.txt", ("ss3", "ds3"));
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
    std::fs::write(scratch.path("destination/taken"), b"a file where a tree lands\n").unwrap();
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

// #276 (b): `serve`, here `copy`'s in-process source half, makes the one
// directory its state root sits in; a deeper missing chain still refuses,
// with nothing made.
#[test]
fn serve_makes_the_one_missing_level_of_its_state_parent() {
    let scratch = Scratch::new("stateparent");
    std::fs::write(scratch.path("source/a.txt"), b"a\n").unwrap();
    std::fs::create_dir(scratch.path("states")).unwrap();
    settle(&scratch.path("source"));
    let ran = scratch.copy(false, "source", "destination", ("states/serve/corpus", "ds"));
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(scratch.path("states/serve").is_dir());
    assert_eq!(mode(&scratch.path("states/serve")), 0o700);
    assert!(scratch.path("states/serve/corpus").is_dir());
    let ran = scratch.copy(false, "source", "destination2", ("states/a/b/corpus", "ds2"));
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("refused: IO (errno 2)"), "{}", ran.stderr);
    assert!(!scratch.path("states/a").exists());
    assert!(!scratch.path("destination2").exists());
}
