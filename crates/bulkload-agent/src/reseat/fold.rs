//! Fold a re-seat's copy of an append log back into the live file it
//! duplicates (SPEC #298 rules 3 and 4, ratified 2026-10-20).
//!
//! A re-seat stages copies of transcripts beside their live names: an A-106
//! replacement's staged landing, a Q198 regularization's copy of the archive
//! target. Most are removed once their bytes are placed. Some survive a
//! race:
//! - a destination writer appended through the live name while the copy
//!   stood there;
//! - the archive target was written while it was copied.
//!
//! Such a copy is a second copy of the session on the destination, hidden
//! or not, and both it and the live file are append logs. So it is unioned
//! into the live file by [`crate::consolidate::append_log::union`] (the
//! homes' one union, ledgered in the state's `consolidate.sqlite`), and
//! removed only once the merged bytes are durable.
//!
//! Only a union that refuses holds: a live file a process holds open, or
//! one ending inside a line (`HOMES_CONSOLIDATE_RACED`), or a copy a
//! process still holds open. Then nothing is written. The copy stays under
//! a kept name, `.<name>.<token>.kept-<inode>.bulkload-reseat` (the
//! temporary name is every pass's own and would be cleared), never a new
//! copy, and every later pass at that name tries the union again
//! ([`fold_kept`]).

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::ledger::{Identity, Ledger};
use super::place::{c_name, identity_at, lstat, open_dir, temp_name};
use crate::consolidate::{append_log, Action, Live};
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The kept name of the copy of `name` whose inode is `ino`.
#[must_use]
pub fn kept_name(name: &OsStr, token: &str, ino: u64) -> OsString {
    let tag = if token.is_empty() {
        format!("kept-{ino}")
    } else {
        format!("{token}.kept-{ino}")
    };
    temp_name(name, &tag)
}

/// The path of a transcript relative to the `.claude` root: its last three
/// components (`projects/<key>/<name>`), the union's key for its records.
#[must_use]
pub fn rel_of(link: &Path) -> Vec<u8> {
    let parts: Vec<&OsStr> = link.iter().collect();
    let tail = parts
        .get(parts.len().saturating_sub(3)..)
        .unwrap_or_default();
    let mut rel = Vec::new();
    for (at, part) in tail.iter().enumerate() {
        if at > 0 {
            rel.push(b'/');
        }
        rel.extend_from_slice(part.as_bytes());
    }
    rel
}

/// The live file a name stands for: a regular file there, or the regular
/// file a link there resolves to.
#[must_use]
pub fn live_of(at: &Path) -> Option<PathBuf> {
    let meta = lstat(at).ok()??;
    if meta.is_file() {
        return Some(at.to_path_buf());
    }
    if !meta.is_symlink() {
        return None;
    }
    std::fs::canonicalize(at)
        .ok()
        .filter(|target| target.is_file())
}

/// No process holds `file` open apart from this handle
/// ([`crate::procs::holders`]); `false` where this platform cannot say.
fn free(file: &File) -> Result<bool> {
    use std::os::fd::AsRawFd as _;
    Ok(matches!(
        crate::procs::holders(&[file], &[], &[file.as_raw_fd()])?,
        Some(pids) if pids.is_empty()
    ))
}

/// Union the copy `name` in `dir` (at `copy`) into the live file `live`.
///
/// The copy is removed once the merged bytes are durable. `Some`: folded,
/// and the copy is gone. `None`: the union refused or the copy is in use,
/// and nothing was written or removed.
///
/// # Errors
/// The I/O and ledger refusals of the union and the removal.
pub fn fold(
    ledger: &Ledger,
    dir: &File,
    name: &OsStr,
    copy: &Path,
    live: &Path,
    rel: &[u8],
) -> Result<Option<Action>> {
    let Some(meta) = lstat(copy)?.filter(std::fs::Metadata::is_file) else {
        return Ok(None);
    };
    let read = Identity::of(&meta);
    let (Some(live_parent), Some(live_name)) = (live.parent(), live.file_name()) else {
        return Ok(None);
    };
    let file = crate::hash::open_nofollow(copy)?;
    if !free(&file)? {
        return Ok(None);
    }
    let live_dir = open_dir(live_parent)?;
    let outcome = append_log::union_stream(
        &Live {
            dir: &live_dir,
            name: live_name.as_bytes(),
            path: live,
            rel,
        },
        append_log::Feed::File(&file),
        meta.len(),
        ledger.union_ledger()?,
    );
    let action = match outcome {
        Ok(outcome) if !matches!(outcome.action, Action::Held(_)) => outcome.action,
        Ok(_)
        | Err(
            BulkloadRefusal::HomesConsolidateRaced
            | BulkloadRefusal::HomesConsolidateHolderUnknown
            | BulkloadRefusal::DestinationKindMismatch
            | BulkloadRefusal::PathEscapesRoot
            | BulkloadRefusal::DestinationExchangeUnsupported,
        ) => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    if matches!(action, Action::Adopted | Action::Covered) {
        // Nothing written: the live file's own bytes hold the copy's, made
        // durable before the copy goes.
        crate::hash::open_nofollow(live)?
            .sync_all()
            .refuse_at("reseat::fold::sync")?;
    }
    // The copy is still what was unioned, and no process writes it.
    if identity_at(copy)? != Some(read) || !free(&file)? {
        return Ok(None);
    }
    crate::io::sys::unlinkat(dir, &c_name(name)?, false).refuse_at("reseat::fold::unlink")?;
    crate::io::durable::seal_dir(dir).refuse_at("reseat::fold::seal")?;
    counters::bump(Counter::ReseatCopyFolded);
    Ok(Some(action))
}

/// Move the copy `temp` (at `temp_path`, a copy of `name`) to its kept name,
/// never replacing anything: the same file renamed, not a new copy.
///
/// # Errors
/// The I/O refusal of the rename.
pub fn keep(
    ledger: &Ledger,
    dir: &File,
    temp: &OsStr,
    temp_path: &Path,
    name: &OsStr,
) -> Result<Option<PathBuf>> {
    let Some(meta) = lstat(temp_path)?.filter(std::fs::Metadata::is_file) else {
        return Ok(None);
    };
    let ino = std::os::unix::fs::MetadataExt::ino(&meta);
    let kept = kept_name(name, ledger.temp_token(), ino);
    crate::io::publish_noreplace(dir, &c_name(temp)?, &c_name(&kept)?)
        .map_err(|error| crate::refuse::io_in(&error, temp_path, "reseat::fold::keep"))?;
    crate::io::durable::seal_dir(dir).refuse_at("reseat::fold::seal")?;
    Ok(Some(temp_path.with_file_name(kept)))
}

/// Fold the copy under the temporary name into `live`; keep it under its
/// kept name when the union refuses. `None`: folded (or nothing there).
///
/// # Errors
/// As [`fold`] and [`keep`].
pub fn fold_or_keep(
    ledger: &Ledger,
    dir: &File,
    temp_path: &Path,
    name: &OsStr,
    live: Option<&Path>,
    rel: &[u8],
) -> Result<Option<PathBuf>> {
    let Some(temp) = temp_path.file_name() else {
        return Ok(None);
    };
    if lstat(temp_path)?.is_none_or(|meta| !meta.is_file()) {
        return Ok(None);
    }
    if let Some(live) = live {
        if fold(ledger, dir, temp, temp_path, live, rel)?.is_some() {
            return Ok(None);
        }
    }
    keep(ledger, dir, temp, temp_path, name)
}

/// Every kept copy of `at`'s name in its directory, folded into the live
/// file `at` stands for where the union takes it; the ones still held.
///
/// # Errors
/// The listing's and [`fold`]'s refusals.
pub fn fold_kept(ledger: &Ledger, at: &Path) -> Result<Vec<PathBuf>> {
    let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
        return Ok(Vec::new());
    };
    let mut prefix = b".".to_vec();
    prefix.extend_from_slice(name.as_bytes());
    prefix.push(b'.');
    let token = ledger.temp_token();
    if !token.is_empty() {
        prefix.extend_from_slice(token.as_bytes());
        prefix.push(b'.');
    }
    prefix.extend_from_slice(b"kept-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).refuse_at("reseat::fold::list"),
    };
    let mut kept: Vec<OsString> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name())
        .filter(|found| {
            let bytes = found.as_bytes();
            bytes.starts_with(&prefix) && bytes.ends_with(b".bulkload-reseat")
        })
        .collect();
    if kept.is_empty() {
        return Ok(Vec::new());
    }
    kept.sort();
    let live = live_of(at);
    let dir = open_dir(parent)?;
    let rel = rel_of(at);
    let mut held = Vec::new();
    for copy in kept {
        let path = parent.join(&copy);
        let folded = match &live {
            Some(live) => fold(ledger, &dir, &copy, &path, live, &rel)?,
            None => None,
        };
        if folded.is_none() {
            held.push(path);
        }
    }
    Ok(held)
}
