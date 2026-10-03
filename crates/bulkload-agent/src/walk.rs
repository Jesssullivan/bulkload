//! The corpus walker.
//!
//! One pass over a corpus root producing one [`RowSchema`] per seat. The walk
//! is a stream ([`Walker`], W4 PR 3): it yields each seat as it is found, a
//! directory before anything beneath it, with no global sort, so a consumer
//! (the transfer source) offers the first entry before the walk has finished.
//! Names inside one directory are visited in byte order, which keeps the
//! stream deterministic at the cost of one directory's listing in memory.
//!
//! Every seat is reached component by component from one descriptor of the
//! root: each directory is opened with `openat(O_DIRECTORY | O_NOFOLLOW)`
//! beneath its parent's descriptor and listed through that descriptor, and
//! every seat is statted with `fstatat(AT_SYMLINK_NOFOLLOW)` beneath it. A
//! directory swapped for a symlink mid-walk is refused, never followed out
//! of the root. The walk holds one descriptor per level of depth.
//!
//! # Depth and path-length caps
//!
//! Both are bounded, and both refuse as values (#110). A seat at most
//! [`MAX_WALK_DEPTH`] components below the root is carried, so the walk holds
//! at most that many directory descriptors, the root's included: a directory
//! at that depth is a row, and its contents are refused as one subtree with
//! `PATH_DEPTH_EXCEEDED`. A seat
//! whose relative path is longer than [`MAX_REL_PATH_BYTES`] is refused with
//! `PATH_TOO_LONG` before it is statted, and a directory refused so is never
//! entered. Siblings of a refused subtree are carried as usual.
//!
//! The relative path is held once, in one buffer the levels share: each
//! level records only the length of its prefix in it, so the walk's path
//! memory is linear in the depth, not quadratic.
//!
//! Two properties matter and both are measured, not asserted:
//!
//! * Cache lookup uses the walk's metadata. A stale content read additionally
//!   checks the opened descriptor before and after hashing to detect writers.
//!   `files_statted_twice` counts these seats; warm resumes need no such checks.
//! * **Fresh seats are not re-read.** A seat the cache calls
//!   [`Freshness::Fresh`] contributes zero bytes to `bytes_reread_on_resume`.
//!
//! Refusals are forward-progressing: an unreadable or unportable seat is
//! recorded as a typed refusal and the walk continues.
//!
//! # Engine temporaries
//!
//! A regular file whose leaf is in the materializer's *tagged* file-temporary
//! grammar, and a directory in its tagged directory-temporary grammar that
//! holds nothing but such files (`materialize::temporary_name`), is never a
//! row. It is recorded as a [`WalkItem::EngineTemporary`] instead, so no walk
//! carries one (R-N79). Such a name is only ever what a crashed publication
//! left: a partial or complete copy of another output (after
//! `materialize.after_link` a second hard link to it), or an empty directory
//! that was never renamed into place. It holds no state of its own.
//!
//! Only the tagged grammar is excluded: a 16-hex-digit store tag plus
//! canonical decimals is not a name a person or another tool picks. The
//! untagged form (`.bulkload-<n>-<n>`) is indistinguishable from payload
//! such as `.bulkload-2026-09`, so it is carried like any file. So is every
//! other kind, and a tagged directory that holds anything else.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, OsStr};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use bulkload_proto::{BulkloadRefusal, FileKind, Result, RowSchema};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::freshness::{Freshness, FreshnessCache, StatIdentity};
use crate::hash;
use crate::io::{sys, Stat};

/// The deepest seat the walk carries, in components below the root.
///
/// A directory at this depth is a row; its contents are refused with
/// [`BulkloadRefusal::PathDepthExceeded`]. It is also the most directory
/// descriptors one walk holds open, the root's included. 256 is far beyond
/// any real estate tree and well inside the default descriptor limit.
pub const MAX_WALK_DEPTH: usize = 256;

/// The longest relative path, in bytes, the walk carries.
///
/// Linux `PATH_MAX` (4096) less its terminating NUL: every carried seat has
/// a relative path a path-based tool on either side can still name. A longer
/// seat is refused with [`BulkloadRefusal::PathTooLong`].
pub const MAX_REL_PATH_BYTES: usize = 4095;

/// File type bits of `st_mode`, and the symlink type.
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;

/// How the walker should treat file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashPolicy {
    /// Stat only. The fastest arm of the M0 bench.
    Never,
    /// Hash every regular file the cache calls stale.
    StaleOnly,
    /// Hash every regular file regardless of freshness. The cold baseline.
    Always,
}

/// Walker configuration.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Corpus root. Must be absolute.
    pub root: PathBuf,
    /// Whether and when to hash file contents.
    pub hash_policy: HashPolicy,
    /// Whether to descend into other filesystems.
    pub cross_device: bool,
}

impl WalkOptions {
    /// A stat-only walk of `root`, staying on one device.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self {
            root,
            hash_policy: HashPolicy::Never,
            cross_device: false,
        }
    }
}

/// A seat the walker declined, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedSeat {
    /// Path relative to the corpus root, as raw OS bytes. Empty when the
    /// root itself could not be listed.
    pub rel_path: Vec<u8>,
    /// Why the seat was declined.
    pub refusal: BulkloadRefusal,
}

/// Counters for one walk.
///
/// The first two fields are the R25 headline metrics; the M0 bench prints them
/// as its own columns. M1 wires the accounting; the numbers only become
/// meaningful once the M2 resume path exists.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkStats {
    /// Seats visited.
    pub seats_seen: u64,
    /// Apparent bytes across all regular-file seats.
    pub bytes_seen: u64,
    /// Actual bytes read by hash workers, including reads later refused.
    pub bytes_read: u64,
    /// Seats the cache called [`Freshness::Fresh`].
    pub fresh_skipped: u64,
    /// Bytes read again despite the cache calling the seat fresh.
    ///
    /// R25 headline metric. The product bar is zero.
    pub bytes_reread_on_resume: u64,
    /// Seats with actual descriptor metadata calls beyond the census stat.
    /// Warm resumes should keep this at zero.
    pub files_statted_twice: u64,
}

/// The result of one walk.
#[derive(Debug, Default, Clone)]
pub struct WalkOutcome {
    /// One row per accepted seat, in walk order: every directory before
    /// anything beneath it.
    pub rows: Vec<RowSchema>,
    /// One entry per declined seat.
    pub refusals: Vec<RefusedSeat>,
    /// Engine temporaries by relative path. Recorded, never carried; see the
    /// module docs.
    pub engine_temporaries: Vec<Vec<u8>>,
    /// Counters for the pass.
    pub stats: WalkStats,
}

/// One thing the streaming walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkItem {
    /// An accepted seat. A directory's row precedes every item beneath it.
    Row(RowSchema),
    /// A declined seat.
    Refused(RefusedSeat),
    /// An engine temporary, by relative path: recorded, never carried.
    Engine(Vec<u8>),
}

/// One directory being listed: its descriptor, the length of its relative
/// path in the walker's shared path buffer, and the names in it still to
/// visit.
struct Level {
    dir: OwnedFd,
    prefix_len: usize,
    names: std::vec::IntoIter<CString>,
}

/// The streaming walk beneath one root descriptor. An iterator of
/// [`WalkItem`]s; see the module docs for its order and confinement.
pub struct Walker<'root> {
    root_dev: u64,
    cross_device: bool,
    stack: Vec<Level>,
    /// The relative path of the seat being visited. Each level's prefix is
    /// its first `prefix_len` bytes; deeper levels only append past it.
    path: Vec<u8>,
    ready: VecDeque<WalkItem>,
    directories_listed: u64,
    _root: BorrowedFd<'root>,
}

/// What visiting one name produced.
enum Visit {
    Skip,
    Items(Vec<WalkItem>),
    Descend { row: RowSchema, level: Level },
}

impl<'root> Walker<'root> {
    /// Start a walk beneath `root`, a directory descriptor. The root itself is
    /// never a row. With `cross_device` false, a seat on another device than
    /// the root is skipped, and a mount point is not descended into.
    ///
    /// # Errors
    /// Refuses with the `fstat` failure of the root. A root that cannot be
    /// listed is not an error: the walk yields one refusal with an empty
    /// relative path, so no census reads as complete after it.
    pub fn new(root: BorrowedFd<'root>, cross_device: bool) -> Result<Self> {
        let root_dev = sys::fstat(root)?.node.dev;
        let mut walker = Self {
            root_dev,
            cross_device,
            stack: Vec::new(),
            path: Vec::new(),
            ready: VecDeque::new(),
            directories_listed: 0,
            _root: root,
        };
        match sys::open_dir_at(root, c".")
            .map_err(BulkloadRefusal::from)
            .and_then(|dir| level_of(dir, 0))
        {
            Ok(level) => {
                walker.directories_listed += 1;
                walker.stack.push(level);
            }
            Err(refusal) => walker.ready.push_back(WalkItem::Refused(RefusedSeat {
                rel_path: Vec::new(),
                refusal,
            })),
        }
        Ok(walker)
    }

    /// Directories listed so far, the root included. A streaming consumer
    /// sees its first item after one listing, not after the whole tree.
    #[must_use]
    pub const fn directories_listed(&self) -> u64 {
        self.directories_listed
    }

    fn visit(&self, parent: BorrowedFd<'_>, name: &CStr, rel_path: Vec<u8>) -> Visit {
        let stat = match sys::fstatat_nofollow(parent, name) {
            Ok(stat) => stat,
            Err(error) => return refuse(rel_path, BulkloadRefusal::from(error)),
        };
        if !self.cross_device && stat.node.dev != self.root_dev {
            return Visit::Skip;
        }
        match kind_of(&stat) {
            FileKind::Regular if temporary_kind(name.to_bytes()) == Some(FileKind::Regular) => {
                Visit::Items(vec![WalkItem::Engine(rel_path)])
            }
            FileKind::Directory => self.visit_directory(parent, name, rel_path, &stat),
            FileKind::Symlink => {
                let mut row = row_from_stat(rel_path, &stat);
                row.link_target = read_link(parent, name, stat.size);
                Visit::Items(vec![WalkItem::Row(row)])
            }
            _ => Visit::Items(vec![WalkItem::Row(row_from_stat(rel_path, &stat))]),
        }
    }

    /// Open and list a directory through the descriptor its row is taken
    /// from, so the row and the listing name one inode.
    fn visit_directory(
        &self,
        parent: BorrowedFd<'_>,
        name: &CStr,
        rel_path: Vec<u8>,
        stat: &Stat,
    ) -> Visit {
        // The directory's depth below the root: its parent is the top level,
        // at depth `stack.len() - 1`. At the cap it is a row, and its
        // contents are one refused subtree, never opened.
        if self.stack.len() >= MAX_WALK_DEPTH {
            return Visit::Items(vec![
                WalkItem::Row(row_from_stat(rel_path.clone(), stat)),
                WalkItem::Refused(RefusedSeat {
                    rel_path,
                    refusal: BulkloadRefusal::PathDepthExceeded,
                }),
            ]);
        }
        let dir = match sys::open_dir_at(parent, name) {
            Ok(dir) => dir,
            // Swapped for a symlink or a file since the stat: not followed,
            // and not a directory row any more.
            Err(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                return refuse(rel_path, BulkloadRefusal::SourceChangedAfterSnapshot);
            }
            // Unreadable: the directory is a seat, its contents are refused.
            Err(error) => {
                return Visit::Items(vec![
                    WalkItem::Row(row_from_stat(rel_path.clone(), stat)),
                    WalkItem::Refused(RefusedSeat {
                        rel_path,
                        refusal: BulkloadRefusal::from(error),
                    }),
                ]);
            }
        };
        let opened = match sys::fstat(&dir) {
            Ok(opened) => opened,
            Err(error) => return refuse(rel_path, BulkloadRefusal::from(error)),
        };
        if !self.cross_device && opened.node.dev != self.root_dev {
            return Visit::Skip;
        }
        let row = row_from_stat(rel_path.clone(), &opened);
        let level = match level_of(dir, rel_path.len()) {
            Ok(level) => level,
            Err(refusal) => {
                return Visit::Items(vec![
                    WalkItem::Row(row),
                    WalkItem::Refused(RefusedSeat { rel_path, refusal }),
                ]);
            }
        };
        if temporary_kind(name.to_bytes()) == Some(FileKind::Directory) {
            if let Some(temporaries) = self.only_file_temporaries(&level, &rel_path) {
                let mut items: Vec<WalkItem> =
                    temporaries.into_iter().map(WalkItem::Engine).collect();
                items.push(WalkItem::Engine(rel_path));
                return Visit::Items(items);
            }
        }
        Visit::Descend { row, level }
    }

    /// The relative paths of a tagged directory temporary's contents when it
    /// holds nothing but tagged file temporaries (or seats on another device
    /// a same-device walk skips), so the directory is a crash leftover too.
    /// `None` when anything else is in it, or a name cannot be statted.
    fn only_file_temporaries(&self, level: &Level, prefix: &[u8]) -> Option<Vec<Vec<u8>>> {
        let mut temporaries = Vec::new();
        for name in level.names.as_slice() {
            let stat = sys::fstatat_nofollow(&level.dir, name).ok()?;
            if !self.cross_device && stat.node.dev != self.root_dev {
                continue;
            }
            if !stat.is_file() || temporary_kind(name.to_bytes()) != Some(FileKind::Regular) {
                return None;
            }
            temporaries.push(join(prefix, name.to_bytes()));
        }
        Some(temporaries)
    }
}

impl Iterator for Walker<'_> {
    type Item = WalkItem;

    fn next(&mut self) -> Option<WalkItem> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                return Some(item);
            }
            let level = self.stack.last_mut()?;
            let Some(name) = level.names.next() else {
                // Every name beneath this directory is visited; release its
                // descriptor before the walk moves on.
                self.stack.pop();
                continue;
            };
            // The shared buffer still holds this level's prefix: anything
            // past it is the previous sibling's name, or a popped subtree's.
            let prefix_len = level.prefix_len;
            self.path.truncate(prefix_len);
            if prefix_len != 0 {
                self.path.push(b'/');
            }
            self.path.extend_from_slice(name.to_bytes());
            if self.path.len() > MAX_REL_PATH_BYTES {
                // Not statted and never entered: everything beneath it is
                // longer still.
                self.ready.push_back(WalkItem::Refused(RefusedSeat {
                    rel_path: self.path.clone(),
                    refusal: BulkloadRefusal::PathTooLong,
                }));
                continue;
            }
            let rel_path = self.path.clone();
            let level = self.stack.last()?;
            match self.visit(level.dir.as_fd(), &name, rel_path) {
                Visit::Skip => {}
                Visit::Items(items) => self.ready.extend(items),
                Visit::Descend { row, level } => {
                    self.directories_listed += 1;
                    self.stack.push(level);
                    return Some(WalkItem::Row(row));
                }
            }
        }
    }
}

/// List the directory `dir`, names in byte order, as one walk level.
fn level_of(dir: OwnedFd, prefix_len: usize) -> Result<Level> {
    let mut names = sys::list_dir(&dir)?;
    names.sort_unstable();
    Ok(Level {
        dir,
        prefix_len,
        names: names.into_iter(),
    })
}

/// A single declined seat.
fn refuse(rel_path: Vec<u8>, refusal: BulkloadRefusal) -> Visit {
    Visit::Items(vec![WalkItem::Refused(RefusedSeat { rel_path, refusal })])
}

/// `prefix/name`, or `name` beneath the root.
fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
    if !prefix.is_empty() {
        path.extend_from_slice(prefix);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

/// A symlink's literal target, read beneath its directory. `None` when it
/// cannot be read (the row still carries the seat).
fn read_link(dir: BorrowedFd<'_>, name: &CStr, size: u64) -> Option<Vec<u8>> {
    // `st_size` is the target length on both platforms, but the link may be
    // replaced between the stat and the read: grow until it fits.
    let mut capacity = usize::try_from(size).ok()?.saturating_add(1).max(64);
    for _ in 0..8 {
        let mut buf = vec![0_u8; capacity];
        let read = sys::readlinkat(dir, name, &mut buf).ok()?;
        if read < buf.len() {
            buf.truncate(read);
            return Some(buf);
        }
        capacity = capacity.saturating_mul(2);
    }
    None
}

const fn kind_of(stat: &Stat) -> FileKind {
    if stat.is_file() {
        FileKind::Regular
    } else if stat.is_dir() {
        FileKind::Directory
    } else if stat.mode & S_IFMT == S_IFLNK {
        FileKind::Symlink
    } else {
        FileKind::Other
    }
}

/// Walk `options.root`, consulting and updating `cache`: the streaming walk,
/// collected, with the contents hashed as `options.hash_policy` asks.
///
/// # Errors
///
/// Refuses with [`BulkloadRefusal::PathNotAbsolute`] if the root is relative,
/// and [`BulkloadRefusal::Io`] if the root itself cannot be opened. Per-seat
/// problems are recorded in [`WalkOutcome::refusals`], not returned.
pub fn walk<C: FreshnessCache>(options: &WalkOptions, cache: &mut C) -> Result<WalkOutcome> {
    if !options.root.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let root = sys::open_root(&options.root)?;
    let mut outcome = WalkOutcome::default();
    let mut to_hash: Vec<(usize, PathBuf, StatIdentity, bool)> = Vec::new();

    for item in Walker::new(root.as_fd(), options.cross_device)? {
        let mut row = match item {
            WalkItem::Row(row) => row,
            WalkItem::Refused(seat) => {
                outcome.refusals.push(seat);
                continue;
            }
            WalkItem::Engine(rel_path) => {
                outcome.engine_temporaries.push(rel_path);
                continue;
            }
        };

        outcome.stats.seats_seen += 1;
        if row.kind == FileKind::Regular {
            outcome.stats.bytes_seen = outcome.stats.bytes_seen.saturating_add(row.size);
        }

        // Cache lookup uses the metadata already in the row. Only an actual
        // content read needs descriptor checks for concurrent source changes.
        let identity = StatIdentity::from_row(&row);
        let requires_digest =
            row.kind == FileKind::Regular && options.hash_policy != HashPolicy::Never;
        let cached_digest = if requires_digest {
            cache.digest(&identity)?
        } else {
            None
        };
        let freshness = if requires_digest {
            if cached_digest.is_some() {
                Freshness::Fresh
            } else {
                Freshness::Stale
            }
        } else {
            cache.lookup(&identity)?
        };
        if freshness == Freshness::Fresh {
            outcome.stats.fresh_skipped += 1;
        }

        let wants_hash = row.kind == FileKind::Regular
            && match options.hash_policy {
                HashPolicy::Never => false,
                HashPolicy::StaleOnly => freshness == Freshness::Stale,
                HashPolicy::Always => true,
            };
        if wants_hash {
            to_hash.push((
                outcome.rows.len(),
                PathBuf::from(OsStr::from_bytes(&row.rel_path)),
                identity,
                freshness == Freshness::Fresh,
            ));
        } else if requires_digest {
            row.blake3 = cached_digest;
        } else {
            cache.record(&identity)?;
        }
        outcome.rows.push(row);
    }

    complete_hashes(&mut outcome, root.as_fd(), &to_hash, cache)?;
    Ok(outcome)
}

/// The kind a leaf names in the materializer's *tagged* temporary grammar:
/// `Regular` for a file temporary, `Directory` for a directory temporary.
/// `None` for anything else, the untagged form included.
fn temporary_kind(leaf: &[u8]) -> Option<FileKind> {
    use crate::materialize::{temporary_name, TemporaryName};
    match temporary_name(leaf)? {
        TemporaryName::File(_) => Some(FileKind::Regular),
        TemporaryName::Directory(_) => Some(FileKind::Directory),
        TemporaryName::Untagged => None,
    }
}

fn complete_hashes<C: FreshnessCache>(
    outcome: &mut WalkOutcome,
    root: BorrowedFd<'_>,
    to_hash: &[(usize, PathBuf, StatIdentity, bool)],
    cache: &mut C,
) -> Result<()> {
    // Bounded handoff: workers never accumulate an O(N) result vector. The
    // owning thread commits each successful read while other workers continue.
    let (sender, receiver) = std::sync::mpsc::sync_channel(rayon::current_num_threads());
    std::thread::scope(|scope| -> Result<()> {
        let producer = std::thread::Builder::new().spawn_scoped(scope, move || {
            let _ = to_hash
                .par_iter()
                .try_for_each(|(index, rel, identity, reread)| {
                    let read = hash::hash_beneath_observed(root, rel, identity);
                    sender.send((*index, *reread, read)).map_err(|_| ())
                });
        })?;
        let mut result = Ok(());
        for (index, reread, read) in &receiver {
            if let Err(refusal) = finish_read(outcome, cache, index, reread, read) {
                result = Err(refusal);
                break;
            }
        }
        // A failed cache write must unblock senders before joining them.
        drop(receiver);
        producer.join().map_err(|_| BulkloadRefusal::Io(None))?;
        result
    })
}

fn finish_read<C: FreshnessCache>(
    outcome: &mut WalkOutcome,
    cache: &mut C,
    index: usize,
    reread: bool,
    read: hash::HashRead,
) -> Result<()> {
    outcome.stats.bytes_read = outcome.stats.bytes_read.saturating_add(read.bytes_read);
    if reread {
        outcome.stats.bytes_reread_on_resume = outcome
            .stats
            .bytes_reread_on_resume
            .saturating_add(read.bytes_read);
    }
    if read.metadata_checks > 0 {
        outcome.stats.files_statted_twice += 1;
    }
    let row = outcome
        .rows
        .get_mut(index)
        .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
    match read.digest {
        Ok(bytes) => {
            cache.record_digest(&StatIdentity::from_row(row), &bytes)?;
            row.blake3 = Some(bytes);
        }
        Err(refusal) => outcome.refusals.push(RefusedSeat {
            rel_path: row.rel_path.clone(),
            refusal,
        }),
    }
    Ok(())
}

/// A row from a no-follow stat of the seat.
pub(crate) const fn row_from_stat(rel_path: Vec<u8>, stat: &Stat) -> RowSchema {
    RowSchema {
        rel_path,
        kind: kind_of(stat),
        dev: stat.node.dev,
        ino: stat.node.ino,
        size: stat.size,
        mtime_ns: stat.mtime_ns,
        ctime_ns: stat.ctime_ns,
        mode: stat.mode,
        nlink: stat.nlink,
        link_target: None,
        blake3: None,
    }
}

/// The relative path of a row as a path beneath a root descriptor.
pub(crate) fn rel_path(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use std::path::PathBuf;

    use bulkload_proto::FileKind;

    use super::{walk, HashPolicy, WalkItem, WalkOptions, Walker};
    use crate::freshness::MemoryCache;

    struct Corpus {
        root: PathBuf,
    }

    impl Corpus {
        fn new(name: &str) -> Self {
            let mut root = std::env::temp_dir();
            root.push(format!("bulkload-walk-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("nested")).unwrap();
            std::fs::write(root.join("a.txt"), b"alpha").unwrap();
            std::fs::write(root.join("nested/b.txt"), b"bravo!").unwrap();
            Self { root }
        }
    }

    impl Drop for Corpus {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn refuses_a_relative_root() {
        let mut cache = MemoryCache::new();
        let options = WalkOptions::new(PathBuf::from("relative/path"));
        assert!(walk(&options, &mut cache).is_err());
    }

    #[test]
    fn walks_every_seat_once() {
        let corpus = Corpus::new("once");
        let mut cache = MemoryCache::new();
        let outcome = walk(&WalkOptions::new(corpus.root.clone()), &mut cache).unwrap();

        // two files plus one directory
        assert_eq!(outcome.stats.seats_seen, 3);
        assert_eq!(outcome.stats.bytes_seen, 11);
        assert_eq!(outcome.stats.files_statted_twice, 0);
        assert_eq!(outcome.stats.bytes_reread_on_resume, 0);
        assert!(outcome.refusals.is_empty());

        let files = outcome
            .rows
            .iter()
            .filter(|row| row.kind == FileKind::Regular)
            .count();
        assert_eq!(files, 2);
        assert!(outcome.rows.iter().all(|row| row.blake3.is_none()));
    }

    #[test]
    fn hashes_regular_files_when_asked() {
        let corpus = Corpus::new("hash");
        let mut cache = MemoryCache::new();
        let options = WalkOptions {
            hash_policy: HashPolicy::Always,
            ..WalkOptions::new(corpus.root.clone())
        };
        let outcome = walk(&options, &mut cache).unwrap();

        let hashed = outcome
            .rows
            .iter()
            .filter(|row| row.blake3.is_some())
            .count();
        assert_eq!(hashed, 2);
        assert!(outcome.refusals.is_empty());
    }

    #[test]
    fn a_warm_stale_only_walk_rereads_nothing() {
        let corpus = Corpus::new("resume");
        let mut cache = MemoryCache::new();
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };

        let cold = walk(&options, &mut cache).unwrap();
        assert_eq!(cold.stats.fresh_skipped, 0);
        assert_eq!(cold.stats.bytes_reread_on_resume, 0);
        assert_eq!(cold.stats.bytes_read, 11);
        assert_eq!(cold.stats.files_statted_twice, 2);

        let warm = walk(&options, &mut cache).unwrap();
        assert_eq!(warm.stats.fresh_skipped, warm.stats.seats_seen);
        // R25 headline bar: a resume that changed nothing re-reads nothing.
        assert_eq!(warm.stats.bytes_reread_on_resume, 0);
        assert_eq!(warm.stats.bytes_read, 0);
        assert_eq!(warm.stats.files_statted_twice, 0);
        assert_eq!(cold.rows, warm.rows);
    }

    #[test]
    fn an_always_hash_walk_over_a_warm_cache_counts_the_rereads() {
        let corpus = Corpus::new("reread");
        let mut cache = MemoryCache::new();
        let always = WalkOptions {
            hash_policy: HashPolicy::Always,
            ..WalkOptions::new(corpus.root.clone())
        };
        walk(&always, &mut cache).unwrap();
        let warm = walk(&always, &mut cache).unwrap();
        assert_eq!(warm.stats.bytes_reread_on_resume, 11);
    }

    #[test]
    fn stat_only_census_does_not_poison_content_resume() {
        let corpus = Corpus::new("stat-then-hash");
        let mut cache = MemoryCache::new();
        walk(&WalkOptions::new(corpus.root.clone()), &mut cache).unwrap();
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };
        let hashed = walk(&options, &mut cache).unwrap();
        assert_eq!(
            hashed
                .rows
                .iter()
                .filter(|row| row.blake3.is_some())
                .count(),
            2
        );
        assert_eq!(hashed.stats.bytes_reread_on_resume, 0);
    }

    #[test]
    fn refused_hash_never_records_completion() {
        use crate::freshness::{FreshnessCache, StatIdentity};
        use bulkload_proto::Result;
        struct RemovedDuringLookup {
            path: PathBuf,
        }
        impl FreshnessCache for RemovedDuringLookup {
            fn lookup(&self, _: &StatIdentity) -> Result<crate::freshness::Freshness> {
                Ok(crate::freshness::Freshness::Stale)
            }
            fn record(&mut self, _: &StatIdentity) -> Result<()> {
                Ok(())
            }
            fn digest(&self, _: &StatIdentity) -> Result<Option<[u8; 32]>> {
                let _ = std::fs::remove_file(&self.path);
                Ok(None)
            }
            fn record_digest(&mut self, _: &StatIdentity, _: &[u8; 32]) -> Result<()> {
                panic!("failed hash must not become a completion")
            }
        }
        let corpus = Corpus::new("hash-failure");
        std::fs::remove_file(corpus.root.join("nested/b.txt")).unwrap();
        let mut cache = RemovedDuringLookup {
            path: corpus.root.join("a.txt"),
        };
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };
        let result = walk(&options, &mut cache).unwrap();
        assert_eq!(result.refusals.len(), 1);
        assert_eq!(result.stats.bytes_read, 0);
        assert_eq!(result.stats.files_statted_twice, 0);
        assert!(result.rows.iter().all(|row| row.blake3.is_none()));
    }
    #[test]
    fn tagged_temporaries_are_recorded_not_seats() {
        let corpus = Corpus::new("temporaries");
        let empty = ".bulkload-0123456789abcdef-d-1-2";
        let held = ".bulkload-0123456789abcdef-d-1-3";
        let file = ".bulkload-0123456789abcdef-1-4";
        std::fs::create_dir(corpus.root.join(empty)).unwrap();
        std::fs::create_dir(corpus.root.join(held)).unwrap();
        std::fs::write(corpus.root.join(held).join("kept"), b"k").unwrap();
        std::fs::write(corpus.root.join(file), b"orphan").unwrap();
        std::fs::write(corpus.root.join(".bulkload-12-9"), b"untagged").unwrap();
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let mut recorded = outcome.engine_temporaries.clone();
        recorded.sort();
        let mut expected = vec![empty.as_bytes().to_vec(), file.as_bytes().to_vec()];
        expected.sort();
        assert_eq!(recorded, expected);
        let rows: Vec<&[u8]> = outcome
            .rows
            .iter()
            .map(|row| row.rel_path.as_slice())
            .collect();
        assert!(
            rows.contains(&held.as_bytes()),
            "a non-empty directory is carried"
        );
        assert!(
            rows.contains(&b".bulkload-12-9".as_slice()),
            "untagged is payload"
        );
        // Seats: a.txt, nested, nested/b.txt, the held directory and its file,
        // and the untagged file. Neither excluded temporary counts (N7).
        assert_eq!(outcome.stats.seats_seen, rows.len() as u64);
        assert_eq!(outcome.stats.seats_seen, 6);
    }

    fn rows_of(outcome: &super::WalkOutcome) -> Vec<Vec<u8>> {
        outcome
            .rows
            .iter()
            .map(|row| row.rel_path.clone())
            .collect()
    }

    /// W4 PR 3: the stream yields a directory before everything beneath it,
    /// names in byte order within a directory, and no global sort.
    #[test]
    fn parents_precede_children_without_a_global_sort() {
        let corpus = Corpus::new("order");
        std::fs::create_dir(corpus.root.join("a")).unwrap();
        std::fs::write(corpus.root.join("a/x"), b"x").unwrap();
        std::fs::write(corpus.root.join("a-b"), b"ab").unwrap();
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let rows = rows_of(&outcome);
        let expected: Vec<Vec<u8>> = ["a", "a/x", "a-b", "a.txt", "nested", "nested/b.txt"]
            .iter()
            .map(|path| path.as_bytes().to_vec())
            .collect();
        // A global byte sort would put "a-b" before "a/x" ('-' < '/').
        assert_eq!(rows, expected);
        for (at, row) in rows.iter().enumerate() {
            if let Some(slash) = row.iter().rposition(|byte| *byte == b'/') {
                let parent = &row[..slash];
                assert!(
                    rows[..at].iter().any(|seen| seen == parent),
                    "{} precedes its parent",
                    String::from_utf8_lossy(row)
                );
            }
        }
    }

    /// W4 PR 3: the first item comes after one listing, not the whole tree.
    #[test]
    fn the_first_item_needs_no_full_walk() {
        use std::os::fd::AsFd as _;
        let corpus = Corpus::new("stream");
        for directory in ["d0", "d1", "d2", "d3"] {
            std::fs::create_dir_all(corpus.root.join(directory).join("inner")).unwrap();
        }
        let root = crate::io::sys::open_root(&corpus.root).unwrap();
        let mut walker = Walker::new(root.as_fd(), false).unwrap();
        let first = walker.next().unwrap();
        assert!(matches!(first, WalkItem::Row(ref row) if row.rel_path == b"a.txt"));
        assert_eq!(walker.directories_listed(), 1);
        let total = walker.by_ref().count();
        assert!(total > 8, "{total}");
        assert_eq!(walker.directories_listed(), 1 + 1 + 4 * 2);
    }

    /// W4 PR 3: a directory is listed through the descriptor its row came
    /// from. Replacing it with a symlink to outside the root after the walk
    /// entered it walks the original inode, never the symlink's target.
    #[test]
    fn a_directory_replaced_mid_walk_is_never_followed_out_of_the_root() {
        use std::os::fd::AsFd as _;
        let corpus = Corpus::new("swap");
        let outside = Corpus::new("swap-outside");
        std::fs::write(outside.root.join("escaped"), b"secret").unwrap();
        let root = crate::io::sys::open_root(&corpus.root).unwrap();
        let mut walker = Walker::new(root.as_fd(), false).unwrap();
        let mut seen = Vec::new();
        for item in walker.by_ref() {
            let WalkItem::Row(row) = item else {
                continue;
            };
            let entered = row.rel_path == b"nested";
            seen.push(row.rel_path);
            if entered {
                std::fs::rename(corpus.root.join("nested"), corpus.root.join("moved")).unwrap();
                std::os::unix::fs::symlink(&outside.root, corpus.root.join("nested")).unwrap();
            }
        }
        assert!(seen.contains(&b"nested/b.txt".to_vec()), "{seen:?}");
        assert!(
            !seen.iter().any(|path| path.ends_with(b"escaped")),
            "{seen:?}"
        );
    }

    #[test]
    fn a_symlinked_directory_is_a_row_not_a_descent() {
        let corpus = Corpus::new("link");
        let outside = Corpus::new("link-outside");
        std::os::unix::fs::symlink(&outside.root, corpus.root.join("link")).unwrap();
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let link = outcome
            .rows
            .iter()
            .find(|row| row.rel_path == b"link")
            .unwrap();
        assert_eq!(link.kind, FileKind::Symlink);
        assert_eq!(
            link.link_target.as_deref(),
            Some(outside.root.as_os_str().as_encoded_bytes())
        );
        assert!(!rows_of(&outcome)
            .iter()
            .any(|path| path.starts_with(b"link/")));
    }

    /// A tagged directory temporary holding only tagged file temporaries is a
    /// crash leftover as a whole: neither it nor its contents are rows.
    #[test]
    fn a_directory_temporary_of_file_temporaries_is_recorded_whole() {
        let corpus = Corpus::new("temporary-tree");
        let directory = ".bulkload-0123456789abcdef-d-1-2";
        let file = ".bulkload-0123456789abcdef-1-4";
        std::fs::create_dir(corpus.root.join(directory)).unwrap();
        std::fs::write(corpus.root.join(directory).join(file), b"orphan").unwrap();
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let mut recorded = outcome.engine_temporaries.clone();
        recorded.sort();
        assert_eq!(
            recorded,
            vec![
                directory.as_bytes().to_vec(),
                format!("{directory}/{file}").into_bytes()
            ]
        );
        assert!(!rows_of(&outcome)
            .iter()
            .any(|path| path.starts_with(directory.as_bytes())));
        assert_eq!(outcome.stats.seats_seen, 3);
    }

    #[test]
    fn an_unlistable_directory_is_a_seat_with_its_contents_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::io::sys::effective_uid() == 0 {
            return;
        }
        let corpus = Corpus::new("unlistable");
        let locked = corpus.root.join("nested");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = outcome.unwrap();
        assert!(rows_of(&outcome).contains(&b"nested".to_vec()));
        assert_eq!(outcome.refusals.len(), 1);
        assert_eq!(outcome.refusals[0].rel_path, b"nested");
        assert_eq!(outcome.refusals[0].refusal.code(), "IO");
    }

    /// #110: a chain deeper than the cap refuses only the subtree below the
    /// cap, as a value; every seat at or above it and every sibling is
    /// carried.
    #[test]
    fn a_tree_deeper_than_the_cap_refuses_only_that_subtree() {
        use super::MAX_WALK_DEPTH;
        let corpus = Corpus::new("depth");
        // `deep` is depth 1; the chain under it reaches depth cap + 2.
        let mut chain = vec!["deep".to_owned()];
        chain.extend((1..MAX_WALK_DEPTH + 2).map(|_| "d".to_owned()));
        let at = |depth: usize| chain[..depth].join("/");
        std::fs::create_dir_all(corpus.root.join(at(MAX_WALK_DEPTH + 2))).unwrap();
        std::fs::write(corpus.root.join(at(MAX_WALK_DEPTH - 1)).join("leaf"), b"l").unwrap();
        std::fs::write(corpus.root.join(at(MAX_WALK_DEPTH)).join("hidden"), b"h").unwrap();
        std::fs::write(corpus.root.join("deep/side"), b"s").unwrap();
        std::fs::write(corpus.root.join("z.txt"), b"z").unwrap();

        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let rows = rows_of(&outcome);
        let capped = at(MAX_WALK_DEPTH).into_bytes();
        assert_eq!(outcome.refusals.len(), 1, "{:?}", outcome.refusals);
        assert_eq!(outcome.refusals[0].rel_path, capped);
        assert_eq!(outcome.refusals[0].refusal.code(), "PATH_DEPTH_EXCEEDED");
        assert!(rows.contains(&capped), "the capped directory is a seat");
        let leaf = format!("{}/leaf", at(MAX_WALK_DEPTH - 1)).into_bytes();
        assert!(rows.contains(&leaf), "a seat at the cap is carried");
        for sibling in ["a.txt", "nested", "nested/b.txt", "deep/side", "z.txt"] {
            assert!(rows.contains(&sibling.as_bytes().to_vec()), "{sibling}");
        }
        let mut beneath = capped;
        beneath.push(b'/');
        assert!(
            !rows.iter().any(|row| row.starts_with(&beneath)),
            "nothing beneath the capped directory is a row"
        );
        assert!(rows
            .iter()
            .all(|row| row.split(|byte| *byte == b'/').count() <= MAX_WALK_DEPTH));
    }

    /// #110: a seat whose relative path is past the cap is refused before it
    /// is statted, and a directory refused so is never entered; its shorter
    /// siblings are carried.
    #[test]
    fn a_path_longer_than_the_cap_refuses_only_that_subtree() {
        use super::MAX_REL_PATH_BYTES;
        use crate::io::sys;
        use std::ffi::CString;
        use std::os::fd::AsFd as _;
        let corpus = Corpus::new("long");
        // 200-byte names: 20 levels are 4019 bytes, 21 are 4220. The
        // absolute path is past PATH_MAX, so build it beneath descriptors.
        let long = "n".repeat(200);
        let name = CString::new(long.clone()).unwrap();
        let mut dir = sys::open_root(&corpus.root).unwrap();
        for _ in 0..21 {
            sys::mkdirat(dir.as_fd(), &name, 0o755).unwrap();
            let next = sys::open_dir_at(dir.as_fd(), &name).unwrap();
            sys::create_excl_at(next.as_fd(), c"f", 0o644).unwrap();
            dir = next;
        }
        let outcome = walk(
            &WalkOptions::new(corpus.root.clone()),
            &mut MemoryCache::new(),
        )
        .unwrap();
        let rows = rows_of(&outcome);
        let level = |depth: usize| vec![long.as_str(); depth].join("/").into_bytes();
        assert!(level(20).len() <= MAX_REL_PATH_BYTES);
        assert!(level(21).len() > MAX_REL_PATH_BYTES);
        assert_eq!(outcome.refusals.len(), 1);
        assert_eq!(outcome.refusals[0].rel_path, level(21));
        assert_eq!(outcome.refusals[0].refusal.code(), "PATH_TOO_LONG");
        assert!(rows.contains(&level(20)));
        let mut file = level(20);
        file.extend_from_slice(b"/f");
        assert!(rows.contains(&file), "a shorter sibling is carried");
        assert!(rows.contains(&b"nested/b.txt".to_vec()));
        assert!(!rows.iter().any(|row| row.len() > MAX_REL_PATH_BYTES));
    }
}
