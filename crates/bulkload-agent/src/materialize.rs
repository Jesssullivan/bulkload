//! Descriptor-relative, no-clobber output publication.
//!
//! # Temporaries
//!
//! Every name this module creates first appears under a tagged temporary name
//! in its own parent directory:
//!
//! - [`Destination::file`] writes an output as `.bulkload-<tag>-<pid>-<n>`,
//!   links the final name to it and unlinks it;
//! - [`Destination::directory`] creates a directory as
//!   `.bulkload-<tag>-d-<pid>-<n>`, binds its record to that inode and renames
//!   it into place without replacement (R-N102).
//!
//! `<tag>` is 16 lowercase hex digits derived from the destination store's
//! random authority, so only a process holding that store can produce it.
//! `<pid>` and `<n>` are canonical decimals (no leading zeros). A crash inside
//! either publication leaves the temporary behind; after
//! `materialize.after_link` a file temporary is a second hard link to the
//! published output.
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

use std::ffi::{CStr, CString};
use std::fs::{File, Permissions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::freshness::StatIdentity;
use crate::transfer_store::{Manifest, PendingDirectory, Store};
use crate::{BulkloadRefusal, Result, RowSchema};

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
    directories: Vec<OwnedDirectory>,
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
        let root = open_dir(libc::AT_FDCWD, &cstring(path.as_os_str().as_bytes())?)?;
        Ok(Self {
            root,
            path: std::fs::canonicalize(path)?,
            tag: temporary_tag(&store.authority()?),
            directories: Vec::new(),
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
    /// descriptor, sync the parent, commit the record bound to that
    /// `(dev, ino)`, then rename into place without replacement. A crash
    /// therefore leaves either an empty tagged temporary (removed by a later
    /// sweep) or a final directory whose record names its inode; no state ever
    /// names only a path. A rename that finds the leaf taken refuses it as
    /// foreign and removes this invocation's temporary.
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
        if stat_at(parent.as_raw_fd(), &leaf)?.is_some() {
            return self.existing_directory(row, &parent, &leaf, &key, store);
        }
        let mode = row.mode & 0o7777;
        #[cfg(feature = "fault-injection")]
        crate::fault::note_directory(Some("rename"));
        let temporary = self.temporary(Some(DIRECTORY_MARK))?;
        // SAFETY: the descriptor and NUL-terminated name remain valid.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), temporary.as_ptr(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        fault_point!(DirectoryAfterMkdir);
        let bound = open_dir(parent.as_raw_fd(), &temporary)
            .and_then(|created| Ok(created.metadata()?))
            .and_then(|metadata| {
                parent.sync_all()?;
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
        if let Err(error) = rename_publish(&parent, &temporary, &leaf) {
            discard_directory(&parent, &temporary, &key, store);
            if rename_unsupported(&error) {
                return self.fallback_directory(row, &parent, &leaf, key, store);
            }
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        fault_point!(DirectoryAfterRename);
        parent.sync_all()?;
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

    /// Create a directory with a plain `mkdirat` at its final name, for a
    /// filesystem with no atomic no-replace rename (R-N119).
    ///
    /// `mkdirat` itself never replaces, so a taken leaf is still refused as
    /// foreign. The cost is one crash window: a crash between `mkdirat` and
    /// the record leaves an unrecorded 0700 directory that a resume refuses
    /// (`directory.after_fallback_mkdir`, a listed known violation). Every
    /// directory created this way is reported in [`Destination::created`].
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
        // SAFETY: the descriptor and NUL-terminated leaf remain valid.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        fault_point!(DirectoryAfterFallbackMkdir);
        let metadata = open_dir(parent.as_raw_fd(), leaf)?.metadata()?;
        parent.sync_all()?;
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
        let existing = open_dir(parent.as_raw_fd(), leaf)?;
        self.sweep(&existing, &row.rel_path, store)?;
        let metadata = existing.metadata()?;
        let (dev, ino) = (metadata.dev(), metadata.ino());
        let owned = match store.directory_record(key) {
            Ok(Some(record)) if record == (PendingDirectory { dev, ino, mode }) => true,
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
        let euid = effective_uid();
        let mut removed = false;
        for (name, kind) in temporary_candidates(directory)? {
            let mut rel_path = rel_dir.to_vec();
            if !rel_path.is_empty() {
                rel_path.push(b'/');
            }
            rel_path.extend_from_slice(name.as_bytes());
            let (expected, flags) = match kind {
                TemporaryName::File(tag) if tag == self.tag => (libc::S_IFREG, 0),
                TemporaryName::Directory(tag) if tag == self.tag => {
                    (libc::S_IFDIR, libc::AT_REMOVEDIR)
                }
                _ => {
                    self.swept.left.push(rel_path);
                    continue;
                }
            };
            let Some(stat) = stat_at(directory.as_raw_fd(), &name)? else {
                continue;
            };
            if stat.st_mode & libc::S_IFMT != expected || stat.st_uid != euid {
                self.swept.left.push(rel_path);
                continue;
            }
            // A directory temporary was never renamed into place, so no record
            // bound to it owns a final directory. Clear such records first:
            // once the inode is gone its number may be reused (N3).
            if expected == libc::S_IFDIR {
                store.clear_directories_bound_to(stat_dev(&stat), stat_ino(&stat))?;
            }
            // SAFETY: descriptor and NUL-terminated name are valid. `unlinkat`
            // removes this one name: with flags 0 never a directory (the inode
            // survives under any other link), with AT_REMOVEDIR only an empty
            // directory.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) } == 0 {
                self.swept.removed += 1;
                removed = true;
            } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
                self.swept.left.push(rel_path);
            }
        }
        if removed {
            directory.sync_all()?;
        }
        Ok(())
    }

    /// Apply final modes to directories this invocation created, deepest first.
    ///
    /// # Errors
    /// Refuses changed/removed directories and failed durable metadata writes.
    pub fn finish_directories(&self, store: &Store) -> Result<()> {
        for pending in self.directories.iter().rev() {
            let (parent, leaf) = self.parent(&pending.path)?;
            let directory = open_dir(parent.as_raw_fd(), &leaf)?;
            let metadata = directory.metadata()?;
            if metadata.dev() != pending.dev || metadata.ino() != pending.ino {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            directory.set_permissions(Permissions::from_mode(pending.mode))?;
            directory.sync_all()?;
            fault_point!(DirectoryBeforeComplete);
            store.complete_directory(&pending.key)?;
        }
        Ok(())
    }

    /// Preserve the literal target of a source symlink, without following it.
    ///
    /// # Errors
    /// Refuses a different existing target or any unsafe ancestor.
    pub fn symlink(&self, row: &RowSchema) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let target = row
            .link_target
            .as_ref()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let target_c = cstring(target)?;
        // SAFETY: descriptor and both NUL-terminated strings remain valid.
        let result =
            unsafe { libc::symlinkat(target_c.as_ptr(), parent.as_raw_fd(), leaf.as_ptr()) };
        if result == 0 {
            parent.sync_all()?;
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let mut bytes = vec![0_u8; target.len().saturating_add(1)];
        // SAFETY: bytes is writable for its full length; strings/descriptors valid.
        let read = unsafe {
            libc::readlinkat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if usize::try_from(read).ok() != Some(target.len())
            || bytes.get(..target.len()) != Some(target.as_slice())
        {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// Verify chunks and publish a complete file with link-at no-replace semantics.
    ///
    /// # Errors
    /// Refuses corrupt/missing chunks, size/digest mismatches or destination divergence.
    pub fn file(
        &self,
        row: &RowSchema,
        manifest: &Manifest,
        store: &Store,
    ) -> Result<StatIdentity> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => return verify_existing(file, row, manifest),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => (),
            Err(error) => return Err(error),
        }
        let temporary = self.temporary(None)?;
        // SAFETY: parent descriptor and path are valid; mode accompanies O_CREAT.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fd is a newly-created uniquely owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = write_chunks(&mut file, row, manifest, store).and_then(|()| {
            // SAFETY: all descriptors/paths valid; linkat never replaces an existing leaf.
            let linked = unsafe {
                libc::linkat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                    0,
                )
            };
            if linked != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            fault_point!(MaterializeAfterLink);
            parent.sync_all()?;
            fault_point!(MaterializeAfterParentSync);
            Ok(())
        });
        // SAFETY: remove only the unique temporary name created above.
        let removed = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
        if removed != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        result?;
        verify_existing(file, row, manifest)
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
            directory = open_dir(directory.as_raw_fd(), &component)?;
        }
        Err(BulkloadRefusal::PathEscapesRoot)
    }
}

fn write_chunks(
    file: &mut File,
    row: &RowSchema,
    manifest: &Manifest,
    store: &Store,
) -> Result<()> {
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    for chunk in &manifest.chunks {
        let data = store
            .chunk(&chunk.digest)?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        if data.len() as u64 != chunk.size {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        size = size
            .checked_add(chunk.size)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if size > row.size {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        hasher.update(&data);
        file.write_all(&data)?;
    }
    if size != row.size || *hasher.finalize().as_bytes() != manifest.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    fault_point!(MaterializeAfterTempWrite);
    file.set_permissions(Permissions::from_mode(row.mode & 0o7777))?;
    file.sync_all()?;
    fault_point!(MaterializeAfterTempSync);
    Ok(())
}

fn verify_existing(mut file: File, row: &RowSchema, manifest: &Manifest) -> Result<StatIdentity> {
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
        hasher.update(buffer.get(..read).ok_or(BulkloadRefusal::Io(None))?);
    }
    if *hasher.finalize().as_bytes() != manifest.digest
        || StatIdentity::from_metadata(&file.metadata()?) != identity
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(identity)
}

/// Best-effort undo of a directory creation that did not publish: clear its
/// record and remove the still-empty temporary. Anything left is swept later.
fn discard_directory(parent: &File, temporary: &CStr, key: &[u8], store: &Store) {
    // Ignored on purpose: the caller already refuses, a surviving record
    // names an inode that no final directory holds, and a surviving empty
    // temporary is removed by the next sweep.
    let _ = store.complete_directory(key);
    // SAFETY: descriptor and NUL-terminated name are valid; AT_REMOVEDIR
    // removes only an empty directory.
    let _ = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), libc::AT_REMOVEDIR) };
}

/// Environment variable that makes a fault-harness child's no-replace renames
/// report EINVAL, forcing the `mkdirat` fallback (R-N119).
#[cfg(feature = "fault-injection")]
pub const RENAME_UNSUPPORTED_ENV: &str = "BULKLOAD_FAULT_RENAME_UNSUPPORTED";

#[cfg(any(test, feature = "fault-injection"))]
std::thread_local! {
    static RENAME_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, this thread's no-replace renames report EINVAL, as
/// on a filesystem without them, so directory creation takes the fallback.
#[cfg(any(test, feature = "fault-injection"))]
pub fn force_rename_unsupported(on: bool) {
    RENAME_UNSUPPORTED.with(|forced| forced.set(on));
}

/// The no-replace rename, behind the test hook.
fn rename_publish(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        #[cfg(feature = "fault-injection")]
        let from_env = std::env::var_os(RENAME_UNSUPPORTED_ENV).is_some();
        #[cfg(not(feature = "fault-injection"))]
        let from_env = false;
        if from_env || RENAME_UNSUPPORTED.with(std::cell::Cell::get) {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
    }
    rename_no_replace(parent, from, to)
}

/// Whether a no-replace rename failed because this filesystem or kernel does
/// not offer it, rather than because of the names involved.
fn rename_unsupported(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
        || error.raw_os_error().is_some_and(|code| {
            [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&code)
        })
}

/// Rename `from` to `to` inside `parent`, failing with EEXIST instead of
/// replacing an existing `to`.
#[cfg(target_os = "linux")]
fn rename_no_replace(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    let fd = libc::c_long::from(parent.as_raw_fd());
    // SAFETY: renameat2 reads two NUL-terminated names relative to a valid
    // descriptor; the raw syscall avoids depending on a libc wrapper.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            fd,
            from.as_ptr(),
            fd,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Rename `from` to `to` inside `parent`, failing with EEXIST instead of
/// replacing an existing `to`.
#[cfg(target_os = "android")]
fn rename_no_replace(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    // SAFETY: both names are NUL-terminated and the descriptor is valid.
    let result = unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Rename `from` to `to` inside `parent`, failing with EEXIST instead of
/// replacing an existing `to`.
#[cfg(target_vendor = "apple")]
fn rename_no_replace(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    // SAFETY: both names are NUL-terminated and the descriptor is valid.
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// No atomic no-replace rename is known here; directory creation takes the
/// plain `mkdirat` fallback.
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn rename_no_replace(_: &File, _: &CStr, _: &CStr) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

// `dev_t` is signed on some targets; this is the same cast
// `MetadataExt::dev` makes, so the value matches what records hold.
#[allow(clippy::cast_sign_loss, clippy::unnecessary_cast)]
const fn stat_dev(stat: &libc::stat) -> u64 {
    stat.st_dev as u64
}

#[allow(clippy::useless_conversion)]
fn stat_ino(stat: &libc::stat) -> u64 {
    u64::from(stat.st_ino)
}

/// `fstatat` without following a final symlink; `None` when the name is absent.
fn stat_at(parent: i32, name: &CStr) -> Result<Option<libc::stat>> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: name is NUL-terminated and `stat` is writable for one struct.
    let result = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: a successful fstatat initialized the whole struct.
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(error.into())
    }
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// Every entry directly inside `directory` whose name is in the temporary
/// grammar. Reads through a fresh descriptor for `.`, so the caller's
/// descriptor keeps its own offset and nothing is resolved through a path.
fn temporary_candidates(directory: &File) -> Result<Vec<(CString, TemporaryName)>> {
    // SAFETY: "." is NUL-terminated; a successful descriptor is owned here.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is an open directory descriptor; fdopendir takes it over.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed, so fd is still owned here and closed once.
        unsafe { libc::close(fd) };
        return Err(error.into());
    }
    let mut found = Vec::new();
    let listed = loop {
        let checked = clear_errno();
        // SAFETY: stream is a live directory stream owned by this function.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            break match error.raw_os_error() {
                Some(code) if checked && code != 0 => Err(error),
                _ => Ok(()),
            };
        }
        // SAFETY: readdir returned a live entry whose NUL-terminated d_name
        // stays valid until the next readdir on this stream; it is copied first.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if let Some(kind) = temporary_name(name.to_bytes()) {
            found.push((name.to_owned(), kind));
        }
    };
    // SAFETY: closes the stream, and the descriptor it owns, exactly once.
    unsafe { libc::closedir(stream) };
    listed?;
    Ok(found)
}

/// Zero this thread's `errno`, so a NULL from `readdir` separates the end of
/// the stream from an error. Returns `false` on a target with no known errno
/// accessor; there a NULL is read as the end of the stream, which can only
/// shorten a sweep (leave temporaries), never widen what it removes.
fn clear_errno() -> bool {
    #[cfg(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "redox",
        target_os = "dragonfly",
        target_os = "l4re",
    ))]
    // SAFETY: __errno_location returns this thread's errno slot, valid for writes.
    unsafe {
        *libc::__errno_location() = 0;
        return true;
    }
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    // SAFETY: __error returns this thread's errno slot, valid for writes.
    unsafe {
        *libc::__error() = 0;
        return true;
    }
    #[cfg(any(
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx",
    ))]
    // SAFETY: __errno returns this thread's errno slot, valid for writes.
    unsafe {
        *libc::__errno() = 0;
        return true;
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    // SAFETY: ___errno returns this thread's errno slot, valid for writes.
    unsafe {
        *libc::___errno() = 0;
        return true;
    }
    #[allow(unreachable_code)]
    false
}

fn open_dir(parent: i32, name: &CString) -> Result<File> {
    // SAFETY: name is NUL-terminated; successful descriptor is uniquely owned.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a newly opened descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_regular(parent: &File, name: &CString) -> Result<File> {
    // SAFETY: parent and name are valid; no create flag needs a mode argument.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is uniquely owned.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(file)
}

fn cstring(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| BulkloadRefusal::PathNotPortable)
}
