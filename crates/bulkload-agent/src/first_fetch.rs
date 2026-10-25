//! SPEC rules 2, 7 and 8: an interrupted first object fetch retains custody
//! of its empty destination root. This is never a successful landing.
//!
//! Callers hold the move-state lock. Only ordinary main repositories use
//! this helper. Bare repositories, linked worktrees and separate Git
//! administration retain their existing admission rules.

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use crate::counters::CountedSync as _;
use crate::git_carry::landed;
use crate::refuse::RefuseAt as _;
use bulkload_proto::{BulkloadRefusal, Result};
use serde::{Deserialize, Serialize};

const SITE: &str = "estate::first_fetch";

/// The original plan identity and its mapped live destination.
pub struct Target<'a> {
    pub item: &'a str,
    pub source_label: &'a str,
    pub source_host: &'a str,
    pub source_path: &'a Path,
    pub live_path: &'a Path,
    /// Exact captured shallow-file bytes. Empty means no frontier file.
    pub expected_shallow: &'a [u8],
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Intent {
    item: String,
    source_label: String,
    source_host: String,
    source_path: PathBuf,
    live_path: PathBuf,
    expected_shallow: Vec<u8>,
    fsid: u64,
    ino: u64,
    uid: u32,
}

impl landed::Record for Intent {
    const MAGIC: &'static [u8; 16] = b"bulk-firstfetch1";
}

fn path(private: &Path, target: &Target<'_>) -> Result<PathBuf> {
    if !target.source_path.is_absolute() || !target.live_path.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    if target
        .live_path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    if target.item.len() != 64 || !target.item.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    Ok(private.join(format!("{}.first-fetch", target.item)))
}

fn identity(target: &Target<'_>) -> Result<Intent> {
    let meta = fs::symlink_metadata(target.live_path).refuse_at(SITE)?;
    if !meta.is_dir() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(Intent {
        item: target.item.to_owned(),
        source_label: target.source_label.to_owned(),
        source_host: target.source_host.to_owned(),
        source_path: target.source_path.to_path_buf(),
        live_path: target.live_path.to_path_buf(),
        expected_shallow: target.expected_shallow.to_vec(),
        fsid: landed::fsid(target.live_path)?,
        ino: meta.ino(),
        uid: meta.uid(),
    })
}

fn bound(private: &Path, target: &Target<'_>) -> Result<Option<Intent>> {
    let Some(intent) = landed::read::<Intent>(&path(private, target)?)? else {
        return Ok(None);
    };
    if intent != identity(target)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(Some(intent))
}

fn directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(BulkloadRefusal::GitDestinationOccupied),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(crate::refuse::io(&error, SITE)),
    }
}

fn children(path: &Path) -> Result<Vec<PathBuf>> {
    if !directory(path)? {
        return Ok(Vec::new());
    }
    fs::read_dir(path)
        .refuse_at(SITE)?
        .map(|entry| entry.map(|entry| entry.path()).refuse_at(SITE))
        .collect()
}

fn only(path: &Path, name: &str) -> Result<()> {
    if children(path)?
        .iter()
        .any(|entry| entry.file_name().is_none_or(|part| part != name))
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(())
}

fn hex(name: &str, widths: &[usize]) -> bool {
    widths.contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn object_files(objects: &Path) -> Result<()> {
    for entry in children(objects)? {
        let name = entry
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(BulkloadRefusal::GitDestinationOccupied)?;
        if name == "info" {
            if !children(&entry)?.is_empty() {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
        } else if name == "pack" {
            for pack in children(&entry)? {
                let name = pack
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(BulkloadRefusal::GitDestinationOccupied)?;
                let valid = name
                    .strip_prefix("pack-")
                    .and_then(|rest| rest.rsplit_once('.'))
                    .is_some_and(|(hash, extension)| {
                        hex(hash, &[40, 64])
                            && matches!(extension, "pack" | "idx" | "rev" | "bitmap" | "keep")
                    });
                if !valid || !fs::symlink_metadata(&pack).refuse_at(SITE)?.is_file() {
                    return Err(BulkloadRefusal::GitDestinationOccupied);
                }
            }
        } else if hex(name, &[2]) {
            for object in children(&entry)? {
                let valid = object
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| hex(name, &[38, 62]));
                if !valid || !fs::symlink_metadata(&object).refuse_at(SITE)?.is_file() {
                    return Err(BulkloadRefusal::GitDestinationOccupied);
                }
            }
        } else {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
    }
    Ok(())
}

fn shallow_file(path: &Path, expected: &[u8], uid: u32) -> Result<()> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(BulkloadRefusal::GitDestinationOccupied),
    };
    let before = file.metadata().refuse_at(SITE)?;
    if expected.is_empty() || !before.is_file() || before.uid() != uid {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let bound = u64::try_from(expected.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    let mut found = Vec::new();
    file.take(bound.saturating_add(1))
        .read_to_end(&mut found)
        .refuse_at(SITE)?;
    let after = fs::symlink_metadata(path).refuse_at(SITE)?;
    if found != expected
        || !after.is_file()
        || before.ino() != after.ino()
        || before.dev() != after.dev()
        || before.uid() != after.uid()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(())
}

/// Prove this source owns an interrupted object-only first fetch.
/// Missing intent means false. Foreign identity, metadata or symlinks hold.
///
/// # Errors
/// IO, a malformed intent or `GIT_DESTINATION_OCCUPIED` for foreign state.
pub fn prove(private: &Path, target: &Target<'_>) -> Result<bool> {
    let Some(intent) = bound(private, target)? else {
        return Ok(false);
    };
    only(target.live_path, ".git")?;
    let admin = target.live_path.join(".git");
    for entry in children(&admin)? {
        if entry
            .file_name()
            .is_none_or(|name| name != "objects" && name != "shallow")
        {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
    }
    shallow_file(&admin.join("shallow"), target.expected_shallow, intent.uid)?;
    object_files(&admin.join("objects"))?;
    // Detect a pathname replacement while the shape was inspected.
    bound(private, target)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    Ok(true)
}

// Publish a complete intent once, with no replacement of another writer's
// record. An interrupted temporary contains ledger bytes only.
fn publish(path: &Path, intent: &Intent) -> Result<()> {
    let bytes = landed::encode(intent)?;
    let mut generation = 0_u64;
    let (temporary, mut file) = loop {
        let temporary = path.with_extension(format!("pending-{generation}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                generation = generation
                    .checked_add(1)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            }
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    };
    file.write_all(&bytes).refuse_at(SITE)?;
    file.sync_file_counted().refuse_at(SITE)?;
    let published = fs::hard_link(&temporary, path);
    fs::remove_file(&temporary).refuse_at(SITE)?;
    published.refuse_at(SITE)?;
    landed::sync_dir(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
}

/// Claim an absent or empty root before any `.git` or payload is written.
/// The parent must already satisfy the caller's ordinary parent policy.
///
/// # Errors
/// IO, an invalid target or `GIT_DESTINATION_OCCUPIED` for foreign state.
pub fn begin(private: &Path, target: &Target<'_>) -> Result<()> {
    let record = path(private, target)?;
    if landed::read::<Intent>(&record)?.is_some() {
        return prove(private, target)?
            .then_some(())
            .ok_or(BulkloadRefusal::GitDestinationOccupied);
    }
    match fs::create_dir(target.live_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    if !children(target.live_path)?.is_empty() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let intent = identity(target)?;
    landed::sync_dir(target.live_path)?;
    landed::sync_dir(
        target
            .live_path
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    )?;
    publish(&record, &intent)?;
    prove(private, target)?
        .then_some(())
        .ok_or(BulkloadRefusal::GitDestinationOccupied)
}

/// Remove only this source's bound intent after its outcome and journal
/// are durable. The completed repository may now contain ordinary metadata.
///
/// # Errors
/// IO, a malformed intent or `GIT_DESTINATION_OCCUPIED` for another root.
pub fn finish(private: &Path, target: &Target<'_>) -> Result<()> {
    if bound(private, target)?.is_some() {
        landed::remove(&path(private, target)?)?;
    }
    Ok(())
}
