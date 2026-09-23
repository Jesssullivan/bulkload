//! Descriptor-relative, no-clobber output publication.
//!
//! # Temporaries
//!
//! [`Destination::file`] writes each output under a temporary name in the
//! output's own directory, links the final name to it and unlinks it. The name
//! is `.bulkload-<tag>-<pid>-<n>`: `<tag>` is 16 lowercase hex digits derived
//! from the destination store's random authority, so only a process holding
//! that store can produce it. A crash inside `file` leaves the name behind;
//! after `materialize.after_link` it is a second hard link to the published
//! output.
//!
//! A later invocation removes such a name only when every check holds (R-N79):
//!
//! - it sits in a directory this state is materializing into (the root, or an
//!   existing directory row of the carry), swept while the store's exclusive
//!   publisher is held, so no temporary of this store is in flight;
//! - the leaf matches the grammar exactly and carries this store's tag;
//! - `fstatat(AT_SYMLINK_NOFOLLOW)`, relative to that directory's descriptor,
//!   reports a regular file owned by the effective uid.
//!
//! Removal is `unlinkat` of the temporary name alone, relative to the same
//! descriptor, so a published name sharing its inode is never touched. A
//! grammar match that fails any check (another store's tag, the untagged form
//! earlier engines generated, a symlink, a directory, another owner) is left
//! in place and reported by [`Destination::swept`]. A name outside the grammar
//! is not a temporary at all and is never considered.

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

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// Prefix of every temporary name [`Destination::file`] publishes through.
pub const TEMPORARY_PREFIX: &[u8] = b".bulkload-";
/// Hex digits in a temporary's store tag.
const TAG_HEX: usize = 16;
/// Key-derivation context for the temporary tag.
const TAG_CONTEXT: &str = "bulkload 2026-09-23 materialize temporary tag v1";

/// A leaf name in the temporary-name grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporaryName {
    /// `.bulkload-<16 hex>-<pid>-<n>`, carrying one store's tag.
    Tagged([u8; TAG_HEX]),
    /// `.bulkload-<pid>-<n>`, the untagged form earlier engines generated.
    Untagged,
}

/// Classify `leaf` under the temporary-name grammar; `None` for any name
/// the materializer never generates.
#[must_use]
pub fn temporary_name(leaf: &[u8]) -> Option<TemporaryName> {
    let rest = leaf.strip_prefix(TEMPORARY_PREFIX)?;
    let digits =
        |part: &[u8]| (1..=20).contains(&part.len()) && part.iter().all(u8::is_ascii_digit);
    let mut parts = rest.split(|byte| *byte == b'-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(tag), Some(pid), Some(serial), None)
            if digits(pid)
                && digits(serial)
                && tag
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)) =>
        {
            tag.try_into().ok().map(TemporaryName::Tagged)
        }
        (Some(pid), Some(serial), None, None) if digits(pid) && digits(serial) => {
            Some(TemporaryName::Untagged)
        }
        _ => None,
    }
}

/// The tag this destination store's temporaries carry.
pub(crate) fn temporary_tag(authority: &[u8; 32]) -> [u8; TAG_HEX] {
    use std::fmt::Write as _;
    let mut tag = String::with_capacity(TAG_HEX);
    for byte in blake3::derive_key(TAG_CONTEXT, authority)
        .iter()
        .take(TAG_HEX / 2)
    {
        let _ = write!(tag, "{byte:02x}");
    }
    tag.as_bytes().try_into().unwrap_or([b'0'; TAG_HEX])
}

/// What the temporary sweep did in this invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// Temporaries of this store removed by name.
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
        })
    }

    /// Remove this store's orphaned temporaries from the root. Call only
    /// while the store's exclusive publisher is held.
    ///
    /// # Errors
    /// Refuses an unreadable root directory.
    pub fn sweep_root(&mut self) -> Result<()> {
        let root = self.root.try_clone()?;
        self.sweep(&root, &[])
    }

    /// What the temporary sweep has done so far.
    #[must_use]
    pub const fn swept(&self) -> &Sweep {
        &self.swept
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
    /// The pending-directory intent is durable before `mkdirat` (R-N78), so a
    /// crash at any point leaves either no directory, or a directory with a
    /// record naming it. A resume adopts an existing directory only when its
    /// record owns it: the inode it was bound to, or, for an intent never
    /// bound, the exact shape `mkdirat` produced (mode 0700, owned by this
    /// user) at the recorded path. A directory with no record keeps the old
    /// rule and fails closed on a divergent mode. An existing directory is
    /// swept of this store's orphaned temporaries; see the module docs.
    ///
    /// # Errors
    /// Refuses non-directory conflicts and divergent existing modes.
    pub fn directory(&mut self, row: &RowSchema, store: &Store, authority: &[u8]) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let key = postcard::to_stdvec(&(authority, &row.rel_path))?;
        let mode = row.mode & 0o7777;
        if stat_at(parent.as_raw_fd(), &leaf)?.is_some() {
            let record = store.directory_record(&key)?;
            return self.existing_directory(row, &parent, &leaf, key, record, store);
        }
        store.record_directory_intent(&key, mode)?;
        fault_point!(DirectoryAfterIntent);
        // SAFETY: both descriptors and the NUL-terminated leaf remain valid.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) };
        if created == 0 {
            fault_point!(DirectoryAfterMkdir);
            let metadata = open_dir(parent.as_raw_fd(), &leaf)?.metadata()?;
            store.record_directory_created(&key, metadata.dev(), metadata.ino(), mode)?;
            fault_point!(DirectoryAfterPendingRecord);
            self.directories.push(OwnedDirectory {
                path: row.rel_path.clone(),
                mode,
                dev: metadata.dev(),
                ino: metadata.ino(),
                key,
            });
            parent.sync_all()?;
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // The leaf appeared after the existence check, so this invocation did
        // not create it; its intent must never let a resume adopt it.
        store.complete_directory(&key)?;
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        self.existing_directory(row, &parent, &leaf, key, None, store)
    }

    fn existing_directory(
        &mut self,
        row: &RowSchema,
        parent: &File,
        leaf: &CString,
        key: Vec<u8>,
        record: Option<PendingDirectory>,
        store: &Store,
    ) -> Result<()> {
        let mode = row.mode & 0o7777;
        let existing = open_dir(parent.as_raw_fd(), leaf)?;
        self.sweep(&existing, &row.rel_path)?;
        let metadata = existing.metadata()?;
        let (dev, ino) = (metadata.dev(), metadata.ino());
        let owned = match record {
            Some(PendingDirectory::Created {
                dev: recorded_dev,
                ino: recorded_ino,
                mode: recorded_mode,
            }) => recorded_dev == dev && recorded_ino == ino && recorded_mode == mode,
            Some(PendingDirectory::Intent {
                mode: recorded_mode,
            }) => {
                recorded_mode == mode
                    && metadata.mode() & 0o7777 == 0o700
                    && metadata.uid() == effective_uid()
            }
            None => false,
        };
        if owned {
            if matches!(record, Some(PendingDirectory::Intent { .. })) {
                store.record_directory_created(&key, dev, ino, mode)?;
            }
            self.directories.push(OwnedDirectory {
                path: row.rel_path.clone(),
                mode,
                dev,
                ino,
                key,
            });
        } else if metadata.mode() & 0o7777 != mode {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// Unlink this store's orphaned temporaries directly inside `directory`,
    /// by name and relative to its descriptor; record every other grammar
    /// match and leave it alone. See the module docs.
    fn sweep(&mut self, directory: &File, rel_dir: &[u8]) -> Result<()> {
        let euid = effective_uid();
        let mut removed = false;
        for (name, kind) in temporary_candidates(directory)? {
            let mut rel_path = rel_dir.to_vec();
            if !rel_path.is_empty() {
                rel_path.push(b'/');
            }
            rel_path.extend_from_slice(name.as_bytes());
            if kind != TemporaryName::Tagged(self.tag) {
                self.swept.left.push(rel_path);
                continue;
            }
            let Some(stat) = stat_at(directory.as_raw_fd(), &name)? else {
                continue;
            };
            if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_uid != euid {
                self.swept.left.push(rel_path);
                continue;
            }
            // SAFETY: descriptor and NUL-terminated name are valid. With flags
            // 0 `unlinkat` removes this one name and never a directory; the
            // inode survives under any other name linked to it.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } == 0 {
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
        let mut temporary = TEMPORARY_PREFIX.to_vec();
        temporary.extend_from_slice(&self.tag);
        temporary.extend_from_slice(
            format!(
                "-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        );
        let temporary = cstring(&temporary)?;
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
        clear_errno();
        // SAFETY: stream is a live directory stream owned by this function.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            break match error.raw_os_error() {
                Some(0) | None => Ok(()),
                Some(_) => Err(error),
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn clear_errno() {
    // SAFETY: __errno_location returns this thread's errno slot, valid for writes.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn clear_errno() {
    // SAFETY: __error returns this thread's errno slot, valid for writes.
    unsafe { *libc::__error() = 0 };
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
