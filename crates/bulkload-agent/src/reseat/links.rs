//! Links in a live home that the homes pass resolves before it pulls
//! (operator interview and A-055, 2026-10-21).
//!
//! - **Live links** ([`live_links`]): a link in a home, at any depth, to a
//!   directory or file under a declared live-link root (sting's `agent-notes-rescue`
//!   into `/srv/data/jess/fast-local-relocated`) is the live directory
//!   itself. The pull merges the source's files there, at its real path, by
//!   the normal consolidate rules; the link stays as it is, one copy, no new
//!   bytes beside it. A link anywhere else is held, typed, as before.
//! - **Regularized links** ([`regularize`]): a link in a home into a declared
//!   regularize root (a repair stage such as
//!   `neo-claude-repair-20261001T150701Z`) is regularized before the pull:
//!   its live name becomes a real file or directory holding the target's
//!   bytes, which the pull then unions with the source's. The stage itself
//!   is never written. Each is counted, so a later pass can prove nothing in
//!   the live home references the stage ([`Regularized::remaining`]).
//!
//! The copy is made under a private name beside the link, made durable, and
//! traded for the link only while the link is the one read (its text and
//! identity); the link then goes. Bytes are copied by reading and writing,
//! never cloned (P84).

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "reseat::links";

/// The roots, each resolved to its real path (one that does not resolve is
/// dropped: nothing can be under it).
fn real_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect()
}

/// The real target of the link at `path`, when it is one and resolves
/// under one of `roots`.
fn target_under(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    if !std::fs::symlink_metadata(path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let target = std::fs::canonicalize(path).ok()?;
    roots
        .iter()
        .any(|root| target.starts_with(root))
        .then_some(target)
}

/// A link in a live home into a declared live-link root
/// ([`live_links`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveLink {
    /// Its path relative to the home's real root.
    pub rel: PathBuf,
    /// Its target, resolved to its real path.
    pub target: PathBuf,
    /// Whether the target is a directory (else a regular file).
    pub dir: bool,
}

/// Every link in the live home `root`, at any depth (links are never
/// followed), whose target is a directory or regular file under one of the
/// live-link `roots`.
///
/// Each is the live file or directory itself: the source's same path is
/// merged into its target, and the link stays (operator interview
/// 2026-10-21, "Merge in place at link targets").
#[must_use]
pub fn live_links(root: &Path, roots: &[PathBuf]) -> Vec<LiveLink> {
    let roots = real_roots(roots);
    let mut found = Vec::new();
    if roots.is_empty() {
        return found;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(target) = target_under(&path, &roots) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let dir = target.is_dir();
            if dir || target.is_file() {
                found.push(LiveLink {
                    rel: rel.to_path_buf(),
                    target,
                    dir,
                });
            }
        }
    }
    found.sort();
    found
}

/// What [`regularize`] did, or would do under a plan.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Regularized {
    /// Links regularized (or, planning, to be).
    pub links: u64,
    /// Files and bytes their copies hold.
    pub files: u64,
    pub bytes: u64,
    /// Links into the roots still in the home after the pass (refused, or
    /// planning): 0 proves nothing there references the roots.
    pub remaining: u64,
}

/// Regularize every link under the live home `root` (not following links)
/// whose target resolves under one of `roots`; `plan`: count them, write
/// nothing. `emit` gets one line per link.
///
/// # Errors
/// The I/O refusals of the walk and the copies.
pub fn regularize(
    root: &Path,
    roots: &[PathBuf],
    plan: bool,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Regularized> {
    let roots = real_roots(roots);
    let mut done = Regularized::default();
    if roots.is_empty() {
        return Ok(done);
    }
    let mut pending = vec![root.to_path_buf()];
    let mut links = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(path);
            } else if let Some(target) = target_under(&path, &roots) {
                links.push((path, target));
            }
        }
    }
    links.sort();
    for (link, target) in links {
        let (files, bytes) = size_of(&target);
        let kind = if target.is_dir() { "dir" } else { "file" };
        let regularized = if plan {
            Ok(false)
        } else {
            regularize_one(&link, &target)
        };
        let word = match &regularized {
            Ok(true) => "regularized",
            Ok(false) if plan => "planned",
            Ok(false) => "raced",
            Err(_) => "refused",
        };
        emit(&format!(
            "estate-move regularize link={} target={} kind={kind} files={files} bytes={bytes} outcome={word}{}",
            link.display(),
            target.display(),
            regularized
                .as_ref()
                .err()
                .map_or_else(String::new, |refusal| format!(" refused={}", refusal.code()))
        ))?;
        done.links += 1;
        done.files += files;
        done.bytes += bytes;
        if !matches!(regularized, Ok(true)) {
            done.remaining += 1;
        }
    }
    Ok(done)
}

/// Files and bytes under `path` (following nothing below it).
fn size_of(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, 0);
    };
    if meta.is_file() {
        return (1, meta.len());
    }
    let mut total = (0, 0);
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() {
                total.0 += 1;
                total.1 += meta.len();
            }
        }
    }
    total
}

/// Copy `from` (a regular file) to the new `to`, by reading and writing,
/// with its mode and modification time, durable.
fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let mut source = crate::hash::open_nofollow(from)?;
    let meta = source.metadata().refuse_at(SITE)?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(meta.mode() & 0o7777)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(to)
        .refuse_at(SITE)?;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = source.read(&mut buffer).refuse_at(SITE)?;
        if read == 0 {
            break;
        }
        out.write_all(buffer.get(..read).unwrap_or_default())
            .refuse_at(SITE)?;
    }
    out.set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o7777))
        .refuse_at(SITE)?;
    out.set_modified(meta.modified().refuse_at(SITE)?)
        .refuse_at(SITE)?;
    out.sync_all().refuse_at(SITE)
}

/// Copy the tree at `from` to the new `to`: files read and written, links
/// as links, directories made; durable.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from).refuse_at(SITE)?;
    if meta.is_file() {
        return copy_file(from, to);
    }
    if meta.file_type().is_symlink() {
        let text = std::fs::read_link(from).refuse_at(SITE)?;
        return std::os::unix::fs::symlink(text, to).refuse_at(SITE);
    }
    if !meta.is_dir() {
        return Err(BulkloadRefusal::DestinationKindMismatch);
    }
    std::fs::DirBuilder::new().create(to).refuse_at(SITE)?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(meta.mode() & 0o7777))
        .refuse_at(SITE)?;
    for entry in std::fs::read_dir(from).refuse_at(SITE)? {
        let entry = entry.refuse_at(SITE)?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    File::open(to)
        .and_then(|dir| dir.sync_all())
        .refuse_at(SITE)
}

/// Remove the tree at `path` (a private copy this pass made).
fn remove_tree(path: &Path) {
    let _ = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => Ok(()),
    };
}

/// Regularize the link at `link` (target `target`): `false` when the link
/// moved before the trade (nothing changed, the copy removed).
fn regularize_one(link: &Path, target: &Path) -> Result<bool> {
    let (Some(parent), Some(name)) = (link.parent(), link.file_name()) else {
        return Err(BulkloadRefusal::PathEscapesRoot);
    };
    let read = std::fs::symlink_metadata(link).refuse_at(SITE)?;
    let text = std::fs::read_link(link).refuse_at(SITE)?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".bulkload-regularize.{}", std::process::id()));
    let temp = parent.join(&temp_name);
    remove_tree(&temp);
    if let Err(refusal) = copy_tree(target, &temp) {
        remove_tree(&temp);
        return Err(refusal);
    }
    let unchanged = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|now| {
            now.file_type().is_symlink() && now.ino() == read.ino() && now.dev() == read.dev()
        }) && std::fs::read_link(path).is_ok_and(|now| now == text)
    };
    if !unchanged(link) {
        remove_tree(&temp);
        return Ok(false);
    }
    let directory = File::open(parent).refuse_at(SITE)?;
    let c = |name: &OsStr| crate::io::c_name(name.as_bytes()).refuse_at(SITE);
    if let Err(error) = crate::io::exchange(&directory, &c(&temp_name)?, &c(name)?) {
        remove_tree(&temp);
        if crate::io::rename_unsupported(&error) {
            return Err(BulkloadRefusal::DestinationExchangeUnsupported);
        }
        return Err(error).refuse_at(SITE);
    }
    // What the trade took out must be the link that was read; else it goes
    // back.
    if !unchanged(&temp) {
        crate::io::exchange(&directory, &c(&temp_name)?, &c(name)?).refuse_at(SITE)?;
        remove_tree(&temp);
        directory.sync_all().refuse_at(SITE)?;
        return Ok(false);
    }
    std::fs::remove_file(&temp).refuse_at(SITE)?;
    crate::io::durable::seal_dir(&directory).refuse_at(SITE)?;
    Ok(true)
}
