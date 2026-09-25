//! Descriptor-relative, no-clobber output publication.
//!
//! # Temporaries
//!
//! Every name this module creates first appears under a tagged temporary name
//! in its own parent directory:
//!
//! - [`Destination::stage`] writes an output as `.bulkload-<tag>-<pid>-<n>`;
//!   its group commit seals it and publishes it under the final name without
//!   replacement: an exclusive rename, or `linkat` then `unlinkat` where the
//!   filesystem has none (R-N119);
//! - [`Destination::directory`] creates a directory as
//!   `.bulkload-<tag>-d-<pid>-<n>`, binds its record to that inode and renames
//!   it into place without replacement (R-N102).
//!
//! `<tag>` is 16 lowercase hex digits derived from the destination store's
//! random authority, so only a process holding that store can produce it.
//! `<pid>` and `<n>` are canonical decimals (no leading zeros). A crash inside
//! either publication leaves the temporary behind.
//!
//! A later invocation removes such a name only when every check holds (R-N79):
//!
//! - it sits in a directory this state is materializing into (the root, or an
//!   existing directory row of the carry), swept while the store's exclusive
//!   publisher is held, so no temporary of this store is in flight;
//! - the leaf matches the grammar exactly and carries this store's tag;
//! - `fstatat(AT_SYMLINK_NOFOLLOW)`, relative to that directory's descriptor,
//!   reports the kind the grammar names (a regular file, or a directory)
//!   owned by the effective uid.
//!
//! Removal is `unlinkat` of the temporary name alone, relative to the same
//! descriptor: a published name sharing a file's inode is never touched, and a
//! directory goes only if it is empty (`AT_REMOVEDIR` refuses anything else).
//! A grammar match that fails any check (another store's tag, the untagged
//! form earlier engines generated, the wrong kind, another owner, a non-empty
//! directory) is left in place and reported by [`Destination::swept`]. A name
//! outside the grammar is not a temporary at all and is never considered.
//! Untagged `.bulkload-<pid>-<n>` names left by engines before the tagged
//! grammar are therefore reported, never removed; the operator removes them.
//!
//! # Durability
//!
//! Directory entries are sealed (`io::durable::seal_dir`) rather than fully
//! flushed one by one. A record naming a directory is committed only after
//! the entry it depends on is sealed and, when that entry lives on a device
//! other than the store's, fully flushed: the store commit's own full flush
//! drains only the store's device.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read as _, Seek as _};
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::counters::{self, CountedSync as _, Counter};
use crate::freshness::StatIdentity;
use crate::transfer_store::{
    ChunkHint, Manifest, OutputRecord, PendingDirectory, Store, StorePublisher,
};
use crate::{BulkloadRefusal, Result, RowSchema};

#[cfg(any(test, feature = "fault-injection"))]
pub use crate::io::force_rename_unsupported;
#[cfg(feature = "fault-injection")]
pub use crate::io::RENAME_UNSUPPORTED_ENV;

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Prefix of every temporary name this module publishes through.
pub const TEMPORARY_PREFIX: &[u8] = b".bulkload-";
/// Hex digits in a temporary's store tag.
const TAG_HEX: usize = 16;
/// Key-derivation context for the temporary tag.
const TAG_CONTEXT: &str = "bulkload 2026-09-23 materialize temporary tag v1";
/// Marks a directory temporary: `.bulkload-<tag>-d-<pid>-<n>`.
const DIRECTORY_MARK: &[u8] = b"d";

/// A leaf name in the temporary-name grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporaryName {
    /// `.bulkload-<16 hex>-<pid>-<n>`: an output file, carrying one store's tag.
    File([u8; TAG_HEX]),
    /// `.bulkload-<16 hex>-d-<pid>-<n>`: a directory, carrying one store's tag.
    Directory([u8; TAG_HEX]),
    /// `.bulkload-<pid>-<n>`, the untagged form earlier engines generated.
    /// Never removed and never excluded from a walk: it proves nothing.
    Untagged,
}

impl TemporaryName {
    /// The store tag, for a tagged name.
    #[must_use]
    pub const fn tag(self) -> Option<[u8; TAG_HEX]> {
        match self {
            Self::File(tag) | Self::Directory(tag) => Some(tag),
            Self::Untagged => None,
        }
    }
}

/// A canonical decimal `u64`: digits only, no sign, no leading zero.
fn canonical_decimal(part: &[u8]) -> bool {
    std::str::from_utf8(part)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .is_some_and(|value| value.to_string().as_bytes() == part)
}

fn tag_of(part: &[u8]) -> Option<[u8; TAG_HEX]> {
    if part
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        part.try_into().ok()
    } else {
        None
    }
}

/// Classify `leaf` under the temporary-name grammar; `None` for any name
/// the materializer never generates.
#[must_use]
pub fn temporary_name(leaf: &[u8]) -> Option<TemporaryName> {
    let rest = leaf.strip_prefix(TEMPORARY_PREFIX)?;
    let parts: Vec<&[u8]> = rest.split(|byte| *byte == b'-').collect();
    match parts.as_slice() {
        [tag, mark, pid, serial]
            if *mark == DIRECTORY_MARK && canonical_decimal(pid) && canonical_decimal(serial) =>
        {
            tag_of(tag).map(TemporaryName::Directory)
        }
        [tag, pid, serial] if canonical_decimal(pid) && canonical_decimal(serial) => {
            tag_of(tag).map(TemporaryName::File)
        }
        [pid, serial] if canonical_decimal(pid) && canonical_decimal(serial) => {
            Some(TemporaryName::Untagged)
        }
        _ => None,
    }
}

/// The tag this destination store's temporaries carry.
pub(crate) fn temporary_tag(authority: &[u8; 32]) -> [u8; TAG_HEX] {
    const fn hex(nibble: u8) -> u8 {
        if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        }
    }
    let key = blake3::derive_key(TAG_CONTEXT, authority);
    let mut tag = [0_u8; TAG_HEX];
    for (pair, byte) in tag.chunks_exact_mut(2).zip(key) {
        if let [high, low] = pair {
            *high = hex(byte >> 4);
            *low = hex(byte & 0x0f);
        }
    }
    tag
}

/// What the temporary sweep did in this invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// Temporaries of this store removed by name, files and empty directories.
    pub removed: u64,
    /// Grammar matches left in place because they failed a check, by
    /// destination-relative path.
    pub left: Vec<Vec<u8>>,
}

/// An opened destination root; descendants never traverse symlinks.
pub struct Destination {
    root: File,
    path: PathBuf,
    tag: [u8; TAG_HEX],
    /// Device of the destination store, whose commits drain only it.
    store_device: u64,
    /// The most recent staged file's parent, shared by its siblings so each
    /// directory costs one descriptor rather than one per file.
    last_parent: std::cell::RefCell<Option<(Vec<u8>, Arc<File>)>>,
    directories: Vec<OwnedDirectory>,
    /// Devices holding a directory entry sealed by barrier only, awaiting a
    /// full flush, each with one descriptor on it.
    unflushed: std::collections::HashMap<u64, File>,
    swept: Sweep,
    created: Creation,
}

/// How this invocation created directories (R-N119).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Creation {
    /// Directories published by the no-replace rename of a tagged temporary.
    pub renamed: u64,
    /// Directories created by the plain `mkdirat` fallback because the
    /// filesystem has no atomic no-replace rename, by relative path.
    pub fallback: Vec<Vec<u8>>,
}

struct OwnedDirectory {
    path: Vec<u8>,
    mode: u32,
    dev: u64,
    ino: u64,
    key: Vec<u8>,
}

impl Destination {
    /// Open an existing destination directory without following its final
    /// link, naming temporaries with `store`'s tag.
    ///
    /// # Errors
    /// Refuses a missing root or symlink root, or a store without authority.
    pub fn open(path: &Path, store: &Store) -> Result<Self> {
        let root = File::from(crate::io::sys::open_dir_path_nofollow(path)?);
        Ok(Self {
            root,
            path: std::fs::canonicalize(path)?,
            tag: temporary_tag(&store.authority()?),
            store_device: std::fs::metadata(store.root())?.dev(),
            last_parent: std::cell::RefCell::new(None),
            directories: Vec::new(),
            unflushed: std::collections::HashMap::new(),
            swept: Sweep::default(),
            created: Creation::default(),
        })
    }

    /// Remove this store's orphaned temporaries from the root. Call only
    /// while the store's exclusive publisher is held.
    ///
    /// # Errors
    /// Refuses an unreadable root directory.
    pub fn sweep_root(&mut self, store: &Store) -> Result<()> {
        let root = self.root.try_clone()?;
        self.sweep(&root, &[], store)
    }

    /// What the temporary sweep has done so far.
    #[must_use]
    pub const fn swept(&self) -> &Sweep {
        &self.swept
    }

    /// How this invocation has created directories so far.
    #[must_use]
    pub const fn created(&self) -> &Creation {
        &self.created
    }

    /// Canonical destination root for completion-store namespacing.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Identity of an existing regular output. No symlink is followed.
    ///
    /// # Errors
    /// Refuses a conflicting node or unsafe ancestor.
    pub fn identity(&self, row: &RowSchema) -> Result<Option<StatIdentity>> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => Ok(Some(StatIdentity::from_metadata(&file.metadata()?))),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Create a directory, retaining an existing directory's metadata.
    ///
    /// Creation is create-then-rename (R-N102): `mkdirat` under a tagged
    /// temporary name, `fstat` through an `O_NOFOLLOW | O_DIRECTORY`
    /// descriptor, seal the parent (and fully flush it when it is not on the
    /// store's device), commit the record bound to that `(dev, ino)`, then
    /// rename into place without replacement and seal the parent again. A
    /// crash therefore leaves either an empty tagged temporary (removed by a
    /// later sweep) or a final directory whose record names its inode; no
    /// state ever names only a path. A rename that finds the leaf taken
    /// refuses it as foreign and removes this invocation's temporary.
    ///
    /// An existing directory is adopted only when its record names its inode
    /// and mode. Any other record is cleared, so it can never adopt later. A
    /// directory with no owning record fails closed on a divergent mode. An
    /// existing directory is swept of this store's orphaned temporaries.
    ///
    /// # Errors
    /// Refuses non-directory conflicts, divergent existing modes and foreign
    /// directories that appear during creation.
    pub fn directory(&mut self, row: &RowSchema, store: &Store, authority: &[u8]) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let key = postcard::to_stdvec(&(authority, &row.rel_path))?;
        if stat_at(&parent, &leaf)?.is_some() {
            return self.existing_directory(row, &parent, &leaf, &key, store);
        }
        let mode = row.mode & 0o7777;
        #[cfg(feature = "fault-injection")]
        crate::fault::note_directory(Some("rename"));
        let temporary = self.temporary(Some(DIRECTORY_MARK))?;
        crate::io::sys::mkdirat(&parent, &temporary, 0o700)?;
        fault_point!(DirectoryAfterMkdir);
        let bound = open_dir(&parent, &temporary)
            .and_then(|created| Ok(created.metadata()?))
            .and_then(|metadata| {
                self.seal_entry(&parent)?;
                store.record_directory_created(&key, metadata.dev(), metadata.ino(), mode)?;
                Ok(metadata)
            });
        let metadata = match bound {
            Ok(metadata) => metadata,
            Err(refusal) => {
                discard_directory(&parent, &temporary, &key, store);
                return Err(refusal);
            }
        };
        fault_point!(DirectoryAfterPendingRecord);
        if let Err(error) = crate::io::rename_exclusive(&parent, &temporary, &leaf) {
            discard_directory(&parent, &temporary, &key, store);
            if crate::io::rename_unsupported(&error) {
                return self.fallback_directory(row, &parent, &leaf, key, store);
            }
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        fault_point!(DirectoryAfterRename);
        crate::io::durable::seal_dir(&parent)?;
        self.note_unflushed(parent);
        self.created.renamed += 1;
        self.directories.push(OwnedDirectory {
            path: row.rel_path.clone(),
            mode,
            dev: metadata.dev(),
            ino: metadata.ino(),
            key,
        });
        Ok(())
    }

    /// Seal `parent`'s entries ahead of a record that depends on them, with a
    /// full flush when `parent` is not on the store's device.
    fn seal_entry(&self, parent: &File) -> Result<()> {
        crate::io::durable::seal_dir(parent)?;
        if parent.metadata()?.dev() != self.store_device {
            full_flush_counted(parent)?;
        }
        Ok(())
    }

    /// Create a directory with a plain `mkdirat` at its final name, for a
    /// filesystem with no atomic no-replace rename (R-N119).
    ///
    /// `mkdirat` itself never replaces, so a taken leaf is still refused as
    /// foreign. The record comes first: an *intent* (a directory record bound
    /// to no inode, see [`INTENT`]) is committed before the `mkdirat`, then
    /// bound to the new inode once its entry is sealed. A crash between the
    /// two leaves a directory the intent owns, which a resume adopts when it
    /// is still what `mkdirat` made: an empty 0700 directory owned by this
    /// user (`directory.after_fallback_mkdir`). A `mkdirat` that fails clears
    /// the intent at once, so it never claims a directory someone else made.
    /// Every directory created this way is reported in
    /// [`Destination::created`].
    fn fallback_directory(
        &mut self,
        row: &RowSchema,
        parent: &File,
        leaf: &CString,
        key: Vec<u8>,
        store: &Store,
    ) -> Result<()> {
        let mode = row.mode & 0o7777;
        #[cfg(feature = "fault-injection")]
        crate::fault::note_directory(Some("fallback"));
        store.record_directory_created(&key, INTENT.0, INTENT.1, mode)?;
        if let Err(error) = crate::io::sys::mkdirat(parent, leaf, 0o700) {
            let _ = store.complete_directory(&key);
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        fault_point!(DirectoryAfterFallbackMkdir);
        let metadata = open_dir(parent, leaf)?.metadata()?;
        self.seal_entry(parent)?;
        store.record_directory_created(&key, metadata.dev(), metadata.ino(), mode)?;
        self.created.fallback.push(row.rel_path.clone());
        self.directories.push(OwnedDirectory {
            path: row.rel_path.clone(),
            mode,
            dev: metadata.dev(),
            ino: metadata.ino(),
            key,
        });
        Ok(())
    }

    fn existing_directory(
        &mut self,
        row: &RowSchema,
        parent: &File,
        leaf: &CString,
        key: &[u8],
        store: &Store,
    ) -> Result<()> {
        let mode = row.mode & 0o7777;
        let existing = open_dir(parent, leaf)?;
        self.sweep(&existing, &row.rel_path, store)?;
        let metadata = existing.metadata()?;
        let (dev, ino) = (metadata.dev(), metadata.ino());
        let owned = match store.directory_record(key) {
            Ok(Some(record)) if record == (PendingDirectory { dev, ino, mode }) => true,
            // A fallback `mkdirat` whose intent committed but whose binding
            // did not (R-N119): adopt the directory only while it is still
            // exactly what `mkdirat` left, then bind the record to it.
            Ok(Some(record))
                if record
                    == (PendingDirectory {
                        dev: INTENT.0,
                        ino: INTENT.1,
                        mode,
                    })
                    && fresh_fallback(&existing, &metadata)? =>
            {
                // The crashed run's `mkdirat` entry was never sealed: seal it
                // (fully flushing a parent off the store's device) before the
                // record binds its inode, so no durable record names an entry
                // a power loss can still lose (#74 review, B1).
                self.seal_entry(parent)?;
                store.record_directory_created(key, dev, ino, mode)?;
                true
            }
            // A record that does not own this directory is stale; clear it
            // so it can never adopt one later (R-N102, F3).
            Ok(Some(_)) | Err(BulkloadRefusal::SchemaMismatch) => {
                store.complete_directory(key)?;
                false
            }
            Ok(None) => false,
            Err(refusal) => return Err(refusal),
        };
        if owned {
            self.directories.push(OwnedDirectory {
                path: row.rel_path.clone(),
                mode,
                dev,
                ino,
                key: key.to_vec(),
            });
        } else if metadata.mode() & 0o7777 != mode {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// A fresh temporary name in this store's grammar; `mark` names a kind.
    fn temporary(&self, mark: Option<&[u8]>) -> Result<CString> {
        let mut name = TEMPORARY_PREFIX.to_vec();
        name.extend_from_slice(&self.tag);
        if let Some(mark) = mark {
            name.push(b'-');
            name.extend_from_slice(mark);
        }
        name.extend_from_slice(
            format!(
                "-{}-{}",
                std::process::id(),
                NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        );
        cstring(&name)
    }

    /// Remove this store's orphaned temporaries directly inside `directory`,
    /// by name and relative to its descriptor; record every other grammar
    /// match and leave it alone. See the module docs.
    fn sweep(&mut self, directory: &File, rel_dir: &[u8], store: &Store) -> Result<()> {
        let euid = crate::io::sys::effective_uid();
        let mut removed = false;
        for (name, kind) in temporary_candidates(directory)? {
            let mut rel_path = rel_dir.to_vec();
            if !rel_path.is_empty() {
                rel_path.push(b'/');
            }
            rel_path.extend_from_slice(name.as_bytes());
            let want_directory = match kind {
                TemporaryName::File(tag) if tag == self.tag => false,
                TemporaryName::Directory(tag) if tag == self.tag => true,
                _ => {
                    self.swept.left.push(rel_path);
                    continue;
                }
            };
            let Some(stat) = stat_at(directory, &name)? else {
                continue;
            };
            let kind_matches = if want_directory {
                stat.is_dir()
            } else {
                stat.is_file()
            };
            if !kind_matches || stat.uid != euid {
                self.swept.left.push(rel_path);
                continue;
            }
            // A directory temporary was never renamed into place, so no record
            // bound to it owns a final directory. Clear such records first:
            // once the inode is gone its number may be reused (N3).
            if want_directory {
                store.clear_directories_bound_to(stat.node.dev, stat.node.ino)?;
            }
            // `unlinkat` removes this one name: without AT_REMOVEDIR never a
            // directory (the inode survives under any other link), with it
            // only an empty directory.
            match crate::io::sys::unlinkat(directory, &name, want_directory) {
                Ok(()) => {
                    self.swept.removed += 1;
                    removed = true;
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                Err(_) => self.swept.left.push(rel_path),
            }
        }
        if removed {
            directory.sync_dir_counted()?;
        }
        Ok(())
    }

    /// Apply final modes to directories this invocation created, deepest
    /// first, one directory at a time: set the mode, seal it (fully flushing a
    /// directory off the store's device), then commit its completion. Each
    /// completion commit drains the store's device.
    ///
    /// One descriptor is open at a time, so any number of new directories
    /// finish within the descriptor limit.
    ///
    /// # Errors
    /// Refuses changed/removed directories and failed durable metadata writes.
    pub fn finish_directories(&mut self, store: &Store) -> Result<()> {
        if self.directories.is_empty() {
            return Ok(());
        }
        for index in (0..self.directories.len()).rev() {
            let Some(pending) = self.directories.get(index) else {
                continue;
            };
            let (path, dev, ino, mode) =
                (pending.path.clone(), pending.dev, pending.ino, pending.mode);
            let (parent, leaf) = self.parent(&path)?;
            let directory = open_dir(&parent, &leaf)?;
            drop(parent);
            let metadata = directory.metadata()?;
            if metadata.dev() != dev || metadata.ino() != ino {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            crate::io::sys::fchmod(&directory, mode)?;
            self.seal_entry(&directory)?;
            fault_point!(DirectoryBeforeComplete);
            if let Some(pending) = self.directories.get(index) {
                store.complete_directory(&pending.key)?;
            }
            counters::bump(Counter::DirectoriesFinished);
        }
        // The completion commits drained the store's device; entries sealed
        // on other devices still wait for `flush_session`.
        self.unflushed.remove(&self.store_device);
        Ok(())
    }

    fn note_unflushed(&mut self, directory: File) {
        if let Ok(metadata) = directory.metadata() {
            self.unflushed.entry(metadata.dev()).or_insert(directory);
        }
    }

    /// End a session: fully flush each device that still holds a directory
    /// or symlink entry sealed only by a barrier, so those entries reach
    /// stable media.
    ///
    /// # Errors
    /// Returns the flush failure.
    pub fn flush_session(&mut self) -> Result<()> {
        for (_, handle) in self.unflushed.drain() {
            full_flush_counted(&handle)?;
        }
        Ok(())
    }

    /// Preserve the literal target of a source symlink, without following it.
    ///
    /// # Errors
    /// Refuses a different existing target or any unsafe ancestor.
    pub fn symlink(&mut self, row: &RowSchema) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let target = row
            .link_target
            .as_ref()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let target_c = cstring(target)?;
        match crate::io::sys::symlinkat(&target_c, &parent, &leaf) {
            Ok(()) => {
                crate::io::durable::seal_dir(&parent)?;
                self.note_unflushed(parent);
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let mut bytes = vec![0_u8; target.len().saturating_add(1)];
        // Any failure (`EINVAL`: the leaf is a file or a directory; `ENOENT`:
        // it went away) means the leaf is not this symlink: refused as
        // occupied, as before the move to `io::sys` (#74 review, B2).
        let Ok(read) = crate::io::sys::readlinkat(&parent, &leaf, &mut bytes) else {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        };
        if read != target.len() || bytes.get(..target.len()) != Some(target.as_slice()) {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// An existing regular output at `row`'s path with its parent directory,
    /// opened without following any link, or `None` when the path is free.
    ///
    /// # Errors
    /// Refuses a conflicting node or unsafe ancestor.
    pub(crate) fn existing(&self, row: &RowSchema) -> Result<Option<(File, Arc<File>)>> {
        let (parent, leaf) = self.shared_parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => Ok(Some((file, parent))),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Create a private temporary file (`O_EXCL`, mode 0600) beside `row`'s
    /// leaf. The caller writes it and hands it to a [`PublishSink`].
    ///
    /// # Errors
    /// Refuses an unsafe ancestor or a failed create.
    pub(crate) fn stage(&self, row: &RowSchema) -> Result<StagedFile> {
        let (parent, leaf) = self.shared_parent(&row.rel_path)?;
        let temporary = self.temporary(None)?;
        let file = File::from(crate::io::sys::create_excl_at(
            parent.as_fd(),
            &temporary,
            0o600,
        )?);
        Ok(StagedFile {
            parent,
            temporary,
            leaf,
            file: Arc::new(file),
        })
    }

    /// Open a published output read-only by relative path, component by
    /// component, following no link.
    ///
    /// # Errors
    /// Refuses a missing output, a non-regular node or an unsafe ancestor.
    pub(crate) fn open_output(&self, rel_path: &[u8]) -> Result<File> {
        let (parent, leaf) = self.parent(rel_path)?;
        open_regular(&parent, &leaf)
    }

    fn shared_parent(&self, path: &[u8]) -> Result<(Arc<File>, CString)> {
        let directory = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .and_then(|end| path.get(..end))
            .unwrap_or_default();
        if let Some((cached, parent)) = self.last_parent.borrow().as_ref() {
            if cached.as_slice() == directory {
                let leaf = path
                    .get(directory.len()..)
                    .map(|rest| rest.strip_prefix(b"/").unwrap_or(rest))
                    .ok_or(BulkloadRefusal::PathEscapesRoot)?;
                if leaf.is_empty() || leaf == b"." || leaf == b".." {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
                return Ok((Arc::clone(parent), cstring(leaf)?));
            }
        }
        let (parent, leaf) = self.parent(path)?;
        let parent = Arc::new(parent);
        self.last_parent
            .replace(Some((directory.to_vec(), Arc::clone(&parent))));
        Ok((parent, leaf))
    }

    fn parent(&self, path: &[u8]) -> Result<(File, CString)> {
        let mut parts = path.split(|byte| *byte == b'/').peekable();
        let mut directory = self.root.try_clone()?;
        while let Some(part) = parts.next() {
            if part.is_empty() || part == b"." || part == b".." {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            let component = cstring(part)?;
            if parts.peek().is_none() {
                return Ok((directory, component));
            }
            directory = open_dir(&directory, &component)?;
        }
        Err(BulkloadRefusal::PathEscapesRoot)
    }
}

/// A destination file written under a private temporary name, published
/// by a [`PublishSink`] once its data is sealed.
pub(crate) struct StagedFile {
    parent: Arc<File>,
    temporary: CString,
    leaf: CString,
    file: Arc<File>,
}

impl StagedFile {
    /// The open temporary file. It stays readable after publication.
    pub(crate) const fn file(&self) -> &Arc<File> {
        &self.file
    }

    /// Remove the temporary name, abandoning the file.
    ///
    /// # Errors
    /// Returns a failed unlink.
    pub(crate) fn discard(self) -> Result<()> {
        unlink(&self.parent, &self.temporary)
    }

    /// Seal the data, then rename into place without replacing anything.
    /// The identity is taken from the open file after the rename.
    fn publish(self) -> Result<(StatIdentity, Arc<File>)> {
        if let Err(error) = crate::io::durable::seal_file(&self.file) {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(error.into());
        }
        fault_point!(MaterializeAfterTempSeal);
        if let Err(error) = crate::io::publish_noreplace(&self.parent, &self.temporary, &self.leaf)
        {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        fault_point!(MaterializeAfterRename);
        counters::bump(Counter::FilesMaterialized);
        Ok((
            StatIdentity::from_metadata(&self.file.metadata()?),
            self.parent,
        ))
    }
}

fn full_flush_counted(handle: &File) -> Result<()> {
    Ok(counters::timed(
        Counter::FlushFull,
        Counter::FlushFullNs,
        || crate::io::sys::full_flush(handle),
    )?)
}

fn unlink(parent: &File, name: &CString) -> Result<()> {
    // Without AT_REMOVEDIR this removes only a non-directory entry.
    Ok(crate::io::sys::unlinkat(parent, name, false)?)
}

/// One destination output for a group commit.
pub(crate) enum Publication {
    /// A fully written staged file to seal, rename into place and record.
    Staged {
        staged: StagedFile,
        record: PendingOutput,
    },
    /// An existing output already verified against its manifest, with the
    /// descriptor it was verified through and its parent directory. Its data
    /// and entry are sealed like a written file's before the record commits.
    Adopted {
        record: OutputRecord,
        file: File,
        parent: Arc<File>,
    },
}

/// The record for a staged file, completed with its identity once published.
pub(crate) struct PendingOutput {
    pub key: Vec<u8>,
    pub rel_path: Vec<u8>,
    pub size: u64,
    pub hints: Vec<ChunkHint>,
}

/// Group-commit sink for a destination: seal each file, rename it into
/// place, seal each touched directory once, then commit every record of the
/// group in one transaction.
pub(crate) struct PublishSink {
    publisher: StorePublisher,
    /// Device of the store. Its commit's full flush drains only this device.
    store_device: u64,
    outcomes: Vec<(Vec<u8>, Result<()>)>,
}

impl PublishSink {
    /// `publisher` holds the destination store's single-writer guard.
    ///
    /// # Errors
    /// Refuses if the store root cannot be stat'ed.
    pub(crate) fn new(publisher: StorePublisher) -> Result<Self> {
        let store_device = std::fs::metadata(publisher.store().root())?.dev();
        Ok(Self {
            publisher,
            store_device,
            outcomes: Vec::new(),
        })
    }

    /// Treat the store as if it lived on `device` (tests of the
    /// cross-device flush without a second volume).
    #[cfg(test)]
    const fn assume_store_device(mut self, device: u64) -> Self {
        self.store_device = device;
        self
    }
}

/// One descriptor per device a group touched, for its full flush.
#[derive(Default)]
struct TouchedDevices {
    directories: Vec<Arc<File>>,
    seen: std::collections::HashSet<(u64, u64)>,
    devices: std::collections::HashMap<u64, Arc<File>>,
}

impl TouchedDevices {
    fn directory(&mut self, directory: Arc<File>) {
        if let Ok(metadata) = directory.metadata() {
            self.devices
                .entry(metadata.dev())
                .or_insert_with(|| Arc::clone(&directory));
            if !self.seen.insert((metadata.dev(), metadata.ino())) {
                return;
            }
        }
        self.directories.push(directory);
    }

    /// Seal every touched directory once, then, in group mode, fully flush
    /// each touched device other than `store_device`: the store commit that
    /// follows drains only its own device. Returns the devices flushed.
    fn seal(&self, store_device: u64) -> Result<usize> {
        for directory in &self.directories {
            crate::io::durable::seal_dir(directory)?;
        }
        let mut flushed = 0;
        if crate::io::durable::durability() == crate::io::durable::Durability::Group {
            for (device, handle) in &self.devices {
                if *device != store_device {
                    full_flush_counted(handle)?;
                    flushed += 1;
                }
            }
        }
        Ok(flushed)
    }
}

impl crate::io::durable::GroupSink for PublishSink {
    const SIDE: crate::io::durable::GroupSide = crate::io::durable::GroupSide::Destination;
    type Item = Publication;
    /// `(relative path, outcome)` for every submitted output, in commit order.
    type Report = Vec<(Vec<u8>, Result<()>)>;

    fn weight(item: &Publication) -> (u64, u64) {
        match item {
            Publication::Staged { record, .. } => (1, record.size),
            Publication::Adopted { .. } => (1, 0),
        }
    }

    fn commit(&mut self, items: Vec<Publication>) {
        #[cfg(feature = "fault-injection")]
        let _note = {
            let ids: Vec<usize> = (0..items.len()).collect();
            let chunks = items
                .iter()
                .map(|item| match item {
                    Publication::Staged { record, .. } => record.hints.len(),
                    Publication::Adopted { .. } => 0,
                })
                .sum();
            crate::fault::note_group(&ids, chunks)
        };
        let mut records = Vec::with_capacity(items.len());
        let mut touched = TouchedDevices::default();
        for item in items {
            match item {
                Publication::Staged { staged, record } => match staged.publish() {
                    Ok((identity, parent)) => {
                        touched.directory(parent);
                        records.push(OutputRecord {
                            key: record.key,
                            rel_path: record.rel_path,
                            identity,
                            hints: record.hints,
                        });
                    }
                    Err(refusal) => self.outcomes.push((record.rel_path, Err(refusal))),
                },
                Publication::Adopted {
                    record,
                    file,
                    parent,
                } => match crate::io::durable::seal_file(&file) {
                    Ok(()) => {
                        touched.directory(parent);
                        records.push(record);
                    }
                    Err(error) => self.outcomes.push((record.rel_path, Err(error.into()))),
                },
            }
        }
        let committed = touched.seal(self.store_device).and_then(|_| {
            fault_point_in!(
                PublishDestinationAfterDirSeal,
                self.publisher.store().root()
            );
            self.publisher.commit_outputs(&records)
        });
        for record in records {
            self.outcomes.push((record.rel_path, committed.clone()));
        }
    }

    fn failure(&self) -> Option<BulkloadRefusal> {
        None
    }

    fn finish(self) -> Self::Report {
        self.outcomes
    }
}

/// Verify an existing output byte-for-byte against `manifest` before adopting it.
///
/// Only the adopt path uses this; freshly written outputs are built from
/// verified chunks and are not read back.
///
/// # Errors
/// Refuses a size, mode or digest difference, or a change while reading.
pub(crate) fn verify_existing(
    file: &File,
    row: &RowSchema,
    manifest: &Manifest,
) -> Result<StatIdentity> {
    let mut file = file;
    file.rewind()?;
    let before = file.metadata()?;
    if before.len() != row.size || before.mode() & 0o7777 != row.mode & 0o7777 {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let identity = StatIdentity::from_metadata(&before);
    let mut buffer = vec![0; 256 * 1024];
    let mut hasher = blake3::Hasher::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        counters::add_len(Counter::DestVerifyRead, read);
        counters::update(
            &mut hasher,
            Counter::HashVerifyExisting,
            buffer.get(..read).ok_or(BulkloadRefusal::Io(None))?,
        );
    }
    if *hasher.finalize().as_bytes() != manifest.digest
        || StatIdentity::from_metadata(&file.metadata()?) != identity
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(identity)
}

/// The `(dev, ino)` of a directory record committed before its `mkdirat`
/// (R-N119). No inode on Darwin or Linux has number 0.
const INTENT: (u64, u64) = (0, 0);

/// Whether `directory` is still exactly what the fallback `mkdirat` made: an
/// empty directory with mode 0700, owned by this user.
fn fresh_fallback(directory: &File, metadata: &std::fs::Metadata) -> Result<bool> {
    Ok(metadata.is_dir()
        && metadata.mode() & 0o7777 == 0o700
        && metadata.uid() == crate::io::sys::effective_uid()
        && crate::io::sys::list_dir(directory)?.is_empty())
}

/// Best-effort undo of a directory creation that did not publish: clear its
/// record and remove the still-empty temporary. Anything left is swept later.
fn discard_directory(parent: &File, temporary: &CStr, key: &[u8], store: &Store) {
    // Ignored on purpose: the caller already refuses, a surviving record
    // names an inode that no final directory holds, and a surviving empty
    // temporary is removed by the next sweep.
    let _ = store.complete_directory(key);
    // AT_REMOVEDIR removes only an empty directory.
    let _ = crate::io::sys::unlinkat(parent, temporary, true);
}

/// `fstatat` without following a final symlink; `None` when the name is absent.
fn stat_at(parent: &File, name: &CStr) -> Result<Option<crate::io::Stat>> {
    match crate::io::sys::fstatat_nofollow(parent, name) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Every entry directly inside `directory` whose name is in the temporary
/// grammar, read through a fresh descriptor for `.` (see `sys::list_dir`).
fn temporary_candidates(directory: &File) -> Result<Vec<(CString, TemporaryName)>> {
    Ok(crate::io::sys::list_dir(directory)?
        .into_iter()
        .filter_map(|name| temporary_name(name.to_bytes()).map(|kind| (name, kind)))
        .collect())
}

fn open_dir(parent: &File, name: &CStr) -> Result<File> {
    Ok(File::from(crate::io::sys::open_dir_at(parent, name)?))
}

fn open_regular(parent: &File, name: &CStr) -> Result<File> {
    let file = File::from(crate::io::sys::open_read_at(parent, name)?);
    if !file.metadata()?.is_file() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(file)
}

fn cstring(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| BulkloadRefusal::PathNotPortable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::durable::GroupSink as _;
    use crate::transfer_store::PublisherSide;
    use std::io::Write as _;

    #[test]
    fn publish_never_replaces_an_output_that_appeared_meanwhile() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "bulkload-materialize-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&destination)?;
        std::fs::write(source.join("file"), b"ours")?;
        let row = crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )?
        .rows
        .into_iter()
        .next()
        .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let target = Destination::open(&destination, &Store::open(&base.join("state"))?)?;
        let staged = target.stage(&row)?;
        (&**staged.file()).write_all(b"ours")?;
        std::fs::write(destination.join("file"), b"theirs")?;
        let mut sink = PublishSink::new(
            Store::open(&base.join("state"))?.into_publisher(PublisherSide::Destination)?,
        )?
        .assume_store_device(u64::MAX);
        sink.commit(vec![Publication::Staged {
            staged,
            record: PendingOutput {
                key: b"key".to_vec(),
                rel_path: b"file".to_vec(),
                size: 4,
                hints: Vec::new(),
            },
        }]);
        let report = sink.finish();
        let listed = std::fs::read_dir(&destination)?.count();
        let kept = std::fs::read(destination.join("file"))?;
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            report,
            [(
                b"file".to_vec(),
                Err(BulkloadRefusal::GitDestinationOccupied)
            )]
        );
        assert_eq!(kept, b"theirs");
        assert_eq!(listed, 1, "the temporary name is removed");
        Ok(())
    }

    /// PR #59 review (5), mutant M10: a group whose directory seal fails
    /// must commit no record, so no output is ever recorded ahead of the
    /// entry that names it.
    #[test]
    fn a_failed_directory_seal_commits_no_record() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "bulkload-seal-failure-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&destination)?;
        std::fs::write(source.join("file"), b"ours")?;
        let row = crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )?
        .rows
        .into_iter()
        .next()
        .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let target = Destination::open(&destination, &Store::open(&base.join("state"))?)?;
        let staged = target.stage(&row)?;
        (&**staged.file()).write_all(b"ours")?;
        let mut sink = PublishSink::new(
            Store::open(&base.join("state"))?.into_publisher(PublisherSide::Destination)?,
        )?;
        crate::io::durable::fail_dir_seals(true);
        sink.commit(vec![Publication::Staged {
            staged,
            record: PendingOutput {
                key: b"key".to_vec(),
                rel_path: b"file".to_vec(),
                size: 4,
                hints: Vec::new(),
            },
        }]);
        crate::io::durable::fail_dir_seals(false);
        let report = sink.finish();
        let identity = StatIdentity::from_metadata(&std::fs::metadata(destination.join("file"))?);
        let recorded = Store::open(&base.join("state"))?.output_matches(b"key", &identity)?;
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            report,
            [(b"file".to_vec(), Err(BulkloadRefusal::Io(Some(libc::EIO))))]
        );
        assert!(
            !recorded,
            "no record may commit after a failed directory seal"
        );
        Ok(())
    }

    /// #74 review, B2: a symlink whose leaf is already a regular file or a
    /// directory is refused as occupied, never as an I/O fault (`EINVAL`
    /// from `readlinkat`), as before the move to `io::sys`.
    #[test]
    fn a_symlink_onto_an_occupied_leaf_refuses_as_occupied() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "bulkload-symlink-occupied-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&destination)?;
        std::os::unix::fs::symlink("target", source.join("as-file"))?;
        std::os::unix::fs::symlink("target", source.join("as-dir"))?;
        std::fs::write(destination.join("as-file"), b"someone else's")?;
        std::fs::create_dir(destination.join("as-dir"))?;
        let store = Store::open(&base.join("state"))?;
        let mut target = Destination::open(&destination, &store)?;
        let mut codes = Vec::new();
        for row in crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )?
        .rows
        {
            codes.push(target.symlink(&row).err().map(|refusal| refusal.code()));
        }
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            codes,
            [
                Some("GIT_DESTINATION_OCCUPIED"),
                Some("GIT_DESTINATION_OCCUPIED")
            ]
        );
        Ok(())
    }

    #[test]
    fn a_group_fully_flushes_each_touched_device_the_store_is_not_on() -> Result<()> {
        let directory = Arc::new(File::open(std::env::temp_dir())?);
        let device = directory.metadata()?.dev();
        let mut touched = TouchedDevices::default();
        touched.directory(Arc::clone(&directory));
        touched.directory(directory);
        assert_eq!(touched.directories.len(), 1, "one seal per directory");
        // Same device as the store: its commit drains the device.
        assert_eq!(touched.seal(device)?, 0);
        // Store elsewhere (PR #59 review): one full flush on this device.
        assert_eq!(touched.seal(device.wrapping_add(1))?, 1);
        Ok(())
    }

    #[test]
    fn publish_falls_back_to_link_where_exclusive_rename_is_unsupported() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "bulkload-rename-fallback-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&destination)?;
        std::fs::write(source.join("free"), b"ours")?;
        std::fs::write(source.join("taken"), b"ours")?;
        let rows = crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )?
        .rows;
        std::fs::write(destination.join("taken"), b"theirs")?;
        let target = Destination::open(&destination, &Store::open(&base.join("state"))?)?;
        let mut publications = Vec::new();
        for row in &rows {
            let staged = target.stage(row)?;
            (&**staged.file()).write_all(b"ours")?;
            publications.push(Publication::Staged {
                staged,
                record: PendingOutput {
                    key: row.rel_path.clone(),
                    rel_path: row.rel_path.clone(),
                    size: 4,
                    hints: Vec::new(),
                },
            });
        }
        let before = counters::Counters::snapshot();
        let mut sink = PublishSink::new(
            Store::open(&base.join("state"))?.into_publisher(PublisherSide::Destination)?,
        )?;
        crate::io::force_rename_unsupported(true);
        sink.commit(publications);
        crate::io::force_rename_unsupported(false);
        let mut report = sink.finish();
        report.sort_by(|left, right| left.0.cmp(&right.0));
        let fallbacks = counters::Counters::snapshot()
            .since(before)
            .get(Counter::PublishLinkFallback);
        let free = std::fs::read(destination.join("free"))?;
        let taken = std::fs::read(destination.join("taken"))?;
        let listed = std::fs::read_dir(&destination)?.count();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            report,
            [
                (b"free".to_vec(), Ok(())),
                (
                    b"taken".to_vec(),
                    Err(BulkloadRefusal::GitDestinationOccupied)
                ),
            ]
        );
        assert!(fallbacks >= 1, "the fallback path is recorded");
        assert_eq!(free, b"ours");
        assert_eq!(taken, b"theirs", "the fallback never replaces");
        assert_eq!(listed, 2, "no temporary is left behind");
        Ok(())
    }
}

#[cfg(all(test, feature = "io-trace"))]
mod adoption_power_loss;
