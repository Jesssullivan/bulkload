//! Descriptor-relative, no-clobber output publication.

use std::ffi::CString;
use std::fs::{File, Permissions};
use std::io::{Read as _, Seek as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::transfer_store::{ChunkHint, Manifest, OutputRecord, Store, StorePublisher};
use crate::{BulkloadRefusal, Result, RowSchema};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// An opened destination root; descendants never traverse symlinks.
pub struct Destination {
    root: File,
    path: PathBuf,
    directories: Vec<PendingDirectory>,
    /// A directory entry was sealed by barrier only and awaits a full flush.
    unflushed: bool,
}

struct PendingDirectory {
    path: Vec<u8>,
    mode: u32,
    dev: u64,
    ino: u64,
    key: Vec<u8>,
}

impl Destination {
    /// Open an existing destination directory without following its final link.
    ///
    /// # Errors
    /// Refuses a missing root or symlink root.
    pub fn open(path: &Path) -> Result<Self> {
        let root = open_dir(libc::AT_FDCWD, &cstring(path.as_os_str().as_bytes())?)?;
        Ok(Self {
            root,
            path: std::fs::canonicalize(path)?,
            directories: Vec::new(),
            unflushed: false,
        })
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
    /// # Errors
    /// Refuses non-directory conflicts and divergent existing modes.
    pub fn directory(&mut self, row: &RowSchema, store: &Store, authority: &[u8]) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let key = postcard::to_stdvec(&(authority, &row.rel_path))?;
        let mode = row.mode & 0o7777;
        // SAFETY: both descriptors and the NUL-terminated leaf remain valid.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) };
        if created == 0 {
            let metadata = open_dir(parent.as_raw_fd(), &leaf)?.metadata()?;
            store.pending_directory(&key, metadata.dev(), metadata.ino(), mode, true)?;
            self.directories.push(PendingDirectory {
                path: row.rel_path.clone(),
                mode,
                dev: metadata.dev(),
                ino: metadata.ino(),
                key,
            });
            crate::io::durable::seal_dir(&parent)?;
            self.unflushed = true;
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let existing = open_dir(parent.as_raw_fd(), &leaf)?;
        let metadata = existing.metadata()?;
        if store.pending_directory(&key, metadata.dev(), metadata.ino(), mode, false)? {
            self.directories.push(PendingDirectory {
                path: row.rel_path.clone(),
                mode,
                dev: metadata.dev(),
                ino: metadata.ino(),
                key,
            });
        } else if metadata.mode() & 0o7777 != mode {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// Apply final modes to directories this invocation created, deepest first.
    ///
    /// # Errors
    /// Refuses changed/removed directories and failed durable metadata writes.
    pub fn finish_directories(&mut self, store: &Store) -> Result<()> {
        for pending in self.directories.iter().rev() {
            let (parent, leaf) = self.parent(&pending.path)?;
            let directory = open_dir(parent.as_raw_fd(), &leaf)?;
            let metadata = directory.metadata()?;
            if metadata.dev() != pending.dev || metadata.ino() != pending.ino {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            directory.set_permissions(Permissions::from_mode(pending.mode))?;
            crate::io::durable::seal_dir(&directory)?;
            store.complete_directory(&pending.key)?;
            counters::bump(Counter::DirectoriesFinished);
        }
        if !self.directories.is_empty() {
            // Each completion commit above was a full flush issued after every
            // barrier of this session.
            self.unflushed = false;
        }
        Ok(())
    }

    /// End a session: when a directory or symlink was sealed only by a
    /// barrier, issue the session's one full flush on the root so those
    /// entries reach stable media.
    ///
    /// # Errors
    /// Returns the flush failure.
    pub fn flush_session(&mut self) -> Result<()> {
        if self.unflushed {
            counters::timed(Counter::FlushFull, Counter::FlushFullNs, || {
                crate::io::sys::full_flush(&self.root)
            })?;
            self.unflushed = false;
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
        // SAFETY: descriptor and both NUL-terminated strings remain valid.
        let result =
            unsafe { libc::symlinkat(target_c.as_ptr(), parent.as_raw_fd(), leaf.as_ptr()) };
        if result == 0 {
            crate::io::durable::seal_dir(&parent)?;
            self.unflushed = true;
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

    /// An existing regular output at `row`'s path, opened without following
    /// any link, or `None` when the path is free.
    ///
    /// # Errors
    /// Refuses a conflicting node or unsafe ancestor.
    pub(crate) fn existing(&self, row: &RowSchema) -> Result<Option<File>> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => Ok(Some(file)),
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
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let temporary = cstring(
            format!(
                ".bulkload-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
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
        let file = unsafe { File::from_raw_fd(fd) };
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

/// A destination file written under a private temporary name, published
/// by a [`PublishSink`] once its data is sealed.
pub(crate) struct StagedFile {
    parent: File,
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
    fn publish(self) -> Result<(StatIdentity, File)> {
        if let Err(error) = crate::io::durable::seal_file(&self.file) {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(error.into());
        }
        if let Err(error) =
            crate::io::sys::rename_noreplace(&self.parent, &self.temporary, &self.leaf)
        {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                BulkloadRefusal::GitDestinationOccupied
            } else {
                error.into()
            });
        }
        counters::bump(Counter::FilesMaterialized);
        Ok((
            StatIdentity::from_metadata(&self.file.metadata()?),
            self.parent,
        ))
    }
}

fn unlink(parent: &File, name: &CString) -> Result<()> {
    // SAFETY: parent descriptor and NUL-terminated name remain valid; flag 0
    // removes only a non-directory entry.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// One destination output for a group commit.
pub(crate) enum Publication {
    /// A fully written staged file to seal, rename into place and record.
    Staged {
        staged: StagedFile,
        record: PendingOutput,
    },
    /// An existing output already verified against its manifest.
    Adopted(OutputRecord),
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
    outcomes: Vec<(Vec<u8>, Result<()>)>,
}

impl PublishSink {
    /// `publisher` holds the destination store's single-writer guard.
    pub(crate) const fn new(publisher: StorePublisher) -> Self {
        Self {
            publisher,
            outcomes: Vec::new(),
        }
    }
}

impl crate::io::durable::GroupSink for PublishSink {
    type Item = Publication;
    /// `(relative path, outcome)` for every submitted output, in commit order.
    type Report = Vec<(Vec<u8>, Result<()>)>;

    fn weight(item: &Publication) -> (u64, u64) {
        match item {
            Publication::Staged { record, .. } => (1, record.size),
            Publication::Adopted(_) => (1, 0),
        }
    }

    fn commit(&mut self, items: Vec<Publication>) {
        let mut records = Vec::with_capacity(items.len());
        let mut directories: Vec<File> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in items {
            match item {
                Publication::Staged { staged, record } => match staged.publish() {
                    Ok((identity, parent)) => {
                        if let Ok(metadata) = parent.metadata() {
                            if seen.insert((metadata.dev(), metadata.ino())) {
                                directories.push(parent);
                            }
                        } else {
                            directories.push(parent);
                        }
                        records.push(OutputRecord {
                            key: record.key,
                            rel_path: record.rel_path,
                            identity,
                            hints: record.hints,
                        });
                    }
                    Err(refusal) => self.outcomes.push((record.rel_path, Err(refusal))),
                },
                Publication::Adopted(record) => records.push(record),
            }
        }
        let sealed = directories.iter().try_for_each(|directory| {
            crate::io::durable::seal_dir(directory).map_err(BulkloadRefusal::from)
        });
        let committed = sealed.and_then(|()| self.publisher.store().commit_outputs(&records));
        for record in records {
            self.outcomes.push((record.rel_path, committed.clone()));
        }
    }

    fn finish(self) -> Self::Report {
        self.outcomes
    }
}

/// Verify an existing output byte-for-byte against `manifest` before adopting
/// it. Only the adopt path uses this; freshly written outputs are built from
/// verified chunks and are not read back.
///
/// # Errors
/// Refuses a size, mode or digest difference, or a change while reading.
pub(crate) fn verify_existing(
    mut file: File,
    row: &RowSchema,
    manifest: &Manifest,
) -> Result<StatIdentity> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::durable::GroupSink as _;
    use std::io::Write as _;

    #[test]
    fn publish_never_replaces_an_output_that_appeared_meanwhile() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "bulkload-materialize-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
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
        let target = Destination::open(&destination)?;
        let staged = target.stage(&row)?;
        (&**staged.file()).write_all(b"ours")?;
        std::fs::write(destination.join("file"), b"theirs")?;
        let mut sink = PublishSink::new(Store::open(&base.join("state"))?.into_publisher()?);
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
}
