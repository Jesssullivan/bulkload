//! `pull-retire-source`: remove a homes pull's source file once the
//! destination provably holds its content.
//!
//! Operator ruling 2026-10-23 (relayed by the lane coordinator, verbatim):
//! "once migrated, we should clera the duplicates (sting can / shoudl be
//! the only copy for thes codex objects)".
//!
//! The destination side ([`plan`]) reads a pull's state: every closure
//! record of the state directory, its transfer store and the consolidation
//! ledger, all read only. A seat is a candidate only when its recorded
//! outcome proves the destination holds its content:
//! - adopted or landed equal (an `outputs` row), or consolidated `adopt`,
//!   `extend` or `covered` (a link to exactly its bytes), or `extended`
//!   (operator ruling 2026-10-23, "Yes, take neo's full file"): the proof is
//!   the destination's BLAKE3 now, which the source's file must equal;
//! - `covered` by the linked file's events (#325) or by the archived
//!   original's ids (operator ruling 2026-10-23, "Archive original covers
//!   it"): the proof is the ledger's outcome against the linked file's
//!   identity, which must still be that file's.
//!
//! Never a `SQLite` store or its sidecars, never anything but a regular
//! file. Each candidate carries the source identities (device, inode,
//! size, modification and change times) its outcome was recorded against,
//! from the store's row keys.
//!
//! The source side ([`answer`], the remote `retire-source` over the pull's
//! ssh builder) re-verifies each file at removal time and removes only that
//! file: its path is opened beneath the source root following no link; it
//! must be a regular file whose identity is one recorded; no process may
//! hold it open (Linux: `/proc`; Darwin: libproc, read in this process;
//! elsewhere, or when that cannot be said, it is skipped); its BLAKE3 must
//! equal the destination's when that is the proof; and its identity is
//! checked again just before the `unlinkat`. Anything else skips it with a typed reason and leaves it.
//! Nothing on the destination or in an archive is ever written; the run's
//! receipt lines are appended to `DEST_STATE/retire-source.receipts`.
//! `--dry-run` answers every check and removes nothing.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result, RowSchema};

const SITE: &str = "retire";

/// Each checked candidate holds its parent directory open until its final
/// identity check. Use at most a quarter of the soft descriptor limit,
/// leaving the rest for existing handles, the holder probe and traversal.
fn check_batch(soft_limit: u64) -> usize {
    usize::try_from((soft_limit / 4).clamp(1, 200)).unwrap_or(16)
}

/// A file's identity: device, inode, size, modification and change times
/// (nanoseconds).
pub type Identity = (u64, u64, u64, i128, i128);

/// What the destination proves of a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Proof {
    /// The destination's content now has this BLAKE3; the source's must.
    Blake3([u8; 32]),
    /// The recorded outcome (#325, ruling 2) still holds against the
    /// linked file it was judged against.
    Recorded(String),
}

impl Proof {
    fn kind(&self) -> String {
        match self {
            Self::Blake3(digest) => format!("blake3:{}", hex(digest)),
            Self::Recorded(outcome) => format!("recorded:{outcome}"),
        }
    }
}

/// One candidate: its path under the source root, and each identity its
/// outcome was recorded against with that outcome's proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Want {
    pub rel: Vec<u8>,
    pub allowed: Vec<(Identity, Proof)>,
}

/// The source side's question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    /// The pull's source root, as the source names it.
    pub root: Vec<u8>,
    pub dry_run: bool,
    pub files: Vec<Want>,
}

/// What became of one candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Removed,
    WouldRemove,
    Skipped(String),
}

/// One candidate's answer: its identity as found and the proof it met.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub rel: Vec<u8>,
    pub identity: Option<Identity>,
    pub proof: Option<String>,
    pub verdict: Verdict,
}

/// The destination side's plan: the source to ask, its question, and the
/// seats skipped here (with why).
#[derive(Debug, Clone)]
pub struct Plan {
    /// `-` for a local source.
    pub host: String,
    pub ask: Ask,
    pub skipped: Vec<Done>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Whether `rel` is a plain relative path: no `..`, `.`, empty part or
/// leading `/`.
fn contained(rel: &[u8]) -> bool {
    !rel.is_empty()
        && !rel.starts_with(b"/")
        && rel
            .split(|byte| *byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

fn path_of(root: &Path, rel: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    root.join(std::ffi::OsStr::from_bytes(rel))
}

/// The outcomes whose proof is the destination's content, by BLAKE3.
const BY_CONTENT: [&str; 4] = [
    "adopt",
    "extend",
    "covered",
    crate::consolidate::rollout::EXTENDED,
];
/// The outcomes whose proof is the ledger's record against the linked file.
const BY_RECORD: [&str; 2] = [
    crate::consolidate::rollout::COVERED,
    crate::consolidate::rollout::REWRITE_COVERED,
];

/// Take `DEST_STATE/writer.lock` without waiting: `MOVE_STATE_BUSY` while a
/// pass holds it. Released when the returned file closes.
fn idle(state: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(state.join("writer.lock"))
        .refuse_at(SITE)?;
    // SAFETY: `lock` is an open descriptor this function owns for the call;
    // `flock` takes no pointer.
    let taken = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if taken != 0 {
        use std::os::unix::ffi::OsStrExt as _;
        return Err(BulkloadRefusal::MoveStateBusy(
            state.as_os_str().as_bytes().to_vec(),
        ));
    }
    Ok(lock)
}

/// Each path's recorded rows: the row, its key, and whether it is an
/// `outputs` row (landed or adopted) rather than a `consolidated` one.
type Recorded = BTreeMap<Vec<u8>, Vec<(RowSchema, Vec<u8>, bool)>>;

/// The rows the store recorded, by path: each row's source identity and
/// its key, from the `consolidated` and `outputs` tables (row keys are
/// postcard `(authority, RowSchema)`; a `SQLite` snapshot seat's key has
/// bytes after its row and is left out).
fn recorded_rows(state: &Path) -> Result<Recorded> {
    let conn = rusqlite::Connection::open_with_flags(
        state.join("transfer.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    let mut rows = Recorded::new();
    for (table, output) in [("consolidated", false), ("outputs", true)] {
        let mut statement = conn
            .prepare(&format!("SELECT key FROM {table}"))
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        let keys = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
        for key in keys {
            let key = key.map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
            let Ok(((_, row), rest)) = postcard::take_from_bytes::<(Vec<u8>, RowSchema)>(&key)
            else {
                continue;
            };
            if !rest.is_empty() {
                continue;
            }
            rows.entry(row.rel_path.clone())
                .or_default()
                .push((row, key.clone(), output));
        }
    }
    Ok(rows)
}

/// The BLAKE3 of the content at `path` (through a link), streamed; `None`
/// when it is not a regular file there.
fn content_blake3(path: &Path) -> Option<[u8; 32]> {
    use std::io::Read as _;
    let found = std::fs::metadata(path).ok()?;
    if !found.is_file() {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read)?);
    }
    Some(*hasher.finalize().as_bytes())
}

/// The destination's proof for the seat at `rel` recorded `action`, as it
/// stands now: `Err` names why it no longer holds.
/// The outcome the consolidation ledger recorded for the seat `key` at
/// `rel`, and the linked file's identity digest it was judged against
/// (`rollout_link_outcome`); `None` when there is none.
fn recorded_outcome(
    ledger: &rusqlite::Connection,
    rel: &[u8],
    key: &[u8],
) -> std::result::Result<Option<(String, [u8; 32])>, String> {
    use rusqlite::OptionalExtension as _;
    let found = ledger
        .query_row(
            "SELECT outcome, linked FROM rollout_link_outcome WHERE file = ?1 AND seat_key = ?2",
            rusqlite::params![rel, key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("the ledger cannot be read: {error}"))?;
    Ok(found.and_then(|(outcome, linked)| {
        <[u8; 32]>::try_from(linked.as_slice())
            .ok()
            .map(|linked| (outcome, linked))
    }))
}

fn proof_now(
    destination: &Path,
    ledger: Option<&rusqlite::Connection>,
    rel: &[u8],
    action: &str,
    key: &[u8],
) -> std::result::Result<Proof, String> {
    let live = path_of(destination, rel);
    if BY_RECORD.contains(&action) {
        let ledger = ledger.ok_or("no consolidation ledger was named (--ledger)")?;
        let (outcome, linked) = recorded_outcome(ledger, rel, key)?
            .ok_or("the ledger holds no proof for this outcome")?;
        if outcome != action {
            return Err(format!("the ledger's outcome is {outcome}"));
        }
        let is_link =
            std::fs::symlink_metadata(&live).is_ok_and(|found| found.file_type().is_symlink());
        let target = std::fs::canonicalize(&live).ok();
        if !is_link || target.as_deref().and_then(crate::transfer::linked_identity) != Some(linked)
        {
            return Err("the destination link or its linked file changed".to_owned());
        }
        return Ok(Proof::Recorded(outcome));
    }
    content_blake3(&live)
        .map(Proof::Blake3)
        .ok_or_else(|| "the destination holds no regular file there".to_owned())
}

/// The candidates of the pull whose state is `state` (see the module).
///
/// Its consolidation ledger is in `ledger` when given. Read only: the
/// state's `writer.lock` is taken without waiting while it is read
/// (`MOVE_STATE_BUSY` while a pass holds it).
///
/// # Errors
/// `MOVE_STATE_BUSY`; a state with no pull record, or records of more
/// than one source (`FIELD_DOMAIN_VIOLATION`); the store's and ledger's
/// refusals.
#[allow(clippy::too_many_lines)] // Records, rows, each seat's proof.
pub fn plan(state: &Path, ledger: Option<&Path>, dry_run: bool) -> Result<Plan> {
    use crate::transfer_outcome::EntryOutcome;
    let _idle = idle(state)?;
    let ledger = ledger
        .map(|dir| {
            rusqlite::Connection::open_with_flags(
                dir.join("consolidate.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)
        })
        .transpose()?;
    let mut source: Option<(String, PathBuf, PathBuf)> = None;
    let mut actions: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    for (_, path) in crate::transfer_outcome::records(state) {
        let Some(Ok(record)) = crate::transfer_outcome::read_file(&path) else {
            continue;
        };
        if record.verb != "pull" && record.verb != "copy" {
            continue;
        }
        let (host, root) = match record.source.split_once(':') {
            Some((host, root)) if !host.contains('/') => (host.to_owned(), root.to_owned()),
            _ => ("-".to_owned(), record.source.clone()),
        };
        let named = (host, PathBuf::from(root), record.destination.clone());
        if source.as_ref().is_some_and(|known| *known != named) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        source = Some(named);
        for entry in record.entries {
            if let EntryOutcome::Consolidated { action } = entry.outcome {
                actions.insert(entry.rel_path, action);
            }
        }
    }
    let (host, root, destination) = source.ok_or(BulkloadRefusal::FieldDomainViolation)?;
    let rows = recorded_rows(state)?;
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let skip = |rel: &[u8], why: String| Done {
        rel: rel.to_vec(),
        identity: None,
        proof: None,
        verdict: Verdict::Skipped(why),
    };
    for (rel, recorded) in rows {
        if !contained(&rel)
            || crate::consolidate::Class::of(&rel, false) == crate::consolidate::Class::Sqlite
        {
            continue;
        }
        let action = actions.get(&rel).map(String::as_str);
        let mut allowed = Vec::new();
        let mut why = None;
        let mut content: Option<std::result::Result<Proof, String>> = None;
        for (row, key, output) in recorded {
            if row.kind != bulkload_proto::FileKind::Regular {
                continue;
            }
            let action = match (action, output) {
                (Some(action), _) => action,
                (None, true) => "adopt",
                (None, false) => continue,
            };
            if !BY_CONTENT.contains(&action) && !BY_RECORD.contains(&action) {
                why = Some(format!("its outcome ({action}) proves no copy"));
                continue;
            }
            let proof = if BY_RECORD.contains(&action) {
                proof_now(&destination, ledger.as_ref(), &rel, action, &key)
            } else {
                content
                    .get_or_insert_with(|| {
                        proof_now(&destination, ledger.as_ref(), &rel, action, &key)
                    })
                    .clone()
            };
            match proof {
                Ok(proof) => allowed.push((row.stat_identity(), proof)),
                Err(reason) => why = Some(reason),
            }
        }
        if allowed.is_empty() {
            if let Some(why) = why {
                skipped.push(skip(&rel, format!("destination proof: {why}")));
            }
            continue;
        }
        files.push(Want { rel, allowed });
    }
    {
        use std::os::unix::ffi::OsStrExt as _;
        Ok(Plan {
            host,
            ask: Ask {
                root: root.as_os_str().as_bytes().to_vec(),
                dry_run,
                files,
            },
            skipped,
        })
    }
}

/// Which of `files` (path, (device, inode)) a process holds open, by
/// index; `None` when this platform cannot say.
fn held_open(files: &[(PathBuf, (u64, u64))]) -> Option<HashSet<usize>> {
    #[cfg(target_os = "linux")]
    {
        let mut held = HashSet::new();
        for (index, (path, _)) in files.iter().enumerate() {
            if !crate::procs::inside(&[path.as_path()]).ok()??.is_empty() {
                held.insert(index);
            }
        }
        Some(held)
    }
    #[cfg(target_vendor = "apple")]
    {
        // Matched by device and inode, never by a path spelling, from the
        // descriptors libproc lists in this process: no child is started
        // (BL2-F1, Linear TIN-3692 `c10e9bec`).
        let open = crate::procs::open_vnodes()?;
        Some(
            files
                .iter()
                .enumerate()
                .filter(|(_, (_, node))| open.contains(node))
                .map(|(index, _)| index)
                .collect(),
        )
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        let _ = files;
        None
    }
}

/// A file's identity from its stat.
const fn identity(stat: &crate::io::Stat) -> Identity {
    (
        stat.node.dev,
        stat.node.ino,
        stat.size,
        stat.mtime_ns,
        stat.ctime_ns,
    )
}

/// The source-side checks of one candidate up to removal: its directory
/// handle, name and stat, the proof met, or why it is skipped.
struct Checked {
    dir: std::os::fd::OwnedFd,
    name: std::ffi::CString,
    stat: crate::io::Stat,
    proof: Proof,
}

fn check(root: &std::os::fd::OwnedFd, want: &Want) -> std::result::Result<Checked, String> {
    if !contained(&want.rel)
        || crate::consolidate::Class::of(&want.rel, false) == crate::consolidate::Class::Sqlite
    {
        return Err("not a plain relative path to a non-SQLite file".to_owned());
    }
    let mut parts: Vec<&[u8]> = want.rel.split(|byte| *byte == b'/').collect();
    let last = parts.pop().ok_or("an empty path")?;
    let mut dir = root
        .try_clone()
        .map_err(|error| format!("the root cannot be held: {error}"))?;
    for part in parts {
        let name = crate::io::c_name(part).map_err(|_| "a path with a NUL".to_owned())?;
        dir = crate::io::sys::open_dir_at(&dir, &name).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "the source is absent".to_owned()
            } else {
                format!("a directory on its path is not one, or is a link: {error}")
            }
        })?;
    }
    let name = crate::io::c_name(last).map_err(|_| "a path with a NUL".to_owned())?;
    let stat = crate::io::sys::fstatat_nofollow(&dir, &name).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "the source is absent".to_owned()
        } else {
            format!("the source cannot be read: {error}")
        }
    })?;
    if !stat.is_file() {
        return Err("the source is not a regular file".to_owned());
    }
    let found = identity(&stat);
    let proof = want
        .allowed
        .iter()
        .find(|(recorded, _)| *recorded == found)
        .map(|(_, proof)| proof.clone())
        .ok_or("the source changed since its outcome was recorded")?;
    if let Proof::Blake3(expected) = &proof {
        let file = crate::io::sys::open_read_at(&dir, &name)
            .map_err(|error| format!("the source cannot be read: {error}"))?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0_u8; 1 << 20];
        let mut at = 0_u64;
        loop {
            let read = crate::io::sys::pread_full(&file, &mut buffer, at)
                .map_err(|error| format!("the source cannot be read: {error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(buffer.get(..read).unwrap_or_default());
            at += read as u64;
        }
        if hasher.finalize().as_bytes() != expected {
            return Err("the source's bytes differ from the destination's".to_owned());
        }
        let now = crate::io::sys::fstat(&file)
            .map_err(|error| format!("the source cannot be read: {error}"))?;
        if identity(&now) != found {
            return Err("the source changed while it was read".to_owned());
        }
    }
    Ok(Checked {
        dir,
        name,
        stat,
        proof,
    })
}

/// The source side: each candidate of `ask` checked and, unless a dry run,
/// removed (see the module). Never a directory, never anything else.
#[must_use]
pub fn answer(ask: &Ask) -> Vec<Done> {
    use std::os::unix::ffi::OsStrExt as _;
    let root_path = PathBuf::from(std::ffi::OsStr::from_bytes(&ask.root));
    let skip_all = |why: &str| {
        ask.files
            .iter()
            .map(|want| Done {
                rel: want.rel.clone(),
                identity: None,
                proof: None,
                verdict: Verdict::Skipped(why.to_owned()),
            })
            .collect()
    };
    if !root_path.is_absolute() {
        return skip_all("the source root is not absolute");
    }
    let Ok(root) = crate::io::sys::open_root(&root_path) else {
        return skip_all("the source root cannot be opened");
    };
    // Resolved through the root as opened: the names a holder's open file
    // links resolve to.
    let base = std::fs::canonicalize(&root_path).unwrap_or_else(|_| root_path.clone());
    let mut done = Vec::with_capacity(ask.files.len());
    let batch = crate::io::limits::descriptor_limit().map_or(16, |(soft, _)| check_batch(soft));
    for files in ask.files.chunks(batch) {
        done.extend(answer_batch(&root, &base, files, ask.dry_run));
    }
    done
}

/// Hold, prove and retire only this batch before opening the next. The
/// holder probe runs after the content checks, and removal still verifies
/// each identity through its held directory immediately before unlinking.
fn answer_batch(
    root: &std::os::fd::OwnedFd,
    base: &Path,
    wants: &[Want],
    dry_run: bool,
) -> Vec<Done> {
    let mut checked = Vec::with_capacity(wants.len());
    let mut done = Vec::with_capacity(wants.len());
    for want in wants {
        match check(root, want) {
            Ok(found) => checked.push((want, found)),
            Err(why) => done.push(Done {
                rel: want.rel.clone(),
                identity: None,
                proof: None,
                verdict: Verdict::Skipped(why),
            }),
        }
    }
    let files: Vec<(PathBuf, u64)> = checked
        .iter()
        .map(|(want, found)| {
            (
                path_of(base, &want.rel),
                (found.stat.node.dev, found.stat.node.ino),
            )
        })
        .collect();
    let held = held_open(&files);
    for (index, (want, found)) in checked.into_iter().enumerate() {
        let mut answer = Done {
            rel: want.rel.clone(),
            identity: Some(identity(&found.stat)),
            proof: Some(found.proof.kind()),
            verdict: Verdict::WouldRemove,
        };
        answer.verdict = match &held {
            None => {
                Verdict::Skipped("whether a process holds it open cannot be told here".to_owned())
            }
            Some(held) if held.contains(&index) => {
                Verdict::Skipped("a process holds it open".to_owned())
            }
            Some(_) if dry_run => Verdict::WouldRemove,
            Some(_) => remove(&found),
        };
        done.push(answer);
    }
    done
}

/// Remove the checked file, its identity checked once more just before.
fn remove(found: &Checked) -> Verdict {
    match crate::io::sys::fstatat_nofollow(&found.dir, &found.name) {
        Ok(now) if now.is_file() && identity(&now) == identity(&found.stat) => {}
        _ => return Verdict::Skipped("the source changed before its removal".to_owned()),
    }
    if let Err(error) = crate::io::sys::unlinkat(&found.dir, &found.name, false) {
        return Verdict::Skipped(format!("the removal failed: {error}"));
    }
    let dir = std::fs::File::from(match found.dir.try_clone() {
        Ok(dir) => dir,
        Err(_) => return Verdict::Removed,
    });
    let _ = dir.sync_all();
    Verdict::Removed
}

/// Frame `value` for the wire (`preview`'s framing).
///
/// # Errors
/// `FRAME_CODEC`.
pub fn frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    crate::git_carry::preview::frame(value)
}

/// Read one [`frame`].
///
/// # Errors
/// `FRAME_CODEC`; IO.
pub fn unframe<T: for<'de> Deserialize<'de>>(reader: &mut impl std::io::Read) -> Result<T> {
    crate::git_carry::preview::unframe(reader)
}

/// One receipt line: when, the verdict, the path, its identity, the proof,
/// and why.
#[must_use]
pub fn receipt_line(done: &Done, at_ns: i128) -> String {
    let (word, why) = match &done.verdict {
        Verdict::Removed => ("removed", ""),
        Verdict::WouldRemove => ("would-remove", ""),
        Verdict::Skipped(why) => ("skipped", why.as_str()),
    };
    let identity = done.identity.map_or_else(
        || "-".to_owned(),
        |(dev, ino, size, mtime, ctime)| format!("{dev}:{ino}:{size}:{mtime}:{ctime}"),
    );
    format!(
        "{at_ns}\t{word}\t{}\t{identity}\t{}\t{why}",
        done.rel.escape_ascii(),
        done.proof.as_deref().unwrap_or("-")
    )
}

/// Append the run's receipt lines to `DEST_STATE/retire-source.receipts`,
/// made durable.
///
/// # Errors
/// The IO refusal.
pub fn write_receipts(state: &Path, done: &[Done]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let at_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i128::try_from(since.as_nanos()).unwrap_or(0));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(state.join("retire-source.receipts"))
        .refuse_at(SITE)?;
    let mut text = String::new();
    for one in done {
        text.push_str(&receipt_line(one, at_ns));
        text.push('\n');
    }
    file.write_all(text.as_bytes()).refuse_at(SITE)?;
    file.sync_all().refuse_at(SITE)?;
    if let Ok(dir) = std::fs::File::open(state) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]
mod tests;
