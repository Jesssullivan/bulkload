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
//! # Superseding publish (WP0(d), OI-1003-Q18, #187)
//!
//! A changed seat's new bytes replace the output at its path only when that
//! output is this store's own, untouched: its `(dev, ino, size, mtime,
//! ctime)` equals a row this store committed for the path
//! ([`owned_output`]). Anything else stays no-clobber and refuses
//! `DESTINATION_OCCUPIED`.
//!
//! The replacement is the exchange design the formal model checks
//! (`docs/formal`, `MC_wp0d_exchange`), not check-then-rename, which leaves
//! a window a third party's write falls into (`MC_wp0d_check_rename`). In
//! the group's commit ([`PublishSink`]):
//!
//! 1. the staged file is sealed, and the output at the leaf is opened and
//!    must still have the owned identity;
//! 2. one store commit records the group's [`SupersedeIntent`]s and takes
//!    the outputs' rows out of the store, so no row vouches for a path
//!    while either file may sit at it;
//! 3. the staged name and the leaf trade files in one atomic
//!    [`crate::io::exchange`]: nothing is replaced or removed by it;
//! 4. the file now under the staged name, the displaced one, is checked:
//!    this store's own output (the same inode, size and mtime as in step 1)
//!    is removed; any other file is exchanged back, the directory sealed,
//!    and the entry refused;
//! 5. the group's directories are sealed and its commit writes the new
//!    output's row and deletes the intent.
//!
//! A power loss anywhere leaves the leaf holding the whole old output or the
//! whole new one, and no row for it that describes other bytes. The next
//! session's sweep settles an intent a crash left, before any entry of that
//! directory is decided ([`Destination::sweep`]): the old output still in
//! place gets its rows back; the new one in place is adopted like any
//! unrowed output (#169); a displaced file that is neither is exchanged
//! back, or, when the leaf no longer holds the staged file, left where it is
//! and reported, never removed.
//!
//! One limit: between the last look at the old output and the exchange, a
//! third party that rewrites it in place and restores its mtime is not
//! seen, because the exchange itself moves the displaced inode's ctime. A
//! plain write (the mtime moves) or a replacement (another inode) is.
//!
//! # Durability
//!
//! Directory entries are sealed (`io::durable::seal_dir`) rather than fully
//! flushed one by one. A record naming a directory is committed only after
//! the entry it depends on is sealed and, when that entry lives on a device
//! other than the store's, fully flushed: the store commit's own full flush
//! drains only the store's device.

use crate::refuse::RefuseAt as _;
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
    identity_bytes, ChunkHint, Manifest, OutputRecord, OutputRow, PendingDirectory, Store,
    StorePublisher, SupersedeIntent, SupersedeSettle,
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
    /// Whether each device probed so far offers the atomic exchange a
    /// superseding publish needs ([`Destination::exchange_supported`]).
    exchange: std::cell::RefCell<std::collections::HashMap<u64, bool>>,
    directories: Vec<OwnedDirectory>,
    /// Devices holding a directory entry sealed by barrier only, awaiting a
    /// full flush, each with one descriptor on it.
    unflushed: std::collections::HashMap<u64, File>,
    swept: Sweep,
    created: Creation,
    /// This store's orphaned file temporaries, kept for their chunks: bytes
    /// the destination already holds are not sent again within the session.
    /// Every one is indexed, however many: a session can leave up to a whole
    /// committer group and queue of sealed temporaries unrenamed (#77 round
    /// 2, N2). When the session finishes, only those whose chunks a refused
    /// entry staged outlive it, up to a bound; the rest are removed (#97,
    /// #124, see [`Destination::retire_salvaged`]). No descriptor is held;
    /// each is opened by name when read.
    salvage: Vec<Salvaged>,
}

/// An orphaned file temporary of this store, renamed to a name of this
/// session (so a restarted agent with the same pid never collides with it,
/// #77 round 2, N3) and kept until the session finishes.
struct Salvaged {
    parent: Arc<File>,
    name: CString,
    rel_path: Vec<u8>,
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
        let root = File::from(
            crate::io::sys::open_dir_path_nofollow(path).refuse_at("materialize::open")?,
        );
        Ok(Self {
            root,
            path: std::fs::canonicalize(path).refuse_at("materialize::open")?,
            tag: temporary_tag(&store.authority()?),
            store_device: std::fs::metadata(store.root())
                .refuse_at("materialize::open")?
                .dev(),
            last_parent: std::cell::RefCell::new(None),
            exchange: std::cell::RefCell::new(std::collections::HashMap::new()),
            directories: Vec::new(),
            unflushed: std::collections::HashMap::new(),
            swept: Sweep::default(),
            created: Creation::default(),
            salvage: Vec::new(),
        })
    }

    /// How many orphaned file temporaries the sweep has salvaged so far.
    pub(crate) const fn salvaged(&self) -> usize {
        self.salvage.len()
    }

    /// Open the salvaged temporary at `index` read-only, by name, following
    /// no link; `None` once it is gone or is not a regular file.
    pub(crate) fn salvaged_file(&self, index: usize) -> Option<File> {
        let salvaged = self.salvage.get(index)?;
        open_regular(&salvaged.parent, &salvaged.name).ok()
    }

    /// The size of the salvaged temporary at `index`, by name; `None` once it
    /// is gone or is not a regular file.
    pub(crate) fn salvaged_size(&self, index: usize) -> Option<u64> {
        self.salvaged_file(index)
            .and_then(|file| file.metadata().ok())
            .map(|metadata| metadata.len())
    }

    /// The destination-relative path the salvaged temporary at `index` has
    /// now (the session name the sweep gave it).
    pub(crate) fn salvaged_path(&self, index: usize) -> Option<&[u8]> {
        self.salvage
            .get(index)
            .map(|salvaged| salvaged.rel_path.as_slice())
    }

    /// Retire the session's salvage: keep the temporaries at `keep` (indices
    /// as [`Destination::salvaged_file`] takes them) for a later session,
    /// reported in [`Sweep::left`] under their current names, and remove
    /// every other one by name, as the sweep would have, sealing each
    /// directory that lost one. Call once the session's outputs are queued:
    /// their bytes no longer depend on the temporaries.
    ///
    /// # Errors
    /// Refuses a failed directory seal.
    pub(crate) fn retire_salvaged(&mut self, keep: &[usize]) -> Result<()> {
        let mut touched: Vec<Arc<File>> = Vec::new();
        for (index, salvaged) in std::mem::take(&mut self.salvage).into_iter().enumerate() {
            if keep.contains(&index) {
                self.swept.left.push(salvaged.rel_path);
                continue;
            }
            match crate::io::sys::unlinkat(&salvaged.parent, &salvaged.name, false) {
                Ok(()) => {
                    self.swept.removed += 1;
                    touched.push(salvaged.parent);
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                Err(_) => self.swept.left.push(salvaged.rel_path),
            }
        }
        let mut sealed = std::collections::HashSet::new();
        for directory in touched {
            let metadata = directory
                .metadata()
                .refuse_at("materialize::remove_salvaged")?;
            if sealed.insert((metadata.dev(), metadata.ino())) {
                directory
                    .sync_dir_counted()
                    .refuse_at("materialize::remove_salvaged")?;
            }
        }
        Ok(())
    }

    /// Remove this store's orphaned temporaries from the root. Call only
    /// while the store's exclusive publisher is held.
    ///
    /// # Errors
    /// Refuses an unreadable root directory.
    pub fn sweep_root(&mut self, store: &Store) -> Result<()> {
        let root = self.root.try_clone().refuse_at("materialize::sweep_root")?;
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
            Ok(file) => Ok(Some(StatIdentity::from_metadata(
                &file.metadata().refuse_at("materialize::identity")?,
            ))),
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
        let key =
            postcard::to_stdvec(&(authority, &row.rel_path)).refuse_at("materialize::directory")?;
        if stat_at(&parent, &leaf)?.is_some() {
            return self.existing_directory(row, &parent, &leaf, &key, store);
        }
        let mode = row.mode & 0o7777;
        #[cfg(feature = "fault-injection")]
        crate::fault::note_directory(Some("rename"));
        let temporary = self.temporary(Some(DIRECTORY_MARK))?;
        crate::io::sys::mkdirat(&parent, &temporary, 0o700).refuse_at("materialize::directory")?;
        fault_point!(DirectoryAfterMkdir);
        let bound = open_dir(&parent, &temporary)
            .and_then(|created| created.metadata().refuse_at("materialize::directory"))
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
                BulkloadRefusal::DestinationOccupied
            } else {
                crate::refuse::io(&error, "materialize::directory")
            });
        }
        fault_point!(DirectoryAfterRename);
        crate::io::durable::seal_dir(&parent).refuse_at("materialize::directory")?;
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
        crate::io::durable::seal_dir(parent).refuse_at("materialize::seal_entry")?;
        if parent
            .metadata()
            .refuse_at("materialize::seal_entry")?
            .dev()
            != self.store_device
        {
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
                BulkloadRefusal::DestinationOccupied
            } else {
                crate::refuse::io(&error, "materialize::fallback_directory")
            });
        }
        fault_point!(DirectoryAfterFallbackMkdir);
        let metadata = open_dir(parent, leaf)?
            .metadata()
            .refuse_at("materialize::fallback_directory")?;
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
        let metadata = existing
            .metadata()
            .refuse_at("materialize::existing_directory")?;
        let (dev, ino) = (metadata.dev(), metadata.ino());
        let owned = match store.directory_record(key) {
            Ok(Some(record)) if record == (PendingDirectory { dev, ino, mode }) => {
                // A crash at `directory.after_rename` leaves the rename into
                // place unsealed. Seal it before any output inside the
                // directory can commit, or a power loss can keep the output
                // record and lose the directory's name (#74 round 2, N1).
                self.seal_entry(parent)?;
                true
            }
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
            return Err(BulkloadRefusal::DestinationOccupied);
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
        let mut renamed = false;
        let mut shared: Option<Arc<File>> = None;
        // WP0(d): what an interrupted superseding publish displaced is
        // settled first, so a file this store does not own is never taken
        // for one of its temporaries.
        let aside = self.settle_supersedes(directory, rel_dir, store, &mut removed)?;
        for (name, kind) in temporary_candidates(directory)? {
            if aside.contains(name.as_bytes()) {
                continue;
            }
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
            } else if stat.nlink == 1 {
                // An orphan with no other name may hold a whole staged
                // output: keep it for its chunks and retire it when the
                // session finishes (`retire_salvaged`). It takes a name of this session first,
                // so no temporary this session stages can collide with it.
                // A second link to a published output is removed at once;
                // the output keeps the bytes.
                let fresh = self.temporary(None)?;
                let name = match crate::io::publish_noreplace(directory, &name, &fresh) {
                    Ok(_) => {
                        renamed = true;
                        fresh
                    }
                    Err(_) => name,
                };
                if shared.is_none() {
                    shared = Some(Arc::new(
                        directory.try_clone().refuse_at("materialize::sweep")?,
                    ));
                }
                let parent = Arc::clone(shared.as_ref().ok_or(BulkloadRefusal::Io(None))?);
                // Report the name it now has (#77 round 3, F1).
                let mut rel_path = rel_dir.to_vec();
                if !rel_path.is_empty() {
                    rel_path.push(b'/');
                }
                rel_path.extend_from_slice(name.as_bytes());
                self.salvage.push(Salvaged {
                    parent,
                    name,
                    rel_path,
                });
                continue;
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
        if removed || renamed {
            directory
                .sync_dir_counted()
                .refuse_at("materialize::sweep")?;
        }
        Ok(())
    }

    /// Settle every superseding publish (WP0(d), #187) recorded for a name
    /// directly inside `directory` that a crash, or a failed group commit,
    /// left unsettled. Runs before the directory's temporaries are swept and
    /// before any entry in it is decided.
    ///
    /// The record says what the staged name and the leaf may hold (see
    /// [`SupersedeIntent`]):
    ///
    /// - the staged name is gone or still holds the staged file: the record
    ///   is deleted, and when the leaf is still exactly the owned output its
    ///   rows go back (the old output with its old row); the staged file is
    ///   then this store's orphan, for the sweep;
    /// - the staged name holds the owned output, displaced: the exchange
    ///   took effect, so it is removed. The new file at the leaf, when it
    ///   is still the file this store staged, gets an ownership row
    ///   (`transfer_store::owner_key`): it is adopted like any unrowed
    ///   output (#169) while its seat is unchanged, and superseded once the
    ///   seat has changed again. The same holds when the staged name is
    ///   already gone;
    /// - the staged name holds any other file: one this store does not own,
    ///   displaced. While the leaf still holds the staged file the two are
    ///   exchanged back, the directory sealed, and the staged file removed.
    ///   Otherwise the displaced file stays where it is, reported in
    ///   [`Sweep::left`], with its record: never removed.
    ///
    /// Returns the names kept aside, which the sweep must not touch.
    ///
    /// # Errors
    /// Refuses a record that cannot be read or settled.
    fn settle_supersedes(
        &mut self,
        directory: &File,
        rel_dir: &[u8],
        store: &Store,
        removed: &mut bool,
    ) -> Result<std::collections::HashSet<Vec<u8>>> {
        let mut aside = std::collections::HashSet::new();
        for intent in store.supersede_intents(rel_dir)? {
            let temp = cstring(&intent.temp)?;
            let leaf = cstring(&intent.leaf)?;
            let at_temp = stat_at(directory, &temp)?;
            let at_leaf = stat_at(directory, &leaf)?;
            let intact = at_leaf.is_some_and(|found| is_owned(&found, &intent.owned, true));
            // The exchange took effect and the new file's row did not
            // commit: the leaf holds the file this store staged. The record
            // proves it, so the file keeps an ownership row and is adopted
            // (#169) or, once its seat has changed again, superseded.
            let own = at_leaf.and_then(|found| published(&found, &intent));
            match at_temp {
                None => store.settle_supersede(&intent, intact, own)?,
                Some(found) if is_staged(&found, &intent) => {
                    store.settle_supersede(&intent, intact, None)?;
                }
                Some(found) if is_owned(&found, &intent.owned, false) => {
                    crate::io::sys::unlinkat(directory, &temp, false)
                        .refuse_at("materialize::settle_supersedes")?;
                    self.swept.removed += 1;
                    *removed = true;
                    store.settle_supersede(&intent, false, own)?;
                }
                Some(_) => {
                    // Sealed between the exchange back and the unlink, so no
                    // crash keeps the unlink alone: it would then hit the
                    // displaced file.
                    let restored = at_leaf.is_some_and(|found| is_staged(&found, &intent))
                        && crate::io::exchange(directory, &temp, &leaf).is_ok()
                        && crate::io::durable::seal_dir(directory).is_ok();
                    if restored {
                        crate::io::sys::unlinkat(directory, &temp, false)
                            .refuse_at("materialize::settle_supersedes")?;
                        self.swept.removed += 1;
                        *removed = true;
                        counters::bump(Counter::SupersedeRestored);
                        store.settle_supersede(&intent, false, None)?;
                    } else {
                        self.swept.left.push(intent.temp_path());
                        aside.insert(intent.temp);
                    }
                }
            }
        }
        Ok(aside)
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
            let metadata = directory
                .metadata()
                .refuse_at("materialize::finish_directories")?;
            if metadata.dev() != dev || metadata.ino() != ino {
                return Err(BulkloadRefusal::DestinationOccupied);
            }
            crate::io::sys::fchmod(&directory, mode)
                .refuse_at("materialize::finish_directories")?;
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
                crate::io::durable::seal_dir(&parent).refuse_at("materialize::symlink")?;
                self.note_unflushed(parent);
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(crate::refuse::io(&error, "materialize::symlink")),
        }
        let mut bytes = vec![0_u8; target.len().saturating_add(1)];
        // Any failure (`EINVAL`: the leaf is a file or a directory; `ENOENT`:
        // it went away) means the leaf is not this symlink: refused as
        // occupied, as before the move to `io::sys` (#74 review, B2).
        let Ok(read) = crate::io::sys::readlinkat(&parent, &leaf, &mut bytes) else {
            return Err(BulkloadRefusal::DestinationOccupied);
        };
        if read != target.len() || bytes.get(..target.len()) != Some(target.as_slice()) {
            return Err(BulkloadRefusal::DestinationOccupied);
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

    /// Whether the file system holding `row`'s parent directory offers the
    /// atomic exchange of two names, the one call a superseding publish
    /// replaces an output with (WP0(d), #187). A file system the first
    /// publish reaches through its link fallback (NFS, SMB, exFAT) may have
    /// none; a changed seat there is refused before anything is staged or
    /// asked of the source, not at its group commit.
    ///
    /// Probed once per device and session: two empty temporaries of this
    /// store are created in the directory, exchanged and removed. A crash
    /// between leaves them for the sweep, as any temporary.
    ///
    /// # Errors
    /// Refuses an unsafe ancestor or a probe that failed for any other
    /// reason than a missing exchange.
    pub(crate) fn exchange_supported(&self, row: &RowSchema) -> Result<bool> {
        let (parent, _) = self.shared_parent(&row.rel_path)?;
        let device = parent
            .metadata()
            .refuse_at("materialize::exchange_supported")?
            .dev();
        if let Some(known) = self.exchange.borrow().get(&device) {
            return Ok(*known);
        }
        let first = self.temporary(None)?;
        let second = self.temporary(None)?;
        crate::io::sys::create_excl_at(parent.as_fd(), &first, 0o600)
            .refuse_at("materialize::exchange_supported")?;
        if let Err(error) = crate::io::sys::create_excl_at(parent.as_fd(), &second, 0o600) {
            let _ = unlink(&parent, &first);
            return Err(crate::refuse::io(&error, "materialize::exchange_supported"));
        }
        let exchanged = crate::io::exchange(&parent, &first, &second);
        for name in [&first, &second] {
            let _ = unlink(&parent, name);
        }
        let supported = match exchanged {
            Ok(()) => true,
            Err(error) if crate::io::rename_unsupported(&error) => false,
            Err(error) => return Err(crate::refuse::io(&error, "materialize::exchange_supported")),
        };
        counters::bump(Counter::ExchangeProbes);
        self.exchange.borrow_mut().insert(device, supported);
        Ok(supported)
    }

    /// Create a private temporary file (`O_EXCL`, mode 0600) beside `row`'s
    /// leaf. The caller writes it and hands it to a [`PublishSink`].
    ///
    /// # Errors
    /// Refuses an unsafe ancestor or a failed create.
    pub(crate) fn stage(&self, row: &RowSchema) -> Result<StagedFile> {
        let (parent, leaf) = self.shared_parent(&row.rel_path)?;
        // A name of this pid and counter can exist already, left by an
        // earlier agent that had the same pid (a container's pid 1): take
        // the next name rather than refusing the file (#77 round 2, N3).
        let mut attempts = 0;
        let (temporary, file) = loop {
            let temporary = self.temporary(None)?;
            match crate::io::sys::create_excl_at(parent.as_fd(), &temporary, 0o600) {
                Ok(file) => break (temporary, File::from(file)),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && attempts < STAGE_ATTEMPTS =>
                {
                    attempts += 1;
                }
                Err(error) => return Err(crate::refuse::io(&error, "materialize::stage")),
            }
        };
        Ok(StagedFile {
            parent,
            temporary,
            leaf,
            file: Arc::new(file),
            sealed: false,
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
        let mut directory = self.root.try_clone().refuse_at("materialize::parent")?;
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
    /// Sealed by [`StagedFile::seal`] already; publication does not seal it
    /// again.
    sealed: bool,
}

impl StagedFile {
    /// Seal the complete data under the temporary name, once. A sealed
    /// temporary is what the destination reports as held: a crash before its
    /// rename leaves it for the next session to salvage.
    ///
    /// # Errors
    /// Returns the failed seal; the temporary is left for the caller.
    pub(crate) fn seal(&mut self) -> Result<()> {
        if !self.sealed {
            crate::io::durable::seal_file(&self.file).refuse_at("materialize::seal")?;
            self.sealed = true;
            fault_point!(MaterializeAfterTempSeal);
        }
        Ok(())
    }
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
    fn publish(mut self) -> Result<(StatIdentity, Arc<File>)> {
        if let Err(error) = self.seal() {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(error);
        }
        if let Err(error) = crate::io::publish_noreplace(&self.parent, &self.temporary, &self.leaf)
        {
            let _ = unlink(&self.parent, &self.temporary);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                BulkloadRefusal::DestinationOccupied
            } else {
                crate::refuse::io(&error, "materialize::publish")
            });
        }
        fault_point!(MaterializeAfterRename);
        counters::bump(Counter::FilesMaterialized);
        Ok((
            StatIdentity::from_metadata(&self.file.metadata().refuse_at("materialize::publish")?),
            self.parent,
        ))
    }

    /// Step 1 of a superseding publish (module docs): seal the staged file,
    /// open the output at the leaf and require the identity `owned` was
    /// proven under, and draw up the intent to record before the exchange.
    /// A failure removes the temporary: nothing else has changed.
    fn prepare_supersede(
        mut self,
        rel_path: &[u8],
        owned: OwnedOutput,
    ) -> Result<PendingSupersede> {
        let prepared = self.seal().and_then(|()| {
            let old = open_regular(&self.parent, &self.leaf)?;
            let found = old.metadata().refuse_at("materialize::prepare_supersede")?;
            if StatIdentity::from_metadata(&found) != owned.identity {
                return Err(BulkloadRefusal::DestinationOccupied);
            }
            let staged = self
                .file
                .metadata()
                .refuse_at("materialize::prepare_supersede")?;
            let stamp = StatIdentity::from_metadata(&staged);
            Ok((
                old,
                (staged.dev(), staged.ino()),
                (stamp.size, stamp.mtime_ns),
            ))
        });
        match prepared {
            Ok((old, staged, stamp)) => Ok(PendingSupersede {
                old: Arc::new(old),
                intent: SupersedeIntent {
                    dir: rel_path
                        .iter()
                        .rposition(|byte| *byte == b'/')
                        .and_then(|end| rel_path.get(..end))
                        .unwrap_or_default()
                        .to_vec(),
                    temp: self.temporary.as_bytes().to_vec(),
                    leaf: self.leaf.as_bytes().to_vec(),
                    staged,
                    stamp,
                    owned: owned.identity,
                    rows: owned.rows,
                },
                staged: self,
            }),
            Err(refusal) => {
                let _ = unlink(&self.parent, &self.temporary);
                // An absent or foreign leaf is one answer: not ours to replace.
                Err(match refusal {
                    BulkloadRefusal::Io(Some(libc::ENOENT)) => BulkloadRefusal::DestinationOccupied,
                    other => other,
                })
            }
        }
    }

    /// Steps 3 and 4 of a superseding publish (module docs), once its
    /// intent is committed: exchange the staged name with the leaf, then
    /// check the displaced file. `old` is the output as opened in step 1.
    fn exchange(self, old: &File, intent: &SupersedeIntent) -> Exchanged {
        // The last look: the output must still be exactly this store's own.
        let unchanged = old
            .metadata()
            .is_ok_and(|found| StatIdentity::from_metadata(&found) == intent.owned);
        if !unchanged {
            let _ = unlink(&self.parent, &self.temporary);
            return Exchanged::Undone {
                refusal: BulkloadRefusal::DestinationOccupied,
                restore: false,
            };
        }
        #[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
        before_exchange(&self.parent, self.leaf.as_bytes());
        if let Err(error) = crate::io::exchange(&self.parent, &self.temporary, &self.leaf) {
            let _ = unlink(&self.parent, &self.temporary);
            // Nothing moved. The rows go back only if the leaf is still
            // exactly the owned output (it may have vanished meanwhile).
            let restore = stat_at(&self.parent, &self.leaf)
                .ok()
                .flatten()
                .is_some_and(|found| is_owned(&found, &intent.owned, true));
            return Exchanged::Undone {
                // No atomic exchange here: the output stays as it is. The
                // plan's probe refuses such a seat before it is staged;
                // this is the same answer for a directory it did not cover.
                refusal: if crate::io::rename_unsupported(&error) {
                    BulkloadRefusal::DestinationExchangeUnsupported
                } else if error.raw_os_error() == Some(libc::ENOENT) {
                    BulkloadRefusal::DestinationOccupied
                } else {
                    crate::refuse::io(&error, "materialize::exchange")
                },
                restore,
            };
        }
        fault_point!(SupersedeAfterExchange);
        // The displaced file sits under the staged name. It is this store's
        // own when it is the inode step 1 opened, with the size and mtime
        // its row recorded; the exchange itself moved its ctime.
        let displaced = stat_at(&self.parent, &self.temporary).ok().flatten();
        let ours = displaced.is_some_and(|found| is_owned(&found, &intent.owned, false))
            && old.metadata().is_ok_and(|found| {
                let found = StatIdentity::from_metadata(&found);
                found.size == intent.owned.size && found.mtime_ns == intent.owned.mtime_ns
            });
        if ours {
            // A name that survives is this store's orphan: the sweep's.
            let _ = unlink(&self.parent, &self.temporary);
            counters::bump(Counter::FilesMaterialized);
            return match self.file.metadata() {
                Ok(found) => Exchanged::Done(StatIdentity::from_metadata(&found), self.parent),
                Err(error) => Exchanged::Unrecorded(
                    crate::refuse::io(&error, "materialize::exchange"),
                    self.parent,
                ),
            };
        }
        // Not this store's file: exchange it back, if the staged file is
        // still what the leaf holds.
        let in_place = stat_at(&self.parent, &self.leaf)
            .ok()
            .flatten()
            .is_some_and(|found| is_staged(&found, intent));
        if in_place && crate::io::exchange(&self.parent, &self.temporary, &self.leaf).is_ok() {
            // Sealed before the staged file's name goes, so no crash keeps
            // that unlink without the exchange back: the unlink would then
            // hit the displaced file.
            if crate::io::durable::seal_dir(&self.parent).is_ok() {
                let _ = unlink(&self.parent, &self.temporary);
            }
            counters::bump(Counter::SupersedeRestored);
            return Exchanged::Restored(self.parent);
        }
        Exchanged::Stranded(self.parent)
    }
}

/// A superseding publish between its preparation and its exchange.
struct PendingSupersede {
    staged: StagedFile,
    /// The output at the leaf, opened in step 1.
    old: Arc<File>,
    intent: SupersedeIntent,
}

/// Outputs a superseding publish of this session is replacing, or has
/// replaced, by destination-relative path: each stays readable through the
/// descriptor its publish opened, whatever name it has or has lost.
///
/// A chunk hint names an output by path. Once a changed seat's new file has
/// taken that path, a seat planned later in the same session that still
/// wants the old output's chunks (a file moved or copied out of the changed
/// one) would miss them there and ask the source for bytes the destination
/// held when the run began (WP0(c), inequality 2). The receiving side reads
/// them here instead; like every hint read, the bytes are verified against
/// their digest. An output is entered before its exchange, so no planner
/// can see the new file at the path without finding the old one here.
///
/// Bounded: past `capacity` the oldest descriptor is dropped, and its chunks
/// are then asked for like any absent chunk.
pub(crate) struct Displaced {
    files: std::collections::HashMap<Vec<u8>, Arc<File>>,
    order: std::collections::VecDeque<Vec<u8>>,
    capacity: usize,
}

/// A session's [`Displaced`] outputs, shared by its receiving thread and its
/// committer thread.
pub(crate) type SharedDisplaced = Arc<std::sync::Mutex<Displaced>>;

impl Displaced {
    /// An empty set that keeps at most `capacity` descriptors open.
    pub(crate) fn shared(capacity: usize) -> SharedDisplaced {
        Arc::new(std::sync::Mutex::new(Self {
            files: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
        }))
    }

    pub(crate) fn insert(&mut self, rel_path: Vec<u8>, file: Arc<File>) {
        if self.files.insert(rel_path.clone(), file).is_none() {
            self.order.push_back(rel_path);
        }
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.files.remove(&oldest);
            }
        }
    }

    /// The output this session displaced, or is displacing, at `rel_path`.
    pub(crate) fn get(&self, rel_path: &[u8]) -> Option<Arc<File>> {
        self.files.get(rel_path).map(Arc::clone)
    }
}

/// How a superseding publish's exchange ended.
enum Exchanged {
    /// The new file is at the leaf and the old output is removed: the new
    /// file's identity for its row, and the directory to seal.
    Done(StatIdentity, Arc<File>),
    /// The new file is at the leaf but its identity could not be read: no
    /// row is written and its intent stays, so the next sweep gives it an
    /// ownership row.
    Unrecorded(BulkloadRefusal, Arc<File>),
    /// Nothing was exchanged, and the temporary is gone. With `restore` the
    /// leaf is still exactly the owned output, so its rows go back.
    Undone {
        refusal: BulkloadRefusal,
        restore: bool,
    },
    /// The displaced file was not this store's: it is back at the leaf.
    Restored(Arc<File>),
    /// The displaced file was not this store's and could not be put back:
    /// it stays under the staged name, with its intent, for the sweep to
    /// keep aside and report.
    Stranded(Arc<File>),
}

/// An output this store may supersede: the identity one of its rows records
/// for the path, which the file there still has, and every row the store
/// holds for that path.
pub(crate) struct OwnedOutput {
    pub identity: StatIdentity,
    pub rows: Vec<OutputRow>,
}

/// Whether the existing output `file` at `row`'s path is this store's own,
/// untouched since its row was written (WP0(d), OI-1003-Q18): its
/// `(dev, ino, size, mtime, ctime)` equals a row this store committed for
/// that path under `authority`: a seat's reuse row, or the path's
/// ownership row (`transfer_store::owner_key`), which an output published
/// from a racy capture, or exchanged into place without its row, has in
/// its stead. Bulkload wrote or verified it, and nothing has touched it
/// since. Returns what a superseding publish needs, or `None`
/// for any other file: that one is never replaced.
///
/// # Errors
/// Refuses a failed `fstat` or database read.
pub(crate) fn owned_output(
    store: &Store,
    authority: &[u8],
    row: &RowSchema,
    file: &File,
) -> Result<Option<OwnedOutput>> {
    let identity =
        StatIdentity::from_metadata(&file.metadata().refuse_at("materialize::owned_output")?);
    let rows = store.output_rows(authority, &row.rel_path)?;
    let recorded = identity_bytes(&identity)?;
    Ok(rows
        .iter()
        .any(|(_, held)| *held == recorded)
        .then_some(OwnedOutput { identity, rows }))
}

/// Whether `found` is the file `owned` records: the same inode, size and
/// mtime, and with `exact` the same ctime too. An exchange moves the ctime
/// of the files it trades, so a displaced file is compared without it.
const fn is_owned(found: &crate::io::Stat, owned: &StatIdentity, exact: bool) -> bool {
    found.is_file()
        && found.node.dev == owned.dev
        && found.node.ino == owned.ino
        && found.size == owned.size
        && found.mtime_ns == owned.mtime_ns
        && (!exact || found.ctime_ns == owned.ctime_ns)
}

/// Whether `found` is the staged file an intent names.
fn is_staged(found: &crate::io::Stat, intent: &SupersedeIntent) -> bool {
    found.is_file() && (found.node.dev, found.node.ino) == intent.staged
}

/// The identity of the file an intent's staged file is now, when `found`
/// (the leaf) is that file and has not been written since it was staged:
/// its inode, and the size and mtime the intent recorded before the
/// exchange, which moves only its ctime. Such a file is this store's own.
fn published(found: &crate::io::Stat, intent: &SupersedeIntent) -> Option<StatIdentity> {
    (is_staged(found, intent) && (found.size, found.mtime_ns) == intent.stamp).then_some(
        StatIdentity {
            dev: found.node.dev,
            ino: found.node.ino,
            size: found.size,
            mtime_ns: found.mtime_ns,
            ctime_ns: found.ctime_ns,
        },
    )
}

/// Test hooks run on the committer thread between a superseding publish's
/// last look at the output and its exchange, where a third party's write
/// can still land: by the output's directory (its device and inode) and
/// leaf name.
#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
type ExchangeHooks = Vec<(u64, (u64, u64), Vec<u8>, Arc<dyn Fn() + Send + Sync>)>;

#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
static BEFORE_EXCHANGE: std::sync::Mutex<ExchangeHooks> = std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
static NEXT_EXCHANGE_HOOK: AtomicU64 = AtomicU64::new(0);

/// Removes its hook when dropped.
#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
#[must_use = "the hook is removed as soon as the guard is dropped"]
pub struct BeforeExchange(u64);

#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
impl Drop for BeforeExchange {
    fn drop(&mut self) {
        BEFORE_EXCHANGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, ..)| *id != self.0);
    }
}

/// Test hook: run `hook` just before every superseding publish exchanges
/// the output named `leaf` in `directory`, after its last identity check.
///
/// # Errors
/// Returns the failure to stat `directory`.
#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
pub fn set_before_exchange(
    directory: &Path,
    leaf: &[u8],
    hook: impl Fn() + Send + Sync + 'static,
) -> std::io::Result<BeforeExchange> {
    let found = std::fs::metadata(directory)?;
    let id = NEXT_EXCHANGE_HOOK.fetch_add(1, Ordering::Relaxed);
    BEFORE_EXCHANGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((
            id,
            (found.dev(), found.ino()),
            leaf.to_vec(),
            Arc::new(hook),
        ));
    Ok(BeforeExchange(id))
}

#[cfg(any(test, feature = "fault-injection", feature = "io-trace"))]
fn before_exchange(directory: &File, leaf: &[u8]) {
    let Ok(found) = directory.metadata() else {
        return;
    };
    let hooks: Vec<Arc<dyn Fn() + Send + Sync>> = BEFORE_EXCHANGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(_, node, name, _)| *node == (found.dev(), found.ino()) && name == leaf)
        .map(|(.., hook)| Arc::clone(hook))
        .collect();
    for hook in hooks {
        hook();
    }
}

fn full_flush_counted(handle: &File) -> Result<()> {
    counters::timed(Counter::FlushFull, Counter::FlushFullNs, || {
        crate::io::sys::full_flush(handle)
    })
    .refuse_at("materialize::full_flush_counted")
}

fn unlink(parent: &File, name: &CString) -> Result<()> {
    // Without AT_REMOVEDIR this removes only a non-directory entry.
    crate::io::sys::unlinkat(parent, name, false).refuse_at("materialize::unlink")
}

/// The serial the next temporary name of this process takes.
#[cfg(test)]
pub(crate) fn next_temporary_serial() -> u64 {
    NEXT_TEMPORARY.load(Ordering::Relaxed)
}

/// Names a stage tries before an occupied name is a refusal.
const STAGE_ATTEMPTS: u32 = 64;

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
    /// A fully written staged file that replaces this store's own output at
    /// its path (WP0(d), #187; module docs).
    Superseding {
        staged: StagedFile,
        record: PendingOutput,
        owned: OwnedOutput,
    },
}

/// The record for a staged file, completed with its identity once published.
pub(crate) struct PendingOutput {
    pub key: Vec<u8>,
    pub rel_path: Vec<u8>,
    pub size: u64,
    /// See [`OutputRecord::racy`].
    pub racy: bool,
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
    /// Told each group's outcomes, by relative path, once the group's
    /// commit has returned: what the receiving side reports as held.
    notify: Option<std::sync::mpsc::Sender<GroupOutcomes>>,
    /// Where the receiving side finds the outputs this sink supersedes.
    displaced: Option<SharedDisplaced>,
}

/// One group's outcomes by relative path: `true` when committed.
pub(crate) type GroupOutcomes = Vec<(Vec<u8>, bool)>;

impl PublishSink {
    /// `publisher` holds the destination store's single-writer guard.
    ///
    /// # Errors
    /// Refuses if the store root cannot be stat'ed.
    pub(crate) fn new(publisher: StorePublisher) -> Result<Self> {
        let store_device = std::fs::metadata(publisher.store().root())
            .refuse_at("materialize::new")?
            .dev();
        Ok(Self {
            publisher,
            store_device,
            outcomes: Vec::new(),
            notify: None,
            displaced: None,
        })
    }

    /// Enter every output this sink supersedes into `displaced`, before its
    /// exchange.
    pub(crate) fn with_displaced(mut self, displaced: SharedDisplaced) -> Self {
        self.displaced = Some(displaced);
        self
    }

    /// Report every group's outcomes on `notify` after its commit.
    pub(crate) fn with_notify(mut self, notify: std::sync::mpsc::Sender<GroupOutcomes>) -> Self {
        self.notify = Some(notify);
        self
    }

    /// Steps 2 to 4 of the group's superseding publishes (module docs):
    /// record every intent in one commit, then exchange each staged file
    /// with its output and check what it displaced. A replaced output's
    /// record joins `records` and its directory `touched`; a refused one's
    /// outcome is reported here. Returns the intents the group's commit
    /// settles, each with whether its old rows go back.
    fn supersede(
        &mut self,
        superseding: Vec<(PendingSupersede, PendingOutput)>,
        records: &mut Vec<OutputRecord>,
        touched: &mut TouchedDevices,
    ) -> Vec<(SupersedeIntent, bool)> {
        if superseding.is_empty() {
            return Vec::new();
        }
        let intents: Vec<SupersedeIntent> = superseding
            .iter()
            .map(|(pending, _)| pending.intent.clone())
            .collect();
        if let Err(refusal) = self.publisher.begin_supersedes(&intents) {
            // Nothing was recorded, so nothing may be exchanged.
            let refusal = space_refusal(refusal);
            for (pending, record) in superseding {
                let _ = pending.staged.discard();
                self.outcomes.push((record.rel_path, Err(refusal.clone())));
            }
            return Vec::new();
        }
        fault_point!(SupersedeAfterIntent);
        let mut settled = Vec::with_capacity(superseding.len());
        for (pending, record) in superseding {
            let PendingSupersede {
                staged,
                old,
                intent,
            } = pending;
            match staged.exchange(&old, &intent) {
                Exchanged::Done(identity, parent) => {
                    touched.directory(parent);
                    counters::bump(Counter::OutputsSuperseded);
                    records.push(OutputRecord {
                        key: record.key,
                        rel_path: record.rel_path,
                        identity,
                        racy: record.racy,
                        hints: record.hints,
                    });
                    settled.push((intent, false));
                }
                // Its intent stays: the next sweep finds the staged file at
                // the leaf and gives it its ownership row.
                Exchanged::Unrecorded(refusal, parent) => {
                    touched.directory(parent);
                    self.outcomes.push((record.rel_path, Err(refusal)));
                }
                Exchanged::Undone { refusal, restore } => {
                    self.outcomes.push((record.rel_path, Err(refusal)));
                    settled.push((intent, restore));
                }
                Exchanged::Restored(parent) => {
                    touched.directory(parent);
                    self.outcomes
                        .push((record.rel_path, Err(BulkloadRefusal::DestinationOccupied)));
                    settled.push((intent, false));
                }
                // Its intent stays: the sweep keeps the displaced file aside.
                Exchanged::Stranded(parent) => {
                    touched.directory(parent);
                    self.outcomes
                        .push((record.rel_path, Err(BulkloadRefusal::DestinationOccupied)));
                }
            }
        }
        settled
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
            crate::io::durable::seal_dir(directory).refuse_at("materialize::seal")?;
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
            Publication::Staged { record, .. } | Publication::Superseding { record, .. } => {
                (1, record.size)
            }
            Publication::Adopted { .. } => (1, 0),
        }
    }

    fn commit(&mut self, items: Vec<Publication>) {
        let reported = self.outcomes.len();
        #[cfg(feature = "fault-injection")]
        let _note = {
            let ids: Vec<usize> = (0..items.len()).collect();
            let chunks = items
                .iter()
                .map(|item| match item {
                    Publication::Staged { record, .. }
                    | Publication::Superseding { record, .. } => record.hints.len(),
                    Publication::Adopted { .. } => 0,
                })
                .sum();
            crate::fault::note_group(&ids, chunks)
        };
        let mut records = Vec::with_capacity(items.len());
        let mut touched = TouchedDevices::default();
        let mut superseding = Vec::new();
        for item in items {
            match item {
                Publication::Superseding {
                    staged,
                    record,
                    owned,
                } => match staged.prepare_supersede(&record.rel_path, owned) {
                    Ok(pending) => {
                        if let Some(displaced) = &self.displaced {
                            displaced
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(record.rel_path.clone(), Arc::clone(&pending.old));
                        }
                        superseding.push((pending, record));
                    }
                    Err(refusal) => self
                        .outcomes
                        .push((record.rel_path, Err(space_refusal(refusal)))),
                },
                Publication::Staged { staged, record } => match staged.publish() {
                    Ok((identity, parent)) => {
                        touched.directory(parent);
                        records.push(OutputRecord {
                            key: record.key,
                            rel_path: record.rel_path,
                            identity,
                            racy: record.racy,
                            hints: record.hints,
                        });
                    }
                    Err(refusal) => self
                        .outcomes
                        .push((record.rel_path, Err(space_refusal(refusal)))),
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
                    Err(error) => self.outcomes.push((
                        record.rel_path,
                        Err(space_refusal(crate::refuse::io(
                            &error,
                            "materialize::commit",
                        ))),
                    )),
                },
            }
        }
        let settled = self.supersede(superseding, &mut records, &mut touched);
        let committed = touched.seal(self.store_device).and_then(|_| {
            fault_point_in!(
                PublishDestinationAfterDirSeal,
                self.publisher.store().root()
            );
            let settled: Vec<SupersedeSettle<'_>> = settled
                .iter()
                .map(|(intent, restore)| SupersedeSettle::of(intent, *restore))
                .collect();
            self.publisher.commit_outputs(&records, &settled)
        });
        // A full disk under the group (a seal or the store commit) is the
        // typed space refusal, not a bare IO (#100).
        let committed = committed.map_err(space_refusal);
        for record in records {
            self.outcomes.push((record.rel_path, committed.clone()));
        }
        if let Some(notify) = &self.notify {
            let group = self
                .outcomes
                .get(reported..)
                .unwrap_or_default()
                .iter()
                .map(|(rel_path, outcome)| (rel_path.clone(), outcome.is_ok()))
                .collect();
            // The receiving side may have stopped; its outcomes still reach
            // `finish`.
            let _ = notify.send(group);
        }
    }

    fn failure(&self) -> Option<BulkloadRefusal> {
        None
    }

    fn finish(self) -> Self::Report {
        self.outcomes
    }
}

/// `ENOSPC` (or `SQLite`'s full-disk code, which the store reports as it)
/// is [`BulkloadRefusal::DestinationSpaceInsufficient`]: the group could not
/// be made durable for lack of space (OI-1001-Q2, #100).
fn space_refusal(refusal: BulkloadRefusal) -> BulkloadRefusal {
    match refusal {
        BulkloadRefusal::Io(Some(libc::ENOSPC)) => BulkloadRefusal::DestinationSpaceInsufficient,
        other => other,
    }
}

/// Verify an existing output byte-for-byte against `manifest` before adopting
/// it: every chunk, read at its place, hashes to its manifest digest, and the
/// chunks cover the file exactly. The manifest's root is checked by the
/// caller, so this is equivalent to recomputing `manifest_root`.
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
    file.rewind().refuse_at("materialize::verify_existing")?;
    let before = file.metadata().refuse_at("materialize::verify_existing")?;
    if before.len() != row.size
        || manifest.size() != Some(row.size)
        || before.mode() & 0o7777 != row.mode & 0o7777
    {
        return Err(BulkloadRefusal::DestinationOccupied);
    }
    let identity = StatIdentity::from_metadata(&before);
    let mut buffer = vec![0; crate::hash::CDC_MAX_BYTES as usize];
    for chunk in &manifest.chunks {
        let size = usize::try_from(chunk.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        let slot = buffer
            .get_mut(..size)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        file.read_exact(slot)
            .refuse_at("materialize::verify_existing")?;
        counters::add_len(Counter::DestVerifyRead, size);
        if counters::hash(Counter::HashVerifyExisting, slot) != chunk.digest {
            return Err(BulkloadRefusal::DestinationOccupied);
        }
    }
    let mut tail = [0_u8; 1];
    if file
        .read(&mut tail)
        .refuse_at("materialize::verify_existing")?
        != 0
        || StatIdentity::from_metadata(&file.metadata().refuse_at("materialize::verify_existing")?)
            != identity
    {
        return Err(BulkloadRefusal::DestinationOccupied);
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
        && crate::io::sys::list_dir(directory)
            .refuse_at("materialize::fresh_fallback")?
            .is_empty())
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
        Err(error) => Err(crate::refuse::io(&error, "materialize::stat_at")),
    }
}

/// Every entry directly inside `directory` whose name is in the temporary
/// grammar, read through a fresh descriptor for `.` (see `sys::list_dir`).
fn temporary_candidates(directory: &File) -> Result<Vec<(CString, TemporaryName)>> {
    Ok(crate::io::sys::list_dir(directory)
        .refuse_at("materialize::temporary_candidates")?
        .into_iter()
        .filter_map(|name| temporary_name(name.to_bytes()).map(|kind| (name, kind)))
        .collect())
}

fn open_dir(parent: &File, name: &CStr) -> Result<File> {
    Ok(File::from(
        crate::io::sys::open_dir_at(parent, name).refuse_at("materialize::open_dir")?,
    ))
}

fn open_regular(parent: &File, name: &CStr) -> Result<File> {
    let file = File::from(
        crate::io::sys::open_read_at(parent, name).refuse_at("materialize::open_regular")?,
    );
    if !file
        .metadata()
        .refuse_at("materialize::open_regular")?
        .is_file()
    {
        return Err(BulkloadRefusal::DestinationOccupied);
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
        std::fs::create_dir_all(&source).refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
        std::fs::create_dir_all(&destination).refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
        std::fs::write(source.join("file"), b"ours").refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
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
        (&**staged.file()).write_all(b"ours").refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
        std::fs::write(destination.join("file"), b"theirs").refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
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
                racy: false,
                hints: Vec::new(),
            },
        }]);
        let report = sink.finish();
        let listed = std::fs::read_dir(&destination)
            .refuse_at(
                "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
            )?
            .count();
        let kept = std::fs::read(destination.join("file")).refuse_at(
            "materialize::tests::publish_never_replaces_an_output_that_appeared_meanwhile",
        )?;
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            report,
            [(b"file".to_vec(), Err(BulkloadRefusal::DestinationOccupied))]
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
        std::fs::create_dir_all(&source)
            .refuse_at("materialize::tests::a_failed_directory_seal_commits_no_record")?;
        std::fs::create_dir_all(&destination)
            .refuse_at("materialize::tests::a_failed_directory_seal_commits_no_record")?;
        std::fs::write(source.join("file"), b"ours")
            .refuse_at("materialize::tests::a_failed_directory_seal_commits_no_record")?;
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
        (&**staged.file())
            .write_all(b"ours")
            .refuse_at("materialize::tests::a_failed_directory_seal_commits_no_record")?;
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
                racy: false,
                hints: Vec::new(),
            },
        }]);
        crate::io::durable::fail_dir_seals(false);
        let report = sink.finish();
        let identity = StatIdentity::from_metadata(
            &std::fs::metadata(destination.join("file"))
                .refuse_at("materialize::tests::a_failed_directory_seal_commits_no_record")?,
        );
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
        std::fs::create_dir_all(&source)
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
        std::fs::create_dir_all(&destination)
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
        std::os::unix::fs::symlink("target", source.join("as-file"))
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
        std::os::unix::fs::symlink("target", source.join("as-dir"))
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
        std::fs::write(destination.join("as-file"), b"someone else's")
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
        std::fs::create_dir(destination.join("as-dir"))
            .refuse_at("materialize::tests::a_symlink_onto_an_occupied_leaf_refuses_as_occupied")?;
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
            [Some("DESTINATION_OCCUPIED"), Some("DESTINATION_OCCUPIED")]
        );
        Ok(())
    }

    #[test]
    fn a_group_fully_flushes_each_touched_device_the_store_is_not_on() -> Result<()> {
        let directory = Arc::new(File::open(std::env::temp_dir()).refuse_at(
            "materialize::tests::a_group_fully_flushes_each_touched_device_the_store_is_not_on",
        )?);
        let device = directory
            .metadata()
            .refuse_at(
                "materialize::tests::a_group_fully_flushes_each_touched_device_the_store_is_not_on",
            )?
            .dev();
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
        std::fs::create_dir_all(&source).refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        std::fs::create_dir_all(&destination).refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        std::fs::write(source.join("free"), b"ours").refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        std::fs::write(source.join("taken"), b"ours").refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        let rows = crate::walk::walk(
            &crate::walk::WalkOptions::new(source),
            &mut crate::freshness::NullCache,
        )?
        .rows;
        std::fs::write(destination.join("taken"), b"theirs").refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        let target = Destination::open(&destination, &Store::open(&base.join("state"))?)?;
        let mut publications = Vec::new();
        for row in &rows {
            let staged = target.stage(row)?;
            (&**staged.file()).write_all(b"ours").refuse_at("materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported")?;
            publications.push(Publication::Staged {
                staged,
                record: PendingOutput {
                    key: row.rel_path.clone(),
                    rel_path: row.rel_path.clone(),
                    size: 4,
                    racy: false,
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
        let free = std::fs::read(destination.join("free")).refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        let taken = std::fs::read(destination.join("taken")).refuse_at(
            "materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported",
        )?;
        let listed = std::fs::read_dir(&destination).refuse_at("materialize::tests::publish_falls_back_to_link_where_exclusive_rename_is_unsupported")?.count();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            report,
            [
                (b"free".to_vec(), Ok(())),
                (b"taken".to_vec(), Err(BulkloadRefusal::DestinationOccupied)),
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

#[cfg(test)]
mod supersede_tests;
