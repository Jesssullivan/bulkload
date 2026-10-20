//! File-level reconcile of a source's untracked and ignored entries.
//!
//! Operator ruling 2026-10-16 21:24Z: "refs in always; a tree beside ONLY
//! where neo has uncommitted work" (lane note
//! `docs/agent-notes/2026-10-19-clean-source-no-beside.md`).
//!
//! When the source checkout holds no tracked change, its tracked content
//! is its HEAD's, which `--refs-in` makes reachable in the live repository:
//! no tree is landed beside the live checkout. What the HEAD does not hold,
//! the source's untracked and ignored entries, is reconciled here one
//! entry at a time, and never by overwriting anything:
//!
//! - absent at the live checkout: **placed** (create-only: a private
//!   temporary in the same directory, fsynced at its mode, then renamed in
//!   with `RENAME_NOREPLACE`; a directory made `0700` and given its mode
//!   once everything under it is in);
//! - present and equal (the same kind, the same bytes or link target, and
//!   for a regular file the same executable bit; a directory is a
//!   directory): **adopted**, nothing written;
//! - present and different: **landed beside** as `<path>SUFFIX` (the
//!   `--beside` suffix, so `.neo-20261016`), create-only, and counted; one
//!   already there and equal is counted present. A directory whose live
//!   path holds a file lands its subtree under `<dir>SUFFIX/`; an entry
//!   below a live file (or a symlink, never followed) lands under
//!   `<that path>SUFFIX/`.
//! - `<path>SUFFIX` itself held by something else: **occupied**, nothing
//!   written there; the caller refuses the item `GIT_BESIDE_FOREIGN`
//!   naming that path, after every other entry is in.
//! - **derived** output ([`class_in`]: `build/`, `dist/`, `.svelte-kit/`,
//!   `.artifacts/`, a `bazel-*` link, a `mktemp`-named directory,
//!   `.serena/`, the capture's rebuildable roots, each a directory nothing
//!   tracked is under) is **rebuildable** (operator ruling 2026-10-20,
//!   "Ledger as rebuildable"): never placed, never compared, and never
//!   written into the live checkout; each entry's path, kind, mode, size
//!   and BLAKE3 (a link's target) is ledgered ([`ledger`]) and counted. An
//!   apply's bytes stay in the capture's provenance; a retirement removes
//!   them with the tree, its ledger naming each one.
//!
//! Nothing is excluded: every entry ends counted in exactly one of those
//! classes, and the count is checked. Every write is
//! made through directory descriptors opened beneath the live checkout one
//! component at a time, never following a link. The temporaries a pass is
//! about to make are recorded write-ahead ([`FilesRecord`]), each directory
//! it made (by its identity) as it makes it, and what it placed or landed
//! beside (by identity) as it goes: [`settle`] finishes a pass a crash cut
//! short (its temporaries removed, the directories it made sealed or, when
//! empty, removed), and [`undo`] takes back every placement still as it was
//! placed. A rerun finds what the last one placed equal, and adopts it.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::batch_objects::BatchObjects;
use super::super::landed::{self, Kind};
use super::super::{git, output, REBUILDABLE_DIRECTORIES};
use super::{holds, lstat, Side};
use crate::io::sys;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::converge::files";

/// One source entry to reconcile: its path under the source root, its
/// kind and mode, and where its bytes are: a blob of the capture
/// (`side.blob`, `side.link`) or a file on disk (`disk`).
#[derive(Debug, Clone)]
pub(in crate::git_carry) struct Want {
    pub path: Vec<u8>,
    pub side: Side,
    /// The entry on disk (a beside tree being retired); `None` for a
    /// capture's seat, read from the object store.
    pub disk: Option<PathBuf>,
    /// An entry on disk's size when it was read (a file's length, a link's
    /// target's); 0 for a directory or a seat (its blob's, ledgered).
    pub size: u64,
}

impl Want {
    /// A capture's seat.
    pub(in crate::git_carry) const fn seat(path: Vec<u8>, side: Side) -> Self {
        Self {
            path,
            side,
            disk: None,
            size: 0,
        }
    }

    /// An entry of a tree on disk (`root`/`path`), read without following
    /// a link; `None` for a kind no seat has (a socket, a device).
    pub(in crate::git_carry) fn on_disk(root: &Path, path: Vec<u8>) -> Result<Option<Self>> {
        let at = root.join(OsStr::from_bytes(&path));
        let meta = fs::symlink_metadata(&at).refuse_at(SITE)?;
        let kind = if meta.file_type().is_symlink() {
            Kind::Symlink
        } else if meta.is_file() {
            Kind::Regular
        } else if meta.is_dir() {
            Kind::Directory
        } else {
            return Ok(None);
        };
        let link = if kind == Kind::Symlink {
            Some(
                fs::read_link(&at)
                    .refuse_at(SITE)?
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            )
        } else {
            None
        };
        Ok(Some(Self {
            path,
            side: Side {
                kind,
                mode: meta.mode() & 0o777,
                blob: None,
                link,
                prefix: Vec::new(),
            },
            disk: Some(at),
            size: if kind == Kind::Directory {
                0
            } else {
                meta.len()
            },
        }))
    }
}

/// What an entry is, for the build-output ruling the operator is asked for
/// (the coordinator, 2026-10-20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Source-like: written by a person or an agent.
    Source,
    /// Derived: build or tool output a build or a tool makes again.
    Derived,
}

// Whether a path component is a `mktemp`-style name: `<stem>.XXXXXX`, six
// letters and digits with at least one upper-case letter or digit
// (`.native-osstore-vendor.Ab3xZ9`).
fn mktemp_named(name: &[u8]) -> bool {
    let Some(dot) = name.iter().rposition(|byte| *byte == b'.') else {
        return false;
    };
    let tail = name.get(dot + 1..).unwrap_or_default();
    dot > 0
        && tail.len() == 6
        && tail.iter().all(u8::is_ascii_alphanumeric)
        && tail
            .iter()
            .any(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// The class of the entry at `path` (relative).
///
/// Derived when a component is `build`, `dist`, `.svelte-kit`,
/// `.artifacts`, `.serena`, one of the capture's rebuildable roots, a
/// `bazel-*` name or a `mktemp`-style name; source-like otherwise.
#[must_use]
pub fn class_of(path: &[u8]) -> Class {
    let derived = path.split(|byte| *byte == b'/').any(|part| {
        matches!(
            part,
            b"build" | b"dist" | b".svelte-kit" | b".artifacts" | b".serena"
        ) || part.starts_with(b"bazel-")
            || REBUILDABLE_DIRECTORIES
                .iter()
                .any(|name| name.as_bytes() == part)
            || mktemp_named(part)
    });
    if derived {
        Class::Derived
    } else {
        Class::Source
    }
}

/// Whether `path` or a path under it is in `tracked`.
pub(in crate::git_carry) fn tracks(tracked: &BTreeSet<Vec<u8>>, path: &[u8]) -> bool {
    let mut under = path.to_vec();
    under.push(b'/');
    let mut past = path.to_vec();
    past.push(b'/' + 1);
    tracked.contains(path) || tracked.range(under..past).next().is_some()
}

/// The class of the entry at `path` (relative) in a checkout whose indexes
/// track `tracked`.
///
/// Derived when one of its components is a derived name ([`class_of`])
/// and nothing any of `tracked` holds is at or under that component's
/// path: a `build/` directory that holds tracked files is a source
/// directory, and an untracked file in it source-like.
#[must_use]
pub fn class_in(path: &[u8], tracked: &[&BTreeSet<Vec<u8>>]) -> Class {
    let mut end = 0;
    for part in path.split(|byte| *byte == b'/') {
        end += part.len();
        let root = path.get(..end).unwrap_or_default();
        if class_of(part) == Class::Derived && !tracked.iter().any(|set| tracks(set, root)) {
            return Class::Derived;
        }
        end += 1;
    }
    Class::Source
}

/// How every entry of one reconcile ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Files and links placed at their own path.
    pub placed: u64,
    /// Files and links already there, equal.
    pub adopted: u64,
    /// Files and links that differ from the live one, landed at
    /// `<path>SUFFIX` (or under a beside directory).
    pub beside: u64,
    /// Files and links an earlier pass landed beside, there and equal.
    pub beside_present: u64,
    /// Directories made (the source's own, at their own path).
    pub dirs_placed: u64,
    /// Directories already there.
    pub dirs_adopted: u64,
    /// Directories landed (or already) at a beside path.
    pub dirs_beside: u64,
    /// Entries whose beside path holds something else: nothing written.
    pub occupied: u64,
    /// Derived entries ([`class_in`]): ledgered as rebuildable, never
    /// written (operator ruling 2026-10-20).
    pub rebuildable: u64,
    /// Their bytes (a file's length, a link's target's).
    pub rebuildable_bytes: u64,
    /// Bytes written by placements and beside landings.
    pub bytes: u64,
    /// The entries written or to write (placed, beside, made): every one
    /// source-like.
    pub written_source: u64,
}

impl Tally {
    /// Every entry counted.
    #[must_use]
    pub const fn entries(&self) -> u64 {
        self.placed
            + self.adopted
            + self.beside
            + self.beside_present
            + self.dirs_placed
            + self.dirs_adopted
            + self.dirs_beside
            + self.occupied
            + self.rebuildable
    }

    /// The receipt words.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "files-placed={} files-adopted={} files-beside={} files-beside-present={} dirs-placed={} dirs-adopted={} dirs-beside={} files-occupied={} files-rebuildable={} files-rebuildable-bytes={} files-written-source={} files-bytes-placed={}",
            self.placed,
            self.adopted,
            self.beside,
            self.beside_present,
            self.dirs_placed,
            self.dirs_adopted,
            self.dirs_beside,
            self.occupied,
            self.rebuildable,
            self.rebuildable_bytes,
            self.written_source,
            self.bytes,
        )
    }
}

/// One directory a pass made, by identity, and the mode it ends at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    pub path: Vec<u8>,
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
}

/// One file or link a pass placed (or landed beside), by identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    pub path: Vec<u8>,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
    /// Landed beside (`<path>SUFFIX`), not at its own path.
    pub beside: bool,
}

/// One derived entry, ledgered as rebuildable (operator ruling 2026-10-20,
/// "Ledger as rebuildable"): never placed in the live checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebuildable {
    /// Its path, relative to the source root.
    pub path: Vec<u8>,
    pub kind: Kind,
    pub mode: u32,
    /// A file's length, a link's target's; 0 for a directory.
    pub size: u64,
    /// A file's BLAKE3; `None` for a link or a directory.
    pub blake3: Option<[u8; 32]>,
    /// A link's target.
    pub link: Option<Vec<u8>>,
    /// Whether the live checkout holds something at its path (never read:
    /// build output is not compared).
    pub live_present: bool,
}

impl Rebuildable {
    /// The receipt word of its kind.
    #[must_use]
    pub const fn kind_word(&self) -> &'static str {
        match self.kind {
            Kind::Directory => "dir",
            Kind::Regular => "file",
            Kind::Symlink => "link",
        }
    }
}

/// What STATE records of one reconcile (`{item}-{blake3(SOURCE)}.files`
/// for an estate item; a retirement's record for a beside tree).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesRecord {
    /// `estate::id` of the item, or the retirement's tag.
    pub item: String,
    /// The capture reconciled (an item's bundle digest; a retirement's
    /// tree tag).
    pub capture: [u8; 32],
    /// The live checkout's path.
    pub live: Vec<u8>,
    /// The beside suffix.
    pub suffix: String,
    /// How many tracked entries the source's HEAD holds (not reconciled
    /// here: the HEAD is reachable in the live repository).
    pub tracked: u64,
    /// The temporaries this pass may leave (write-ahead).
    pub temps: Vec<Vec<u8>>,
    /// The directories this pass made, as it made them.
    pub made: Vec<Made>,
    /// The files and links it placed or landed beside, as it went (in
    /// batches: a crash may leave the last few unlisted, each the source's
    /// bytes, which the next pass adopts).
    pub placed: Vec<Placed>,
    /// The pass ended: `tally` is its count.
    pub done: bool,
    pub tally: Tally,
    /// `(source path, where it landed)` of each entry landed beside.
    pub besides: Vec<(Vec<u8>, Vec<u8>)>,
    /// The beside paths that hold something else.
    pub occupied: Vec<Vec<u8>>,
    /// The derived entries, ledgered as rebuildable ([`ledger`]).
    pub rebuildable: Vec<Rebuildable>,
    /// [`undo`] took the placements back.
    pub undone: bool,
}

impl landed::Record for FilesRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-files03";
}

/// The record at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<FilesRecord>> {
    landed::read::<FilesRecord>(path)
}

/// A pass's record, written as it goes.
pub(in crate::git_carry) struct Journal<'a> {
    path: &'a Path,
    pub record: FilesRecord,
    unsaved: usize,
}

impl<'a> Journal<'a> {
    /// Write `record` at `path` (write-ahead) and keep it.
    ///
    /// # Errors
    /// IO.
    pub(in crate::git_carry) fn begin(path: &'a Path, record: FilesRecord) -> Result<Self> {
        landed::write(path, &record)?;
        Ok(Self {
            path,
            record,
            unsaved: 0,
        })
    }

    fn made(&mut self, made: Made) -> Result<()> {
        self.record.made.push(made);
        self.save()
    }

    fn placed(&mut self, placed: Placed) -> Result<()> {
        self.record.placed.push(placed);
        self.unsaved += 1;
        if self.unsaved >= 128 {
            self.save()?;
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        self.unsaved = 0;
        landed::write(self.path, &self.record)
    }

    /// Record the pass finished with `tally`.
    ///
    /// # Errors
    /// IO.
    pub(in crate::git_carry) fn finish(mut self, tally: Tally) -> Result<FilesRecord> {
        self.record.tally = tally;
        self.record.done = true;
        self.save()?;
        Ok(self.record)
    }
}

fn path_of(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

/// Settle what an unfinished pass recorded at `path` left.
///
/// Its temporaries are removed (a regular file or a link at a recorded
/// private name), and each directory it made (its identity unchanged),
/// deepest first, is removed when empty or given its mode when still the
/// `0700` it was made at; nothing else is touched. The next pass plans
/// again. A finished record is left as it is.
///
/// # Errors
/// IO.
pub fn settle(path: &Path) -> Result<()> {
    let Some(record) = read_record(path)? else {
        return Ok(());
    };
    if record.done {
        return Ok(());
    }
    for temp in &record.temps {
        let temp = path_of(temp);
        let private = temp
            .file_name()
            .is_some_and(|name| name.as_bytes().starts_with(TEMP_PREFIX.as_bytes()));
        if !private {
            continue;
        }
        if let Some(meta) = lstat(temp)? {
            if meta.is_file() || meta.file_type().is_symlink() {
                fs::remove_file(temp).refuse_at(SITE)?;
            }
        }
    }
    seal_made(&record.made, false)
}

// Each directory in `made` still the one made (identity), deepest first:
// removed when empty; else, unless `only_empty`, given its mode when still
// `0700`.
fn seal_made(made: &[Made], only_empty: bool) -> Result<()> {
    let mut made: Vec<&Made> = made.iter().collect();
    made.sort_by_key(|made| std::cmp::Reverse(made.path.len()));
    for made in made {
        let dir = path_of(&made.path);
        let Some(meta) = lstat(dir)? else {
            continue;
        };
        if !meta.is_dir() || meta.dev() != made.dev || meta.ino() != made.ino {
            continue;
        }
        if fs::read_dir(dir).refuse_at(SITE)?.next().is_none() {
            fs::remove_dir(dir).refuse_at(SITE)?;
        } else if !only_empty && meta.mode() & 0o7777 == 0o700 && made.mode & 0o7777 != 0o700 {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(dir, fs::Permissions::from_mode(made.mode & 0o7777))
                .refuse_at(SITE)?;
        }
    }
    Ok(())
}

/// What [`undo`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Undone {
    /// Files and links removed (still as placed).
    pub removed: u64,
    /// Files and links kept: gone, or changed since (identity, size or
    /// time).
    pub kept: u64,
    /// Directories removed (made by the pass, empty now).
    pub dirs: u64,
}

/// Take back the placements the record at `path` lists.
///
/// Each file or link still the one placed (identity, size and time
/// unchanged) is removed, then each directory the pass made that is empty
/// now; anything changed since is kept and counted. The record is marked
/// undone.
///
/// # Errors
/// IO.
pub fn undo(path: &Path) -> Result<Undone> {
    let Some(mut record) = read_record(path)? else {
        return Ok(Undone::default());
    };
    let mut undone = Undone::default();
    for placed in record.placed.iter().rev() {
        let at = path_of(&placed.path);
        let same = lstat(at)?.is_some_and(|meta| {
            let identity = crate::freshness::StatIdentity::from_metadata(&meta);
            meta.dev() == placed.dev
                && meta.ino() == placed.ino
                && identity.size == placed.size
                && identity.mtime_ns == placed.mtime_ns
                && !meta.is_dir()
        });
        if same {
            fs::remove_file(at).refuse_at(SITE)?;
            undone.removed += 1;
        } else {
            undone.kept += 1;
        }
    }
    let before = record
        .made
        .iter()
        .filter(|made| lstat(path_of(&made.path)).ok().flatten().is_some())
        .count();
    seal_made(&record.made, true)?;
    let after = record
        .made
        .iter()
        .filter(|made| lstat(path_of(&made.path)).ok().flatten().is_some())
        .count();
    undone.dirs = u64::try_from(before.saturating_sub(after)).unwrap_or(u64::MAX);
    record.undone = true;
    landed::write(path, &record)?;
    Ok(undone)
}

/// The private name prefix of a placement's temporary.
pub const TEMP_PREFIX: &str = ".bulkload-place-";

/// What one entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::git_carry) enum Act {
    Place,
    Adopt,
    Beside,
    BesidePresent,
    DirPlace,
    DirAdopt,
    DirBeside,
    DirBesideAdopt,
    Occupied,
    /// A derived entry: ledgered as rebuildable, never written.
    Rebuildable,
}

impl Act {
    /// Whether the entry is written (placed, landed beside or made).
    const fn writes(self) -> bool {
        matches!(
            self,
            Self::Place | Self::Beside | Self::DirPlace | Self::DirBeside
        )
    }
}

/// One entry's decision: what it does and where (relative to the live
/// root).
#[derive(Debug, Clone)]
pub(in crate::git_carry) struct Step {
    pub want: usize,
    pub act: Act,
    pub at: Vec<u8>,
    /// For a rebuildable entry: whether the live checkout holds something
    /// at its path.
    pub present: bool,
}

/// A planned reconcile.
#[derive(Debug, Clone, Default)]
pub(in crate::git_carry) struct Planned {
    pub steps: Vec<Step>,
}

impl Planned {
    /// What the plan would count, written or not.
    pub(in crate::git_carry) fn tally(&self, wants: &[Want]) -> Tally {
        let mut tally = Tally::default();
        for step in &self.steps {
            if step.act.writes() {
                tally.written_source += 1;
            }
            match step.act {
                Act::Place => tally.placed += 1,
                Act::Adopt => tally.adopted += 1,
                Act::Beside => tally.beside += 1,
                Act::BesidePresent => tally.beside_present += 1,
                Act::DirPlace => tally.dirs_placed += 1,
                Act::DirAdopt => tally.dirs_adopted += 1,
                Act::DirBeside | Act::DirBesideAdopt => tally.dirs_beside += 1,
                Act::Occupied => tally.occupied += 1,
                Act::Rebuildable => {
                    tally.rebuildable += 1;
                    tally.rebuildable_bytes += wants.get(step.want).map_or(0, |want| want.size);
                }
            }
        }
        tally
    }

    /// The beside landings, as `(source path, landed path)`.
    pub(in crate::git_carry) fn besides(&self, wants: &[Want]) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.steps
            .iter()
            .filter(|step| {
                matches!(
                    step.act,
                    Act::Beside | Act::BesidePresent | Act::DirBeside | Act::DirBesideAdopt
                )
            })
            .filter_map(|step| {
                wants
                    .get(step.want)
                    .map(|want| (want.path.clone(), step.at.clone()))
            })
            .collect()
    }

    /// The occupied beside paths.
    pub(in crate::git_carry) fn occupied(&self) -> Vec<Vec<u8>> {
        self.steps
            .iter()
            .filter(|step| step.act == Act::Occupied)
            .map(|step| step.at.clone())
            .collect()
    }
}

/// What a live path holds.
enum At {
    Absent,
    Dir,
    Other,
    /// A path above it that is no directory (a file, or a link: never
    /// followed): its relative path.
    Blocked(Vec<u8>),
}

fn join(base: &[u8], leaf: &[u8]) -> Vec<u8> {
    if base.is_empty() {
        return leaf.to_vec();
    }
    let mut path = base.to_vec();
    path.push(b'/');
    path.extend_from_slice(leaf);
    path
}

fn suffixed(path: &[u8], suffix: &str) -> Vec<u8> {
    let mut path = path.to_vec();
    path.extend_from_slice(suffix.as_bytes());
    path
}

// The proper ancestors of a relative path, shallowest first.
fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .filter_map(|(at, _)| path.get(..at))
}

// Whether a relative path is one this reconcile may name: plain
// components, none `.git` (in any case), none empty, `.` or `..`.
fn plain(path: &[u8]) -> bool {
    !path.is_empty()
        && path.split(|byte| *byte == b'/').all(|part| {
            !part.is_empty() && part != b"." && part != b".." && !part.eq_ignore_ascii_case(b".git")
        })
}

/// The paths the index of the checkout at `worktree` holds.
///
/// # Errors
/// Whatever listing the index refuses.
pub(in crate::git_carry) fn index_paths(worktree: &Path) -> Result<BTreeSet<Vec<u8>>> {
    let listed = output(git(worktree).args(["ls-files", "-z"]))?;
    Ok(listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// Every want's path is plain (no `.git` component, no `..`): `false`
/// when one is not, which no reconcile names.
pub(in crate::git_carry) fn all_plain(wants: &[Want]) -> bool {
    wants.iter().all(|want| plain(&want.path))
}

// What the live tree holds at `rel`, never following a link; a path
// under one `planned` will make is absent.
fn at(live: &Path, rel: &[u8], planned: &BTreeSet<Vec<u8>>) -> Result<At> {
    for above in ancestors(rel) {
        if planned.contains(above) {
            return Ok(At::Absent);
        }
        match lstat(&live.join(OsStr::from_bytes(above)))? {
            None => return Ok(At::Absent),
            Some(meta) if meta.is_dir() => {}
            Some(_) => return Ok(At::Blocked(above.to_vec())),
        }
    }
    if planned.contains(rel) {
        return Ok(At::Dir);
    }
    Ok(match lstat(&live.join(OsStr::from_bytes(rel)))? {
        None => At::Absent,
        Some(meta) if meta.is_dir() => At::Dir,
        Some(_) => At::Other,
    })
}

/// Whether the live entry at `path` holds `want`: the same kind, the same
/// bytes or link target, and for a regular file the same executable bit.
///
/// # Errors
/// Whatever reading either side refuses.
pub(in crate::git_carry) fn holds_want(
    objects: Option<&mut BatchObjects>,
    path: &Path,
    want: &Want,
) -> Result<bool> {
    let Some(meta) = lstat(path)? else {
        return Ok(false);
    };
    match want.side.kind {
        Kind::Directory => Ok(meta.is_dir()),
        Kind::Symlink => Ok(meta.file_type().is_symlink()
            && fs::read_link(path).refuse_at(SITE)?.as_os_str().as_bytes()
                == want.side.link.as_deref().unwrap_or_default()),
        Kind::Regular => {
            if !meta.is_file() || (meta.mode() & 0o111 == 0) != (want.side.mode & 0o111 == 0) {
                return Ok(false);
            }
            match (&want.disk, objects) {
                (Some(disk), _) => same_bytes(disk, path),
                (None, Some(objects)) => holds(objects, path, &want.side),
                (None, None) => Err(BulkloadRefusal::GitInventoryMalformed),
            }
        }
    }
}

// Whether two regular files hold the same bytes (read in step, counted).
fn same_bytes(a: &Path, b: &Path) -> Result<bool> {
    use std::io::Read as _;
    let open = |path: &Path| {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .refuse_at(SITE)
    };
    let (left, right) = (open(a)?, open(b)?);
    if left.metadata().refuse_at(SITE)?.len() != right.metadata().refuse_at(SITE)?.len() {
        return Ok(false);
    }
    let mut left = std::io::BufReader::with_capacity(1 << 16, left);
    let mut right = std::io::BufReader::with_capacity(1 << 16, right);
    let mut one = vec![0u8; 1 << 16];
    let mut two = vec![0u8; 1 << 16];
    loop {
        let read = left.read(&mut one).refuse_at(SITE)?;
        let mut filled = 0;
        while filled < read {
            let more = right
                .read(two.get_mut(filled..read).unwrap_or_default())
                .refuse_at(SITE)?;
            if more == 0 {
                return Ok(false);
            }
            filled += more;
        }
        crate::counters::add_len(crate::counters::Counter::DestVerifyRead, read * 2);
        if read == 0 {
            return Ok(right.read(&mut two).refuse_at(SITE)? == 0);
        }
        if one.get(..read) != two.get(..read) {
            return Ok(false);
        }
    }
}

/// Plan the reconcile of `wants` (sorted by path, so a directory comes
/// before what it holds) into the live tree at `live`. Read-only.
///
/// `tracked` holds the paths the live index tracks: one absent on disk is
/// the live checkout's own deletion, never undone, so a source entry there
/// lands beside it. `sourced` holds the paths the source's own index
/// tracks. A derived entry ([`class_in`] of both) is
/// [`Act::Rebuildable`]: never compared and never written, only looked
/// for at its live path (operator ruling 2026-10-20, "Ledger as
/// rebuildable").
///
/// # Errors
/// Whatever reading the live tree or the source refuses.
#[allow(clippy::too_many_lines)] // One decision per entry, in order.
pub(in crate::git_carry) fn plan(
    live: &Path,
    suffix: &str,
    (wants, tracked, sourced): (&[Want], &BTreeSet<Vec<u8>>, &BTreeSet<Vec<u8>>),
    mut objects: Option<&mut BatchObjects>,
) -> Result<Planned> {
    let mut steps = Vec::with_capacity(wants.len());
    // Where each source directory lands, and what this plan will make.
    let mut dest: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut made: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut occupied: BTreeSet<Vec<u8>> = BTreeSet::new();
    // Derived directories, and those of them the live checkout lacks (so
    // what they hold is absent there too, with no look).
    let mut derived: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut absent: BTreeSet<Vec<u8>> = BTreeSet::new();
    for (n, want) in wants.iter().enumerate() {
        if !plain(&want.path) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let (parent, leaf) = want.path.iter().rposition(|byte| *byte == b'/').map_or(
            (&[][..], want.path.as_slice()),
            |slash| {
                (
                    want.path.get(..slash).unwrap_or_default(),
                    want.path.get(slash + 1..).unwrap_or_default(),
                )
            },
        );
        // A derived entry (its subtree with it) is rebuildable: never
        // written, only looked for at its own live path.
        let under_derived =
            derived.contains(parent) || ancestors(parent).any(|above| derived.contains(above));
        if under_derived || class_in(&want.path, &[tracked, sourced]) == Class::Derived {
            let present = if absent.contains(parent) {
                false
            } else {
                matches!(at(live, &want.path, &BTreeSet::new())?, At::Dir | At::Other)
            };
            if want.side.kind == Kind::Directory {
                derived.insert(want.path.clone());
                if !present {
                    absent.insert(want.path.clone());
                }
            }
            steps.push(Step {
                want: n,
                act: Act::Rebuildable,
                at: want.path.clone(),
                present,
            });
            continue;
        }
        // An entry under a source directory whose beside path is occupied
        // is occupied with it, nothing written.
        if occupied.contains(parent) || ancestors(parent).any(|above| occupied.contains(above)) {
            if want.side.kind == Kind::Directory {
                occupied.insert(want.path.clone());
            }
            steps.push(Step {
                want: n,
                act: Act::Occupied,
                at: join(dest.get(parent).map_or(parent, Vec::as_slice), leaf),
                present: false,
            });
            continue;
        }
        let base = dest.get(parent).cloned().unwrap_or_else(|| parent.to_vec());
        let here = join(&base, leaf);
        let directory = want.side.kind == Kind::Directory;
        let found = match at(live, &here, &made)? {
            // A path the live index tracks, deleted in its worktree.
            At::Absent if tracked.contains(&here) => At::Other,
            found => found,
        };
        let equal = matches!(found, At::Other)
            && !directory
            && holds_want(
                objects.as_deref_mut(),
                &live.join(OsStr::from_bytes(&here)),
                want,
            )?;
        let (act, there) = match found {
            At::Absent if directory => (Act::DirPlace, here),
            At::Dir if directory => (Act::DirAdopt, here),
            At::Absent => (Act::Place, here),
            At::Other if equal => (Act::Adopt, here),
            // A differing entry, or one below a path that is no directory:
            // beside, at `<path>SUFFIX` or under `<blocking path>SUFFIX/`.
            found => {
                let beside = match found {
                    At::Blocked(above) => {
                        let rest = here.get(above.len()..).unwrap_or_default();
                        let mut beside = suffixed(&above, suffix);
                        beside.extend_from_slice(rest);
                        beside
                    }
                    _ => suffixed(&here, suffix),
                };
                let held = at(live, &beside, &made)?;
                let equal = matches!(held, At::Other)
                    && !directory
                    && holds_want(
                        objects.as_deref_mut(),
                        &live.join(OsStr::from_bytes(&beside)),
                        want,
                    )?;
                let act = match held {
                    At::Absent if directory => Act::DirBeside,
                    At::Dir if directory => Act::DirBesideAdopt,
                    At::Absent => Act::Beside,
                    At::Other if equal => Act::BesidePresent,
                    _ => Act::Occupied,
                };
                (act, beside)
            }
        };
        match act {
            Act::DirPlace | Act::DirBeside => {
                made.insert(there.clone());
                dest.insert(want.path.clone(), there.clone());
            }
            Act::DirAdopt | Act::DirBesideAdopt => {
                dest.insert(want.path.clone(), there.clone());
            }
            Act::Occupied if directory => {
                occupied.insert(want.path.clone());
            }
            _ => {}
        }
        steps.push(Step {
            want: n,
            act,
            at: there,
            present: false,
        });
    }
    Ok(Planned { steps })
}

/// The private temporary of a placement at `at` (relative to `live`), in
/// its directory; `salt` keeps two reconciles' names apart.
pub(in crate::git_carry) fn temp_of(live: &Path, at: &[u8], salt: &[u8]) -> PathBuf {
    let parent = at.iter().rposition(|byte| *byte == b'/').map_or_else(
        || live.to_path_buf(),
        |slash| live.join(OsStr::from_bytes(at.get(..slash).unwrap_or_default())),
    );
    parent.join(OsStr::from_bytes(&temp_name(at, salt)))
}

// The temporary's file name for a placement at `at`.
fn temp_name(at: &[u8], salt: &[u8]) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-place-v1\0");
    hasher.update(&(salt.len() as u64).to_le_bytes());
    hasher.update(salt);
    hasher.update(at);
    let tag = hasher.finalize().to_hex();
    let mut name = TEMP_PREFIX.as_bytes().to_vec();
    name.extend_from_slice(tag.get(..24).unwrap_or_default().as_bytes());
    name
}

/// Every temporary `planned` may make (write-ahead).
pub(in crate::git_carry) fn temps(live: &Path, planned: &Planned, salt: &[u8]) -> Vec<Vec<u8>> {
    planned
        .steps
        .iter()
        .filter(|step| matches!(step.act, Act::Place | Act::Beside))
        .map(|step| {
            temp_of(live, &step.at, salt)
                .as_os_str()
                .as_bytes()
                .to_vec()
        })
        .collect()
}

fn cname(name: &[u8]) -> Result<CString> {
    CString::new(name).map_err(|_| BulkloadRefusal::PathNotPortable)
}

// `(parent, leaf)` of a relative path.
fn split(rel: &[u8]) -> (&[u8], &[u8]) {
    rel.iter()
        .rposition(|byte| *byte == b'/')
        .map_or((&[][..], rel), |slash| {
            (
                rel.get(..slash).unwrap_or_default(),
                rel.get(slash + 1..).unwrap_or_default(),
            )
        })
}

// An IO failure where the live tree moved under the pass (a link or a file
// where a directory was): a destination change, typed.
fn moved(error: &std::io::Error) -> BulkloadRefusal {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => BulkloadRefusal::GitDestinationModified,
        _ => crate::refuse::io(error, SITE),
    }
}

/// The live tree, written through directory descriptors opened beneath its
/// root one component at a time, never following a link.
struct Tree<'a> {
    live: &'a Path,
    root: OwnedFd,
}

impl<'a> Tree<'a> {
    fn open(live: &'a Path) -> Result<Self> {
        Ok(Self {
            live,
            root: sys::open_root(live).refuse_at(SITE)?,
        })
    }

    // The directory `rel` beneath the root (the root for an empty `rel`);
    // with a journal, each absent component is made (`0755`) and recorded.
    fn dir(&self, rel: &[u8], mut journal: Option<&mut Journal<'_>>) -> Result<OwnedFd> {
        let mut held = self.root.try_clone().refuse_at(SITE)?;
        if rel.is_empty() {
            return Ok(held);
        }
        let mut walked: Vec<u8> = Vec::new();
        for part in rel.split(|byte| *byte == b'/') {
            if !walked.is_empty() {
                walked.push(b'/');
            }
            walked.extend_from_slice(part);
            let name = cname(part)?;
            let next = match sys::open_dir_at(held.as_fd(), &name) {
                Ok(next) => next,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let Some(journal) = journal.as_deref_mut() else {
                        return Err(crate::refuse::io(&error, SITE));
                    };
                    self.make(held.as_fd(), (&name, &walked), 0o755, journal)?;
                    sys::open_dir_at(held.as_fd(), &name).map_err(|error| moved(&error))?
                }
                Err(error) => return Err(moved(&error)),
            };
            held = next;
        }
        Ok(held)
    }

    // Make the directory `name` in `parent` (its path `rel`) at `mode`,
    // recorded by identity; `false` when it is there already.
    fn make(
        &self,
        parent: std::os::fd::BorrowedFd<'_>,
        (name, rel): (&CStr, &[u8]),
        mode: u32,
        journal: &mut Journal<'_>,
    ) -> Result<bool> {
        match sys::mkdirat(parent, name, mode) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                let found = sys::fstatat_nofollow(parent, name).map_err(|error| moved(&error))?;
                if !found.is_dir() {
                    return Err(BulkloadRefusal::GitDestinationModified);
                }
                return Ok(false);
            }
            Err(error) => return Err(moved(&error)),
        }
        let identity = sys::fstatat_nofollow(parent, name).refuse_at(SITE)?;
        journal.made(Made {
            path: self.live.join(path_of(rel)).as_os_str().as_bytes().to_vec(),
            dev: identity.node.dev,
            ino: identity.node.ino,
            mode,
        })?;
        Ok(true)
    }
}

// Write `want`'s bytes (or link) at `temp` in `dir`, fsynced at its mode;
// the bytes written.
fn write_temp(
    dir: &OwnedFd,
    temp: &CStr,
    want: &Want,
    objects: Option<&mut BatchObjects>,
) -> Result<u64> {
    match want.side.kind {
        Kind::Symlink => {
            let link = want
                .side
                .link
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            sys::symlinkat(&cname(link)?, dir.as_fd(), temp).refuse_at(SITE)?;
            Ok(0)
        }
        Kind::Regular => {
            let mut file =
                fs::File::from(sys::create_excl_at(dir.as_fd(), temp, 0o600).refuse_at(SITE)?);
            let written = match (&want.disk, objects) {
                (Some(disk), _) => copy_bytes(disk, &mut file)?,
                (None, Some(objects)) => objects.copy_into(
                    want.side
                        .blob
                        .as_deref()
                        .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                    &mut file,
                    None,
                )?,
                (None, None) => return Err(BulkloadRefusal::GitInventoryMalformed),
            };
            file.flush().refuse_at(SITE)?;
            sys::fchmod(&file, want.side.mode & 0o7777).refuse_at(SITE)?;
            crate::counters::CountedSync::sync_file_counted(&file).refuse_at(SITE)?;
            Ok(written)
        }
        Kind::Directory => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

// Copy the bytes of the regular file `from` into `to` through a buffer
// (never `copy_file_range`, which clones on a reflink file system: S2 P84);
// the bytes copied, counted as destination reads.
fn copy_bytes(from: &Path, to: &mut fs::File) -> Result<u64> {
    use std::io::Read as _;
    let mut from = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(from)
        .refuse_at(SITE)?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut total = 0_u64;
    loop {
        let read = from.read(&mut buffer).refuse_at(SITE)?;
        if read == 0 {
            return Ok(total);
        }
        to.write_all(buffer.get(..read).unwrap_or_default())
            .refuse_at(SITE)?;
        crate::counters::add_len(crate::counters::Counter::DestVerifyRead, read);
        total += u64::try_from(read).unwrap_or(u64::MAX);
    }
}

/// The rebuildable ledger of `planned`'s derived entries (operator ruling
/// 2026-10-20, "Ledger as rebuildable"): each one's path, kind, mode, size
/// and BLAKE3 (a link's target), read from the tree on disk without
/// following a link, or from the capture's objects; whether the live
/// checkout holds something at its path. Nothing is written.
///
/// # Errors
/// `GIT_DESTINATION_MODIFIED` when an entry on disk is no longer the kind
/// it was planned as; whatever reading one refuses.
pub(in crate::git_carry) fn ledger(
    planned: &Planned,
    wants: &[Want],
    mut objects: Option<&mut BatchObjects>,
) -> Result<Vec<Rebuildable>> {
    let mut ledger = Vec::new();
    for step in planned
        .steps
        .iter()
        .filter(|step| step.act == Act::Rebuildable)
    {
        let want = wants
            .get(step.want)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let (size, blake3, link) = match want.side.kind {
            Kind::Directory => (0, None, None),
            Kind::Symlink => {
                let link = match &want.disk {
                    Some(disk) => {
                        if !lstat(disk)?.is_some_and(|meta| meta.file_type().is_symlink()) {
                            return Err(BulkloadRefusal::GitDestinationModified);
                        }
                        fs::read_link(disk)
                            .refuse_at(SITE)?
                            .as_os_str()
                            .as_bytes()
                            .to_vec()
                    }
                    None => want
                        .side
                        .link
                        .clone()
                        .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                };
                (
                    u64::try_from(link.len()).unwrap_or(u64::MAX),
                    None,
                    Some(link),
                )
            }
            Kind::Regular => {
                let mut hasher = blake3::Hasher::new();
                let size = match (&want.disk, objects.as_deref_mut()) {
                    (Some(disk), _) => hash_file(disk, &mut hasher)?,
                    (None, Some(objects)) => objects.copy_into(
                        want.side
                            .blob
                            .as_deref()
                            .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                        &mut hasher,
                        None,
                    )?,
                    (None, None) => return Err(BulkloadRefusal::GitInventoryMalformed),
                };
                (size, Some(*hasher.finalize().as_bytes()), None)
            }
        };
        ledger.push(Rebuildable {
            path: want.path.clone(),
            kind: want.side.kind,
            mode: want.side.mode,
            size,
            blake3,
            link,
            live_present: step.present,
        });
    }
    Ok(ledger)
}

// Hash the regular file at `path` (never following a link) into `hasher`;
// its length.
fn hash_file(path: &Path, hasher: &mut blake3::Hasher) -> Result<u64> {
    use std::io::Read as _;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| BulkloadRefusal::GitDestinationModified)?;
    if !file.metadata().refuse_at(SITE)?.is_file() {
        return Err(BulkloadRefusal::GitDestinationModified);
    }
    let mut buffer = vec![0u8; 1 << 16];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).refuse_at(SITE)?;
        if read == 0 {
            return Ok(total);
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
        crate::counters::add_len(crate::counters::Counter::DestVerifyRead, read);
        total += u64::try_from(read).unwrap_or(u64::MAX);
    }
}

/// Run `planned` (from [`plan`], under the caller's lock, recorded in
/// `journal`): directories made, files placed and landed beside,
/// create-only, through descriptors opened beneath the live root (never
/// following a link). An entry that appeared since the plan is adopted
/// when it is equal; anything else that moved since refuses
/// `GIT_DESTINATION_MODIFIED`, the rest left for the next pass. Every
/// entry is counted once; a count that does not add up refuses.
///
/// # Errors
/// `GIT_DESTINATION_MODIFIED`; whatever a write refuses.
#[allow(clippy::too_many_lines)]
pub(in crate::git_carry) fn execute(
    live: &Path,
    planned: &Planned,
    wants: &[Want],
    (salt, journal): (&[u8], &mut Journal<'_>),
    mut objects: Option<&mut BatchObjects>,
) -> Result<Tally> {
    let tree = Tree::open(live)?;
    let mut tally = Tally::default();
    let mut synced: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut modes: Vec<(Vec<u8>, u32)> = Vec::new();
    for step in &planned.steps {
        let want = wants
            .get(step.want)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if step.act.writes() {
            tally.written_source += 1;
        }
        let (parent, leaf) = split(&step.at);
        let path = live.join(path_of(&step.at));
        match step.act {
            Act::Adopt => tally.adopted += 1,
            Act::BesidePresent => tally.beside_present += 1,
            Act::DirAdopt => tally.dirs_adopted += 1,
            Act::DirBesideAdopt => tally.dirs_beside += 1,
            Act::Occupied => tally.occupied += 1,
            Act::Rebuildable => {
                tally.rebuildable += 1;
                tally.rebuildable_bytes += want.size;
            }
            Act::DirPlace | Act::DirBeside => {
                let dir = tree.dir(parent, Some(journal))?;
                let made = tree.make(dir.as_fd(), (&cname(leaf)?, &step.at), 0o700, journal)?;
                synced.insert(parent.to_vec());
                if made {
                    modes.push((step.at.clone(), want.side.mode));
                    // The record names the mode the directory ends at.
                    if let Some(last) = journal.record.made.last_mut() {
                        last.mode = want.side.mode;
                    }
                }
                match (step.act, made) {
                    (Act::DirPlace, true) => tally.dirs_placed += 1,
                    (Act::DirPlace, false) => tally.dirs_adopted += 1,
                    _ => tally.dirs_beside += 1,
                }
            }
            Act::Place | Act::Beside => {
                let dir = tree.dir(parent, Some(journal))?;
                let temp = cname(&temp_name(&step.at, salt))?;
                match sys::fstatat_nofollow(dir.as_fd(), &temp) {
                    Ok(found) => {
                        // A directory there is no temporary of ours.
                        if found.is_dir() {
                            return Err(BulkloadRefusal::GitDestinationModified);
                        }
                        sys::unlinkat(dir.as_fd(), &temp, false).refuse_at(SITE)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(moved(&error)),
                }
                let written = write_temp(&dir, &temp, want, objects.as_deref_mut())?;
                let leaf = cname(leaf)?;
                match sys::rename_exclusive_at(dir.as_fd(), &temp, dir.as_fd(), &leaf) {
                    Ok(()) => {}
                    Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                        sys::unlinkat(dir.as_fd(), &temp, false).refuse_at(SITE)?;
                        // Taken since the plan: equal is adopted, else the
                        // next pass decides it again.
                        if !holds_want(objects.as_deref_mut(), &path, want)? {
                            return Err(BulkloadRefusal::GitDestinationModified);
                        }
                        if step.act == Act::Place {
                            tally.adopted += 1;
                        } else {
                            tally.beside_present += 1;
                        }
                        continue;
                    }
                    Err(error) => return Err(moved(&error)),
                }
                let placed = sys::fstatat_nofollow(dir.as_fd(), &leaf).refuse_at(SITE)?;
                journal.placed(Placed {
                    path: path.as_os_str().as_bytes().to_vec(),
                    dev: placed.node.dev,
                    ino: placed.node.ino,
                    size: placed.size,
                    mtime_ns: placed.mtime_ns,
                    beside: step.act == Act::Beside,
                })?;
                synced.insert(parent.to_vec());
                tally.bytes += written;
                if step.act == Act::Place {
                    tally.placed += 1;
                } else {
                    tally.beside += 1;
                }
            }
        }
    }
    // The directories this pass made take their modes last, deepest first,
    // so a read-only one still received what it holds.
    for (rel, mode) in modes.iter().rev() {
        let dir = tree.dir(rel, None)?;
        sys::fchmod(&dir, mode & 0o7777).refuse_at(SITE)?;
        synced.insert(split(rel).0.to_vec());
    }
    for rel in &synced {
        let dir = fs::File::from(tree.dir(rel, None)?);
        crate::counters::CountedSync::sync_dir_counted(&dir).refuse_at(SITE)?;
    }
    if tally.entries() != u64::try_from(planned.steps.len()).unwrap_or(u64::MAX) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(tally)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use std::os::unix::fs::DirBuilderExt as _;

    fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (source, live) = (root.join("source"), root.join("live"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&live).unwrap();
        (dir, source, live)
    }

    fn wants(source: &Path, paths: &[&str]) -> Vec<Want> {
        let mut wants: Vec<Want> = paths
            .iter()
            .filter_map(|path| Want::on_disk(source, path.as_bytes().to_vec()).unwrap())
            .collect();
        wants.sort_by(|a, b| a.path.cmp(&b.path));
        wants
    }

    // Execute `planned` with a journal beside the live tree.
    fn run(live: &Path, planned: &Planned, wants: &[Want]) -> Result<Tally> {
        let record = live.parent().unwrap().join("run.files");
        let mut journal = Journal::begin(&record, FilesRecord::default())?;
        let tally = execute(live, planned, wants, (b"t", &mut journal), None)?;
        journal.finish(tally)?;
        Ok(tally)
    }

    fn acts(planned: &Planned, wants: &[Want]) -> Vec<(String, Act, String)> {
        planned
            .steps
            .iter()
            .map(|step| {
                (
                    String::from_utf8_lossy(&wants[step.want].path).into_owned(),
                    step.act,
                    String::from_utf8_lossy(&step.at).into_owned(),
                )
            })
            .collect()
    }

    /// Absent placed, equal adopted, different beside, a beside already
    /// equal present, an occupied beside path refused; nothing live is
    /// overwritten, and a second pass adopts everything the first placed.
    #[test]
    fn each_entry_ends_in_exactly_one_class_and_nothing_is_overwritten() {
        let (_dir, source, live) = tree();
        fs::create_dir_all(source.join("notes/deep")).unwrap();
        fs::write(source.join("notes/todo.md"), b"neo todo\n").unwrap();
        fs::write(source.join("notes/deep/x"), b"x\n").unwrap();
        fs::write(source.join("same.log"), b"same\n").unwrap();
        fs::write(source.join("differs.log"), b"neo\n").unwrap();
        fs::write(source.join("again.log"), b"neo again\n").unwrap();
        fs::write(source.join("taken.log"), b"neo taken\n").unwrap();
        std::os::unix::fs::symlink("same.log", source.join("link")).unwrap();
        fs::write(live.join("same.log"), b"same\n").unwrap();
        fs::write(live.join("differs.log"), b"sting\n").unwrap();
        fs::write(live.join("again.log"), b"sting again\n").unwrap();
        fs::write(live.join("again.log.neo-20261016"), b"neo again\n").unwrap();
        fs::write(live.join("taken.log"), b"sting taken\n").unwrap();
        fs::write(live.join("taken.log.neo-20261016"), b"someone else\n").unwrap();
        let wants = wants(
            &source,
            &[
                "notes",
                "notes/deep",
                "notes/deep/x",
                "notes/todo.md",
                "same.log",
                "differs.log",
                "again.log",
                "taken.log",
                "link",
            ],
        );
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &BTreeSet::new()),
            None,
        )
        .unwrap();
        let got = acts(&planned, &wants);
        let find = |path: &str| got.iter().find(|row| row.0 == path).unwrap().clone();
        assert_eq!(find("notes").1, Act::DirPlace);
        assert_eq!(find("notes/deep/x").1, Act::Place);
        assert_eq!(find("same.log").1, Act::Adopt);
        assert_eq!(
            find("differs.log"),
            (
                "differs.log".into(),
                Act::Beside,
                "differs.log.neo-20261016".into()
            )
        );
        assert_eq!(find("again.log").1, Act::BesidePresent);
        assert_eq!(find("taken.log").1, Act::Occupied);
        assert_eq!(find("link").1, Act::Place);
        let tally = run(&live, &planned, &wants).unwrap();
        assert_eq!(tally, planned.tally(&wants).with_bytes(tally.bytes));
        assert_eq!(tally.entries(), 9);
        assert_eq!(fs::read(live.join("differs.log")).unwrap(), b"sting\n");
        assert_eq!(
            fs::read(live.join("differs.log.neo-20261016")).unwrap(),
            b"neo\n"
        );
        assert_eq!(
            fs::read(live.join("taken.log.neo-20261016")).unwrap(),
            b"someone else\n"
        );
        assert_eq!(fs::read(live.join("notes/deep/x")).unwrap(), b"x\n");
        assert_eq!(
            fs::read_link(live.join("link")).unwrap(),
            Path::new("same.log")
        );
        // The second pass: everything placed is adopted, nothing written.
        let again = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &BTreeSet::new()),
            None,
        )
        .unwrap();
        let tally = run(&live, &again, &wants).unwrap();
        assert_eq!(
            (tally.placed, tally.beside, tally.dirs_placed, tally.bytes),
            (0, 0, 0, 0)
        );
        assert_eq!(tally.occupied, 1);
        assert!(!fs::read_dir(&live).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .as_bytes()
            .starts_with(TEMP_PREFIX.as_bytes())));
    }

    /// A source directory whose live path is a file lands its subtree under
    /// `<dir>SUFFIX/`; an entry below a live symlink is never written
    /// through it.
    #[test]
    fn a_file_or_a_link_in_the_way_moves_the_subtree_beside() {
        let (_dir, source, live) = tree();
        fs::create_dir_all(source.join("cache/sub")).unwrap();
        fs::write(source.join("cache/sub/a"), b"a\n").unwrap();
        fs::create_dir_all(source.join("via")).unwrap();
        fs::write(source.join("via/b"), b"b\n").unwrap();
        fs::write(live.join("cache"), b"a live file\n").unwrap();
        let outside = live.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, live.join("via")).unwrap();
        let wants = wants(
            &source,
            &["cache", "cache/sub", "cache/sub/a", "via", "via/b"],
        );
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &BTreeSet::new()),
            None,
        )
        .unwrap();
        run(&live, &planned, &wants).unwrap();
        assert_eq!(fs::read(live.join("cache")).unwrap(), b"a live file\n");
        assert_eq!(
            fs::read(live.join("cache.neo-20261016/sub/a")).unwrap(),
            b"a\n"
        );
        assert_eq!(fs::read(live.join("via.neo-20261016/b")).unwrap(), b"b\n");
        assert!(!outside.join("b").exists(), "written through a link");
    }

    /// An unfinished pass's temporaries are removed by the next one, which
    /// then places as if nothing had run.
    #[test]
    fn settle_removes_only_recorded_private_temporaries() {
        let (_dir, source, live) = tree();
        fs::write(source.join("a"), b"a\n").unwrap();
        let wants = wants(&source, &["a"]);
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &BTreeSet::new()),
            None,
        )
        .unwrap();
        let temp = temp_of(&live, b"a", b"t");
        fs::write(&temp, b"half").unwrap();
        fs::write(live.join("operator"), b"mine\n").unwrap();
        let record = live.parent().unwrap().join("item.files");
        landed::write(
            &record,
            &FilesRecord {
                temps: temps(&live, &planned, b"t"),
                ..FilesRecord::default()
            },
        )
        .unwrap();
        settle(&record).unwrap();
        assert!(!temp.exists());
        assert!(live.join("operator").exists());
        let tally = run(&live, &planned, &wants).unwrap();
        assert_eq!(tally.placed, 1);
        assert_eq!(fs::read(live.join("a")).unwrap(), b"a\n");
    }

    /// A path the live index tracks and its worktree deleted is the live
    /// checkout's own change: the source's entry there lands beside it,
    /// never undoing the deletion.
    #[test]
    fn a_live_deletion_of_a_tracked_path_is_never_undone() {
        let (_dir, source, live) = tree();
        fs::write(source.join("gone.txt"), b"neo\n").unwrap();
        let wants = wants(&source, &["gone.txt"]);
        let tracked = BTreeSet::from([b"gone.txt".to_vec()]);
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &tracked, &BTreeSet::new()),
            None,
        )
        .unwrap();
        run(&live, &planned, &wants).unwrap();
        assert!(!live.join("gone.txt").exists());
        assert_eq!(
            fs::read(live.join("gone.txt.neo-20261016")).unwrap(),
            b"neo\n"
        );
    }

    /// The directories an unfinished pass made are settled by identity: an
    /// empty one removed, one still at `0700` given its mode; a directory
    /// at a recorded path that is not the one made (another inode), or one
    /// no pass made, is never touched (review U1).
    #[test]
    fn settle_touches_only_the_directories_the_pass_made() {
        let (_dir, _source, live) = tree();
        let (empty, full, other) = (live.join("empty"), live.join("full"), live.join("other"));
        for dir in [&empty, &full, &other] {
            fs::DirBuilder::new().mode(0o700).create(dir).unwrap();
        }
        fs::write(full.join("a"), b"a\n").unwrap();
        let made = |path: &Path, mode: u32| {
            let meta = fs::metadata(path).unwrap();
            Made {
                path: path.as_os_str().as_bytes().to_vec(),
                dev: meta.dev(),
                ino: meta.ino(),
                mode,
            }
        };
        let mut other_made = made(&other, 0o755);
        other_made.ino += 1;
        let record = live.parent().unwrap().join("dirs.files");
        landed::write(
            &record,
            &FilesRecord {
                made: vec![made(&empty, 0o755), made(&full, 0o750), other_made],
                ..FilesRecord::default()
            },
        )
        .unwrap();
        settle(&record).unwrap();
        assert!(!empty.exists());
        assert_eq!(fs::metadata(&full).unwrap().mode() & 0o7777, 0o750);
        assert!(
            other.exists(),
            "a directory the pass did not make was removed"
        );
    }

    /// A derived entry (build output, a `mktemp` directory, `.serena`) is
    /// rebuildable, its subtree with it: never compared or written, counted,
    /// and ledgered with its kind, mode, size and BLAKE3 (a link's target);
    /// a source-like one is placed. A `build/` directory that holds tracked
    /// files is a source directory (operator ruling 2026-10-20).
    #[test]
    fn derived_output_is_ledgered_rebuildable_never_written() {
        let (_dir, source, live) = tree();
        fs::create_dir_all(source.join("build/out")).unwrap();
        fs::write(source.join("build/out/a.o"), b"object\n").unwrap();
        fs::create_dir_all(source.join(".native-osstore-vendor.Ab3xZ9")).unwrap();
        std::os::unix::fs::symlink("/nowhere/bazel-out", source.join("bazel-out")).unwrap();
        fs::write(source.join("notes.md"), b"n\n").unwrap();
        fs::create_dir_all(source.join("tools/build")).unwrap();
        fs::write(source.join("tools/build/new.sh"), b"#!/bin/sh\n").unwrap();
        // The live checkout holds its own `build/out/a.o`: never compared.
        fs::create_dir_all(live.join("build/out")).unwrap();
        fs::write(live.join("build/out/a.o"), b"sting's own\n").unwrap();
        fs::create_dir_all(live.join("tools/build")).unwrap();
        let wants = wants(
            &source,
            &[
                "build",
                "build/out",
                "build/out/a.o",
                ".native-osstore-vendor.Ab3xZ9",
                "bazel-out",
                "notes.md",
                "tools",
                "tools/build",
                "tools/build/new.sh",
            ],
        );
        assert_eq!(class_of(b"build/out/a.o"), Class::Derived);
        assert_eq!(class_of(b".native-osstore-vendor.Ab3xZ9"), Class::Derived);
        assert_eq!(class_of(b"bazel-out"), Class::Derived);
        assert_eq!(class_of(b"docs/notes.md"), Class::Source);
        assert_eq!(class_of(b"docs.backup"), Class::Source);
        // `tools/build/` holds a tracked file: a source directory.
        let sourced: BTreeSet<Vec<u8>> = [b"tools/build/make.sh".to_vec()].into();
        assert_eq!(class_in(b"tools/build/new.sh", &[&sourced]), Class::Source);
        assert_eq!(class_in(b"build/out/a.o", &[&sourced]), Class::Derived);
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &sourced),
            None,
        )
        .unwrap();
        let ledger = ledger(&planned, &wants, None).unwrap();
        let tally = run(&live, &planned, &wants).unwrap();
        assert_eq!(tally, planned.tally(&wants).with_bytes(tally.bytes));
        assert_eq!(
            (
                tally.rebuildable,
                tally.placed,
                tally.adopted,
                tally.dirs_adopted
            ),
            (5, 2, 0, 2)
        );
        assert_eq!(tally.rebuildable_bytes, 7 + 18);
        assert_eq!(tally.entries(), 9);
        assert_eq!(
            fs::read(live.join("build/out/a.o")).unwrap(),
            b"sting's own\n"
        );
        assert!(!live.join("bazel-out").exists());
        assert!(!live.join(".native-osstore-vendor.Ab3xZ9").exists());
        assert!(live.join("notes.md").exists());
        assert!(live.join("tools/build/new.sh").exists());
        let find = |path: &str| {
            ledger
                .iter()
                .find(|entry| entry.path == path.as_bytes())
                .unwrap()
        };
        let object = find("build/out/a.o");
        assert_eq!(object.kind_word(), "file");
        assert_eq!(object.size, 7);
        assert_eq!(object.blake3, Some(*blake3::hash(b"object\n").as_bytes()));
        assert!(object.live_present);
        let link = find("bazel-out");
        assert_eq!(link.kind_word(), "link");
        assert_eq!(link.link.as_deref(), Some(b"/nowhere/bazel-out".as_slice()));
        assert!(!link.live_present);
        assert_eq!(find("build").kind_word(), "dir");
        assert_eq!(ledger.len(), 5);
    }

    /// Every placement is recorded by identity and can be taken back: what
    /// is still as placed is removed with the directories the pass made;
    /// what changed since is kept.
    #[test]
    fn undo_takes_back_only_what_is_still_as_placed() {
        let (_dir, source, live) = tree();
        fs::create_dir_all(source.join("d")).unwrap();
        fs::write(source.join("d/a"), b"a\n").unwrap();
        fs::write(source.join("b"), b"b\n").unwrap();
        let wants = wants(&source, &["b", "d", "d/a"]);
        let planned = plan(
            &live,
            ".neo-20261016",
            (&wants, &BTreeSet::new(), &BTreeSet::new()),
            None,
        )
        .unwrap();
        run(&live, &planned, &wants).unwrap();
        let record = live.parent().unwrap().join("run.files");
        assert_eq!(read_record(&record).unwrap().unwrap().placed.len(), 2);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(live.join("b"), b"edited since\n").unwrap();
        let undone = undo(&record).unwrap();
        assert_eq!((undone.removed, undone.kept, undone.dirs), (1, 1, 1));
        assert!(!live.join("d").exists());
        assert_eq!(fs::read(live.join("b")).unwrap(), b"edited since\n");
        assert!(read_record(&record).unwrap().unwrap().undone);
    }

    impl Tally {
        fn with_bytes(mut self, bytes: u64) -> Self {
            self.bytes = bytes;
            self
        }
    }
}
