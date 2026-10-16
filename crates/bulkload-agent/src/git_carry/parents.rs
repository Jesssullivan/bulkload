//! The missing parents of a first landing (M2 finding 1, 2026-10-16).
//!
//! A planned workspace, linked worktree or bare repository whose path does
//! not exist used to refuse `GIT_DESTINATION_PARENT_MISSING` whenever its
//! parent was absent too: tonight's in-place run refused 42 of 104 items
//! this way, and a hand script made the directories. `estate-apply` now
//! makes them itself, under one policy:
//!
//! - **Only inside a real directory.** The absent ancestors are found by
//!   `lstat` from the destination's parent upward. The nearest one that
//!   exists must be a directory, not a symlink and not any other file: a
//!   new directory is never made through a link (it would land wherever the
//!   link points, outside the plan's spelling), so a symlink or a
//!   non-directory there keeps `GIT_DESTINATION_PARENT_MISSING`. Symlinks
//!   above that ancestor are the operator's own layout and resolve as an
//!   existing parent already does (`linked_destination` canonicalizes it).
//!   An existing parent that is not a directory is typed the same way.
//!   A `..` in the planned path, or an ancestor this user cannot write and
//!   search, keeps the refusal too: nothing is made where the lexical
//!   parent is not the real one, and the diff and the apply agree.
//! - **Race-safe.** That ancestor is opened `O_DIRECTORY | O_NOFOLLOW`, and
//!   each level is made under a temporary name (`.bulkload-parent-*`),
//!   given its mode, synced, and renamed into place without replacement,
//!   then opened again `O_NOFOLLOW`, so a link swapped in meanwhile refuses
//!   rather than redirects. A level another writer made first is used, not
//!   counted as bulkload's, and its parent is synced all the same.
//! - **0755, durable, recorded.** A level's final name only ever holds a
//!   0755 directory (`fchmod` before the rename, whatever the umask). Each
//!   is synced with its parent, and only then listed in the apply's
//!   `{item}-{source}.parents` record in STATE (the durability rule: a
//!   record never precedes what it describes), the levels made before a
//!   refusal deeper down included. A crash between leaves an empty 0755
//!   directory no record names, or an empty `.bulkload-parent-*` temporary;
//!   the rerun finds the parent present and lands.
//! - **After its path checks.** A landing creates its parents just before
//!   its first write: after the bare, occupied and parent checks of a
//!   standalone or bare landing (whose private stage, the next write, is
//!   where the capture's own refusals are decided), and after
//!   `check_linked` (metadata, exclude policy, intent-to-add custody) of a
//!   linked one.
//! - **Estate apply only.** A landing without a record path (the CLI
//!   restore verbs) refuses `GIT_DESTINATION_PARENT_MISSING` as before.
//!
//! `estate-diff` reports the same decision read-only: `landing` with the
//! component `parents=<n>` where `n` levels above the item are absent now
//! (items that share absent levels each count them; the apply makes each
//! once), and `parent-missing` only where the policy refuses.

use std::fs;
use std::os::fd::AsFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::landed;
use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The mode of every directory a landing creates.
pub const MODE: u32 = 0o755;

/// The directories one item's apply made above its destination.
///
/// Outermost first, in its STATE (`{item}-{source}.parents`). Appended to,
/// never rewritten: a later apply of the item that makes more adds them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedParents {
    /// `estate::id` of the item.
    pub item: String,
    /// Each directory's absolute path, as created.
    pub paths: Vec<Vec<u8>>,
}

impl landed::Record for CreatedParents {
    const MAGIC: &'static [u8; 16] = b"bulkload-parent1";
}

/// A first landing's path, decided before anything is written.
#[derive(Debug, Clone)]
pub struct FirstTarget {
    /// The path as the plan gives it.
    given: PathBuf,
    /// Its canonical form, when its parent exists.
    canonical: Option<PathBuf>,
    /// The absent ancestors a landing creates, outermost first.
    absent: Vec<PathBuf>,
}

impl FirstTarget {
    /// The path of a landing at `destination` (which must not exist): its
    /// parent canonical, or the ancestors the policy lets an estate apply
    /// create.
    ///
    /// # Errors
    /// `GIT_DESTINATION_PARENT_MISSING` when the nearest existing ancestor
    /// is a symlink or not a directory, or a path above is not a directory.
    pub fn of(destination: &Path) -> Result<Self> {
        let absent = absent_parents(destination)?;
        let canonical = if absent.is_empty() {
            let canonical = super::linked_destination(destination)?;
            // An existing parent that is not a directory (a file, or a link
            // to one) is typed too, never a bare errno at the first write.
            if !canonical.parent().is_some_and(Path::is_dir) {
                return Err(BulkloadRefusal::GitDestinationParentMissing);
            }
            Some(canonical)
        } else {
            None
        };
        Ok(Self {
            given: destination.to_path_buf(),
            canonical,
            absent,
        })
    }

    /// How many levels a landing creates (0 when the parent exists).
    #[must_use]
    pub const fn absent(&self) -> usize {
        self.absent.len()
    }

    /// The canonical path, its parent exists, when it does already.
    #[must_use]
    pub fn existing(&self) -> Option<&Path> {
        self.canonical.as_deref()
    }

    /// The landing's canonical path, creating its absent parents first when
    /// `record` (the apply's STATE record path for this item) allows it;
    /// with the number of directories this call created.
    ///
    /// # Errors
    /// `GIT_DESTINATION_PARENT_MISSING` without a record, or when the policy
    /// refuses (a link or file appeared meanwhile); IO.
    pub fn place(&self, record: Option<(&Path, &str)>) -> Result<(PathBuf, u64)> {
        if let Some(canonical) = &self.canonical {
            return Ok((canonical.clone(), 0));
        }
        let (record, item) = record.ok_or(BulkloadRefusal::GitDestinationParentMissing)?;
        let (created, outcome) = create(&self.absent);
        fault_point!(EstateApplyParentsMade);
        // Every level this call made is recorded, the levels before a
        // refusal included (a level made and then a refusal deeper down).
        if !created.is_empty() {
            let mut kept = landed::read::<CreatedParents>(record)?.unwrap_or_default();
            item.clone_into(&mut kept.item);
            kept.paths.extend(
                created
                    .iter()
                    .map(|path| path.as_os_str().as_bytes().to_vec()),
            );
            landed::write(record, &kept)?;
        }
        outcome?;
        let count = u64::try_from(created.len()).unwrap_or(u64::MAX);
        Ok((super::linked_destination(&self.given)?, count))
    }
}

/// The absent ancestors of `destination`, outermost first; empty when its
/// parent exists (whatever it is: that case is the caller's, as before).
///
/// # Errors
/// `GIT_DESTINATION_PARENT_MISSING` when the nearest existing ancestor is a
/// symlink or not a directory, when `ENOTDIR` says a path above is a file,
/// when this user cannot write and search that ancestor, or when an absent
/// level would have to be found through a `..` component (its lexical
/// parent is not its real one, so nothing is made for it).
pub fn absent_parents(destination: &Path) -> Result<Vec<PathBuf>> {
    let mut absent = Vec::new();
    let mut at = match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return Ok(absent),
    };
    loop {
        match fs::symlink_metadata(at) {
            Ok(_) if absent.is_empty() => return Ok(absent),
            Ok(meta) if meta.file_type().is_dir() => break,
            Ok(_) => return Err(BulkloadRefusal::GitDestinationParentMissing),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absent.push(at.to_path_buf());
                match at.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => at = parent,
                    // A relative path's first component: its parent is the
                    // current directory, which exists.
                    _ => break,
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => {
                return Err(BulkloadRefusal::GitDestinationParentMissing)
            }
            Err(error) => return Err(crate::refuse::io(&error, "git_carry::parents::absent")),
        }
    }
    if destination
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(BulkloadRefusal::GitDestinationParentMissing);
    }
    let boundary = match absent.last().and_then(|outermost| outermost.parent()) {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if !writable(boundary)? {
        return Err(BulkloadRefusal::GitDestinationParentMissing);
    }
    absent.reverse();
    Ok(absent)
}

// Whether this user may make an entry in `directory` (write and search),
// so the diff and the apply agree on a boundary the apply cannot write.
fn writable(directory: &Path) -> Result<bool> {
    let name = crate::io::c_name(directory.as_os_str().as_bytes())
        .refuse_at("git_carry::parents::writable")?;
    // SAFETY: `name` is NUL-terminated and outlives the call; `access`
    // reads only the path and returns a status.
    Ok(unsafe { libc::access(name.as_ptr(), libc::W_OK | libc::X_OK) } == 0)
}

// Make each of `absent` (outermost first, each the parent of the next)
// inside its existing parent, opened without following a link. Each level
// is made under a temporary name, given mode 0755 and synced, then renamed
// into place without replacement, so its final name never holds another
// mode; where the file system has no such rename, `mkdirat` at the name,
// then the mode. Every parent is synced after its level is made or found,
// a level another writer made included. Returns the levels this call made,
// and the refusal that stopped it, if any.
fn create(absent: &[PathBuf]) -> (Vec<PathBuf>, Result<()>) {
    let mut created = Vec::new();
    let outcome = create_into(absent, &mut created);
    (created, outcome)
}

fn create_into(absent: &[PathBuf], created: &mut Vec<PathBuf>) -> Result<()> {
    const SITE: &str = "git_carry::parents::create";
    let Some(first) = absent.first() else {
        return Ok(());
    };
    let boundary = match first.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let typed = |error: std::io::Error| match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR | libc::ENOENT) => {
            BulkloadRefusal::GitDestinationParentMissing
        }
        _ => crate::refuse::io(&error, SITE),
    };
    let mut parent =
        fs::File::from(crate::io::sys::open_dir_path_nofollow(boundary).map_err(typed)?);
    for path in absent {
        let name = path
            .file_name()
            .ok_or(BulkloadRefusal::GitDestinationParentMissing)?;
        let name = crate::io::c_name(name.as_bytes()).refuse_at(SITE)?;
        let made = make_level(&parent, &name).map_err(typed)?;
        let child =
            fs::File::from(crate::io::sys::open_dir_at(parent.as_fd(), &name).map_err(typed)?);
        parent.sync_dir_counted().refuse_at(SITE)?;
        if made {
            created.push(path.clone());
        }
        parent = child;
    }
    Ok(())
}

// One level `name` in `parent`: `true` when this call made it, `false` when
// it was there already (another writer's).
fn make_level(parent: &fs::File, name: &std::ffi::CStr) -> std::io::Result<bool> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = crate::io::c_name(
        format!(
            ".bulkload-parent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
        .as_bytes(),
    )?;
    crate::io::sys::mkdirat(parent.as_fd(), &temporary, 0o700)?;
    let settled = crate::io::sys::open_dir_at(parent.as_fd(), &temporary).and_then(|made| {
        crate::io::sys::fchmod(made.as_fd(), MODE)?;
        fs::File::from(made).sync_dir_counted()
    });
    let renamed = settled.and_then(|()| crate::io::rename_exclusive(parent, &temporary, name));
    match renamed {
        Ok(()) => Ok(true),
        Err(error) => {
            let _ = crate::io::sys::unlinkat(parent.as_fd(), &temporary, true);
            if error.raw_os_error() == Some(libc::EEXIST) {
                Ok(false)
            } else if crate::io::rename_unsupported(&error) {
                match crate::io::sys::mkdirat(parent.as_fd(), name, MODE) {
                    Ok(()) => {
                        let made = crate::io::sys::open_dir_at(parent.as_fd(), name)?;
                        crate::io::sys::fchmod(made.as_fd(), MODE)?;
                        fs::File::from(made).sync_dir_counted()?;
                        Ok(true)
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(false),
                    Err(error) => Err(error),
                }
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn scratch(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "bulkload-parents-{tag}-{}-{}",
            std::process::id(),
            blake3::hash(tag.as_bytes()).to_hex().get(..8).unwrap()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        fs::canonicalize(&base).unwrap()
    }

    #[test]
    fn an_existing_parent_needs_nothing() {
        let base = scratch("existing");
        let target = FirstTarget::of(&base.join("dest")).unwrap();
        assert_eq!(target.absent(), 0);
        assert_eq!(target.existing(), Some(base.join("dest").as_path()));
        assert_eq!(target.place(None).unwrap(), (base.join("dest"), 0));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn absent_levels_are_created_0755_and_recorded() {
        let base = scratch("created");
        let dest = base.join("a/b/c/dest");
        let target = FirstTarget::of(&dest).unwrap();
        assert_eq!(target.absent(), 3);
        assert!(target.existing().is_none());
        assert_eq!(
            target.place(None),
            Err(BulkloadRefusal::GitDestinationParentMissing),
            "without a record the old refusal stands"
        );
        assert!(!base.join("a").exists());
        let record = base.join("item.parents");
        let (path, made) = target.place(Some((&record, "item"))).unwrap();
        assert_eq!((path, made), (dest, 3));
        for level in ["a", "a/b", "a/b/c"] {
            let meta = fs::symlink_metadata(base.join(level)).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.permissions().mode() & 0o7777, MODE, "{level}");
        }
        let kept = landed::read::<CreatedParents>(&record).unwrap().unwrap();
        assert_eq!(kept.item, "item");
        assert_eq!(
            kept.paths,
            ["a", "a/b", "a/b/c"]
                .iter()
                .map(|level| base.join(level).as_os_str().as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
        // A second landing below the same parents creates and records
        // nothing new.
        let again = FirstTarget::of(&base.join("a/b/c/other")).unwrap();
        assert_eq!(again.absent(), 0);
        let sibling = FirstTarget::of(&base.join("a/b/d/other")).unwrap();
        assert_eq!(sibling.absent(), 1);
        sibling.place(Some((&record, "item"))).unwrap();
        let kept = landed::read::<CreatedParents>(&record).unwrap().unwrap();
        assert_eq!(kept.paths.len(), 4, "appended");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_symlink_or_file_at_the_boundary_keeps_the_refusal() {
        let base = scratch("policy");
        let outside = scratch("policy-outside");
        std::os::unix::fs::symlink(&outside, base.join("link")).unwrap();
        assert_eq!(
            FirstTarget::of(&base.join("link/new/dest")).map(|t| t.absent()),
            Err(BulkloadRefusal::GitDestinationParentMissing),
            "a new level is never made through a link"
        );
        std::os::unix::fs::symlink(base.join("nowhere"), base.join("dangling")).unwrap();
        assert_eq!(
            FirstTarget::of(&base.join("dangling/new/dest")).map(|t| t.absent()),
            Err(BulkloadRefusal::GitDestinationParentMissing)
        );
        fs::write(base.join("file"), b"x").unwrap();
        assert_eq!(
            FirstTarget::of(&base.join("file/new/dest")).map(|t| t.absent()),
            Err(BulkloadRefusal::GitDestinationParentMissing)
        );
        assert_eq!(
            FirstTarget::of(&base.join("file/dest")).map(|t| t.absent()),
            Err(BulkloadRefusal::GitDestinationParentMissing),
            "an existing parent that is a file is typed"
        );
        // A link to a directory as the direct parent resolves, as before.
        assert_eq!(
            FirstTarget::of(&base.join("link/dest"))
                .unwrap()
                .existing()
                .map(Path::to_path_buf),
            Some(outside.join("dest"))
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        // A link to a directory one level up from the boundary is the
        // operator's layout: the absent levels below it are created inside
        // the real directory it names.
        fs::create_dir(outside.join("real")).unwrap();
        let target = FirstTarget::of(&base.join("link/real/x/dest")).unwrap();
        assert_eq!(target.absent(), 1);
        fs::remove_dir_all(&base).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_link_swapped_in_after_the_check_refuses() {
        let base = scratch("swap");
        let outside = scratch("swap-outside");
        let target = FirstTarget::of(&base.join("a/b/dest")).unwrap();
        assert_eq!(target.absent(), 2);
        std::os::unix::fs::symlink(&outside, base.join("a")).unwrap();
        let record = base.join("item.parents");
        assert_eq!(
            target.place(Some((&record, "item"))).map(|(_, n)| n),
            Err(BulkloadRefusal::GitDestinationParentMissing)
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        assert!(!record.exists());
        fs::remove_dir_all(&base).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_dotdot_path_or_an_unwritable_boundary_makes_nothing() {
        let base = scratch("dotdot");
        let dest = base.join("a/../b/dest");
        assert_eq!(
            FirstTarget::of(&dest).map(|t| t.absent()),
            Err(BulkloadRefusal::GitDestinationParentMissing)
        );
        assert!(!base.join("a").exists() && !base.join("b").exists());
        // `..` through levels that exist is the operator's spelling.
        fs::create_dir(base.join("a")).unwrap();
        assert_eq!(
            FirstTarget::of(&base.join("a/../dest")).map(|t| t.absent()),
            Ok(0)
        );
        if crate::io::sys::effective_uid() != 0 {
            let locked = base.join("locked");
            fs::create_dir(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
            assert_eq!(
                FirstTarget::of(&locked.join("new/dest")).map(|t| t.absent()),
                Err(BulkloadRefusal::GitDestinationParentMissing)
            );
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn no_temporary_is_left_and_levels_are_0755() {
        let base = scratch("temps");
        let record = base.join("item.parents");
        FirstTarget::of(&base.join("x/y/dest"))
            .unwrap()
            .place(Some((&record, "item")))
            .unwrap();
        for dir in [&base, &base.join("x"), &base.join("x/y")] {
            for entry in fs::read_dir(dir).unwrap() {
                let name = entry.unwrap().file_name();
                assert!(
                    !name.to_string_lossy().starts_with(".bulkload-parent-"),
                    "{name:?} left in {}",
                    dir.display()
                );
            }
        }
        for level in ["x", "x/y"] {
            let mode = fs::symlink_metadata(base.join(level))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o7777, MODE, "{level}");
        }
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_level_made_by_another_writer_is_used_not_recorded() {
        let base = scratch("raced");
        let target = FirstTarget::of(&base.join("a/b/dest")).unwrap();
        fs::create_dir(base.join("a")).unwrap();
        let record = base.join("item.parents");
        let (_, made) = target.place(Some((&record, "item"))).unwrap();
        assert_eq!(made, 1);
        let kept = landed::read::<CreatedParents>(&record).unwrap().unwrap();
        assert_eq!(
            kept.paths,
            vec![base.join("a/b").as_os_str().as_bytes().to_vec()]
        );
        fs::remove_dir_all(&base).unwrap();
    }
}
