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
//!
//! Nothing is excluded and nothing is dropped: every entry ends counted in
//! exactly one of those classes. The temporaries a pass is about to make
//! are recorded write-ahead ([`FilesRecord`]); a pass a crash cut short
//! leaves only those, which the next pass removes ([`settle`]) before it
//! plans again. A rerun finds what the last one placed equal, and adopts
//! it.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::batch_objects::BatchObjects;
use super::super::landed::{self, Kind};
use super::super::{git, output};
use super::{holds, lstat, Side};
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
}

impl Want {
    /// A capture's seat.
    pub(in crate::git_carry) const fn seat(path: Vec<u8>, side: Side) -> Self {
        Self {
            path,
            side,
            disk: None,
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
        }))
    }
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
    /// Bytes written by placements and beside landings.
    pub bytes: u64,
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
    }

    /// The receipt words.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "files-placed={} files-adopted={} files-beside={} files-beside-present={} dirs-placed={} dirs-adopted={} dirs-beside={} files-occupied={} files-bytes-placed={}",
            self.placed,
            self.adopted,
            self.beside,
            self.beside_present,
            self.dirs_placed,
            self.dirs_adopted,
            self.dirs_beside,
            self.occupied,
            self.bytes,
        )
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
    /// The directories this pass may make, shallowest first, with the mode
    /// each ends at (write-ahead).
    pub dirs: Vec<(Vec<u8>, u32)>,
    /// The pass ended: `tally` is its count.
    pub done: bool,
    pub tally: Tally,
    /// `(source path, where it landed)` of each entry landed beside.
    pub besides: Vec<(Vec<u8>, Vec<u8>)>,
    /// The beside paths that hold something else.
    pub occupied: Vec<Vec<u8>>,
}

impl landed::Record for FilesRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-files01";
}

/// The record at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<FilesRecord>> {
    landed::read::<FilesRecord>(path)
}

/// Settle what an unfinished pass recorded at `path` left: its
/// temporaries removed (a regular file or a link at a recorded private
/// name), and each directory it may have made, deepest first, removed when
/// empty or given its mode when still the `0700` it was made at (nothing
/// else is touched). The next pass plans again. A finished record is left
/// as it is.
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
        let temp = Path::new(OsStr::from_bytes(temp));
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
    for (dir, mode) in record.dirs.iter().rev() {
        let dir = Path::new(OsStr::from_bytes(dir));
        let Some(meta) = lstat(dir)? else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        if fs::read_dir(dir).refuse_at(SITE)?.next().is_none() {
            fs::remove_dir(dir).refuse_at(SITE)?;
        } else if meta.mode() & 0o7777 == 0o700 && mode & 0o7777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode & 0o7777)).refuse_at(SITE)?;
        }
    }
    Ok(())
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
}

/// One entry's decision: what it does and where (relative to the live
/// root).
#[derive(Debug, Clone)]
pub(in crate::git_carry) struct Step {
    pub want: usize,
    pub act: Act,
    pub at: Vec<u8>,
}

/// A planned reconcile.
#[derive(Debug, Clone, Default)]
pub(in crate::git_carry) struct Planned {
    pub steps: Vec<Step>,
}

impl Planned {
    /// What the plan would count, written or not.
    pub(in crate::git_carry) fn tally(&self) -> Tally {
        let mut tally = Tally::default();
        for step in &self.steps {
            match step.act {
                Act::Place => tally.placed += 1,
                Act::Adopt => tally.adopted += 1,
                Act::Beside => tally.beside += 1,
                Act::BesidePresent => tally.beside_present += 1,
                Act::DirPlace => tally.dirs_placed += 1,
                Act::DirAdopt => tally.dirs_adopted += 1,
                Act::DirBeside | Act::DirBesideAdopt => tally.dirs_beside += 1,
                Act::Occupied => tally.occupied += 1,
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
/// lands beside it.
///
/// # Errors
/// Whatever reading the live tree or the source refuses.
pub(in crate::git_carry) fn plan(
    live: &Path,
    suffix: &str,
    (wants, tracked): (&[Want], &BTreeSet<Vec<u8>>),
    mut objects: Option<&mut BatchObjects>,
) -> Result<Planned> {
    let mut steps = Vec::with_capacity(wants.len());
    // Where each source directory lands, and what this plan will make.
    let mut dest: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut made: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut occupied: BTreeSet<Vec<u8>> = BTreeSet::new();
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
        });
    }
    Ok(Planned { steps })
}

/// The private temporary of a placement at `at` (relative to `live`), in
/// its directory; `salt` keeps two reconciles' names apart.
pub(in crate::git_carry) fn temp_of(live: &Path, at: &[u8], salt: &[u8]) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-place-v1\0");
    hasher.update(&(salt.len() as u64).to_le_bytes());
    hasher.update(salt);
    hasher.update(at);
    let tag = hasher.finalize().to_hex();
    let parent = at.iter().rposition(|byte| *byte == b'/').map_or_else(
        || live.to_path_buf(),
        |slash| live.join(OsStr::from_bytes(at.get(..slash).unwrap_or_default())),
    );
    parent.join(format!(
        "{TEMP_PREFIX}{}",
        tag.get(..24).unwrap_or_default()
    ))
}

/// Every directory `planned` may make (write-ahead), shallowest first,
/// with the mode it ends at: its directory steps, and the absent parents
/// of every step (made `0755`).
///
/// # Errors
/// Whatever reading the live tree refuses.
pub(in crate::git_carry) fn dirs(
    live: &Path,
    planned: &Planned,
    wants: &[Want],
) -> Result<Vec<(Vec<u8>, u32)>> {
    let mut found: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    for step in &planned.steps {
        if step.act == Act::Occupied || step.act == Act::Adopt || step.act == Act::BesidePresent
        {
            continue;
        }
        for above in ancestors(&step.at) {
            if !found.contains_key(above) && lstat(&live.join(OsStr::from_bytes(above)))?.is_none()
            {
                found.insert(above.to_vec(), 0o755);
            }
        }
        if matches!(step.act, Act::DirPlace | Act::DirBeside) {
            let mode = wants.get(step.want).map_or(0o755, |want| want.side.mode);
            found.insert(step.at.clone(), mode);
        }
    }
    let mut dirs: Vec<(Vec<u8>, u32)> = found
        .into_iter()
        .map(|(at, mode)| {
            (
                live.join(OsStr::from_bytes(&at)).as_os_str().as_bytes().to_vec(),
                mode,
            )
        })
        .collect();
    dirs.sort_by_key(|(at, _)| at.iter().filter(|byte| **byte == b'/').count());
    Ok(dirs)
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

fn cname(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| BulkloadRefusal::PathNotPortable)
}

// Make every absent directory above `rel` (0755, never through a link); a
// path above it that is no directory is a destination change since the
// plan.
fn parents(live: &Path, rel: &[u8], synced: &mut BTreeSet<PathBuf>) -> Result<()> {
    for above in ancestors(rel) {
        let path = live.join(OsStr::from_bytes(above));
        match lstat(&path)? {
            Some(meta) if meta.is_dir() => {}
            Some(_) => return Err(BulkloadRefusal::GitDestinationModified),
            None => {
                fs::DirBuilder::new()
                    .mode(0o755)
                    .create(&path)
                    .or_else(|error| match error.kind() {
                        std::io::ErrorKind::AlreadyExists if path.is_dir() => Ok(()),
                        _ => Err(crate::refuse::io(&error, SITE)),
                    })?;
                if let Some(parent) = path.parent() {
                    synced.insert(parent.to_path_buf());
                }
            }
        }
    }
    Ok(())
}

// Write `want`'s bytes (or link) at `temp`, fsynced at its mode; the
// bytes written.
fn write_temp(temp: &Path, want: &Want, objects: Option<&mut BatchObjects>) -> Result<u64> {
    match want.side.kind {
        Kind::Symlink => {
            let link = want
                .side
                .link
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            std::os::unix::fs::symlink(OsStr::from_bytes(link), temp).refuse_at(SITE)?;
            Ok(0)
        }
        Kind::Regular => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(temp)
                .refuse_at(SITE)?;
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
            file.set_permissions(fs::Permissions::from_mode(want.side.mode & 0o7777))
                .refuse_at(SITE)?;
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

// Rename `temp` to `to` in their one directory, never over anything:
// `false` when `to` is taken.
fn rename_in(temp: &Path, to: &Path) -> Result<bool> {
    let parent = to.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let directory = fs::File::open(parent).refuse_at(SITE)?;
    let from = cname(temp.file_name().ok_or(BulkloadRefusal::PathEscapesRoot)?)?;
    let leaf = cname(to.file_name().ok_or(BulkloadRefusal::PathEscapesRoot)?)?;
    match crate::io::sys::rename_exclusive_at(&directory, &from, &directory, &leaf) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(false),
        Err(error) => Err(crate::refuse::io(&error, SITE)),
    }
}

/// Run `planned` (from [`plan`], under the caller's lock, its temporaries
/// recorded write-ahead): directories made, files placed and landed
/// beside, create-only. An entry that appeared since the plan is adopted
/// when it is equal; anything else that moved since refuses
/// `GIT_DESTINATION_MODIFIED`, the rest left for the next pass.
///
/// # Errors
/// `GIT_DESTINATION_MODIFIED`; whatever a write refuses.
pub(in crate::git_carry) fn execute(
    live: &Path,
    planned: &Planned,
    wants: &[Want],
    salt: &[u8],
    mut objects: Option<&mut BatchObjects>,
) -> Result<Tally> {
    let mut tally = Tally::default();
    let mut synced: BTreeSet<PathBuf> = BTreeSet::new();
    let mut modes: Vec<(PathBuf, u32)> = Vec::new();
    for step in &planned.steps {
        let want = wants
            .get(step.want)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let path = live.join(OsStr::from_bytes(&step.at));
        match step.act {
            Act::Adopt => tally.adopted += 1,
            Act::BesidePresent => tally.beside_present += 1,
            Act::DirAdopt => tally.dirs_adopted += 1,
            Act::DirBesideAdopt => tally.dirs_beside += 1,
            Act::Occupied => tally.occupied += 1,
            Act::DirPlace | Act::DirBeside => {
                parents(live, &step.at, &mut synced)?;
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => modes.push((path.clone(), want.side.mode)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !lstat(&path)?.is_some_and(|meta| meta.is_dir()) {
                            return Err(BulkloadRefusal::GitDestinationModified);
                        }
                    }
                    Err(error) => return Err(crate::refuse::io(&error, SITE)),
                }
                if let Some(parent) = path.parent() {
                    synced.insert(parent.to_path_buf());
                }
                if step.act == Act::DirPlace {
                    tally.dirs_placed += 1;
                } else {
                    tally.dirs_beside += 1;
                }
            }
            Act::Place | Act::Beside => {
                parents(live, &step.at, &mut synced)?;
                let temp = temp_of(live, &step.at, salt);
                if let Some(meta) = lstat(&temp)? {
                    if !(meta.is_file() || meta.file_type().is_symlink()) {
                        return Err(BulkloadRefusal::GitDestinationModified);
                    }
                    fs::remove_file(&temp).refuse_at(SITE)?;
                }
                let written = write_temp(&temp, want, objects.as_deref_mut())?;
                if !rename_in(&temp, &path)? {
                    fs::remove_file(&temp).refuse_at(SITE)?;
                    // Taken since the plan: equal is adopted, else the next
                    // pass decides it again.
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
                if let Some(parent) = path.parent() {
                    synced.insert(parent.to_path_buf());
                }
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
    for (path, mode) in modes.iter().rev() {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777)).refuse_at(SITE)?;
        if let Some(parent) = path.parent() {
            synced.insert(parent.to_path_buf());
        }
    }
    for directory in &synced {
        landed::sync_dir(directory)?;
    }
    Ok(tally)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

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
        let planned = plan(&live, ".neo-20261016", (&wants, &BTreeSet::new()), None).unwrap();
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
        let tally = execute(&live, &planned, &wants, b"t", None).unwrap();
        assert_eq!(tally, planned.tally().with_bytes(tally.bytes));
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
        let again = plan(&live, ".neo-20261016", (&wants, &BTreeSet::new()), None).unwrap();
        let tally = execute(&live, &again, &wants, b"t", None).unwrap();
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
        let planned = plan(&live, ".neo-20261016", (&wants, &BTreeSet::new()), None).unwrap();
        execute(&live, &planned, &wants, b"t", None).unwrap();
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
        let planned = plan(&live, ".neo-20261016", (&wants, &BTreeSet::new()), None).unwrap();
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
        let tally = execute(&live, &planned, &wants, b"t", None).unwrap();
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
        let planned = plan(&live, ".neo-20261016", (&wants, &tracked), None).unwrap();
        execute(&live, &planned, &wants, b"t", None).unwrap();
        assert!(!live.join("gone.txt").exists());
        assert_eq!(
            fs::read(live.join("gone.txt.neo-20261016")).unwrap(),
            b"neo\n"
        );
    }

    /// The directories an unfinished pass may have made are settled: an
    /// empty one removed, one still at `0700` given its mode.
    #[test]
    fn settle_removes_empty_made_directories_and_seals_the_rest() {
        let (_dir, _source, live) = tree();
        let (empty, full) = (live.join("empty"), live.join("full"));
        fs::DirBuilder::new().mode(0o700).create(&empty).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&full).unwrap();
        fs::write(full.join("a"), b"a\n").unwrap();
        let record = live.parent().unwrap().join("dirs.files");
        let named = |path: &Path| path.as_os_str().as_bytes().to_vec();
        landed::write(
            &record,
            &FilesRecord {
                dirs: vec![(named(&empty), 0o755), (named(&full), 0o750)],
                ..FilesRecord::default()
            },
        )
        .unwrap();
        settle(&record).unwrap();
        assert!(!empty.exists());
        assert_eq!(fs::metadata(&full).unwrap().mode() & 0o7777, 0o750);
    }

    impl Tally {
        fn with_bytes(mut self, bytes: u64) -> Self {
            self.bytes = bytes;
            self
        }
    }
}
