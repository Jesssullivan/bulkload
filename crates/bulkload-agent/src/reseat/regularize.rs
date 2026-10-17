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

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::ledger::{Identity, Ledger, Regular, RegularState};
use super::place::{c_name, identity_at, lstat, open_dir, temp_name};
use super::{Summary, SCHEMA};
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
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"regularize\",\"tool\":\"{TOOL}\",\"link\":{},\"outcome\":\"{}\",\"refused\":{},\"kept\":{}}}",
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
fn read_digest(path: &Path) -> Result<Option<(Identity, [u8; 32])>> {
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
            self.ledger.delete_regular(self.link)?;
            return Ok(Outcome::raced("raced-restored", kept));
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
        Ok(Outcome::raced("raced-kept", None))
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
        self.ledger.delete_regular(self.link)?;
        Ok(None)
    }

    fn regularize(&self, min_free_percent: u8) -> Result<Outcome> {
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
        // The free-space rule per link, as it is copied (M2 round 2, item
        // 1): a pass's other work is never held back by every link's bytes
        // at once, and a link that does not fit now is this line's refusal.
        let parent = self.link.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
        let size = target.len();
        if size > 0
            && crate::space::check_at(
                parent,
                (size, size),
                crate::space::probe_limits(parent)?,
                min_free_percent,
            )
            .is_err()
        {
            return Ok(Outcome::new(
                "space-refused",
                Some(BulkloadRefusal::DestinationSpaceInsufficient),
            ));
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

/// Regularize every symlinked transcript under the destination `.claude`
/// root `root`, one `regularize` line each.
///
/// Each link's copy is checked
/// against the free-space rule (`min_free_percent`) when it is made, never
/// all of them before the pass: a link that does not fit is its own
/// `space-refused` line (`DESTINATION_SPACE_INSUFFICIENT`) and the next
/// pass retries it.
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn regularize(
    root: &Path,
    ledger: &Ledger,
    min_free_percent: u8,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    // Rows a crash left in flight, whatever the link is now.
    for row in ledger.regulars()? {
        if row.state == RegularState::Intent && row.link.starts_with(root) {
            if let Some(site) = Site::new(ledger, &row.link)? {
                if let Some(outcome) = site.settle_intent(row.clone())? {
                    *summary
                        .rows
                        .entry(format!("link-{}", outcome.word))
                        .or_default() += 1;
                    emit(&line(&row.link, &outcome))?;
                }
            }
        }
    }
    for link in links(root)? {
        if ledger.regular(&link)?.is_some() {
            // Regularized, raced or undone: never again by itself.
            continue;
        }
        let Some(site) = Site::new(ledger, &link)? else {
            continue;
        };
        let outcome = site.regularize(min_free_percent)?;
        *summary
            .rows
            .entry(format!("link-{}", outcome.word))
            .or_default() += 1;
        emit(&line(&link, &outcome))?;
    }
    Ok(())
}

/// Put every regularized transcript's symlink back (`--undo`).
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn undo(ledger: &Ledger, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<u64> {
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
            RegularState::Undone => continue,
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
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"{TOOL}\",\"link\":{},\"target\":{},\"outcome\":\"{}\",\"refused\":{}}}",
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
