//! Regularize the destination's symlinked transcripts (OI-1003-Q198 (2),
//! TIN-6587).
//!
//! On sting, 388 of 417 Claude transcripts (and 43 of the 45 sessions of
//! the 2026-10-01 repair) are symlinks into `/srv/data/jess/archive`, and
//! Claude Code's picker lists regular files only, so `claude --resume`
//! cannot see them. The operator ruled (Linear TIN-6587, comment
//! `fbccc49b`): each is replaced by a regular-file copy of its target, with
//! the target's modification time, the archive copy kept (the TIN-5268
//! hold is unaffected), ledgered and reversible.
//!
//! Per symlink `projects/<key>/<name>.jsonl` (a real key directory; a
//! symlinked key directory is never written through), race-safe against a
//! live session appending through the link (OI-1003-Q198 re-review):
//!
//! 1. The target (its real path, a regular file) is copied byte for byte
//!    into `.<name>.<token>.bulkload-reseat` beside the link, with the
//!    target's mode and modification time, sealed, and the BLAKE3 of the
//!    bytes read kept. No remap call is made (no reflink: P84
//!    SOURCE-NO-CLONE, OI-1003-Q165, holds every crate to a byte copy).
//! 2. Before the swap the copy is read back from its name and must be the
//!    bytes read (same length, same BLAKE3), and the target's identity
//!    (inode, size, modification time) must be the one read, by the open
//!    descriptor and by its path; the link's and the copy's too.
//! 3. The ledger row (`intent`) records the link text, the link's, the
//!    target's and the copy's identities and the digest.
//! 4. The copy and the link are exchanged atomically (`RENAME_EXCHANGE`),
//!    and the target's identity is checked again under the swap: unchanged,
//!    the link (now under the temporary name) is removed (`done`).
//!
//! Nothing that holds a session's bytes is removed or replaced unless its
//! bytes are known to be held elsewhere. On any mismatch the transcript is
//! refused (`RESEAT_REGULARIZE_RACED`) and both copies stay intact: before
//! the swap, the archive is untouched and the staged copy is kept aside as
//! `.<name>.<token>.kept-<inode>.bulkload-reseat`; under the swap, an
//! unchanged copy gets the link put back over it and is kept aside the
//! same way, and a copy written since stays at the path with the archive
//! untouched (`raced`). A kept copy is never removed by any pass; the
//! report line names it. A crash anywhere settles forward from the ledger
//! and the two names.
//!
//! `--undo` first verifies the archive target the row's link names: it
//! must exist, be a regular file, and hold the recorded bytes (the size and
//! BLAKE3 of the copy). Otherwise the row is refused
//! (`RESEAT_UNDO_TARGET_CHANGED`) and nothing is changed for it. Then it
//! puts the link back (a new symlink with the recorded text, exchanged in),
//! only while the regular file is exactly the copy this ledger made; one
//! changed since (a resume appended, or a re-seat pass extended it) is
//! kept, `RESEAT_DEST_CONTINUED`.
//!
//! **Companions** (A-20261018-111, "Extend regularize to companions"). On
//! sting about 37,900 companion files (subagent transcripts and their
//! `.meta.json`, tool results, workflows, `file-history`, and the rest a
//! re-seat places beside a session) are symlinks into the archive, so a
//! lane whose companions are links fails `--check` (`companion-missing`).
//! Every symlink at a companion's place ([`companion_links`]: under a real
//! `projects/<key>/` other than a top-level transcript, `file-history/`,
//! `tasks/`, `session-env/`, `debug/<name>.txt`, `todos/`, `plans/*.md`,
//! through real directories only) whose target is a regular file outside
//! the live root is regularized with the same discipline, row and undo as
//! a transcript. A directory symlink there (the 2026-10-01 repair's
//! `file-history/<sid>` and the like) is reported `dir-link`, never written
//! through. With `--replace-archived-dest`, a link into a root at the
//! sidecar of a session the pass places is left to the placement (A-106:
//! a fork is replaced by the landed file); after the placement, each link
//! it proved equal to or a prefix of its target (kept a link, `occupied`
//! or `dest-ahead`), transcripts and rollouts included, and each such
//! sidecar link it never placed, is regularized ([`after_placement`]).
//!
//! **Re-linked paths** (A-20261018-111). A `done` or `raced` row whose path
//! is a symlink again was re-linked by someone else since (384 transcripts
//! were, at 2026-10-18T04:58Z). It is never skipped as handled: the pass
//! marks it `relinked` and drops its placement row in one commit (a
//! `relinked` line), then decides the path again as if it had no row; its
//! regularization line carries `"relinked":true`.
//!
//! A target some process holds open (`/proc`, read once per sweep,
//! observation only) is not regularized: `held-open`
//! (`DESTINATION_OCCUPIED`), decided again next pass.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::ledger::{Identity, Ledger, Regular, RegularState};
use super::place::{c_name, identity_at, lstat, open_dir, temp_name};
use super::{Kind, Summary, SCHEMA};
use crate::closure::json_string;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The tool name in report lines.
const TOOL: &str = "claude";
/// Bytes per copy chunk.
const CHUNK: usize = 1024 * 1024;

/// Every symlinked transcript `projects/<key>/<name>.jsonl` under the
/// destination `.claude` root, in real key directories.
///
/// # Errors
/// The I/O refusal of a listing.
pub fn links(root: &Path) -> Result<Vec<PathBuf>> {
    let projects = root.join("projects");
    let mut out = Vec::new();
    let keys = match std::fs::read_dir(&projects) {
        Ok(keys) => keys,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error).refuse_at("reseat::regularize::links"),
    };
    for key in keys {
        let key = key.refuse_at("reseat::regularize::links")?;
        if !key
            .file_type()
            .refuse_at("reseat::regularize::links")?
            .is_dir()
        {
            continue;
        }
        for entry in std::fs::read_dir(key.path()).refuse_at("reseat::regularize::links")? {
            let entry = entry.refuse_at("reseat::regularize::links")?;
            let name = entry.file_name();
            let is_transcript = Path::new(&name)
                .extension()
                .is_some_and(|extension| extension == "jsonl")
                && !name.as_bytes().starts_with(b".");
            if is_transcript
                && entry
                    .file_type()
                    .refuse_at("reseat::regularize::links")?
                    .is_symlink()
            {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `name` is a top-level transcript's: `<name>.jsonl`, not hidden.
fn transcript_name(name: &std::ffi::OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "jsonl")
        && !name.as_bytes().starts_with(b".")
}

/// Whether `name` is a session ID (a UUID: 36 characters, dashes at 8,
/// 13, 18 and 23, hex digits elsewhere).
fn session_id(name: &str) -> bool {
    name.len() == 36
        && name.bytes().enumerate().all(|(at, byte)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// What a companion at `rel` (relative to the live `.claude`) is, by its
/// place; `None` for a top-level transcript or a place re-seat does not
/// place anything.
#[must_use]
pub fn companion_kind(rel: &Path) -> Option<Kind> {
    let parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(std::path::Component::as_os_str)
        .collect();
    let first = parts.first()?.to_str()?;
    match first {
        "projects" => match parts.len() {
            0..=2 => None,
            3 => (!transcript_name(parts.get(2)?)).then_some(Kind::Extra),
            _ => Some(match parts.get(2)?.to_str() {
                Some("memory") => Kind::Memory,
                Some(dir) if session_id(dir) => Kind::Sidecar,
                _ => Kind::Extra,
            }),
        },
        "file-history" | "tasks" | "session-env" if parts.len() >= 3 => Some(Kind::Global),
        "debug" if parts.len() == 2 => Path::new(parts.get(1)?)
            .extension()
            .is_some_and(|extension| extension == "txt")
            .then_some(Kind::Global),
        "todos" if parts.len() == 2 => Some(Kind::Global),
        "plans" if parts.len() == 2 => Path::new(parts.get(1)?)
            .extension()
            .is_some_and(|extension| extension == "md")
            .then_some(Kind::Plan),
        _ => None,
    }
}

/// The symlinks at companions' places under the live `.claude` root.
#[derive(Debug, Default)]
pub struct CompanionLinks {
    /// `(link, kind)`, sorted.
    pub files: Vec<(PathBuf, Kind)>,
    /// Directory symlinks at a companion's place (never written through).
    pub dir_links: Vec<PathBuf>,
}

/// Every symlink at a companion's place under the destination `.claude`
/// root `root` ([`companion_kind`]).
///
/// Found through real directories only: a symlinked directory is never descended (a link to a directory is a
/// `dir_links` entry, a symlinked key directory is skipped as for
/// transcripts). This ledger's own temporaries and kept copies are regular
/// files and never listed.
///
/// # Errors
/// The I/O refusal of a listing; `PATH_DEPTH_EXCEEDED`.
pub fn companion_links(root: &Path) -> Result<CompanionLinks> {
    let mut out = CompanionLinks::default();
    let mut pending: Vec<(PathBuf, usize)> = Vec::new();
    for top in [
        "projects",
        "file-history",
        "tasks",
        "session-env",
        "debug",
        "todos",
        "plans",
    ] {
        if lstat(&root.join(top))?.is_some_and(|meta| meta.is_dir()) {
            pending.push((PathBuf::from(top), 0));
        }
    }
    while let Some((rel, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(root.join(&rel)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).refuse_at("reseat::regularize::companion_links"),
        };
        for entry in entries {
            let entry = entry.refuse_at("reseat::regularize::companion_links")?;
            let child = rel.join(entry.file_name());
            let kind = entry
                .file_type()
                .refuse_at("reseat::regularize::companion_links")?;
            if kind.is_dir() {
                // `projects/` itself holds key directories, every other top
                // its entries.
                if depth >= super::claude::DEPTH_LIMIT {
                    return Err(BulkloadRefusal::PathDepthExceeded);
                }
                pending.push((child, depth + 1));
                continue;
            }
            if !kind.is_symlink() || entry.file_name().as_bytes().ends_with(b".bulkload-reseat") {
                // A regular file, or one of this or another ledger's
                // temporaries (an interrupted undo's link): never listed.
                continue;
            }
            let in_projects_root = rel == Path::new("projects");
            if in_projects_root {
                // A symlinked key directory: never written through.
                continue;
            }
            if std::fs::metadata(root.join(&child)).is_ok_and(|meta| meta.is_dir()) {
                if companion_place(&child) {
                    out.dir_links.push(root.join(&child));
                }
                continue;
            }
            if let Some(of) = companion_kind(&child) {
                out.files.push((root.join(&child), of));
            }
        }
    }
    out.files.sort();
    out.dir_links.sort();
    Ok(out)
}

/// Whether a directory at `rel` is a place companions live under (a
/// session's sidecar or per-session state directory, a key's `memory/`).
fn companion_place(rel: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(std::path::Component::as_os_str)
        .collect();
    match parts.first().and_then(|first| first.to_str()) {
        Some("projects") => parts.len() >= 3,
        Some("file-history" | "tasks" | "session-env") => parts.len() >= 2,
        _ => false,
    }
}

/// Where a test acts as a live session between two steps of a
/// regularization (the hook runs only in unit tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The copy is staged and read back; the swap has not happened.
    Staged,
    /// The copy and the link were just exchanged.
    Exchanged,
}

/// The unit tests' live-session hook.
#[cfg(test)]
type Hook = Box<dyn FnMut(Step, &Path)>;

#[cfg(test)]
std::thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Run `hook` at every [`Step`] of the regularizations on this thread
/// (`None` clears it).
#[cfg(test)]
pub fn set_hook(hook: Option<Hook>) {
    HOOK.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
fn hook(step: Step, link: &Path) {
    HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(step, link);
        }
    });
}

#[cfg(not(test))]
const fn hook(_step: Step, _link: &Path) {}

/// One link's outcome: its report word, its refusal, and where a kept
/// copy is.
struct Outcome {
    word: &'static str,
    refusal: Option<BulkloadRefusal>,
    kept: Option<PathBuf>,
}

impl Outcome {
    const fn new(word: &'static str, refusal: Option<BulkloadRefusal>) -> Self {
        Self {
            word,
            refusal,
            kept: None,
        }
    }

    const fn raced(word: &'static str, kept: Option<PathBuf>) -> Self {
        Self {
            word,
            refusal: Some(BulkloadRefusal::ReseatRegularizeRaced),
            kept,
        }
    }
}

fn line(link: &Path, outcome: &Outcome) -> String {
    line_of("regularize", TOOL, link, outcome)
}

/// A report line of `kind` (`regularize`, or `replace` for an archive
/// replacement, A-106) for `tool`.
fn line_of(kind: &str, tool: &str, link: &Path, outcome: &Outcome) -> String {
    report(kind, tool, link, outcome, None, false)
}

/// [`line_of`], naming the companion's kind (`"companion"`) and a path
/// decided again after a re-link (`"relinked":true`) when they apply; a
/// transcript's line is unchanged.
fn report(
    kind: &str,
    tool: &str,
    link: &Path,
    outcome: &Outcome,
    of: Option<Kind>,
    relinked: bool,
) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"{kind}\",\"tool\":\"{tool}\",\"link\":{},\"outcome\":\"{}\",\"refused\":{},\"kept\":{}{}{}}}",
        json_string(&link.to_string_lossy()),
        outcome.word,
        outcome
            .refusal
            .as_ref()
            .map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
        outcome.kept.as_ref().map_or_else(
            || "null".to_owned(),
            |kept| json_string(&kept.to_string_lossy())
        ),
        of.map_or_else(String::new, |of| format!(",\"companion\":\"{}\"", of.word())),
        if relinked { ",\"relinked\":true" } else { "" },
    )
}

/// The lstat identity at `path`, `None` when absent or unreadable.
fn lstat_identity(path: &Path) -> Option<Identity> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| Identity::of(&meta))
}

/// The identity and BLAKE3 of the regular file at `path` (not following a
/// terminal symlink), read whole; `None` when it is absent, not a regular
/// file, cannot be opened, or changed while it was read.
pub(super) fn read_digest(path: &Path) -> Result<Option<(Identity, [u8; 32])>> {
    let Ok(mut file) = crate::hash::open_nofollow(path) else {
        return Ok(None);
    };
    let meta = file.metadata().refuse_at("reseat::regularize::digest")?;
    if !meta.is_file() {
        return Ok(None);
    }
    let identity = Identity::of(&meta);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; CHUNK];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .refuse_at("reseat::regularize::digest")?;
        if read == 0 {
            break;
        }
        counters::add_len(Counter::ReseatDestRead, read);
        let bytes = buffer.get(..read).unwrap_or_default();
        counters::update(&mut hasher, Counter::HashOther, bytes);
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    let after = Identity::of(&file.metadata().refuse_at("reseat::regularize::digest")?);
    if after != identity || total != identity.size {
        return Ok(None);
    }
    Ok(Some((identity, *hasher.finalize().as_bytes())))
}

/// A staged copy.
struct Staged {
    /// The copy's identity once sealed.
    copy: Identity,
    /// The target's identity when it was opened.
    source: Identity,
    /// BLAKE3 of the bytes read from the target.
    digest: [u8; 32],
    /// The target held still while it was read (by its descriptor and its
    /// path), and the copy read back from its name is those bytes.
    verified: bool,
}

/// One link's names.
struct Site<'a> {
    ledger: &'a Ledger,
    link: &'a Path,
    directory: File,
    file_name: std::ffi::OsString,
    name: std::ffi::CString,
    temp: PathBuf,
    temp_c: std::ffi::CString,
}

impl<'a> Site<'a> {
    fn new(ledger: &'a Ledger, link: &'a Path) -> Result<Option<Self>> {
        let (Some(parent), Some(name)) = (link.parent(), link.file_name()) else {
            return Err(BulkloadRefusal::PathEscapesRoot);
        };
        if !lstat(parent)?.is_some_and(|meta| meta.is_dir()) {
            return Ok(None);
        }
        let temp_os = temp_name(name, ledger.temp_token());
        Ok(Some(Self {
            ledger,
            link,
            directory: open_dir(parent)?,
            file_name: name.to_os_string(),
            name: c_name(name)?,
            temp: parent.join(&temp_os),
            temp_c: c_name(&temp_os)?,
        }))
    }

    fn exchange(&self) -> std::io::Result<()> {
        crate::io::exchange(&self.directory, &self.temp_c, &self.name)
    }

    fn unlink_temp(&self) -> Result<()> {
        crate::io::sys::unlinkat(&self.directory, &self.temp_c, false)
            .refuse_at("reseat::regularize::unlink")
    }

    fn seal(&self) -> Result<()> {
        crate::io::durable::seal_dir(&self.directory).refuse_at("reseat::regularize::seal")
    }

    /// Whether the node at `path` is a symlink holding `text`.
    fn is_link_to(path: &Path, text: &Path) -> bool {
        lstat(path)
            .ok()
            .flatten()
            .is_some_and(|meta| meta.is_symlink())
            && std::fs::read_link(path).is_ok_and(|now| now == text)
    }

    /// Move the regular file under the temporary name aside, never
    /// replacing anything, as `.<name>.<token>.kept-<inode>.bulkload-reseat`
    /// (an inode names one file while it exists, and a kept copy is never
    /// removed by a pass). `None` when the temporary holds no regular file.
    fn keep(&self) -> Result<Option<PathBuf>> {
        let Some(meta) = lstat(&self.temp)? else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Ok(None);
        }
        let ino = std::os::unix::fs::MetadataExt::ino(&meta);
        let token = self.ledger.temp_token();
        let tag = if token.is_empty() {
            format!("kept-{ino}")
        } else {
            format!("{token}.kept-{ino}")
        };
        let kept_os = temp_name(&self.file_name, &tag);
        crate::io::publish_noreplace(&self.directory, &self.temp_c, &c_name(&kept_os)?)
            .map_err(|error| crate::refuse::io_in(&error, self.link, "reseat::regularize::keep"))?;
        self.seal()?;
        Ok(Some(self.link.with_file_name(kept_os)))
    }

    /// The staged copy under the temporary name: removed when it is
    /// unchanged and the target still holds the bytes it was copied from
    /// (nothing is lost), else kept aside.
    fn retire_staged(&self, row: &Regular) -> Result<Option<PathBuf>> {
        if identity_at(&self.temp)? == Some(row.regular)
            && lstat_identity(&row.resolved) == Some(row.target_identity)
        {
            self.unlink_temp()?;
            self.seal()?;
            return Ok(None);
        }
        self.keep()
    }

    /// Copy the target into the temporary: its bytes, mode and mtime,
    /// sealed; then read the copy back from its name and check the target
    /// held still.
    fn stage(&self, resolved: &Path) -> Result<Staged> {
        let mut target = crate::hash::open_nofollow(resolved)?;
        let meta = target.metadata().refuse_at("reseat::regularize::target")?;
        let source = Identity::of(&meta);
        if lstat(&self.temp)?.is_some_and(|meta| meta.is_file()) {
            // Left by an interrupted pass: kept aside, never removed.
            self.keep()?;
        }
        let mut copy = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.temp)
            .map_err(|error| {
                crate::refuse::io_in(&error, self.link, "reseat::regularize::stage")
            })?;
        let mut hasher = blake3::Hasher::new();
        let mut copied = 0_u64;
        let written = (|| -> Result<()> {
            let mut buffer = vec![0_u8; CHUNK];
            loop {
                let read = target
                    .read(&mut buffer)
                    .refuse_at("reseat::regularize::read")?;
                if read == 0 {
                    break;
                }
                counters::add_len(Counter::ReseatDestRead, read);
                let bytes = buffer.get(..read).unwrap_or_default();
                counters::update(&mut hasher, Counter::HashOther, bytes);
                copy.write_all(bytes).map_err(|error| {
                    crate::refuse::io_in(&error, self.link, "reseat::regularize::write")
                })?;
                counters::add_len(Counter::ReseatDestWrite, read);
                copied = copied.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            }
            copy.set_permissions(std::fs::Permissions::from_mode(
                meta.permissions().mode() & 0o777,
            ))
            .refuse_at("reseat::regularize::mode")?;
            copy.set_modified(meta.modified().refuse_at("reseat::regularize::mtime")?)
                .refuse_at("reseat::regularize::mtime")?;
            crate::io::durable::seal_file(&copy).refuse_at("reseat::regularize::seal_copy")
        })();
        if let Err(refusal) = written {
            // Never published, and the archive holds every byte of it.
            let _ = self.unlink_temp();
            return Err(refusal);
        }
        let digest = *hasher.finalize().as_bytes();
        let staged = Identity::of(&copy.metadata().refuse_at("reseat::regularize::staged")?);
        drop(copy);
        let after = Identity::of(&target.metadata().refuse_at("reseat::regularize::target")?);
        let back = read_digest(&self.temp)?;
        // The target held still (its descriptor and its path), every byte
        // was read, and the copy on disk is those bytes.
        let held = [Some(after), lstat_identity(resolved)] == [Some(source); 2];
        let whole = copied == source.size;
        let same = back == Some((staged, digest));
        let verified = held && whole && same;
        Ok(Staged {
            copy: staged,
            source,
            digest,
            verified,
        })
    }

    /// Under the swap, before it: the target, the link and the copy are
    /// each still what was read.
    fn unchanged(&self, row: &Regular) -> bool {
        lstat_identity(&row.resolved) == Some(row.target_identity)
            && lstat_identity(self.link) == Some(row.link_identity)
            && Self::is_link_to(self.link, &row.target)
            && lstat_identity(&self.temp) == Some(row.regular)
    }

    /// After the exchange (or a crash after it): the copy at the path, the
    /// link under the temporary name. Settle by the target's and the
    /// copy's identities.
    fn settle(&self, mut row: Regular) -> Result<Outcome> {
        let link_at_temp = Self::is_link_to(&self.temp, &row.target);
        if lstat_identity(&row.resolved) == Some(row.target_identity) {
            // The target held still under the swap: the copy is it.
            if link_at_temp {
                self.unlink_temp()?;
            }
            self.seal()?;
            row.state = RegularState::Done;
            if row.replace {
                // A replacement: done, and any row of the path that is not
                // this ledger's dropped in the same commit (review B1).
                self.ledger.settle_replace_done(&row)?;
                counters::bump(Counter::ReseatArchivedReplaced);
                return Ok(Outcome::new("replaced-archived", None));
            }
            self.ledger.put_regular(&row)?;
            return Ok(Outcome::new("regularized", None));
        }
        if link_at_temp && identity_at(self.link)? == Some(row.regular) {
            // The target was written under the swap and the copy was not:
            // the link back over it, the copy kept aside.
            self.exchange()
                .refuse_at("reseat::regularize::exchange_back")?;
            self.seal()?;
            let kept = self.keep()?;
            self.forget(&row)?;
            return Ok(Outcome::raced(
                if row.replace {
                    "replace-raced-restored"
                } else {
                    "raced-restored"
                },
                kept,
            ));
        }
        // Both written (or the link gone): the copy stays at the path, the
        // archive is untouched. The row keeps the staged identity: `--undo`
        // removes only the copy exactly as it was made, over a verified
        // target.
        if link_at_temp {
            self.unlink_temp()?;
        }
        self.seal()?;
        row.state = RegularState::Raced;
        self.ledger.put_regular(&row)?;
        Ok(Outcome::raced(
            if row.replace {
                "replace-raced-kept"
            } else {
                "raced-kept"
            },
            None,
        ))
    }

    /// Finish what a crash left under an `intent` row; `None` when the
    /// link is as it was (the pass decides again).
    fn settle_intent(&self, row: Regular) -> Result<Option<Outcome>> {
        let at_path = identity_at(self.link)?;
        let exchanged =
            at_path.is_some_and(|now| now.dev == row.regular.dev && now.ino == row.regular.ino);
        if exchanged {
            return self.settle(row).map(Some);
        }
        // Not exchanged (or exchanged back): the temporary, if any, is the
        // staged copy.
        self.retire_staged(&row)?;
        self.forget(&row)?;
        Ok(None)
    }

    /// Drop the row of a regularization or replacement that did not take
    /// place; for a replacement, any placement row of the path too (the
    /// destination's link is there again: decided afresh).
    fn forget(&self, row: &Regular) -> Result<()> {
        if row.replace {
            self.ledger.delete_placement(self.link)?;
        }
        self.ledger.delete_regular(self.link)
    }

    fn regularize(&self, space: Space<'_>, open: &HashSet<PathBuf>) -> Result<Outcome> {
        let Some(link_meta) = lstat(self.link)? else {
            return Ok(Outcome::new("absent", None));
        };
        if !link_meta.is_symlink() {
            return Ok(Outcome::new("not-a-link", None));
        }
        let text = std::fs::read_link(self.link).refuse_at("reseat::regularize::readlink")?;
        let Ok(resolved) = std::fs::canonicalize(self.link) else {
            return Ok(Outcome::new(
                "dangling",
                Some(BulkloadRefusal::DestinationOccupied),
            ));
        };
        let Some(target) = lstat(&resolved)?.filter(std::fs::Metadata::is_file) else {
            return Ok(Outcome::new(
                "target-not-regular",
                Some(BulkloadRefusal::DestinationOccupied),
            ));
        };
        if open.contains(&resolved) {
            // A live writer holds the target open: its writes would go on
            // into the archive after the swap. Decided again next pass.
            return Ok(Outcome::new(
                "held-open",
                Some(BulkloadRefusal::DestinationOccupied),
            ));
        }
        // The free-space rule per link, as it is copied (M2 round 2, item
        // 1): a pass's other work is never held back by every link's bytes
        // at once, and a link that does not fit now is this line's refusal.
        // The bytes the pass's placements were admitted for stay reserved
        // beside it (review B1), so a regularization never takes what the
        // placements after it will write.
        let parent = self.link.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
        let size = target.len();
        if size > 0 {
            let mut wants = space.reserved.to_vec();
            wants.push((parent, size));
            match crate::space::preflight_many(&wants, space.min_free_percent) {
                Ok(()) => {}
                Err(BulkloadRefusal::DestinationSpaceInsufficient) => {
                    return Ok(Outcome::new(
                        "space-refused",
                        Some(BulkloadRefusal::DestinationSpaceInsufficient),
                    ));
                }
                Err(refusal) => return Err(refusal),
            }
        }
        let staged = self.stage(&resolved)?;
        hook(Step::Staged, self.link);
        let row = Regular {
            link: self.link.to_path_buf(),
            target: text,
            resolved,
            link_identity: Identity::of(&link_meta),
            target_identity: staged.source,
            regular: staged.copy,
            digest: Some(staged.digest),
            state: RegularState::Intent,
            replace: false,
        };
        if !staged.verified || !self.unchanged(&row) {
            // Written while it was copied or before the swap: the archive
            // and the link untouched, the copy kept aside.
            let kept = self.keep()?;
            return Ok(Outcome::raced("raced-skipped", kept));
        }
        self.ledger.put_regular(&row)?;
        if let Err(error) = self.exchange() {
            self.retire_staged(&row)?;
            self.ledger.delete_regular(self.link)?;
            if crate::io::rename_unsupported(&error) {
                return Ok(Outcome::new(
                    "exchange-unsupported",
                    Some(BulkloadRefusal::DestinationExchangeUnsupported),
                ));
            }
            return Err(error).refuse_at("reseat::regularize::exchange");
        }
        hook(Step::Exchanged, self.link);
        fault_point!(ReseatAfterRegularizeExchange);
        if !Self::is_link_to(&self.temp, &row.target)
            || lstat_identity(&self.temp) != Some(row.link_identity)
        {
            // Something other than the link was displaced (the path changed
            // between the check and the exchange): put it back, the copy
            // kept aside.
            self.exchange()
                .refuse_at("reseat::regularize::exchange_back")?;
            self.seal()?;
            let kept = self.keep()?;
            self.ledger.delete_regular(self.link)?;
            return Ok(Outcome::raced("raced-restored", kept));
        }
        self.settle(row)
    }

    /// The identity of the archive target the row's link names, when it
    /// exists, is a regular file and holds the recorded bytes (the size
    /// and BLAKE3 of the copy this ledger made); `None` otherwise.
    fn verify_target(&self, row: &Regular) -> Result<Option<Identity>> {
        let Some(digest) = row.digest else {
            return Ok(None);
        };
        let Some(parent) = self.link.parent() else {
            return Ok(None);
        };
        // The link text, read from the link's directory, must still name
        // the recorded target.
        if std::fs::canonicalize(parent.join(&row.target))
            .ok()
            .as_deref()
            != Some(row.resolved.as_path())
        {
            return Ok(None);
        }
        Ok(match read_digest(&row.resolved)? {
            Some((identity, now)) if identity.size == row.target_identity.size && now == digest => {
                Some(identity)
            }
            _ => None,
        })
    }

    /// Put the link back over an unchanged copy (`--undo`), only over a
    /// verified target.
    fn undo(&self, mut row: Regular) -> Result<Outcome> {
        let target_changed = || {
            Outcome::new(
                "undo-refused",
                Some(BulkloadRefusal::ReseatUndoTargetChanged),
            )
        };
        let Some(verified) = self.verify_target(&row)? else {
            return Ok(target_changed());
        };
        // A crash inside an earlier undo: the link already back.
        if Self::is_link_to(self.link, &row.target) {
            if identity_at(&self.temp)? == Some(row.regular) {
                self.unlink_temp()?;
                self.seal()?;
            }
            row.state = RegularState::Undone;
            self.ledger.put_regular(&row)?;
            return Ok(Outcome::new("undone", None));
        }
        if identity_at(self.link)? != Some(row.regular) {
            return Ok(Outcome::new(
                "undo-kept",
                Some(BulkloadRefusal::ReseatDestContinued),
            ));
        }
        match lstat(&self.temp)? {
            None => {}
            // An interrupted undo's own link.
            Some(meta) if meta.is_symlink() => self.unlink_temp()?,
            Some(_) => {
                return Ok(Outcome::new(
                    "undo-kept",
                    Some(BulkloadRefusal::DestinationOccupied),
                ))
            }
        }
        let text = std::ffi::CString::new(row.target.as_os_str().as_bytes())
            .map_err(|_| BulkloadRefusal::PathNotPortable)?;
        crate::io::sys::symlinkat(&text, &self.directory, &self.temp_c)
            .refuse_at("reseat::regularize::undo_link")?;
        if lstat_identity(&row.resolved) != Some(verified) {
            // Written since it was verified: nothing changed.
            self.unlink_temp()?;
            return Ok(target_changed());
        }
        self.exchange()
            .refuse_at("reseat::regularize::undo_exchange")?;
        fault_point!(ReseatAfterUndoExchange);
        if identity_at(&self.temp)? == Some(row.regular) {
            self.unlink_temp()?;
            self.seal()?;
            row.state = RegularState::Undone;
            self.ledger.put_regular(&row)?;
            Ok(Outcome::new("undone", None))
        } else {
            // Written between the check and the exchange: the copy back.
            self.exchange()
                .refuse_at("reseat::regularize::undo_exchange_back")?;
            if Self::is_link_to(&self.temp, &row.target) {
                self.unlink_temp()?;
            }
            self.seal()?;
            Ok(Outcome::new(
                "undo-kept",
                Some(BulkloadRefusal::ReseatDestContinued),
            ))
        }
    }
}

/// The free-space rule a regularization pass is held to: the floor, and
/// the bytes already admitted for the pass's placements, `(destination,
/// bytes)`, which stay reserved beside each link's copy.
#[derive(Clone, Copy)]
pub struct Space<'a> {
    pub min_free_percent: u8,
    pub reserved: &'a [(&'a Path, u64)],
}

/// What a regularization pass needs besides the ledger.
#[derive(Clone, Copy)]
pub struct Pass<'a> {
    /// `--replace-archived-dest` roots (canonical).
    pub archived: &'a [PathBuf],
    /// `(destination key, session id)` of each landed session this pass
    /// places, to its landed key ([`super::claude::Planned`]): its
    /// sidecars' archive links are left to the placement.
    pub placed: &'a BTreeMap<(String, String), String>,
    /// The landed `.claude`: whether a deferred sidecar has a landed file.
    pub landing: &'a Path,
    pub space: Space<'a>,
}

/// A sidecar link left to the placement, and its landed counterpart (the
/// file the placement would place there).
#[derive(Debug, Clone)]
pub struct Deferred {
    pub link: PathBuf,
    pub landed: PathBuf,
}

/// Mark every re-linked row under `root` `relinked`.
///
/// A `done` or `raced` row whose path is a symlink again has its placement
/// row dropped in the same commit, one `relinked` line each: the path is
/// decided again (A-20261018-111).
///
/// # Errors
/// Ledger refusals.
pub fn relinks(
    root: &Path,
    tool: &str,
    ledger: &Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for row in ledger.regulars()? {
        if !row.link.starts_with(root) || !row.relinked_now() {
            continue;
        }
        let previous = match row.state {
            RegularState::Raced => "raced",
            _ => "done",
        };
        if ledger.relink(&row.link)? {
            *summary.rows.entry("relinked".to_owned()).or_default() += 1;
            emit(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"relinked\",\"tool\":\"{tool}\",\"link\":{},\"was\":\"{previous}\",\"replace\":{},\"link_text\":{}}}",
                json_string(&row.link.to_string_lossy()),
                row.replace,
                std::fs::read_link(&row.link).map_or_else(
                    |_| "null".to_owned(),
                    |text| json_string(&text.to_string_lossy())
                ),
            ))?;
        }
    }
    Ok(())
}

/// The summary-row prefix of a link's outcome: `link-` for a transcript
/// (and a Codex rollout), `companion-` for a companion.
const fn prefix(of: Option<Kind>) -> &'static str {
    if of.is_some() {
        "companion-"
    } else {
        "link-"
    }
}

/// Regularize the symlink at `link` (of kind `of`, `None` for a transcript
/// or rollout) unless a row of this ledger stands for it; one line.
#[allow(clippy::too_many_arguments)]
fn one(
    root: &Path,
    tool: &str,
    link: &Path,
    of: Option<Kind>,
    ledger: &Ledger,
    (space, open): (Space<'_>, &HashSet<PathBuf>),
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let relinked = match ledger.regular(link)? {
        // Regularized, raced or undone: never again by itself.
        Some(row) if row.stands() => return Ok(()),
        Some(_) => true,
        None => false,
    };
    let Some(site) = Site::new(ledger, link)? else {
        return Ok(());
    };
    let outcome =
        if of.is_some() && std::fs::canonicalize(link).is_ok_and(|real| real.starts_with(root)) {
            // A link within the live state (the agent's own structure) is not
            // an archive copy: left as it is.
            Outcome::new("inside-root", Some(BulkloadRefusal::DestinationOccupied))
        } else {
            site.regularize(space, open)?
        };
    *summary
        .rows
        .entry(format!("{}{}", prefix(of), outcome.word))
        .or_default() += 1;
    emit(&report("regularize", tool, link, &outcome, of, relinked))
}

/// Regularize every symlinked transcript and companion under the
/// destination `.claude` root `root` before placement.
///
/// One `regularize` line each; returns the companion links left to the placement (a landed
/// session's sidecar linked into a `--replace-archived-dest` root), for
/// [`after_placement`].
///
/// Each link's copy is checked
/// against the free-space rule ([`Space`]: the floor, with the placements'
/// admitted bytes still reserved) when it is made, never all of them before
/// the pass: a link that does not fit is its own
/// `space-refused` line (`DESTINATION_SPACE_INSUFFICIENT`) and the next
/// pass retries it.
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn regularize(
    root: &Path,
    pass: Pass<'_>,
    ledger: &Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Vec<Deferred>> {
    settle_intents(root, TOOL, ledger, summary, emit)?;
    let open = super::place::open_files();
    let into_archive = |link: &Path| {
        !pass.archived.is_empty()
            && std::fs::canonicalize(link)
                .is_ok_and(|real| pass.archived.iter().any(|root| real.starts_with(root)))
    };
    for link in links(root)? {
        if into_archive(&link) {
            // A link into a `--replace-archived-dest` root is left for the
            // placement to replace with the landed file (A-106); one it
            // proves equal or ahead is regularized after it.
            continue;
        }
        one(
            root,
            TOOL,
            &link,
            None,
            ledger,
            (pass.space, &open),
            summary,
            emit,
        )?;
    }
    let found = companion_links(root)?;
    for dir in &found.dir_links {
        *summary
            .rows
            .entry("companion-dir-link".to_owned())
            .or_default() += 1;
        emit(&report(
            "regularize",
            TOOL,
            dir,
            &Outcome::new("dir-link", Some(BulkloadRefusal::DestinationOccupied)),
            None,
            false,
        ))?;
    }
    let mut deferred = Vec::new();
    for (link, of) in found.files {
        if of == Kind::Sidecar && into_archive(&link) {
            if let Some(landed) = placed_sidecar(root, &link, &pass) {
                deferred.push(Deferred { link, landed });
                continue;
            }
        }
        one(
            root,
            TOOL,
            &link,
            Some(of),
            ledger,
            (pass.space, &open),
            summary,
            emit,
        )?;
    }
    Ok(deferred)
}

/// The landed counterpart of `link` when it is under `projects/<key>/<sid>/`
/// of a session the pass places.
fn placed_sidecar(root: &Path, link: &Path, pass: &Pass<'_>) -> Option<PathBuf> {
    let rel = link.strip_prefix(root).ok()?;
    let mut parts = rel.components();
    let (Some(projects), Some(key), Some(sid)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if projects.as_os_str() != "projects" {
        return None;
    }
    let source_key = pass.placed.get(&(
        key.as_os_str().to_str()?.to_owned(),
        sid.as_os_str().to_str()?.to_owned(),
    ))?;
    Some(
        pass.landing
            .join("projects")
            .join(source_key)
            .join(sid)
            .join(parts.as_path()),
    )
}

/// After the placement, regularize the archive links it kept.
///
/// With `--replace-archived-dest` only: every link it left into a root
/// under the destination roots `roots` (`(root,
/// tool)`, canonical): each path whose placement row proved the landed
/// file equal to, or a prefix of, the link's target (`occupied` or
/// `dest-ahead` with a digest: transcripts, sidecars and rollouts), and
/// each `deferred` sidecar link it never placed (no row) that no landed
/// file names (sting's own file in a placed session's directory; one a
/// landed file names is left to a later placement, `left-to-placement`).
/// Nothing the placement replaced, refused or has not proved is touched.
/// One line each, its `kind` `regularize`.
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn after_placement(
    roots: &[(PathBuf, &'static str)],
    deferred: &[Deferred],
    archived: &[PathBuf],
    ledger: &Ledger,
    min_free_percent: u8,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if archived.is_empty() {
        return Ok(());
    }
    let mut todo: BTreeSet<PathBuf> = ledger.proved_links()?.into_iter().collect();
    for item in deferred {
        if todo.contains(&item.link) || ledger.placement(&item.link)?.is_some() {
            continue;
        }
        if lstat(&item.landed)?.is_some() {
            // A landed file names it, and this pass did not place it (its
            // transcript kept out): a later placement decides it (A-106: a
            // fork is replaced, never regularized as sting's).
            *summary
                .rows
                .entry("companion-left-to-placement".to_owned())
                .or_default() += 1;
            emit(&report(
                "regularize",
                TOOL,
                &item.link,
                &Outcome::new("left-to-placement", None),
                Some(Kind::Sidecar),
                false,
            ))?;
            continue;
        }
        todo.insert(item.link.clone());
    }
    let open = super::place::open_files();
    let space = Space {
        min_free_percent,
        reserved: &[],
    };
    for link in todo {
        let Some((root, tool)) = roots.iter().find(|(root, _)| link.starts_with(root)) else {
            continue;
        };
        if super::place::archive_link(&link, archived)?.is_none() {
            // Replaced, regularized, or no longer a link into a root.
            continue;
        }
        // Through real directories only: the placement made them so; one
        // replaced by a link since is not written through.
        let real_parent = link
            .parent()
            .is_some_and(|parent| std::fs::canonicalize(parent).ok().as_deref() == Some(parent));
        let of = if *tool == TOOL {
            link.strip_prefix(root).ok().and_then(companion_kind)
        } else {
            None
        };
        if !real_parent {
            *summary
                .rows
                .entry(format!("{}parent-not-real", prefix(of)))
                .or_default() += 1;
            emit(&report(
                "regularize",
                tool,
                &link,
                &Outcome::new(
                    "parent-not-real",
                    Some(BulkloadRefusal::DestinationOccupied),
                ),
                of,
                false,
            ))?;
            continue;
        }
        one(root, tool, &link, of, ledger, (space, &open), summary, emit)?;
    }
    Ok(())
}

/// Settle every regularization or replacement row a crash left `intent`
/// under the destination root `root`, whatever the path is now (run
/// before any placement, regularizing or not).
///
/// # Errors
/// Ledger and I/O refusals.
pub fn settle_intents(
    root: &Path,
    tool: &str,
    ledger: &Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for row in ledger.regulars()? {
        if row.state == RegularState::Intent && row.link.starts_with(root) {
            if let Some(site) = Site::new(ledger, &row.link)? {
                if let Some(outcome) = site.settle_intent(row.clone())? {
                    if row.replace {
                        // A replacement a crash interrupted, finished (or
                        // put back): reported as one (review B2).
                        if outcome.word == "replaced-archived" {
                            summary.file(tool, super::place::Outcome::ReplacedArchived);
                        } else {
                            *summary.rows.entry(outcome.word.to_owned()).or_default() += 1;
                        }
                        emit(&line_of("replace", tool, &row.link, &outcome))?;
                    } else {
                        let of = if tool == TOOL {
                            row.link.strip_prefix(root).ok().and_then(companion_kind)
                        } else {
                            None
                        };
                        *summary
                            .rows
                            .entry(format!("{}{}", prefix(of), outcome.word))
                            .or_default() += 1;
                        emit(&report("regularize", tool, &row.link, &outcome, of, false))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Put every regularized transcript's symlink back (`--undo`).
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn undo(
    ledger: &Ledger,
    codex_root: Option<&Path>,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<u64> {
    let mut undone = 0;
    for row in ledger.regulars()? {
        let Some(site) = Site::new(ledger, &row.link)? else {
            emit(&line(
                &row.link,
                &Outcome::new("undo-kept", Some(BulkloadRefusal::DestinationOccupied)),
            ))?;
            continue;
        };
        let outcome = match row.state {
            // Undone, or re-linked by someone else since (the link is
            // there already, not this ledger's to put back).
            RegularState::Undone | RegularState::Relinked => continue,
            RegularState::Intent => match site.settle_intent(row.clone())? {
                None => continue,
                Some(_) => match ledger.regular(&row.link)? {
                    Some(settled) if settled.state != RegularState::Undone => site.undo(settled)?,
                    _ => continue,
                },
            },
            RegularState::Done | RegularState::Raced => site.undo(row.clone())?,
        };
        undone += u64::from(outcome.word == "undone");
        if row.replace && outcome.word == "undone" {
            // The destination's link is back: no landed file is placed
            // there any more.
            ledger.delete_placement(&row.link)?;
        }
        // A Codex rollout's replacement is the codex tool's (review A5a).
        let tool = if codex_root.is_some_and(|root| row.link.starts_with(root)) {
            "codex"
        } else {
            TOOL
        };
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"{tool}\",\"link\":{},\"target\":{},\"outcome\":\"{}\",\"refused\":{}}}",
            json_string(&row.link.to_string_lossy()),
            json_string(&row.target.to_string_lossy()),
            outcome.word,
            outcome
                .refusal
                .as_ref()
                .map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
        ))?;
    }
    Ok(undone)
}
