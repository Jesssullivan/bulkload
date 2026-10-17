//! A symlinked live path into a side root, seated (M2r2 item e).
//!
//! The 10-16 in-place move found live checkout paths (`~/git/<name>`) that
//! were symlinks into a side root, the tree an earlier estate landing made
//! (`estate/gftb-20261015/git/<name>`). `estate-apply --adopt` refuses a
//! checkout spelled through a symlink (`GIT_DESTINATION_OCCUPIED`,
//! `symlinked-path`), so the lane copied each target over its link by hand
//! (`cp -a`) and repaired the worktree links. This module is the bulkload
//! way: `estate-apply --side-root ROOT` seats each planned path that is such
//! a link before any item lands, and the items then land or adopt (A11a)
//! or re-key (A11b) at the live path as they would at any real directory.
//!
//! Seating `LIVE -> TARGET` (TARGET a directory strictly inside ROOT):
//!
//! 1. a record `seat-<blake3(LIVE)>.seat` in STATE (the live path, the
//!    target, the link's text, the target's device and inode), durable
//!    before any rename;
//! 2. one `renameat2(RENAME_EXCHANGE)` of the two names: LIVE is now the
//!    directory (the same inode: no byte is copied, read or written) and
//!    TARGET the old link;
//! 3. TARGET's link is replaced by one naming LIVE (a temporary link
//!    renamed over it), so every path that names the side root (a linked
//!    worktree's `gitdir`, another tool's configuration) still resolves,
//!    to the same tree; both parents are synced.
//!
//! Only a link whose single hop names a directory: the link's own text
//! resolves, lexically, to the path the system resolves (no other link on
//! the way), LIVE's parent is a real path, TARGET is not a planned path or
//! under one, and both are on one file system. Anything else is refused
//! and left alone; a link outside every side root is not this verb's.
//! `estate-unseat STATE [LIVE ...]` undoes a seat exactly (the inode back
//! at TARGET, the link back at LIVE with its text) and removes its record.
//! A crash at any step leaves a state the record names; the next
//! `estate-apply` or `estate-unseat` finishes it.

use std::ffi::CString;
use std::fs;
use std::os::fd::AsFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{read, write_new, Item};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "estate::seat";

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

/// What a seat did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seated {
    /// Seated now.
    Seated,
    /// A seat a crash interrupted, finished.
    Resumed,
    /// Already seated: nothing written.
    Present,
}

impl Seated {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Seated => "seated",
            Self::Resumed => "seat-resumed",
            Self::Present => "previous-seated",
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

/// Every symlink a plan's paths are spelled through whose target is inside
/// a side root (`roots`, canonical), once each, in path order: seatable or
/// refused. A link outside every side root is not listed.
///
/// # Errors
/// None of its own: each link's failure is its answer.
pub fn candidates(items: &[Item], roots: &[PathBuf]) -> Vec<Candidate> {
    let mut links: Vec<PathBuf> = planned(items).into_iter().filter_map(first_link).collect();
    links.sort();
    links.dedup();
    let planned = planned(items);
    links
        .into_iter()
        .filter_map(|live| {
            let resolved = fs::canonicalize(&live).ok();
            let inside = resolved.as_ref().is_some_and(|resolved| {
                roots
                    .iter()
                    .any(|root| resolved.starts_with(root) && resolved != root)
            });
            if !inside {
                // A link into a side root that no longer resolves is still
                // ours to refuse: its text names one.
                let text = fs::read_link(&live).ok()?;
                let named = live.parent()?.join(text);
                if !roots.iter().any(|root| named.starts_with(root)) {
                    return None;
                }
                return Some(Candidate {
                    live,
                    found: Found::Refused(BulkloadRefusal::PathEscapesRoot),
                });
            }
            let found = examine(&live, resolved?, &planned).map_or_else(
                Found::Refused,
                |(target, text)| Found::Seatable { target, text },
            );
            Some(Candidate { live, found })
        })
        .collect()
}

// The checks a seatable link passes (module notes).
fn examine(live: &Path, resolved: PathBuf, planned: &[&Path]) -> Result<(PathBuf, Vec<u8>)> {
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
    // One hop, no other link on the way: the text's lexical target is the
    // resolved path.
    if !lexical(&named) || named != resolved {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let meta = fs::symlink_metadata(&resolved).refuse_at(SITE)?;
    if !meta.is_dir() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    // The live path inside the tree it names, or a planned path at or
    // under the target (another item's tree), is never moved.
    if parent.starts_with(&resolved)
        || planned
            .iter()
            .any(|path| path.starts_with(&resolved) || resolved.starts_with(path))
    {
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

// Put a link with `text` at `path` in place of the link there now: a
// temporary link beside it, renamed over it. The caller checked what the
// link there is.
fn relink(path: &Path, text: &[u8]) -> Result<()> {
    let (dir, name) = parent_of(path)?;
    let temporary = crate::io::c_name(
        format!(
            ".bulkload-seat-{}-{}",
            std::process::id(),
            blake3::hash(name.as_bytes()).to_hex()
        )
        .as_bytes(),
    )
    .refuse_at(SITE)?;
    let _ = crate::io::sys::unlinkat(dir.as_fd(), &temporary, false);
    let text = crate::io::c_name(text).refuse_at(SITE)?;
    crate::io::sys::symlinkat(&text, dir.as_fd(), &temporary).refuse_at(SITE)?;
    let parent = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let from = parent.join(std::ffi::OsStr::from_bytes(temporary.as_bytes()));
    if let Err(error) = fs::rename(&from, path) {
        let _ = crate::io::sys::unlinkat(dir.as_fd(), &temporary, false);
        return Err(crate::refuse::io(&error, SITE));
    }
    sync(&dir)
}

fn exchange(a: &Path, b: &Path) -> Result<()> {
    let (a_dir, a_name) = parent_of(a)?;
    let (b_dir, b_name) = parent_of(b)?;
    crate::io::sys::exchange_at(a_dir.as_fd(), &a_name, b_dir.as_fd(), &b_name).refuse_at(SITE)?;
    sync(&a_dir)?;
    sync(&b_dir)
}

/// Seat `live` (a link with `text` naming `target`), recorded in `state`.
///
/// The module notes give the steps. A record already there for `live` must
/// name the same seat; the step a crash left is finished.
///
/// # Errors
/// `RECEIPT_BINDING_INVALID` when the record or what the two paths hold is
/// not a state of this seat (nothing is written); the refusal of a rename.
pub fn seat(state: &Path, live: &Path, target: &Path, text: &[u8]) -> Result<Seated> {
    let path = record_path(state, live);
    let record = if path.try_exists().refuse_at(SITE)? {
        let record: SeatRecord = read(&path)?;
        if record.live != live || record.target != target || record.text != text {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        record
    } else {
        let At::Dir(dev, ino) = at(target)? else {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        };
        if at(live)? != At::Link(text.to_vec()) {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        let record = SeatRecord {
            live: live.to_path_buf(),
            target: target.to_path_buf(),
            text: text.to_vec(),
            dev,
            ino,
        };
        write_new(&path, &record)?;
        return finish(&record).map(|_| Seated::Seated);
    };
    finish(&record)
}

// Take `record`'s seat from whatever step it is at to seated.
fn finish(record: &SeatRecord) -> Result<Seated> {
    let tree = At::Dir(record.dev, record.ino);
    let back = At::Link(record.live.as_os_str().as_bytes().to_vec());
    let link = At::Link(record.text.clone());
    let (live, target) = (at(&record.live)?, at(&record.target)?);
    if live == tree && target == back {
        return Ok(Seated::Present);
    }
    if live == link && target == tree {
        exchange(&record.live, &record.target)?;
    } else if !(live == tree && target == link) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    // The old link is at the target now: replace it with the link back.
    relink(&record.target, record.live.as_os_str().as_bytes())?;
    if at(&record.live)? != tree || at(&record.target)? != back {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    Ok(Seated::Resumed)
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

/// Finish every seat a crash interrupted in `state`.
///
/// # Errors
/// As [`seat`], for the first record whose state is not one of its seat's.
pub fn resume(state: &Path) -> Result<Vec<(SeatRecord, Seated)>> {
    records(state)?
        .into_iter()
        .map(|(_, record)| finish(&record).map(|done| (record, done)))
        .collect()
}

/// Undo the seat `record` names (module notes) and remove its record
/// (`path`): the tree back at the target, the link back at the live path
/// with its own text.
///
/// # Errors
/// `RECEIPT_BINDING_INVALID` when what the two paths hold is not a state
/// of this seat (a tree replaced, a link rewritten): nothing is written.
pub fn unseat(path: &Path, record: &SeatRecord) -> Result<()> {
    let tree = At::Dir(record.dev, record.ino);
    let back = At::Link(record.live.as_os_str().as_bytes().to_vec());
    let link = At::Link(record.text.clone());
    let (live, target) = (at(&record.live)?, at(&record.target)?);
    if live == tree && (target == back || target == link) {
        exchange(&record.live, &record.target)?;
    } else if !(target == tree && (live == back || live == link)) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    if at(&record.live)? == back {
        relink(&record.live, &record.text)?;
    }
    if at(&record.live)? != link || at(&record.target)? != tree {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    fs::remove_file(path).refuse_at(SITE)?;
    let state = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    sync(&fs::File::open(state).refuse_at(SITE)?)
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

    fn seatable(world: &World) -> (PathBuf, PathBuf, Vec<u8>) {
        let live = world.root.join("live/name");
        let found = candidates(&[item(&live)], &[world.root.join("side")]);
        assert_eq!(found.len(), 1, "{found:?}");
        let Found::Seatable { target, text } = found[0].found.clone() else {
            panic!("{found:?}");
        };
        (live, target, text)
    }

    #[test]
    fn a_seat_moves_the_inode_and_leaves_a_link_back_and_undoes_exactly() {
        let world = world();
        let (live, target, text) = seatable(&world);
        let ino = fs::metadata(&target).unwrap().ino();
        let state = world.root.join("state");
        assert_eq!(seat(&state, &live, &target, &text).unwrap(), Seated::Seated);
        let meta = fs::symlink_metadata(&live).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.ino(), ino);
        assert_eq!(fs::read(live.join("file")).unwrap(), b"bytes");
        assert_eq!(fs::read_link(&target).unwrap(), live);
        assert_eq!(fs::read(target.join("file")).unwrap(), b"bytes");
        // A rerun writes nothing.
        assert_eq!(
            seat(&state, &live, &target, &text).unwrap(),
            Seated::Present
        );
        assert!(candidates(&[item(&live)], &[world.root.join("side")]).is_empty());
        let mut found = records(&state).unwrap();
        assert_eq!(found.len(), 1);
        let (path, record) = found.remove(0);
        unseat(&path, &record).unwrap();
        assert_eq!(
            fs::read_link(&live).unwrap().as_os_str().as_bytes(),
            &text[..]
        );
        assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), ino);
        assert!(records(&state).unwrap().is_empty());
        assert!(!fs::read_dir(world.root.join("live"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".bulkload")));
    }

    #[test]
    fn every_crash_step_is_finished_by_the_next_run() {
        for step in 0..2 {
            let world = world();
            let (live, target, text) = seatable(&world);
            let state = world.root.join("state");
            let ino = fs::metadata(&target).unwrap().ino();
            let record = SeatRecord {
                live: live.clone(),
                target: target.clone(),
                text: text.clone(),
                dev: fs::metadata(&target).unwrap().dev(),
                ino,
            };
            // Step 0: the record only. Step 1: exchanged, link not replaced.
            write_new(&record_path(&state, &live), &record).unwrap();
            if step == 1 {
                exchange(&live, &target).unwrap();
            }
            let done = resume(&state).unwrap();
            assert_eq!(done.len(), 1);
            assert_eq!(done[0].1, Seated::Resumed, "step {step}");
            assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), ino);
            assert_eq!(fs::read_link(&target).unwrap(), live);
        }
    }

    #[test]
    fn a_seat_whose_paths_moved_on_refuses_and_writes_nothing() {
        let world = world();
        let (live, target, text) = seatable(&world);
        let state = world.root.join("state");
        seat(&state, &live, &target, &text).unwrap();
        // The live tree replaced by another directory: never undone over.
        fs::rename(&live, world.root.join("moved")).unwrap();
        fs::create_dir(&live).unwrap();
        let (path, record) = records(&state).unwrap().remove(0);
        assert_eq!(
            unseat(&path, &record),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert!(path.exists());
        assert_eq!(
            seat(&state, &live, &target, &text),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
    }

    #[test]
    fn only_a_single_hop_into_a_side_root_is_seatable() {
        let world = world();
        let side = world.root.join("side");
        // Through another link on the way: refused.
        std::os::unix::fs::symlink(world.root.join("side"), world.root.join("alias")).unwrap();
        let hop = world.root.join("live/hop");
        std::os::unix::fs::symlink(world.root.join("alias/tree"), &hop).unwrap();
        // A `..` in the text: refused.
        let dots = world.root.join("live/dots");
        std::os::unix::fs::symlink("../side/tree", &dots).unwrap();
        // Outside every side root: not listed.
        fs::create_dir(world.root.join("elsewhere")).unwrap();
        let outside = world.root.join("live/outside");
        std::os::unix::fs::symlink(world.root.join("elsewhere"), &outside).unwrap();
        // To a file: refused.
        fs::write(side.join("file"), b"f").unwrap();
        let file = world.root.join("live/file");
        std::os::unix::fs::symlink(side.join("file"), &file).unwrap();
        // The side root itself: not inside it.
        let whole = world.root.join("live/whole");
        std::os::unix::fs::symlink(&side, &whole).unwrap();
        let items: Vec<Item> = [&hop, &dots, &outside, &file, &whole]
            .iter()
            .map(|path| item(path))
            .collect();
        let found = candidates(&items, std::slice::from_ref(&side));
        let answer = |path: &Path| {
            found
                .iter()
                .find(|candidate| candidate.live == path)
                .map(|candidate| candidate.found.clone())
        };
        assert_eq!(
            answer(&hop),
            Some(Found::Refused(BulkloadRefusal::PathEscapesRoot))
        );
        assert_eq!(
            answer(&dots),
            Some(Found::Refused(BulkloadRefusal::PathEscapesRoot))
        );
        assert_eq!(answer(&outside), None);
        assert_eq!(
            answer(&file),
            Some(Found::Refused(BulkloadRefusal::GitDestinationOccupied))
        );
        assert_eq!(
            answer(&whole),
            Some(Found::Refused(BulkloadRefusal::PathEscapesRoot))
        );
        // A relative single hop is seatable.
        let relative = world.root.join("live/relative");
        std::os::unix::fs::symlink(side.join("tree"), &relative).unwrap();
        let found = candidates(&[item(&relative)], &[side]);
        assert!(
            matches!(found[0].found, Found::Seatable { .. }),
            "{found:?}"
        );
    }

    #[test]
    fn a_target_another_item_plans_is_never_moved() {
        let world = world();
        let live = world.root.join("live/name");
        let target = world.root.join("side/tree");
        let found = candidates(&[item(&live), item(&target)], &[world.root.join("side")]);
        assert_eq!(
            found[0].found,
            Found::Refused(BulkloadRefusal::GitDestinationOccupied)
        );
    }
}
