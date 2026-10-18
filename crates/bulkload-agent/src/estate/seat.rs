//! A symlinked live path into a side root, seated (M2r2 item e).
//!
//! The 10-16 in-place move found live checkout paths (`~/git/<name>`) that
//! were symlinks into a side root, the tree an earlier estate landing made
//! (`estate/gftb-20261015/git/<name>`). `estate-apply --adopt` refuses a
//! checkout spelled through a symlink (`GIT_DESTINATION_OCCUPIED`,
//! `symlinked-path`), so the lane copied each target over its link by hand
//! (`cp -a`) and repaired the worktree links. This module is the bulkload
//! way: `estate-apply --adopt --side-root ROOT` seats each planned path that
//! is such a link, after the apply's preflights and before any item lands,
//! and the items then adopt (A11a) or re-key (A11b) at the live path as
//! they would at any real directory.
//!
//! Seating `LIVE -> TARGET` (TARGET a directory strictly inside ROOT),
//! holding an `flock` on both parent directories:
//!
//! 1. a record `seat-<blake3(LIVE)>.seat` in STATE (the live path, the
//!    target, the link's text, the target's device and inode, the plan
//!    that asked, phase `seating`), durable before any rename;
//! 2. one `renameat2(RENAME_EXCHANGE)` of the two names: LIVE is now the
//!    directory (the same inode: no byte is copied, read or written) and
//!    TARGET the old link. A swap that fails with both names still as they
//!    were removes the record again;
//! 3. at once, before either parent is synced, TARGET's link is replaced by
//!    one naming LIVE (a link made under a fixed temporary name, swapped in
//!    by a second exchange, the old link then unlinked), so every path that
//!    names the side root (a linked worktree's `gitdir`, another tool's
//!    configuration) resolves to the same tree again; then both parents are
//!    synced.
//!
//! The swap is atomic for the live spelling. Between steps 2 and 3 (no
//! sync between them) the side-root spelling names the old link.
//!
//! Only a link whose single hop names a directory: the link's own text
//! resolves, lexically, to the path the system resolves (no other link on
//! the way), LIVE's parent is a real path, TARGET is neither at, inside nor
//! above another candidate's target or the real path of any other planned
//! path, and both are on one file system. Each is checked again right
//! before its seat. Anything else is refused and left alone; a link outside
//! every side root is not this verb's.
//!
//! `estate-unseat STATE [LIVE ...]` marks the record `unseating` (durable)
//! and then undoes the seat exactly (the inode back at TARGET, the link back
//! at LIVE with its text) and removes the record. A crash at any step of
//! either leaves a state its record's phase names; the next `estate-apply`
//! finishes it (a seat or an undo), and reports a record it cannot finish
//! as a refused line without stopping the apply.

use std::ffi::CString;
use std::fs;
use std::os::fd::AsFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{read, write, write_new, Item};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "estate::seat";

/// Which way a seat record is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// `estate-apply --side-root`: towards seated.
    Seating,
    /// `estate-unseat`: towards the link back at the live path.
    Unseating,
}

/// One seat's record in STATE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRecord {
    /// The live path: a symlink before, the directory after.
    pub live: PathBuf,
    /// The side-root directory the link named: the link back after.
    pub target: PathBuf,
    /// The link's text, restored by an undo.
    pub text: Vec<u8>,
    /// The directory's device and inode, the same before and after.
    pub dev: u64,
    pub ino: u64,
    /// The plan whose apply seated it.
    pub plan: PathBuf,
    pub phase: Phase,
}

/// What a planned path's first symlink is, under the side roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A link into a side root that can be seated.
    Seatable { target: PathBuf, text: Vec<u8> },
    /// A link into a side root that is refused, and left alone.
    Refused(BulkloadRefusal),
}

/// One link a plan's paths are spelled through, and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub live: PathBuf,
    pub found: Found,
}

/// What a seat or an undo did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seated {
    /// Seated now.
    Seated,
    /// A seat a crash interrupted, finished.
    Resumed,
    /// Already seated: nothing written.
    Present,
    /// An undo a crash interrupted, finished.
    Unseated,
}

impl Seated {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Seated => "seated",
            Self::Resumed => "seat-resumed",
            Self::Present => "previous-seated",
            Self::Unseated => "unseated",
        }
    }
}

/// The side roots, each a real directory, as given canonicalized.
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a relative root; the canonicalize failure of one
/// that does not exist; `PATH_ESCAPES_ROOT` for a root that is not a
/// directory.
pub fn roots(given: &[PathBuf]) -> Result<Vec<PathBuf>> {
    given
        .iter()
        .map(|root| {
            if !root.is_absolute() {
                return Err(BulkloadRefusal::PathNotAbsolute);
            }
            let real = fs::canonicalize(root).refuse_at(SITE)?;
            if !fs::metadata(&real).refuse_at(SITE)?.is_dir() {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            Ok(real)
        })
        .collect()
}

// Every path a plan names: each item's repository and workspace.
fn planned(items: &[Item]) -> Vec<&Path> {
    let mut paths: Vec<&Path> = Vec::new();
    for item in items {
        paths.push(&item.repository);
        if let Some(workspace) = &item.workspace {
            paths.push(workspace);
        }
    }
    paths
}

// The first symlink at or above `path` (from the root down), if any.
fn first_link(path: &Path) -> Option<PathBuf> {
    let mut ancestors: Vec<&Path> = path.ancestors().collect();
    ancestors.reverse();
    for at in ancestors {
        match fs::symlink_metadata(at) {
            Ok(meta) if meta.file_type().is_symlink() => return Some(at.to_path_buf()),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

// `path` with no `.`, `..` or empty component: the lexical form a single
// hop must already be in.
fn lexical(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

// The real path `path` names: its nearest existing ancestor resolved, the
// rest as spelled.
fn real_spelling(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Every symlink a plan's paths are spelled through whose target is inside
/// a side root (`roots`, canonical), once each, in path order: seatable or
/// refused. A link outside every side root is not listed.
#[must_use]
pub fn candidates(items: &[Item], roots: &[PathBuf]) -> Vec<Candidate> {
    let planned = planned(items);
    let mut links: Vec<PathBuf> = planned.iter().filter_map(|path| first_link(path)).collect();
    links.sort();
    links.dedup();
    // Each link inside a side root, with what it resolves to.
    let mut ours: Vec<Ours> = Vec::new();
    for live in links {
        let resolved = fs::canonicalize(&live).ok();
        let inside = resolved.as_ref().is_some_and(|resolved| {
            roots
                .iter()
                .any(|root| resolved.starts_with(root) && resolved != root)
        });
        if inside {
            let answer = examine(&live, resolved.clone().unwrap_or_default());
            ours.push((live, resolved, answer));
            continue;
        }
        // A link that names a side root but does not resolve inside one is
        // still ours to refuse.
        let Ok(text) = fs::read_link(&live) else {
            continue;
        };
        let Some(parent) = live.parent() else {
            continue;
        };
        if roots
            .iter()
            .any(|root| parent.join(&text).starts_with(root))
        {
            ours.push((live, resolved, Err(BulkloadRefusal::PathEscapesRoot)));
        }
    }
    let others: Vec<(&Path, PathBuf)> = planned
        .iter()
        .map(|path| (*path, real_spelling(path)))
        .collect();
    ours.iter()
        .map(|(live, _, answer)| {
            let found = match answer {
                Err(refusal) => Found::Refused(refusal.clone()),
                Ok((target, text)) => {
                    // Another link's target at, inside or above this one, or
                    // another planned path's real spelling there (a path
                    // spelled through this link is the seat's own).
                    let crossed = ours.iter().any(|(other, resolved, _)| {
                        other != live && resolved.as_ref().is_some_and(|r| nested(r, target))
                    }) || others
                        .iter()
                        .any(|(path, real)| !path.starts_with(live) && nested(real, target));
                    if crossed {
                        Found::Refused(BulkloadRefusal::GitDestinationOccupied)
                    } else {
                        Found::Seatable {
                            target: target.clone(),
                            text: text.clone(),
                        }
                    }
                }
            };
            Candidate {
                live: live.clone(),
                found,
            }
        })
        .collect()
}

// A link inside a side root: its path, what it resolves to, and its
// single-hop answer.
type Ours = (PathBuf, Option<PathBuf>, Result<(PathBuf, Vec<u8>)>);

// The single-hop checks a seatable link passes (module notes).
fn examine(live: &Path, resolved: PathBuf) -> Result<(PathBuf, Vec<u8>)> {
    let parent = live.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    if !live.is_absolute() || fs::canonicalize(parent).refuse_at(SITE)? != parent {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let text = fs::read_link(live).refuse_at(SITE)?;
    let named = if text.is_absolute() {
        text.clone()
    } else {
        parent.join(&text)
    };
    if !lexical(&named) || named != resolved {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let meta = fs::symlink_metadata(&resolved).refuse_at(SITE)?;
    if !meta.is_dir() || parent.starts_with(&resolved) {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    if fs::symlink_metadata(parent).refuse_at(SITE)?.dev() != meta.dev() {
        return Err(BulkloadRefusal::Io(Some(libc::EXDEV)));
    }
    Ok((resolved, text.as_os_str().as_bytes().to_vec()))
}

/// Where `live`'s seat record lives in `state`.
#[must_use]
pub fn record_path(state: &Path, live: &Path) -> PathBuf {
    state.join(format!(
        "seat-{}.seat",
        blake3::hash(live.as_os_str().as_bytes()).to_hex()
    ))
}

// The parent directory and file name of `path`, opened without following
// the final component (the parent is a real path).
fn parent_of(path: &Path) -> Result<(fs::File, CString)> {
    let parent = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let name = path.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let dir = fs::File::from(crate::io::sys::open_dir_path_nofollow(parent).refuse_at(SITE)?);
    let name = crate::io::c_name(name.as_bytes()).refuse_at(SITE)?;
    Ok((dir, name))
}

// An `flock` on both parent directories of a seat, taken in path order and
// held while it is read or changed: two applies or undos with different
// STATEs never move one live path at once. Refused (`IO`, `EWOULDBLOCK`)
// while another holds either.
struct Parents {
    held: Vec<fs::File>,
}

// Unlocked explicitly, not only closed: a child another thread forked
// shares the descriptor until it execs, and would keep the lock past our
// close.
impl Drop for Parents {
    fn drop(&mut self) {
        for dir in &self.held {
            let _ = crate::io::sys::flock_unlock(dir.as_fd());
        }
    }
}

fn lock(record_live: &Path, record_target: &Path) -> Result<Parents> {
    let mut parents: Vec<&Path> = [record_live, record_target]
        .iter()
        .filter_map(|path| path.parent())
        .collect();
    parents.sort();
    parents.dedup();
    let mut held = Vec::new();
    for parent in parents {
        let dir = fs::File::from(crate::io::sys::open_dir_path_nofollow(parent).refuse_at(SITE)?);
        crate::io::sys::flock_exclusive(dir.as_fd()).refuse_at(SITE)?;
        held.push(dir);
    }
    Ok(Parents { held })
}

// What is at `path` now: a directory's (dev, ino), a link's text, or
// nothing else this module knows.
#[derive(Debug, PartialEq, Eq)]
enum At {
    Dir(u64, u64),
    Link(Vec<u8>),
    Other,
}

fn at(path: &Path) -> Result<At> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(At::Other),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    if meta.file_type().is_symlink() {
        Ok(At::Link(
            fs::read_link(path)
                .refuse_at(SITE)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
        ))
    } else if meta.is_dir() {
        Ok(At::Dir(meta.dev(), meta.ino()))
    } else {
        Ok(At::Other)
    }
}

fn sync(dir: &fs::File) -> Result<()> {
    use crate::counters::CountedSync as _;
    dir.sync_dir_counted().refuse_at(SITE)
}

fn sync_parents(a: &Path, b: &Path) -> Result<()> {
    sync(&parent_of(a)?.0)?;
    sync(&parent_of(b)?.0)
}

// Put a link with `text` at `path` in place of the link there now: a link
// made under a fixed temporary name beside it, exchanged with it, the old
// link then unlinked (all by the parent's descriptor). Not synced: the
// caller syncs. A temporary name a crash left (a link) is removed first.
fn relink(path: &Path, text: &[u8]) -> Result<()> {
    let (dir, name) = parent_of(path)?;
    let temporary = crate::io::c_name(
        format!(".bulkload-seat-{}", blake3::hash(name.as_bytes()).to_hex()).as_bytes(),
    )
    .refuse_at(SITE)?;
    match crate::io::sys::fstatat_nofollow(dir.as_fd(), &temporary) {
        // `st_mode` type bits, the same on Darwin and Linux.
        Ok(stat) if stat.mode & 0o170_000 == 0o120_000 => {
            crate::io::sys::unlinkat(dir.as_fd(), &temporary, false).refuse_at(SITE)?;
        }
        Ok(_) => return Err(BulkloadRefusal::ReceiptBindingInvalid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    let text = crate::io::c_name(text).refuse_at(SITE)?;
    crate::io::sys::symlinkat(&text, dir.as_fd(), &temporary).refuse_at(SITE)?;
    if let Err(error) = crate::io::sys::exchange_at(dir.as_fd(), &temporary, dir.as_fd(), &name) {
        let _ = crate::io::sys::unlinkat(dir.as_fd(), &temporary, false);
        return Err(crate::refuse::io(&error, SITE));
    }
    crate::io::sys::unlinkat(dir.as_fd(), &temporary, false).refuse_at(SITE)
}

fn swap(a: &Path, b: &Path) -> Result<()> {
    let (a_dir, a_name) = parent_of(a)?;
    let (b_dir, b_name) = parent_of(b)?;
    crate::io::sys::exchange_at(a_dir.as_fd(), &a_name, b_dir.as_fd(), &b_name).refuse_at(SITE)
}

fn remove_record(path: &Path) -> Result<()> {
    fs::remove_file(path).refuse_at(SITE)?;
    let state = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    sync(&fs::File::open(state).refuse_at(SITE)?)
}

/// Seat the candidate at `live` for `plan`, recorded in `state`, after
/// checking it again.
///
/// `items` and `roots` are the plan's; the module notes give the steps. A
/// record already there for `live` must name the same seat (phase
/// `seating`); the step a crash left is finished.
///
/// # Errors
/// The candidate's refusal now; `RECEIPT_BINDING_INVALID` when the record
/// or what the two paths hold is not a state of this seat (nothing is
/// written); the refusal of a rename (a failed swap removes its record).
pub fn seat(
    state: &Path,
    plan: &Path,
    items: &[Item],
    roots: &[PathBuf],
    live: &Path,
) -> Result<Seated> {
    let now = candidates(items, roots)
        .into_iter()
        .find(|candidate| candidate.live == live)
        .ok_or(BulkloadRefusal::ReceiptBindingInvalid)?;
    let (target, text) = match now.found {
        Found::Seatable { target, text } => (target, text),
        Found::Refused(refusal) => return Err(refusal),
    };
    let _held = lock(live, &target)?;
    let path = record_path(state, live);
    if path.try_exists().refuse_at(SITE)? {
        let record: SeatRecord = read(&path)?;
        if record.live != live
            || record.target != target
            || record.text != text
            || record.phase != Phase::Seating
        {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        return finish_seat(&path, &record);
    }
    let At::Dir(dev, ino) = at(&target)? else {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    };
    if at(live)? != At::Link(text.clone()) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let record = SeatRecord {
        live: live.to_path_buf(),
        target,
        text,
        dev,
        ino,
        plan: plan.to_path_buf(),
        phase: Phase::Seating,
    };
    write_new(&path, &record)?;
    finish_seat(&path, &record).map(|_| Seated::Seated)
}

// Take `record`'s seat from whatever step it is at to seated (both parents
// locked).
fn finish_seat(path: &Path, record: &SeatRecord) -> Result<Seated> {
    let tree = At::Dir(record.dev, record.ino);
    let back = At::Link(record.live.as_os_str().as_bytes().to_vec());
    let link = At::Link(record.text.clone());
    let (live, target) = (at(&record.live)?, at(&record.target)?);
    if live == tree && target == back {
        return Ok(Seated::Present);
    }
    if live == link && target == tree {
        if let Err(refusal) = swap(&record.live, &record.target) {
            // Nothing moved: the record goes, so no later run finishes a
            // seat that never started.
            if at(&record.live)? == link && at(&record.target)? == tree {
                remove_record(path)?;
            }
            return Err(refusal);
        }
    } else if !(live == tree && target == link) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    // The old link is at the target now: the link back replaces it before
    // either parent is synced.
    relink(&record.target, record.live.as_os_str().as_bytes())?;
    sync_parents(&record.live, &record.target)?;
    if at(&record.live)? != tree || at(&record.target)? != back {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    Ok(Seated::Resumed)
}

// Take `record`'s undo (phase `unseating`) from whatever step it is at to
// the link back at the live path, and remove the record (both parents
// locked).
fn finish_unseat(path: &Path, record: &SeatRecord) -> Result<()> {
    let tree = At::Dir(record.dev, record.ino);
    let back = At::Link(record.live.as_os_str().as_bytes().to_vec());
    let link = At::Link(record.text.clone());
    let (live, target) = (at(&record.live)?, at(&record.target)?);
    if live == tree && (target == back || target == link) {
        swap(&record.live, &record.target)?;
    } else if !(target == tree && (live == back || live == link)) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    if at(&record.live)? == back {
        relink(&record.live, &record.text)?;
    }
    sync_parents(&record.live, &record.target)?;
    if at(&record.live)? != link || at(&record.target)? != tree {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    remove_record(path)
}

/// Every seat record in `state`.
///
/// # Errors
/// Reading `state` or a record.
pub fn records(state: &Path) -> Result<Vec<(PathBuf, SeatRecord)>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(state) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        let name = entry.file_name();
        let name = name.as_bytes();
        if name.starts_with(b"seat-") && name.ends_with(b".seat") {
            let path = entry.path();
            found.push((path.clone(), read::<SeatRecord>(&path)?));
        }
    }
    found.sort_by(|a, b| a.1.live.cmp(&b.1.live));
    Ok(found)
}

/// Finish every seat or undo a crash interrupted in `state`, each by its
/// record's phase. A record that cannot be finished is that record's
/// refusal, never the caller's: its paths are left as they are.
///
/// # Errors
/// Reading `state` or a record.
pub fn resume(state: &Path) -> Result<Vec<(SeatRecord, Result<Seated>)>> {
    Ok(records(state)?
        .into_iter()
        .map(|(path, record)| {
            let done = lock(&record.live, &record.target).and_then(|_held| match record.phase {
                Phase::Seating => finish_seat(&path, &record),
                Phase::Unseating => finish_unseat(&path, &record).map(|()| Seated::Unseated),
            });
            (record, done)
        })
        .collect())
}

/// Undo the seat `record` names and remove its record (`path`): marked
/// `unseating` (durable) first, then the tree back at the target and the
/// link back at the live path with its own text.
///
/// # Errors
/// `RECEIPT_BINDING_INVALID` when what the two paths hold is not a state
/// of this seat (a tree replaced, a link rewritten): no path is changed.
pub fn unseat(path: &Path, record: &SeatRecord) -> Result<()> {
    let _held = lock(&record.live, &record.target)?;
    let tree = At::Dir(record.dev, record.ino);
    let back = At::Link(record.live.as_os_str().as_bytes().to_vec());
    let link = At::Link(record.text.clone());
    let (live, target) = (at(&record.live)?, at(&record.target)?);
    let undoable = (live == tree && (target == back || target == link))
        || (target == tree && (live == back || live == link));
    if !undoable {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let marked = SeatRecord {
        phase: Phase::Unseating,
        ..record.clone()
    };
    if record.phase != Phase::Unseating {
        write(path, &marked)?;
    }
    finish_unseat(path, &marked)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl World {
        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }
        fn roots(&self) -> Vec<PathBuf> {
            vec![self.path("side")]
        }
    }

    // `side/tree` (a directory with a file) and `live/name -> side/tree`.
    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("side/tree")).unwrap();
        fs::write(root.join("side/tree/file"), b"bytes").unwrap();
        fs::create_dir_all(root.join("live")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        std::os::unix::fs::symlink(root.join("side/tree"), root.join("live/name")).unwrap();
        World { _dir: dir, root }
    }

    fn item(path: &Path) -> Item {
        Item {
            source: PathBuf::from("/src"),
            repository: path.to_path_buf(),
            workspace: Some(path.to_path_buf()),
        }
    }

    fn found(world: &World, items: &[Item], live: &Path) -> Option<Found> {
        candidates(items, &world.roots())
            .into_iter()
            .find(|candidate| candidate.live == live)
            .map(|candidate| candidate.found)
    }

    // Seat `live/name` for a plan of that one item.
    fn seat_one(world: &World) -> Result<Seated> {
        let live = world.path("live/name");
        seat(
            &world.path("state"),
            Path::new("/plan"),
            &[item(&live)],
            &world.roots(),
            &live,
        )
    }

    fn only_record(world: &World) -> (PathBuf, SeatRecord) {
        let mut found = records(&world.path("state")).unwrap();
        assert_eq!(found.len(), 1);
        found.remove(0)
    }

    #[test]
    fn a_seat_moves_the_inode_and_leaves_a_link_back_and_undoes_exactly() {
        let world = world();
        let (live, target) = (world.path("live/name"), world.path("side/tree"));
        let ino = fs::metadata(&target).unwrap().ino();
        assert_eq!(seat_one(&world).unwrap(), Seated::Seated);
        let meta = fs::symlink_metadata(&live).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.ino(), ino);
        assert_eq!(fs::read(live.join("file")).unwrap(), b"bytes");
        assert_eq!(fs::read_link(&target).unwrap(), live);
        assert_eq!(fs::read(target.join("file")).unwrap(), b"bytes");
        assert!(candidates(&[item(&live)], &world.roots()).is_empty());
        let (path, record) = only_record(&world);
        assert_eq!(record.plan, Path::new("/plan"));
        assert_eq!(record.phase, Phase::Seating);
        // The resume of a finished seat writes nothing.
        let done = resume(&world.path("state")).unwrap();
        assert_eq!(done[0].1, Ok(Seated::Present));
        unseat(&path, &record).unwrap();
        assert_eq!(fs::read_link(&live).unwrap(), target);
        assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), ino);
        assert!(records(&world.path("state")).unwrap().is_empty());
        for dir in ["live", "side"] {
            assert!(!fs::read_dir(world.path(dir)).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".bulkload")));
        }
    }

    #[test]
    fn every_crash_step_of_a_seat_is_finished_by_the_next_run() {
        for step in 0..2 {
            let world = world();
            let (live, target) = (world.path("live/name"), world.path("side/tree"));
            let ino = fs::metadata(&target).unwrap().ino();
            let record = SeatRecord {
                live: live.clone(),
                target: target.clone(),
                text: target.as_os_str().as_bytes().to_vec(),
                dev: fs::metadata(&target).unwrap().dev(),
                ino,
                plan: PathBuf::from("/plan"),
                phase: Phase::Seating,
            };
            // Step 0: the record only. Step 1: swapped, link not replaced.
            write_new(&record_path(&world.path("state"), &live), &record).unwrap();
            if step == 1 {
                swap(&live, &target).unwrap();
            }
            let done = resume(&world.path("state")).unwrap();
            assert_eq!(done[0].1, Ok(Seated::Resumed), "step {step}");
            assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), ino);
            assert_eq!(fs::read_link(&target).unwrap(), live);
        }
    }

    #[test]
    fn every_crash_step_of_an_undo_is_finished_and_never_seated_again() {
        // Step 0: marked only. Step 1: swapped back, the live path a link
        // to itself. Step 2: both restored, the record not yet removed.
        for step in 0..3 {
            let world = world();
            let (live, target) = (world.path("live/name"), world.path("side/tree"));
            let ino = fs::metadata(&target).unwrap().ino();
            seat_one(&world).unwrap();
            let (path, record) = only_record(&world);
            let marked = SeatRecord {
                phase: Phase::Unseating,
                ..record
            };
            write(&path, &marked).unwrap();
            if step >= 1 {
                swap(&live, &target).unwrap();
            }
            if step == 2 {
                relink(&live, &marked.text).unwrap();
            }
            let done = resume(&world.path("state")).unwrap();
            assert_eq!(done[0].1, Ok(Seated::Unseated), "step {step}");
            assert_eq!(fs::read_link(&live).unwrap(), target, "step {step}");
            assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), ino);
            assert!(records(&world.path("state")).unwrap().is_empty());
            // Nothing is left to resume.
            assert!(resume(&world.path("state")).unwrap().is_empty());
        }
    }

    #[test]
    fn a_swap_that_fails_removes_its_record_and_changes_nothing() {
        let world = world();
        let (live, target) = (world.path("live/name"), world.path("side/tree"));
        crate::io::force_rename_unsupported(true);
        let refused = seat_one(&world);
        crate::io::force_rename_unsupported(false);
        assert_eq!(refused, Err(BulkloadRefusal::Io(Some(libc::EINVAL))));
        assert!(records(&world.path("state")).unwrap().is_empty());
        assert_eq!(fs::read_link(&live).unwrap(), target);
        assert!(fs::symlink_metadata(&target).unwrap().is_dir());
        assert_eq!(seat_one(&world).unwrap(), Seated::Seated);
    }

    #[test]
    fn a_record_resume_cannot_finish_is_its_refusal_not_the_callers() {
        let world = world();
        seat_one(&world).unwrap();
        // The live tree replaced by another directory.
        let live = world.path("live/name");
        fs::rename(&live, world.path("moved")).unwrap();
        fs::create_dir(&live).unwrap();
        let done = resume(&world.path("state")).unwrap();
        assert_eq!(done[0].1, Err(BulkloadRefusal::ReceiptBindingInvalid));
        let (path, record) = only_record(&world);
        assert_eq!(
            unseat(&path, &record),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            only_record(&world).1.phase,
            Phase::Seating,
            "an unseat refused before marking"
        );
    }

    #[test]
    fn a_seat_refuses_while_another_holds_a_parent() {
        let world = world();
        let parent = fs::File::open(world.path("live")).unwrap();
        crate::io::sys::flock_exclusive(parent.as_fd()).unwrap();
        assert!(matches!(seat_one(&world), Err(BulkloadRefusal::Io(_))));
        assert!(records(&world.path("state")).unwrap().is_empty());
        // Unlocked, not only closed (a forked child may share it).
        crate::io::sys::flock_unlock(parent.as_fd()).unwrap();
        drop(parent);
        assert_eq!(seat_one(&world).unwrap(), Seated::Seated);
    }

    #[test]
    fn only_a_single_hop_into_a_side_root_is_seatable() {
        let world = world();
        let side = world.path("side");
        std::os::unix::fs::symlink(&side, world.path("alias")).unwrap();
        let hop = world.path("live/hop");
        std::os::unix::fs::symlink(world.path("alias/tree"), &hop).unwrap();
        let dots = world.path("live/dots");
        std::os::unix::fs::symlink("../side/tree", &dots).unwrap();
        fs::create_dir(world.path("elsewhere")).unwrap();
        let outside = world.path("live/outside");
        std::os::unix::fs::symlink(world.path("elsewhere"), &outside).unwrap();
        fs::write(side.join("file"), b"f").unwrap();
        let file = world.path("live/file");
        std::os::unix::fs::symlink(side.join("file"), &file).unwrap();
        let whole = world.path("live/whole");
        std::os::unix::fs::symlink(&side, &whole).unwrap();
        let items: Vec<Item> = [&hop, &dots, &outside, &file, &whole]
            .iter()
            .map(|path| item(path))
            .collect();
        let escapes = Some(Found::Refused(BulkloadRefusal::PathEscapesRoot));
        assert_eq!(found(&world, &items, &hop), escapes);
        assert_eq!(found(&world, &items, &dots), escapes);
        assert_eq!(found(&world, &items, &outside), None);
        assert_eq!(
            found(&world, &items, &file),
            Some(Found::Refused(BulkloadRefusal::GitDestinationOccupied))
        );
        assert_eq!(found(&world, &items, &whole), escapes);
    }

    #[test]
    fn nested_targets_and_planned_paths_inside_a_target_are_refused() {
        let world = world();
        // live/a -> side/tree/inner, inside live/name's target side/tree.
        fs::create_dir(world.path("side/tree/inner")).unwrap();
        let (outer, inner) = (world.path("live/name"), world.path("live/a"));
        std::os::unix::fs::symlink(world.path("side/tree/inner"), &inner).unwrap();
        let occupied = Some(Found::Refused(BulkloadRefusal::GitDestinationOccupied));
        let items = [item(&outer), item(&inner)];
        assert_eq!(found(&world, &items, &outer), occupied);
        assert_eq!(found(&world, &items, &inner), occupied);
        // A planned path whose real spelling is inside the target.
        let second = self::world();
        let live = second.path("live/name");
        let items = [item(&live), item(&second.path("side/tree/sub"))];
        assert_eq!(found(&second, &items, &live), occupied);
        // A path spelled through the link itself is the seat's own.
        let items = [item(&live), item(&live.join(".worktrees/x"))];
        assert!(matches!(
            found(&second, &items, &live),
            Some(Found::Seatable { .. })
        ));
        // A relative single hop is seatable.
        let third = self::world();
        let relative = third.path("relative");
        std::os::unix::fs::symlink("side/tree", &relative).unwrap();
        assert!(matches!(
            found(&third, &[item(&relative)], &relative),
            Some(Found::Seatable { .. })
        ));
    }

    #[test]
    fn a_target_another_item_plans_is_never_moved() {
        let world = world();
        let live = world.path("live/name");
        let items = [item(&live), item(&world.path("side/tree"))];
        assert_eq!(
            found(&world, &items, &live),
            Some(Found::Refused(BulkloadRefusal::GitDestinationOccupied))
        );
        assert_eq!(
            seat(
                &world.path("state"),
                Path::new("/plan"),
                &items,
                &world.roots(),
                &live
            ),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
    }
}
