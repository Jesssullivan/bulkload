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
//! symlinked key directory is never written through):
//!
//! 1. The target (its real path, a regular file) is copied byte for byte
//!    into `.<name>.<token>.bulkload-reseat` beside the link, with the
//!    target's mode and modification time, and sealed. No remap call is
//!    made (no reflink: P84 SOURCE-NO-CLONE, OI-1003-Q165, holds every
//!    crate to a byte copy). A target that changed while it was copied is
//!    left alone this pass (`RESEAT_REGULARIZE_RACED`).
//! 2. The ledger row (`intent`) records the link text, the link's and the
//!    target's identities and the copy's.
//! 3. The copy and the link are exchanged atomically (`RENAME_EXCHANGE`),
//!    then settled: the link (now under the temporary name) is removed when
//!    the target is unchanged (`done`). When the target changed in the
//!    meantime (a session appended through the link), the link is put back
//!    if the copy is unchanged; when both changed, both are kept and the
//!    copy stays at the path (`raced`). A crash anywhere settles forward
//!    from the ledger and the two names.
//!
//! `--undo` puts each link back (a new symlink with the recorded text,
//! exchanged in), only while the regular file is exactly the copy this
//! ledger made; one changed since (a resume appended, or a re-seat pass
//! extended it) is kept, `RESEAT_DEST_CONTINUED`.

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

/// Bytes a pass would write regularizing the links under `root` (each
/// target's size, for the free-space check).
///
/// # Errors
/// The I/O refusal of a listing; ledger refusals.
pub fn planned(root: &Path, ledger: Option<&Ledger>) -> Result<u64> {
    let mut total = 0_u64;
    for link in links(root)? {
        if let Some(ledger) = ledger {
            if ledger.regular(&link)?.is_some() {
                continue;
            }
        }
        if let Ok(meta) = std::fs::metadata(&link) {
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

fn line(link: &Path, outcome: &str, refusal: Option<&BulkloadRefusal>) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"regularize\",\"tool\":\"{TOOL}\",\"link\":{},\"outcome\":\"{outcome}\",\"refused\":{}}}",
        json_string(&link.to_string_lossy()),
        refusal.map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
    )
}

/// One link's names.
struct Site<'a> {
    ledger: &'a Ledger,
    link: &'a Path,
    directory: File,
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

    /// Copy the target into the temporary: its bytes, mode and mtime,
    /// sealed. Returns the copy's identity and the target's identity before
    /// and after the copy.
    fn stage(&self, resolved: &Path) -> Result<(Identity, Identity, Identity)> {
        let mut target = crate::hash::open_nofollow(resolved)?;
        let meta = target.metadata().refuse_at("reseat::regularize::target")?;
        let before = Identity::of(&meta);
        if lstat(&self.temp)?.is_some_and(|meta| meta.is_file()) {
            self.unlink_temp()?;
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
        let copied = (|| -> Result<()> {
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
                copy.write_all(bytes).map_err(|error| {
                    crate::refuse::io_in(&error, self.link, "reseat::regularize::write")
                })?;
                counters::add_len(Counter::ReseatDestWrite, read);
            }
            copy.set_permissions(std::fs::Permissions::from_mode(
                meta.permissions().mode() & 0o777,
            ))
            .refuse_at("reseat::regularize::mode")?;
            copy.set_modified(meta.modified().refuse_at("reseat::regularize::mtime")?)
                .refuse_at("reseat::regularize::mtime")?;
            crate::io::durable::seal_file(&copy).refuse_at("reseat::regularize::seal_copy")
        })();
        if let Err(refusal) = copied {
            let _ = self.unlink_temp();
            return Err(refusal);
        }
        let after = Identity::of(&target.metadata().refuse_at("reseat::regularize::target")?);
        let staged = Identity::of(&copy.metadata().refuse_at("reseat::regularize::staged")?);
        Ok((staged, before, after))
    }

    /// After the exchange (or a crash after it): the copy at the path, the
    /// link under the temporary name. Settle by the target's and the
    /// copy's identities.
    fn settle(&self, mut row: Regular) -> Result<(&'static str, Option<BulkloadRefusal>)> {
        let target_now = std::fs::metadata(&row.resolved)
            .ok()
            .map(|meta| Identity::of(&meta));
        let copy_now = identity_at(self.link)?;
        let link_at_temp = Self::is_link_to(&self.temp, &row.target);
        let target_same = target_now == Some(row.target_identity);
        if target_same || copy_now != Some(row.regular) {
            // The target held still (the copy is it), or the copy has been
            // written since (it is the destination's now): the copy stays.
            if link_at_temp {
                self.unlink_temp()?;
            }
            self.seal()?;
            let (state, word, refusal) = if target_same {
                (RegularState::Done, "regularized", None)
            } else {
                (
                    RegularState::Raced,
                    "raced-kept",
                    Some(BulkloadRefusal::ReseatRegularizeRaced),
                )
            };
            // The row keeps the staged identity: `--undo` removes only the
            // copy exactly as it was made.
            row.state = state;
            self.ledger.put_regular(&row)?;
            return Ok((word, refusal));
        }
        // The target changed and the copy did not: put the link back.
        if link_at_temp {
            self.exchange()
                .refuse_at("reseat::regularize::exchange_back")?;
            if identity_at(&self.temp)? == Some(row.regular) {
                self.unlink_temp()?;
            }
        }
        self.seal()?;
        self.ledger.delete_regular(self.link)?;
        Ok((
            "raced-restored",
            Some(BulkloadRefusal::ReseatRegularizeRaced),
        ))
    }

    /// Finish what a crash left under an `intent` row; `None` when the
    /// link is as it was (the pass decides again).
    fn settle_intent(
        &self,
        row: Regular,
    ) -> Result<Option<(&'static str, Option<BulkloadRefusal>)>> {
        let at_path = identity_at(self.link)?;
        let exchanged =
            at_path.is_some_and(|now| now.dev == row.regular.dev && now.ino == row.regular.ino);
        if exchanged {
            return self.settle(row).map(Some);
        }
        // Not exchanged: the temporary, if any, is the staged copy.
        if identity_at(&self.temp)? == Some(row.regular) {
            self.unlink_temp()?;
            self.seal()?;
        }
        self.ledger.delete_regular(self.link)?;
        Ok(None)
    }

    fn regularize(&self) -> Result<(&'static str, Option<BulkloadRefusal>)> {
        let Some(link_meta) = lstat(self.link)? else {
            return Ok(("absent", None));
        };
        if !link_meta.is_symlink() {
            return Ok(("not-a-link", None));
        }
        let text = std::fs::read_link(self.link).refuse_at("reseat::regularize::readlink")?;
        let Ok(resolved) = std::fs::canonicalize(self.link) else {
            return Ok(("dangling", Some(BulkloadRefusal::DestinationOccupied)));
        };
        if !lstat(&resolved)?.is_some_and(|meta| meta.is_file()) {
            return Ok((
                "target-not-regular",
                Some(BulkloadRefusal::DestinationOccupied),
            ));
        }
        let (staged, before, after) = self.stage(&resolved)?;
        if before != after {
            self.unlink_temp()?;
            return Ok((
                "raced-skipped",
                Some(BulkloadRefusal::ReseatRegularizeRaced),
            ));
        }
        let row = Regular {
            link: self.link.to_path_buf(),
            target: text,
            resolved,
            link_identity: Identity::of(&link_meta),
            target_identity: before,
            regular: staged,
            state: RegularState::Intent,
        };
        self.ledger.put_regular(&row)?;
        if let Err(error) = self.exchange() {
            self.unlink_temp()?;
            self.ledger.delete_regular(self.link)?;
            if crate::io::rename_unsupported(&error) {
                return Ok((
                    "exchange-unsupported",
                    Some(BulkloadRefusal::DestinationExchangeUnsupported),
                ));
            }
            return Err(error).refuse_at("reseat::regularize::exchange");
        }
        fault_point!(ReseatAfterRegularizeExchange);
        if !Self::is_link_to(&self.temp, &row.target)
            || lstat(&self.temp)?.map(|meta| Identity::of(&meta)) != Some(row.link_identity)
        {
            // Something other than the link was displaced (the path changed
            // between the read and the exchange): put it back.
            self.exchange()
                .refuse_at("reseat::regularize::exchange_back")?;
            self.unlink_temp()?;
            self.seal()?;
            self.ledger.delete_regular(self.link)?;
            return Ok((
                "raced-restored",
                Some(BulkloadRefusal::ReseatRegularizeRaced),
            ));
        }
        self.settle(row)
    }

    /// Put the link back over an unchanged copy (`--undo`).
    fn undo(&self, mut row: Regular) -> Result<(&'static str, Option<BulkloadRefusal>)> {
        // A crash inside an earlier undo: the link already back.
        if Self::is_link_to(self.link, &row.target) {
            if identity_at(&self.temp)? == Some(row.regular) {
                self.unlink_temp()?;
                self.seal()?;
            }
            row.state = RegularState::Undone;
            self.ledger.put_regular(&row)?;
            return Ok(("undone", None));
        }
        if identity_at(self.link)? != Some(row.regular) {
            return Ok(("undo-kept", Some(BulkloadRefusal::ReseatDestContinued)));
        }
        if lstat(&self.temp)?.is_some() {
            self.unlink_temp()?;
        }
        let text = std::ffi::CString::new(row.target.as_os_str().as_bytes())
            .map_err(|_| BulkloadRefusal::PathNotPortable)?;
        crate::io::sys::symlinkat(&text, &self.directory, &self.temp_c)
            .refuse_at("reseat::regularize::undo_link")?;
        self.exchange()
            .refuse_at("reseat::regularize::undo_exchange")?;
        fault_point!(ReseatAfterUndoExchange);
        if identity_at(&self.temp)? == Some(row.regular) {
            self.unlink_temp()?;
            self.seal()?;
            row.state = RegularState::Undone;
            self.ledger.put_regular(&row)?;
            Ok(("undone", None))
        } else {
            // Written between the check and the exchange: the copy back.
            self.exchange()
                .refuse_at("reseat::regularize::undo_exchange_back")?;
            self.unlink_temp()?;
            self.seal()?;
            Ok(("undo-kept", Some(BulkloadRefusal::ReseatDestContinued)))
        }
    }
}

/// Regularize every symlinked transcript under the destination `.claude`
/// root `root`, one `regularize` line each.
///
/// # Errors
/// Ledger and I/O refusals; a link's own outcome is its line.
pub fn regularize(
    root: &Path,
    ledger: &Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    // Rows a crash left in flight, whatever the link is now.
    for row in ledger.regulars()? {
        if row.state == RegularState::Intent && row.link.starts_with(root) {
            if let Some(site) = Site::new(ledger, &row.link)? {
                if let Some((word, refusal)) = site.settle_intent(row.clone())? {
                    *summary.rows.entry(format!("link-{word}")).or_default() += 1;
                    emit(&line(&row.link, word, refusal.as_ref()))?;
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
        let (word, refusal) = site.regularize()?;
        *summary.rows.entry(format!("link-{word}")).or_default() += 1;
        emit(&line(&link, word, refusal.as_ref()))?;
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
                "undo-kept",
                Some(&BulkloadRefusal::DestinationOccupied),
            ))?;
            continue;
        };
        let (word, refusal) = match row.state {
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
        undone += u64::from(word == "undone");
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"{TOOL}\",\"link\":{},\"target\":{},\"outcome\":\"{word}\",\"refused\":{}}}",
            json_string(&row.link.to_string_lossy()),
            json_string(&row.target.to_string_lossy()),
            refusal.map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
        ))?;
    }
    Ok(undone)
}
