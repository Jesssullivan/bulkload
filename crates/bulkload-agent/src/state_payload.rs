//! The state payload guard (SPEC rule 2, reset 2026-10-20).
//!
//! The operator, 2026-10-20, after every `estate-move-source` item's private
//! state was found holding a full capture bundle and a full repository copy
//! of its live repository, beside a corpus, on neo's drives: "this pattern
//! can never happen again." The reset rule: neither machine ever gets a
//! staging copy (no capture bundle, repository copy, corpus, side root,
//! landing area or overflow directory, on any drive), and bulkload's
//! private state holds ledgers and resume marks only, never payload.
//!
//! This module is the move verbs' hard guard on their state directories:
//!
//! - [`retired`] finds a retired staging format (a capture bundle, a
//!   private repository copy, a capture attempt directory, a corpus, a
//!   landing area). A verb refuses `STATE_FORMAT_RETIRED` at startup,
//!   naming each one, and never reuses or grows it.
//! - [`refuse_staging`] refuses `STATE_PAYLOAD_LEFT` before a step that
//!   would stage payload under a state directory.
//! - [`measure`] counts what a state directory holds, file by file: a file
//!   is a ledger or a resume mark only when this module's table names it
//!   ([`ledger`]); every other file is payload. A verb prints the result on
//!   its state line (`state_payload_bytes=`) at the end of every pass and
//!   refuses `STATE_PAYLOAD_LEFT` when it is not zero. A new ledger is added
//!   to the table, never let through by size.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "estate::payload";

/// The most offending paths a [`Measure`] or [`retired`] keeps by name (it
/// counts every one).
pub const NAMED: usize = 16;

/// The resume-mark bound's fixed part: the move's own ledgers and the
/// transfer and re-seat ledgers' `SQLite` files, whose write-ahead logs
/// grow before a checkpoint.
pub const MARK_BOUND_FIXED: u64 = 8 << 20;

/// The resume-mark bound's part per planned item: its records (outcome,
/// beside, refs-in, journal, carried) and its transfer ledgers.
pub const MARK_BOUND_PER_ITEM: u64 = 64 << 10;

/// The most ledger bytes a state directory may hold for `items` planned
/// items: what the tests hold a pass's state to, beside zero payload.
#[must_use]
pub const fn resume_mark_bound(items: u64) -> u64 {
    MARK_BOUND_FIXED.saturating_add(items.saturating_mul(MARK_BOUND_PER_ITEM))
}

/// What a state directory holds, classified ([`measure`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Measure {
    /// Bytes of every file that is not a ledger or a resume mark.
    pub payload_bytes: u64,
    /// How many such files (a symlink counts, at its own size).
    pub payload_files: u64,
    /// Bytes of the ledgers and resume marks.
    pub ledger_bytes: u64,
    /// The first [`NAMED`] payload files, by path.
    pub payload: Vec<PathBuf>,
}

impl Measure {
    /// The words a verb prints on its state and closure lines.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "state_payload_bytes={} state_payload_files={} state_ledger_bytes={}",
            self.payload_bytes, self.payload_files, self.ledger_bytes
        )
    }

    /// `STATE_PAYLOAD_LEFT` naming the first payload file, when there is
    /// any payload.
    #[must_use]
    pub fn refusal(&self) -> Option<BulkloadRefusal> {
        (self.payload_files > 0).then(|| {
            BulkloadRefusal::StatePayloadLeft(
                self.payload
                    .first()
                    .map(|path| path.as_os_str().as_bytes().to_vec())
                    .unwrap_or_default(),
            )
        })
    }

    fn add(&mut self, other: Self) {
        self.payload_bytes = self.payload_bytes.saturating_add(other.payload_bytes);
        self.payload_files = self.payload_files.saturating_add(other.payload_files);
        self.ledger_bytes = self.ledger_bytes.saturating_add(other.ledger_bytes);
        for path in other.payload {
            if self.payload.len() < NAMED {
                self.payload.push(path);
            }
        }
    }
}

/// The state directories' ledger homes under which only the transfer and
/// re-seat ledgers live: the re-seat's (`reseat`), one store per carry
/// (`carry-files/ITEM/source` and `/dest`), per `pull` (`pull/NAME`) and
/// per `serve` (`serve/NAME`, `serve/carry/ITEM`). (Not led by `pull`: the
/// S2 registry scan reads a list led by a Git subcommand as Git arguments.)
const STORE_HOMES: &[&str] = &["reseat", "carry-files", "pull", "serve", "consolidate"];

/// The pulls' home, where the session pulls also keep their provenance
/// listings.
const PULLS: &str = "pull";

/// The ledgers a transfer or re-seat store keeps (`SQLite`, its write-ahead
/// log, shared memory and rollback journal; the writer and re-seat locks).
const STORE_LEDGERS: &[&str] = &[
    "transfer.sqlite",
    "transfer.sqlite-wal",
    "transfer.sqlite-shm",
    "transfer.sqlite-journal",
    "writer.lock",
    "reseat.sqlite",
    "reseat.sqlite-wal",
    "reseat.sqlite-shm",
    "reseat.sqlite-journal",
    "reseat.lock",
    // #297's consolidation ledger: every value a union did not keep.
    "consolidate.sqlite",
    "consolidate.sqlite-wal",
    "consolidate.sqlite-shm",
    "consolidate.sqlite-journal",
];

/// The move's own ledgers at the state directory's top.
const ROOT_LEDGERS: &[&str] = &["move.lock", "move.ledger", "closure.json", "diff.jsonl"];

/// The source half's plan and pass marks in `carry/`.
const CARRY_LEDGERS: &[&str] = &[
    "plan.postcard",
    "plan.lock",
    "move-source.started",
    "move-source.done",
    "move-source.refusals",
    "move-source.notes",
    // The source's home, for the destination's default home map.
    "move-source.home",
];

/// The suffix of an item's streamed capture in `carry/` (SPEC rule 2
/// reset, B1): its object ids and host-path files, a ledger.
const CARRY_SUFFIXES: &[&str] = &[".snapshot"];

/// The suffixes of an estate item's records in `private/` (each
/// `IDENTITY[-SOURCE[-DIGEST]].SUFFIX`).
const PRIVATE_SUFFIXES: &[&str] = &[
    ".outcome",
    ".beside",
    ".beside-reason",
    ".refs-in",
    ".parents",
    ".exclude-union",
    // The entries digest of the index a landing last wrote (rule 8).
    ".index",
    // An item consolidated into the destination's own repository (B2).
    ".converged",
    ".done",
    ".carried",
    // A streamed fetch's resume marks (B1).
    ".objects",
    // A source-bound empty root claimed before its first object fetch.
    ".first-fetch",
];

/// A ledger's interrupted publication: `estate::write_bytes` writes
/// `STEM.pending-N` before its rename, and keeps one a crash left.
fn pending(name: &str) -> bool {
    name.rsplit_once(".pending-")
        .is_some_and(|(stem, generation)| {
            !stem.is_empty()
                && !generation.is_empty()
                && generation.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Whether the file at `rel` (its components under the state directory)
/// is a ledger or a resume mark. Everything else is payload.
#[must_use]
pub fn ledger(rel: &[&OsStr]) -> bool {
    let Some((name, dirs)) = rel.split_last() else {
        return false;
    };
    let Some(name) = name.to_str() else {
        return false;
    };
    let dirs: Option<Vec<&str>> = dirs.iter().map(|dir| dir.to_str()).collect();
    let Some(dirs) = dirs else {
        return false;
    };
    match dirs.as_slice() {
        [] => ROOT_LEDGERS.contains(&name) || pending(name),
        ["carry"] => {
            CARRY_LEDGERS.contains(&name)
                || CARRY_SUFFIXES
                    .iter()
                    .any(|suffix| name.len() > suffix.len() && name.ends_with(suffix))
                || pending(name)
        }
        ["private"] => {
            name == "estate.lock"
                || PRIVATE_SUFFIXES
                    .iter()
                    .any(|suffix| name.len() > suffix.len() && name.ends_with(suffix))
                || pending(name)
        }
        [home, .., last]
            if STORE_HOMES.contains(home) && *last == crate::transfer_outcome::RECORD_DIR =>
        {
            // A record's lock (`ITEM.lock`), the record (`ITEM.closure`) or
            // its held reasons (`ITEM.held`, 2026-10-22): the transfer
            // outcome's own names, matched exactly.
            name.rsplit_once('.').is_some_and(|(item, extension)| {
                !item.is_empty()
                    && (extension == "lock"
                        || extension == crate::transfer_outcome::RECORD_EXT
                        || extension == crate::transfer_outcome::HELD_EXT)
            }) || pending(name)
        }
        // The session pulls' provenance listings (`pull/claude.listing`,
        // `pull/codex.listing`): the paths each pull carried, by name.
        [home] if *home == PULLS => {
            name.rsplit_once('.')
                .is_some_and(|(stem, ext)| !stem.is_empty() && ext == "listing")
                || pending(name)
        }
        [home, ..] if STORE_HOMES.contains(home) => STORE_LEDGERS.contains(&name),
        _ => false,
    }
}

/// Whether the directory at `rel` is a retired staging format, named and
/// never entered: the corpus (`carry/corpus`), the session landing area
/// (`landing`), a capture attempt directory (any directory directly in
/// `private/`, which held its bundle, index copy and repository copy) or a
/// private repository copy (`repository.git`, anywhere).
fn retired_dir(rel: &[&OsStr]) -> bool {
    match rel {
        [carry, corpus] if *carry == "carry" && *corpus == "corpus" => true,
        [landing] if *landing == "landing" => true,
        [private, _] if *private == "private" => true,
        [.., last] => *last == "repository.git",
        [] => false,
    }
}

/// Whether the file at `rel` is a retired staging format: a capture
/// bundle, anywhere.
fn retired_file(rel: &[&OsStr]) -> bool {
    rel.last()
        .is_some_and(|name| name.as_bytes().ends_with(b".bundle"))
}

/// One entry of a walk: its path, its components under the state
/// directory, and its metadata (`lstat`, never followed).
fn entries(dir: &Path) -> Result<Vec<(PathBuf, fs::Metadata)>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let mut found = Vec::new();
    for entry in read {
        let entry = entry.refuse_at(SITE)?;
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) => found.push((path, meta)),
            // Gone since the listing: nothing to count.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    }
    found.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(found)
}

fn components<'a>(state: &Path, path: &'a Path) -> Vec<&'a OsStr> {
    path.strip_prefix(state)
        .map(|rel| rel.iter().collect())
        .unwrap_or_default()
}

/// Every retired staging format in `state` ([`retired_dir`], a capture
/// bundle), by path, in path order; a retired directory is named and not
/// entered. A missing `state` holds none.
///
/// # Errors
/// IO for an unreadable directory.
pub fn retired(state: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![state.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for (path, meta) in entries(&dir)? {
            let rel = components(state, &path);
            if meta.is_dir() {
                if retired_dir(&rel) {
                    found.push(path);
                } else {
                    pending.push(path);
                }
            } else if retired_file(&rel) {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// What `state` holds, classified file by file ([`ledger`]); every file in
/// a retired directory is payload. A missing `state` holds nothing.
///
/// # Errors
/// IO for an unreadable directory.
pub fn measure(state: &Path) -> Result<Measure> {
    let mut measured = Measure::default();
    let mut pending = vec![(state.to_path_buf(), false)];
    while let Some((dir, inside_retired)) = pending.pop() {
        for (path, meta) in entries(&dir)? {
            let rel = components(state, &path);
            if meta.is_dir() {
                pending.push((path.clone(), inside_retired || retired_dir(&rel)));
                continue;
            }
            let bytes = meta.len();
            if !inside_retired && meta.is_file() && ledger(&rel) {
                measured.ledger_bytes = measured.ledger_bytes.saturating_add(bytes);
            } else {
                measured.add(Measure {
                    payload_bytes: bytes,
                    payload_files: 1,
                    ledger_bytes: 0,
                    payload: vec![path],
                });
            }
        }
    }
    measured.payload.sort();
    Ok(measured)
}

/// [`measure`] of each state directory, summed.
///
/// # Errors
/// As [`measure`].
pub fn measure_all(states: &[&Path]) -> Result<Measure> {
    let mut total = Measure::default();
    for state in states {
        total.add(measure(state)?);
    }
    total.payload.sort();
    Ok(total)
}

/// `STATE_PAYLOAD_LEFT` naming `target` when it is `state` or under it.
///
/// A step that would stage payload there (a corpus, a landing area, a
/// repository copy) is refused before it writes. A `target` outside the
/// state directory is a live path, and passes.
///
/// # Errors
/// As above.
pub fn refuse_staging(state: &Path, target: &Path) -> Result<()> {
    if target.starts_with(state) {
        Err(BulkloadRefusal::StatePayloadLeft(
            target.as_os_str().as_bytes().to_vec(),
        ))
    } else {
        Ok(())
    }
}

/// `STATE_FORMAT_RETIRED` naming the first retired format in any of
/// `states`, after `print` has been given every one; `Ok` when none holds
/// any.
///
/// # Errors
/// As above, and [`retired`]'s.
pub fn refuse_retired(states: &[&Path], print: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    let mut first = None;
    for state in states {
        // An empty retired directory (an older pass's `carry/corpus`) holds
        // no payload: removed, never refused (2026-10-21).
        for path in retired(state)? {
            if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                match fs::remove_dir(&path) {
                    Err(error)
                        if !matches!(
                            error.raw_os_error(),
                            Some(libc::ENOTEMPTY | libc::EEXIST)
                        ) =>
                    {
                        return Err(crate::refuse::io(&error, SITE));
                    }
                    _ => {}
                }
            }
        }
        for path in retired(state)? {
            print(&path)?;
            first.get_or_insert(path);
        }
    }
    first.map_or(Ok(()), |path| {
        Err(BulkloadRefusal::StateFormatRetired(
            path.as_os_str().as_bytes().to_vec(),
        ))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("bulkload-payload-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn rel(text: &str) -> Vec<&OsStr> {
        Path::new(text).iter().collect()
    }

    #[test]
    fn the_table_names_ledgers_and_marks_and_nothing_else() {
        for path in [
            "move.lock",
            "move.ledger",
            "move.pending-0",
            "closure.json",
            "diff.jsonl",
            "carry/plan.postcard",
            "carry/plan.pending-12",
            "carry/move-source.started",
            "carry/move-source.done",
            "carry/move-source.refusals",
            "carry/move-source.notes",
            "private/estate.lock",
            "private/abc.outcome",
            "private/abc.first-fetch",
            "private/abc-def.beside",
            "private/abc-def.beside-reason",
            "private/abc-def.refs-in",
            "private/abc-def.parents",
            "private/abc-def.exclude-union",
            "private/abc-def-0123.done",
            "private/abc-def.carried",
            "pull/claude.listing",
            "pull/codex.listing",
            "pull/corpus/transfer.sqlite",
            "pull/corpus/transfer.sqlite-wal",
            "pull/corpus/writer.lock",
            "pull/corpus/transfer.closure.d/item.closure",
            "pull/corpus/transfer.closure.d/item.lock",
            "pull/corpus/transfer.closure.d/item.held",
            "pull/corpus/transfer.closure.d/item.pending-0",
            "serve/carry/item/transfer.sqlite-shm",
            "carry-files/item/dest/transfer.sqlite",
            "reseat/reseat.sqlite",
            "reseat/reseat.lock",
            "consolidate/consolidate.sqlite",
        ] {
            assert!(ledger(&rel(path)), "{path} is a ledger");
        }
        for path in [
            "capture.bundle",
            "notes.txt",
            "carry/corpus",
            "carry/x.bundle",
            "carry/plan.postcard.bak",
            "private/.outcome",
            "private/abc-0/index",
            "private/abc.pending-x",
            "pull/corpus/data.bin",
            "pull/.listing",
            "pull/claude.listing/x",
            "pull/corpus/transfer.closure.d/item.json",
            "landing/.claude/projects/p/s.jsonl",
            "other/transfer.sqlite",
            "move.ledger/x",
        ] {
            assert!(!ledger(&rel(path)), "{path} is payload");
        }
    }

    // The retired formats are named, each once, and never entered; a
    // ledger-named file inside one is still payload.
    #[test]
    fn retired_formats_are_named_and_counted_as_payload() {
        let scratch = Scratch::new("retired");
        let state = &scratch.0;
        scratch.file("move.ledger", b"l");
        scratch.file("carry/plan.postcard", b"p");
        scratch.file("carry/corpus/a-b.bundle", &[0; 100]);
        scratch.file("carry/corpus/a.capture", b"c");
        scratch.file("private/a.outcome", b"o");
        scratch.file("private/a-b-0/capture.bundle", &[0; 50]);
        scratch.file("private/a-b-0/repository.git/objects/pack/p.pack", &[0; 70]);
        scratch.file("landing/.claude/writer.lock", b"");
        scratch.file("stray.bundle", b"b");
        let found = retired(state).unwrap();
        assert_eq!(
            found,
            vec![
                state.join("carry/corpus"),
                state.join("landing"),
                state.join("private/a-b-0"),
                state.join("stray.bundle"),
            ]
        );
        let measured = measure(state).unwrap();
        assert_eq!(measured.payload_files, 6, "{measured:?}");
        assert_eq!(
            measured.payload_bytes,
            100 + 1 + 50 + 70 + 1,
            "{measured:?}"
        );
        assert_eq!(measured.ledger_bytes, 3, "{measured:?}");
        let mut printed = Vec::new();
        let refused = refuse_retired(&[state], &mut |path| {
            printed.push(path.to_path_buf());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(printed, found);
        assert_eq!(
            refused,
            BulkloadRefusal::StateFormatRetired(
                state.join("carry/corpus").as_os_str().as_bytes().to_vec()
            )
        );
    }

    // A state of ledgers measures zero payload, and passes every check; one
    // stray file is payload, named, and refused.
    #[test]
    fn a_ledger_only_state_passes_and_one_stray_file_is_refused() {
        let scratch = Scratch::new("ledgers");
        let state = &scratch.0;
        scratch.file("move.lock", b"");
        scratch.file("move.ledger", &[1; 40]);
        scratch.file("carry/plan.postcard", &[2; 30]);
        scratch.file("private/x.outcome", &[3; 20]);
        scratch.file("serve/corpus/transfer.sqlite", &[4; 10]);
        fs::create_dir_all(state.join("serve/carry")).unwrap();
        assert!(retired(state).unwrap().is_empty());
        let clean = measure(state).unwrap();
        assert_eq!(
            clean,
            Measure {
                payload_bytes: 0,
                payload_files: 0,
                ledger_bytes: 100,
                payload: Vec::new(),
            }
        );
        assert_eq!(clean.refusal(), None);
        assert!(clean.ledger_bytes <= resume_mark_bound(1));
        assert!(refuse_retired(&[state], &mut |_| Ok(())).is_ok());
        let stray = scratch.file("carry/seat.bin", &[5; 7]);
        std::os::unix::fs::symlink("/etc/passwd", state.join("link")).unwrap();
        let dirty = measure(state).unwrap();
        assert_eq!(dirty.payload_files, 2);
        assert_eq!(dirty.payload, vec![stray.clone(), state.join("link")]);
        assert_eq!(
            dirty.refusal(),
            Some(BulkloadRefusal::StatePayloadLeft(
                stray.as_os_str().as_bytes().to_vec()
            ))
        );
        assert!(dirty
            .words()
            .starts_with(&format!("state_payload_bytes={} ", dirty.payload_bytes)));
    }

    #[test]
    fn staging_under_a_state_is_refused_and_a_live_path_passes() {
        let state = Path::new("/move/state");
        assert_eq!(
            refuse_staging(state, &state.join("carry/corpus")),
            Err(BulkloadRefusal::StatePayloadLeft(
                b"/move/state/carry/corpus".to_vec()
            ))
        );
        assert!(refuse_staging(state, state).is_err());
        assert!(refuse_staging(state, Path::new("/home/me/git/repo")).is_ok());
        assert!(refuse_staging(state, Path::new("/move/statesibling")).is_ok());
    }

    #[test]
    fn a_missing_state_holds_nothing() {
        let missing =
            std::env::temp_dir().join(format!("bulkload-payload-missing-{}", std::process::id()));
        assert!(retired(&missing).unwrap().is_empty());
        assert_eq!(measure(&missing).unwrap(), Measure::default());
    }

    // 2026-10-21: an empty `carry/corpus` an older pass left holds no
    // payload; it is removed, never refused, and a full one still is.
    #[test]
    fn an_empty_retired_directory_is_removed_not_refused() {
        let scratch = Scratch::new("empty-corpus");
        let state = &scratch.0;
        scratch.file("carry/plan.postcard", &[1; 3]);
        fs::create_dir_all(state.join("carry/corpus")).unwrap();
        let mut printed = Vec::new();
        refuse_retired(&[state], &mut |path| {
            printed.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert!(printed.is_empty(), "{printed:?}");
        assert!(!state.join("carry/corpus").exists());
        scratch.file("carry/corpus/item.capture", b"record");
        assert!(refuse_retired(&[state], &mut |_| Ok(())).is_err());
        assert!(state.join("carry/corpus/item.capture").exists());
    }
}
