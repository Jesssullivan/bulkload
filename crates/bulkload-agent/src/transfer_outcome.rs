//! Transfer outcomes as closure-ledger rows (WP3 PR 4, S4, OI-1003-Q1).
//!
//! Until this change a `copy` or `pull` refusal lived only in the session's
//! [`TransferStats`] and on stderr, so S4's closure report covered estate
//! items only. Every `copy` and `pull` now leaves one durable record of its
//! session in its `DEST_STATE`, under [`RECORD_DIR`], one file per transfer
//! (its session identity: verb's source and destination), and
//! `closure-report --transfer DEST_STATE` joins every record there to the
//! same report, the same disposition ledger and the same gate as the
//! planned estate items:
//!
//! - one **session** row: `applied` when the session ran to its end (every
//!   entry it was asked for is carried, reused or a refusal row below), a
//!   typed refusal when the session itself refused, and `unaccounted` when
//!   the record still says the session is running (a crash or a kill cut it
//!   short: `transfer-interrupted`), when no record exists
//!   (`no-transfer-record`), or when the session refused with a bare `IO`
//!   or `FRAME_CODEC`;
//! - one **entry** row per refused path, typed like an estate refusal
//!   (pending review until a disposition disposes it; a bare `IO` or
//!   `FRAME_CODEC` never counts). A `SQLite` integrity failure
//!   (`SQLITE_INTEGRITY_CHECK_FAILED`, or `SQLITE_BACKUP_FAILED` whose
//!   primary code is 11 or 26) carries its ruled default disposition,
//!   `abandon` (OI-1003-Q148, [`crate::transfer::default_disposition`]): it
//!   closes the row unless an operator review says otherwise;
//! - one **covered** row per live `-wal`, `-shm` or `-journal` the source
//!   reported beside a database carried as a snapshot: never carried, and
//!   accounted for by its database's snapshot (#218);
//! - one **consolidated** row per seat `--consolidate` merged into, or
//!   found covered by, a differing live file (operator ruling 2026-10-20),
//!   accounted. An **applied-beside** row of a record written while
//!   `--beside` existed (#280, retired by SPEC rule 3) still decodes and
//!   stays accounted;
//! - one **mapped** row per `--map-prefix SRC_REL=DST_REL` the session
//!   landed an entry through (#280 finding 9): the source path and the
//!   destination path it landed at, accounted like `applied`. Every other
//!   row of such a session names the destination's (mapped) path.
//!
//! [`record_session`] makes the verb's S2 overlap decision before its first
//! write (review finding 1: a refused overlap writes nothing, not even a
//! record), takes the record's own lock (a second session of the same
//! transfer refuses before it writes), and writes `running` before the
//! session (only when `DEST_STATE` already exists: a record is never what
//! creates a state root) and its end after it. A rerun of the same transfer
//! replaces its record: the latest session is the transfer's state, exactly
//! as the latest outcome record is an estate item's. Another transfer
//! sharing the `DEST_STATE` has its own record and never touches this one
//! (review finding 2). A session that does not run to its end keeps the
//! refused and covered rows of the record it replaces, so a session-level
//! refusal never hides an earlier pass's unresolved path refusals; only a
//! session that walked the whole source again (`Completed`) replaces them.
//! A `DEST_STATE` that exists after the session but holds no closing record
//! refuses (`CLOSURE_UNACCOUNTED`), never succeeds silently.
//!
//! A [`Manifest`] binds the set of transfers a run expects to its PLAN
//! (review finding 3): `transfer-expect` lists each, and `closure-report
//! --transfer-manifest` joins exactly those, a missing record or one whose
//! session started before the manifest's `since` being unaccounted. The
//! persisted form of a record is [`MAGIC`] then postcard, decoded strictly.

use std::fmt::Write as _;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::outcome::{strict, Refusal, Unreadable};
use crate::refuse::RefuseAt as _;
use crate::transfer::TransferStats;
use crate::{BulkloadRefusal, Result};

/// The prefix of a transfer closure record (`bulkload.transfer-closure.v1`).
pub const MAGIC: [u8; 8] = [0x00, b'b', b'l', b't', b'x', b'f', b'r', 0x01];

/// The directory inside `DEST_STATE` holding one record per transfer.
///
/// Each is `<session item>.closure`, with its lock `<session item>.lock`. Not
/// `*.outcome`: an estate state directory never mistakes it for an item's.
pub const RECORD_DIR: &str = "transfer.closure.d";

/// A record's file extension.
pub const RECORD_EXT: &str = "closure";

/// The extension of a record's held-reasons sidecar (2026-10-22).
///
/// Each seat `--consolidate` held, with why, beside `<session item>.closure`.
pub const HELD_EXT: &str = "held";

/// The largest held-reasons sidecar read: 16 MiB.
const HELD_LIMIT: u64 = 16 * 1024 * 1024;

/// The first line of a transfer manifest.
pub const MANIFEST_SCHEMA: &str = "bulkload.transfer-manifest.v1";

/// The largest manifest read: 16 MiB.
const MANIFEST_LIMIT: u64 = 16 * 1024 * 1024;

/// The largest record read: 64 MiB (a refusal row is a path and a code).
const RECORD_LIMIT: u64 = 64 * 1024 * 1024;

/// Domain prefixes of a transfer row's identity and refusal instance.
const ITEM_DOMAIN: &[u8] = b"bulkload.transfer-item.v1\0";
const INSTANCE_DOMAIN: &[u8] = b"bulkload.transfer-instance.v1\0";

/// How the session ended, or that it has not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Session {
    /// Written before the session starts; still here means it never ended.
    Running,
    /// The session ran to its end. Its refused paths are entry rows.
    Completed,
    /// The session itself refused.
    Refused(Refusal),
}

/// One path's outcome a session row does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryOutcome {
    /// The path was refused with this refusal and, for
    /// `SQLITE_BACKUP_FAILED`, `SQLite`'s result code.
    Refused {
        refusal: Refusal,
        sqlite_code: Option<i32>,
    },
    /// A live `-wal`, `-shm` or `-journal` beside a database carried as a
    /// snapshot: covered by it, never carried (#218).
    SidecarCovered,
    /// The seat's bytes were published, reused or adopted at `beside`, the
    /// path with the session's `--beside` suffix appended, because the live
    /// path held other bytes bulkload did not write (or a live sidecar).
    /// Accounted: every captured byte is at the destination, beside the live
    /// file. Appended after the earlier variants: a record from before it
    /// still decodes.
    AppliedBeside { beside: Vec<u8> },
    /// `--consolidate` (operator ruling 2026-10-20): the seat's bytes are in
    /// the differing live file, merged by its class's union (`consolidate`)
    /// or already held by it (`covered`). Accounted: nothing was written
    /// beside it. Appended after the earlier variants: a record from
    /// before it still decodes.
    Consolidated { action: String },
    /// `--map-prefix` (#280 finding 9): the entry's `rel_path` is a map's
    /// source path, and every entry at or under it landed at `to` plus the
    /// rest of its path, once. Accounted like `applied`. Appended after the
    /// earlier variants: a record from before it still decodes.
    Mapped { to: Vec<u8> },
}

/// One path row of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Source-relative path, raw bytes.
    pub rel_path: Vec<u8>,
    pub outcome: EntryOutcome,
}

/// The durable record of one transfer session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRecord {
    /// `copy` or `pull`.
    pub verb: String,
    /// The source as the verb named it: `SOURCE`, or `HOST:SOURCE`.
    pub source: String,
    /// The destination root as the verb named it.
    pub destination: PathBuf,
    pub session: Session,
    /// Unix seconds when the session started (its `running` record). A
    /// manifest's `since` makes an older record stale.
    pub started: u64,
    /// Files newly materialized or verified, and reused, by the session.
    pub completed: u64,
    pub reused: u64,
    /// Databases carried as snapshots.
    pub sqlite_snapshots: u64,
    /// Refused and covered paths, sorted and deduplicated.
    pub entries: Vec<Entry>,
}

impl TransferRecord {
    /// The record of a session that has not ended.
    #[must_use]
    pub fn running(verb: &str, source: &str, destination: &Path) -> Self {
        Self {
            verb: verb.to_owned(),
            source: source.to_owned(),
            destination: destination.to_path_buf(),
            session: Session::Running,
            started: now(),
            completed: 0,
            reused: 0,
            sqlite_snapshots: 0,
            entries: Vec::new(),
        }
    }

    /// The record of a session's end: its stats, or its refusal raised at
    /// `site`.
    #[must_use]
    pub fn ended(
        verb: &str,
        source: &str,
        destination: &Path,
        result: &Result<TransferStats>,
        site: &'static str,
    ) -> Self {
        let mut record = Self::running(verb, source, destination);
        match result {
            Err(refusal) => record.session = Session::Refused(Refusal::of(refusal, site)),
            Ok(stats) => {
                record.session = Session::Completed;
                record.completed = stats.completed;
                record.reused = stats.reused;
                record.sqlite_snapshots = stats.sqlite_snapshots.len() as u64;
                for (path, code) in &stats.refusals {
                    record.entries.push(Entry {
                        rel_path: path.clone(),
                        outcome: EntryOutcome::Refused {
                            refusal: entry_refusal(
                                code,
                                site,
                                stats.refusal_errnos.get(path).copied(),
                            ),
                            sqlite_code: stats.refusal_sqlite_codes.get(path).copied(),
                        },
                    });
                }
                for path in &stats.sqlite_sidecars_covered {
                    record.entries.push(Entry {
                        rel_path: path.clone(),
                        outcome: EntryOutcome::SidecarCovered,
                    });
                }
                for (path, action) in &stats.consolidated {
                    record.entries.push(Entry {
                        rel_path: path.clone(),
                        outcome: EntryOutcome::Consolidated {
                            action: action.clone(),
                        },
                    });
                }
                for (source, destination) in &stats.mapped {
                    record.entries.push(Entry {
                        rel_path: source.clone(),
                        outcome: EntryOutcome::Mapped {
                            to: destination.clone(),
                        },
                    });
                }
                record.entries.sort_by(|a, b| {
                    (&a.rel_path, entry_key(&a.outcome)).cmp(&(&b.rel_path, entry_key(&b.outcome)))
                });
                record.entries.dedup();
            }
        }
        record
    }

    /// The persisted bytes.
    ///
    /// # Errors
    /// `FRAME_CODEC` for a record postcard cannot encode (a non-UTF-8 path).
    pub fn encode(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(&(MAGIC, self)).map_err(|_| BulkloadRefusal::FrameCodec)
    }

    /// The identity of the session row: 64 lowercase hex.
    #[must_use]
    pub fn session_item(&self) -> String {
        self.identity(b"session", &[])
    }

    /// The identity of one entry row: 64 lowercase hex, bound to the
    /// transfer (source and destination), the path, and its code (or
    /// `covered`), so a path refused with two codes is two rows.
    #[must_use]
    pub fn entry_item(&self, entry: &Entry) -> String {
        self.identity(entry_key(&entry.outcome).as_bytes(), &entry.rel_path)
    }

    fn identity(&self, kind: &[u8], path: &[u8]) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ITEM_DOMAIN);
        for part in [
            self.source.as_bytes(),
            self.destination.as_os_str().as_encoded_bytes(),
            kind,
            path,
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hasher.finalize().to_hex().to_string()
    }
}

// The code (or `covered`, `applied-beside`) a row's identity and order are
// keyed on.
fn entry_key(outcome: &EntryOutcome) -> String {
    match outcome {
        EntryOutcome::Refused { refusal, .. } => refusal.code().to_owned(),
        EntryOutcome::SidecarCovered => "covered".to_owned(),
        EntryOutcome::AppliedBeside { .. } => "applied-beside".to_owned(),
        EntryOutcome::Consolidated { .. } => "consolidated".to_owned(),
        EntryOutcome::Mapped { .. } => "mapped".to_owned(),
    }
}

// A refusal code the wire carried, as a record: a current code as it is, a
// well-formed retired one as read (it fails closed in the report), and
// anything else, which no writer of this taxonomy sent, as `FRAME_CODEC`:
// untyped, so it never counts (S4).
// S4 (TIN-6592): an `IO` this side raised keeps its errno (`errno` is
// `None` for any other code, and for every refusal the peer sent).
fn entry_refusal(code: &str, site: &'static str, errno: Option<i32>) -> Refusal {
    let errno = errno.filter(|_| code == "IO");
    Refusal::new(code, site, errno)
        .or_else(|| Refusal::decoded(code, site, errno))
        .unwrap_or_else(|| Refusal::of(&BulkloadRefusal::FrameCodec, site))
}

/// The refusal instance of a transfer row.
///
/// A blake3 digest of its identity and its record (the session's end, or
/// the entry's refusal with its site, errno and `SQLite` code). A rerun that refuses identically is the same
/// instance, so its review stands; any other record is another instance.
#[must_use]
pub fn instance(item: &str, record: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(INSTANCE_DOMAIN);
    hasher.update(item.as_bytes());
    hasher.update(&(record.len() as u64).to_le_bytes());
    hasher.update(record);
    hasher.finalize().to_hex().to_string()
}

/// The directory of `DEST_STATE`'s transfer records.
#[must_use]
pub fn record_dir(state: &Path) -> PathBuf {
    state.join(RECORD_DIR)
}

/// Where `DEST_STATE`'s record of the transfer `item` lives.
#[must_use]
pub fn record_path(state: &Path, item: &str) -> PathBuf {
    record_dir(state).join(format!("{item}.{RECORD_EXT}"))
}

/// Unix seconds now (0 before the epoch).
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

// Whether `state` is a directory itself, never through a symlink.
fn is_dir(state: &Path) -> bool {
    std::fs::symlink_metadata(state).is_ok_and(|meta| meta.is_dir())
}

/// A path as a transfer names it.
///
/// Canonical when it resolves; else its canonical parent joined with its
/// name (a destination a first pass will create names the same path as
/// every later pass); else as given.
#[must_use]
pub fn stable(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            std::fs::canonicalize(parent)
                .map_or_else(|_| path.to_path_buf(), |parent| parent.join(name))
        }
        _ => path.to_path_buf(),
    }
}

/// The lock one transfer's record is written under, held for its session.
/// Dropping it releases the lock.
#[derive(Debug)]
pub struct RecordLock(std::fs::File);

impl Drop for RecordLock {
    fn drop(&mut self) {
        // A failed unlock is released when the descriptor closes.
        let _ = crate::io::sys::flock_unlock(&self.0);
    }
}

impl RecordLock {
    /// Take `item`'s record lock in `state`, making [`RECORD_DIR`] when it
    /// is absent (durably: `state` is synced after). Never blocks.
    ///
    /// # Errors
    /// `IO` (`EWOULDBLOCK`) while another session of the same transfer holds
    /// it; `PATH_ESCAPES_ROOT` for a [`RECORD_DIR`] that is not a directory
    /// (a symlink); the creation's refusals.
    pub fn take(state: &Path, item: &str) -> Result<Self> {
        use crate::counters::CountedSync as _;
        use std::os::unix::fs::DirBuilderExt as _;
        let dir = record_dir(state);
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => std::fs::File::open(state)
                .refuse_at("transfer_outcome::lock")?
                .sync_dir_counted()
                .refuse_at("transfer_outcome::lock")?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(crate::refuse::io(&error, "transfer_outcome::lock")),
        }
        if !is_dir(&dir) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(format!("{item}.lock")))
            .refuse_at("transfer_outcome::lock")?;
        crate::io::sys::flock_exclusive(&lock).refuse_at("transfer_outcome::lock")?;
        Ok(Self(lock))
    }
}

/// Write `record` durably into `state`'s [`RECORD_DIR`], when `state`
/// exists as a directory. The caller holds the record's [`RecordLock`].
///
/// Returns whether it was written. A missing `DEST_STATE` is never created
/// here (S2: a state root is made only by the store, after its overlap
/// checks).
///
/// # Errors
/// The write's refusals.
pub fn write(state: &Path, record: &TransferRecord) -> Result<bool> {
    if !is_dir(state) {
        return Ok(false);
    }
    let dir = record_dir(state);
    if !dir.exists() {
        drop(RecordLock::take(state, &record.session_item())?);
    }
    crate::estate::write_bytes(
        &record_path(state, &record.session_item()),
        &record.encode()?,
    )?;
    Ok(true)
}

/// Where `DEST_STATE`'s held-reasons sidecar of the transfer `item` lives.
#[must_use]
pub fn held_path(state: &Path, item: &str) -> PathBuf {
    record_dir(state).join(format!("{item}.{HELD_EXT}"))
}

// The held seats of a session that ran to its end, with why (2026-10-22):
// the closure lists them on the item they belong to. A session that held
// nothing leaves no sidecar (an earlier one is removed).
fn write_held(state: &Path, item: &str, held: &[(Vec<u8>, &'static str)]) -> Result<()> {
    const SITE: &str = "transfer_outcome::write_held";
    let path = held_path(state, item);
    if held.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(crate::refuse::io(&error, SITE))
            }
            _ => Ok(()),
        };
    }
    let rows: Vec<(&[u8], &str)> = held
        .iter()
        .map(|(path, why)| (path.as_slice(), *why))
        .collect();
    let bytes = postcard::to_allocvec(&rows).map_err(|_| BulkloadRefusal::FrameCodec)?;
    crate::estate::write_bytes(&path, &bytes)
}

/// The held seats `DEST_STATE` recorded for the transfer `item`.
///
/// By path, with why: empty when there is no sidecar or it does not decode
/// (a reason is a note on a typed row, never the row).
#[must_use]
pub fn held_reasons(state: &Path, item: &str) -> std::collections::BTreeMap<Vec<u8>, String> {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(held_path(state, item))
    else {
        return std::collections::BTreeMap::new();
    };
    let mut bytes = Vec::new();
    if file.take(HELD_LIMIT).read_to_end(&mut bytes).is_err() {
        return std::collections::BTreeMap::new();
    }
    postcard::from_bytes::<Vec<(Vec<u8>, String)>>(&bytes)
        .map(|rows| rows.into_iter().collect())
        .unwrap_or_default()
}

// The refused and covered rows a session that does not run to its end
// keeps: the record it replaces. An unreadable record refuses: its rows are
// unknown, and replacing it would lose them (CLOSURE_UNACCOUNTED).
fn carried(state: &Path, item: &str) -> Result<Vec<Entry>> {
    match read(state, item) {
        None => Ok(Vec::new()),
        Some(Ok(record)) => Ok(record.entries),
        Some(Err(_)) => Err(BulkloadRefusal::ClosureUnaccounted),
    }
}

/// Run one transfer session under its closure record.
///
/// In order: `roots_apart`, the verb's read-only S2 overlap decision
/// ([`crate::transfer::copy_roots_apart`],
/// [`crate::transfer::receive_roots_apart`]), before anything is written;
/// then, when `DEST_STATE` exists, the record's [`RecordLock`] and a
/// `running` record that keeps the replaced record's rows; the session;
/// and its end, whatever it was (a `DEST_STATE` the session's store made is
/// locked and recorded then). A session that did not complete keeps the
/// replaced record's refused and covered rows.
///
/// # Errors
/// The overlap refusal (nothing written); the lock's refusal while another
/// session of this transfer runs (nothing written); `CLOSURE_UNACCOUNTED`
/// for an unreadable record it would replace (nothing written); the
/// session's own refusal; or, for a session that ended well, the refusal of
/// the record write that should have closed it, and `CLOSURE_UNACCOUNTED`
/// when `DEST_STATE` exists after the session yet holds no closing record
/// (a session whose end is not recorded is not complete).
pub fn record_session(
    state: &Path,
    verb: &str,
    source: &str,
    destination: &Path,
    site: &'static str,
    roots_apart: impl FnOnce() -> Result<()>,
    session: impl FnOnce() -> Result<TransferStats>,
) -> Result<TransferStats> {
    roots_apart()?;
    let mut running = TransferRecord::running(verb, source, destination);
    let item = running.session_item();
    let mut lock = None;
    let mut earlier = Vec::new();
    if is_dir(state) {
        lock = Some(RecordLock::take(state, &item)?);
        earlier = carried(state, &item)?;
        running.entries.clone_from(&earlier);
        write(state, &running)?;
    }
    let result = session();
    let written = (|| {
        if lock.is_none() && is_dir(state) {
            // The session's store made DEST_STATE: no earlier record.
            lock = Some(RecordLock::take(state, &item)?);
        }
        let mut ended = TransferRecord::ended(verb, source, destination, &result, site);
        ended.started = running.started;
        if ended.session != Session::Completed {
            ended.entries = earlier;
        }
        let written = lock.is_some() && write(state, &ended)?;
        // Why each held seat was held: the record keeps the code only.
        if written {
            if let Ok(stats) = &result {
                write_held(state, &item, &stats.held)?;
            }
        }
        // Review finding 3: a DEST_STATE that is there (a symlink, say)
        // but took no closing record never passes as recorded.
        if !written && std::fs::symlink_metadata(state).is_ok() {
            return Err(BulkloadRefusal::ClosureUnaccounted);
        }
        Ok(())
    })();
    drop(lock);
    match (result, written) {
        (Ok(ended), Ok(())) => Ok(ended),
        (Ok(_), Err(refusal)) | (Err(refusal), _) => Err(refusal),
    }
}

/// The transfer items `DEST_STATE` holds records for, sorted: each record
/// file's stem, and its path.
#[must_use]
pub fn records(state: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(record_dir(state)) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == RECORD_EXT))
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.to_owned();
            Some((stem, path))
        })
        .collect();
    out.sort();
    out
}

/// Read `DEST_STATE`'s record of the transfer `item`: `None` when there is
/// none.
#[must_use]
pub fn read(state: &Path, item: &str) -> Option<std::result::Result<TransferRecord, Unreadable>> {
    read_file(&record_path(state, item))
}

/// Read one record file: `None` when there is none.
#[must_use]
pub fn read_file(path: &Path) -> Option<std::result::Result<TransferRecord, Unreadable>> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Err(Unreadable::Codec)),
    };
    let mut bytes = Vec::new();
    if file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > RECORD_LIMIT
    {
        return Some(Err(Unreadable::Codec));
    }
    Some(decode(&bytes))
}

/// One transfer a run expects (a [`Manifest`] line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// `copy` or `pull`.
    pub verb: String,
    /// The source as the record names it ([`copy_source`], [`pull_source`]).
    pub source: String,
    /// The destination as the record names it ([`stable`]).
    pub destination: PathBuf,
    /// The `DEST_STATE` the record is in ([`stable`]).
    pub state: PathBuf,
}

impl Expected {
    /// The transfer's session identity: its record's file stem.
    #[must_use]
    pub fn item(&self) -> String {
        TransferRecord::running(&self.verb, &self.source, &self.destination).session_item()
    }
}

/// How `copy` names its source in a record: canonical (S4 rows keyed so a
/// rerun spelled another way names the same rows).
#[must_use]
pub fn copy_source(source: &Path) -> String {
    stable(source).to_string_lossy().into_owned()
}

/// How `pull` names its source in a record: `HOST:SOURCE`, as given.
#[must_use]
pub fn pull_source(host: &std::ffi::OsStr, source: &Path) -> String {
    format!("{}:{}", host.to_string_lossy(), source.to_string_lossy())
}

/// The transfers one run expects, bound to its PLAN (review finding 3).
///
/// `closure-report --transfer-manifest` joins exactly these, so a transfer
/// left out of the report, missing its record, or holding a record older
/// than `since` cannot pass the gate.
///
/// Text, one field per tab: [`MANIFEST_SCHEMA`], `plan_digest\tHEX`,
/// `since\tUNIX_SECONDS`, then one `transfer\tVERB\tSOURCE\tDEST\tDEST_STATE`
/// line per transfer. Read strictly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Where it was read from.
    pub path: PathBuf,
    /// The digest of the PLAN it is bound to
    /// ([`crate::disposition::plan_digest`]).
    pub plan_digest: String,
    /// Unix seconds: a record whose session started earlier is stale.
    pub since: u64,
    pub transfers: Vec<Expected>,
}

impl Manifest {
    /// Read `path` and bind it to `plan`.
    ///
    /// # Errors
    /// `RECEIPT_BINDING_INVALID` for a manifest bound to another plan (or
    /// other plan bytes); `SCHEMA_MISMATCH` and `FIELD_DOMAIN_VIOLATION` for
    /// one that does not parse strictly or lists a transfer twice; the
    /// reads' refusals.
    pub fn read(path: &Path, plan: &Path) -> Result<Self> {
        let digest = crate::disposition::plan_digest(plan)?;
        let manifest = Self::read_unbound(path)?;
        if manifest.plan_digest != digest {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Ok(manifest)
    }

    fn read_unbound(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .refuse_at("transfer_outcome::manifest")?;
        let mut bytes = Vec::new();
        file.take(MANIFEST_LIMIT + 1)
            .read_to_end(&mut bytes)
            .refuse_at("transfer_outcome::manifest")?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| BulkloadRefusal::SchemaMismatch)?;
        Self::parse(path, text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self> {
        let body = text
            .strip_suffix('\n')
            .ok_or(BulkloadRefusal::SchemaMismatch)?;
        let mut lines = body.split('\n');
        if lines.next() != Some(MANIFEST_SCHEMA) {
            return Err(BulkloadRefusal::SchemaMismatch);
        }
        let plan_digest = match lines
            .next()
            .and_then(|line| line.strip_prefix("plan_digest\t"))
        {
            Some(hex)
                if hex.len() == 64
                    && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                hex.to_owned()
            }
            _ => return Err(BulkloadRefusal::SchemaMismatch),
        };
        let since = lines
            .next()
            .and_then(|line| line.strip_prefix("since\t"))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok())
            .ok_or(BulkloadRefusal::SchemaMismatch)?;
        let mut transfers: Vec<Expected> = Vec::new();
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            let ["transfer", verb, source, destination, state] = fields.as_slice() else {
                return Err(BulkloadRefusal::SchemaMismatch);
            };
            if !matches!(*verb, "copy" | "pull")
                || [source, destination, state].iter().any(|f| f.is_empty())
            {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            let expected = Expected {
                verb: (*verb).to_owned(),
                source: (*source).to_owned(),
                destination: PathBuf::from(destination),
                state: PathBuf::from(state),
            };
            if transfers
                .iter()
                .any(|other| other.item() == expected.item())
            {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            transfers.push(expected);
        }
        Ok(Self {
            path: path.to_path_buf(),
            plan_digest,
            since,
            transfers,
        })
    }

    /// The manifest's bytes.
    ///
    /// # Errors
    /// `PATH_NOT_PORTABLE` for a field that is not UTF-8 or holds a tab or
    /// a newline.
    pub fn encode(&self) -> Result<String> {
        let field = |text: &str| -> Result<()> {
            if text.is_empty() || text.contains(['\t', '\n']) {
                return Err(BulkloadRefusal::PathNotPortable);
            }
            Ok(())
        };
        let mut out = format!(
            "{MANIFEST_SCHEMA}\nplan_digest\t{}\nsince\t{}\n",
            self.plan_digest, self.since
        );
        for expected in &self.transfers {
            let destination = expected
                .destination
                .to_str()
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let state = expected
                .state
                .to_str()
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            for text in [expected.verb.as_str(), &expected.source, destination, state] {
                field(text)?;
            }
            let _ = writeln!(
                out,
                "transfer\t{}\t{}\t{destination}\t{state}",
                expected.verb, expected.source
            );
        }
        Ok(out)
    }
}

/// Add `expected` to the manifest at `path` bound to `plan`.
///
/// The manifest is created (with `since` now: the run starts) when it is
/// absent. Listing a transfer
/// again is a no-op; the same transfer with another `DEST_STATE` refuses.
///
/// # Errors
/// As [`Manifest::read`] and [`Manifest::encode`];
/// `FIELD_DOMAIN_VIOLATION` for the same transfer in another `DEST_STATE`;
/// the write's refusals.
pub fn expect(path: &Path, plan: &Path, expected: Expected) -> Result<Manifest> {
    let mut manifest = if std::fs::symlink_metadata(path).is_ok() {
        Manifest::read(path, plan)?
    } else {
        Manifest {
            path: path.to_path_buf(),
            plan_digest: crate::disposition::plan_digest(plan)?,
            since: now(),
            transfers: Vec::new(),
        }
    };
    match manifest
        .transfers
        .iter()
        .find(|other| other.item() == expected.item())
    {
        Some(other) if *other == expected => return Ok(manifest),
        Some(_) => return Err(BulkloadRefusal::FieldDomainViolation),
        None => manifest.transfers.push(expected),
    }
    crate::estate::write_bytes(path, manifest.encode()?.as_bytes())?;
    Ok(manifest)
}

/// Decode a persisted record, strictly.
///
/// # Errors
/// [`Unreadable::Codec`] for anything but exactly one record after the magic.
pub fn decode(bytes: &[u8]) -> std::result::Result<TransferRecord, Unreadable> {
    bytes
        .strip_prefix(&MAGIC)
        .and_then(strict::<TransferRecord>)
        .ok_or(Unreadable::Codec)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn stats() -> TransferStats {
        let mut stats = TransferStats {
            completed: 3,
            reused: 1,
            ..TransferStats::default()
        };
        stats
            .refusals
            .push((b"busy.db".to_vec(), "SQLITE_BACKUP_FAILED".to_owned()));
        stats.refusal_sqlite_codes.insert(b"busy.db".to_vec(), 11);
        stats
            .refusals
            .push((b"kept".to_vec(), "DESTINATION_OCCUPIED".to_owned()));
        stats.sqlite_snapshots.push(b"state.db".to_vec());
        stats.sqlite_sidecars_covered.push(b"state.db-wal".to_vec());
        stats
    }

    /// S4 (TIN-6592): an `IO` this side raised keeps its errno in the
    /// record; a refusal the peer sent (no errno on the wire) and any
    /// other code record none. Still untyped: the row stays unaccounted.
    #[test]
    fn a_local_io_entry_keeps_its_errno_and_nothing_else_gains_one() {
        let mut stats = stats();
        stats.refusals.push((b"local".to_vec(), "IO".to_owned()));
        stats.refusal_errnos.insert(b"local".to_vec(), libc::EACCES);
        stats.refusals.push((b"peer".to_vec(), "IO".to_owned()));
        // A stray errno beside a typed code is never recorded.
        stats.refusal_errnos.insert(b"kept".to_vec(), libc::EACCES);
        let record = TransferRecord::ended(
            "pull",
            "neo:/Users/jess",
            Path::new("/srv/dest"),
            &Ok(stats),
            "transfer::pull",
        );
        let errno = |path: &[u8]| {
            record
                .entries
                .iter()
                .find_map(|entry| match &entry.outcome {
                    EntryOutcome::Refused { refusal, .. } if entry.rel_path == path => {
                        Some(refusal.errno())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(errno(b"local"), Some(libc::EACCES));
        assert_eq!(errno(b"peer"), None);
        assert_eq!(errno(b"kept"), None);
        let bytes = record.encode().unwrap();
        assert_eq!(decode(&bytes).unwrap(), record);
    }

    #[test]
    fn a_record_round_trips_strictly_and_keys_rows_stably() {
        let record = TransferRecord::ended(
            "pull",
            "neo:/Users/jess",
            Path::new("/srv/dest"),
            &Ok(stats()),
            "transfer::pull",
        );
        assert_eq!(record.session, Session::Completed);
        assert_eq!(record.entries.len(), 3);
        let bytes = record.encode().unwrap();
        assert_eq!(decode(&bytes).unwrap(), record);
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode(&long), Err(Unreadable::Codec));
        assert_eq!(decode(&bytes[8..]), Err(Unreadable::Codec));
        // Identities: 64 hex, distinct per row, the same on a rerun.
        let again = TransferRecord::ended(
            "pull",
            "neo:/Users/jess",
            Path::new("/srv/dest"),
            &Ok(stats()),
            "transfer::pull",
        );
        let items: Vec<String> = record
            .entries
            .iter()
            .map(|e| record.entry_item(e))
            .collect();
        let again: Vec<String> = again.entries.iter().map(|e| again.entry_item(e)).collect();
        assert_eq!(items, again);
        assert!(items.iter().all(|item| item.len() == 64));
        let unique: std::collections::BTreeSet<_> = items.iter().collect();
        assert_eq!(unique.len(), items.len());
        assert!(!items.contains(&record.session_item()));
    }

    #[test]
    fn a_refused_session_and_a_malformed_wire_code_are_recorded_typed_or_untyped() {
        let refused = TransferRecord::ended(
            "copy",
            "/src",
            Path::new("/dst"),
            &Err(BulkloadRefusal::SnapshotRootsOverlap),
            "transfer::copy",
        );
        assert_eq!(
            refused.session,
            Session::Refused(
                Refusal::new("SNAPSHOT_ROOTS_OVERLAP", "transfer::copy", None).unwrap()
            )
        );
        let mut stats = TransferStats::default();
        stats
            .refusals
            .push((b"x".to_vec(), "not a code".to_owned()));
        let record = TransferRecord::ended(
            "copy",
            "/src",
            Path::new("/dst"),
            &Ok(stats),
            "transfer::copy",
        );
        let EntryOutcome::Refused { refusal, .. } = &record.entries[0].outcome else {
            panic!("{record:?}");
        };
        assert_eq!(refusal.code(), "FRAME_CODEC");
        assert!(!refusal.is_typed());
    }

    fn session(
        state: &Path,
        roots: Result<()>,
        body: impl FnOnce() -> Result<TransferStats>,
    ) -> Result<TransferStats> {
        record_session(
            state,
            "copy",
            "/src",
            Path::new("/dst"),
            "transfer::copy",
            || roots,
            body,
        )
    }

    fn item() -> String {
        TransferRecord::running("copy", "/src", Path::new("/dst")).session_item()
    }

    #[test]
    fn a_session_is_recorded_running_then_ended_and_never_creates_its_state() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        // No DEST_STATE: nothing is written, and none is created.
        let ended = session(&state, Ok(()), || Ok(TransferStats::default())).unwrap();
        assert_eq!(ended.completed, 0);
        assert!(!state.exists());
        std::fs::create_dir(&state).unwrap();
        // While the session runs, the record says so.
        let refusal = session(&state, Ok(()), || {
            assert_eq!(
                read(&state, &item()).unwrap().unwrap().session,
                Session::Running
            );
            Err(BulkloadRefusal::Io(Some(5)))
        })
        .unwrap_err();
        assert_eq!(refusal, BulkloadRefusal::Io(Some(5)));
        let Session::Refused(recorded) = read(&state, &item()).unwrap().unwrap().session else {
            panic!("not refused");
        };
        assert_eq!((recorded.code(), recorded.errno()), ("IO", Some(5)));
        // A rerun replaces it.
        session(&state, Ok(()), || Ok(ended)).unwrap();
        assert_eq!(
            read(&state, &item()).unwrap().unwrap().session,
            Session::Completed
        );
        // An unreadable record is never replaced: its rows are unknown.
        std::fs::write(record_path(&state, &item()), b"garbage").unwrap();
        assert_eq!(read(&state, &item()), Some(Err(Unreadable::Codec)));
        assert_eq!(
            session(&state, Ok(()), || Ok(TransferStats::default())).unwrap_err(),
            BulkloadRefusal::ClosureUnaccounted
        );
        assert_eq!(
            std::fs::read(record_path(&state, &item())).unwrap(),
            b"garbage"
        );
    }

    // Review finding 1 (S2): the overlap decision comes before any write.
    #[test]
    fn an_overlap_refusal_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let mut ran = false;
        let refusal = session(&state, Err(BulkloadRefusal::SnapshotRootsOverlap), || {
            ran = true;
            Ok(TransferStats::default())
        })
        .unwrap_err();
        assert_eq!(refusal, BulkloadRefusal::SnapshotRootsOverlap);
        assert!(!ran);
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 0);
    }

    // Review finding 2: a session that does not run to its end keeps the
    // refused rows of the record it replaces; one that completes replaces
    // them; and a second session of the same transfer refuses before it
    // writes while the first holds the record.
    #[test]
    fn a_session_that_does_not_complete_keeps_the_rows_it_replaces() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        session(&state, Ok(()), || Ok(stats())).unwrap();
        let kept = read(&state, &item()).unwrap().unwrap().entries;
        assert_eq!(kept.len(), 3);
        session(&state, Ok(()), || {
            // `running` already carries them: a crash keeps them too.
            assert_eq!(read(&state, &item()).unwrap().unwrap().entries, kept);
            Err(BulkloadRefusal::SourceChangedAfterSnapshot)
        })
        .unwrap_err();
        let record = read(&state, &item()).unwrap().unwrap();
        assert!(matches!(record.session, Session::Refused(_)));
        assert_eq!(record.entries, kept);
        // A completed pass walked everything again: its rows replace them.
        session(&state, Ok(()), || Ok(TransferStats::default())).unwrap();
        assert!(read(&state, &item()).unwrap().unwrap().entries.is_empty());
        // Another session of the same transfer, while this lock is held.
        let held = RecordLock::take(&state, &item()).unwrap();
        let before = std::fs::read(record_path(&state, &item())).unwrap();
        let mut ran = false;
        assert!(matches!(
            session(&state, Ok(()), || {
                ran = true;
                Ok(stats())
            }),
            Err(BulkloadRefusal::Io(_))
        ));
        assert!(!ran);
        assert_eq!(std::fs::read(record_path(&state, &item())).unwrap(), before);
        drop(held);
    }

    // Review finding 3: a DEST_STATE that exists but takes no closing
    // record (here a symlink) never reports a recorded success.
    #[test]
    fn a_dest_state_that_takes_no_closing_record_refuses() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("real")).unwrap();
        let state = root.path().join("state");
        std::os::unix::fs::symlink(root.path().join("real"), &state).unwrap();
        assert_eq!(
            session(&state, Ok(()), || Ok(TransferStats::default())).unwrap_err(),
            BulkloadRefusal::ClosureUnaccounted
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("real")).unwrap().count(),
            0
        );
    }

    #[test]
    fn a_manifest_round_trips_strictly() {
        let manifest = Manifest {
            path: PathBuf::from("/m"),
            plan_digest: "a".repeat(64),
            since: 1_760_000_000,
            transfers: vec![
                Expected {
                    verb: "pull".into(),
                    source: "neo:/Users/jess".into(),
                    destination: PathBuf::from("/srv/dest"),
                    state: PathBuf::from("/srv/state"),
                },
                Expected {
                    verb: "copy".into(),
                    source: "/a".into(),
                    destination: PathBuf::from("/b"),
                    state: PathBuf::from("/srv/state"),
                },
            ],
        };
        let text = manifest.encode().unwrap();
        assert_eq!(Manifest::parse(Path::new("/m"), &text).unwrap(), manifest);
        assert!(Manifest::parse(Path::new("/m"), text.trim_end()).is_err());
        for bad in [
            text.replace("since\t1760000000", "since\t-1"),
            text.replace("plan_digest\t", "plan_digest\tX"),
            text.replace("transfer\tpull", "transfer\tsync"),
            format!("{text}{}", text.lines().nth(3).unwrap()),
            text.replacen("transfer\t", "transfer\t\t", 1),
        ] {
            assert!(
                Manifest::parse(Path::new("/m"), &format!("{}\n", bad.trim_end())).is_err(),
                "{bad}"
            );
        }
        let mut tabbed = manifest;
        tabbed.transfers[0].source = "neo:/a\tb".into();
        assert_eq!(tabbed.encode(), Err(BulkloadRefusal::PathNotPortable));
    }
}
