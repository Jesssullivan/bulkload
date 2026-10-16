//! Extent sharing: whether a file's blocks belong to another file too
//! (OI-1003-Q165).
//!
//! S2 says bulkload never writes to the source. A clone (`FICLONE`,
//! `FICLONERANGE`, a same-file-system `copy_file_range`, `clonefile`) writes
//! the source inode's reflink flag and its extents' reference counts while
//! every `stat` field stays the same (the clone study,
//! `docs/agent-notes/2026-10-15-s1-clone-lever.md` §3.1). The one place that
//! effect shows outside the kernel is `FIEMAP`: the source's extents come
//! back `FIEMAP_EXTENT_SHARED`.
//!
//! So the rule has two halves. The agent's source access never makes a remap
//! call: it reads with `pread` (and `read`/`splice` into a pipe), and P84
//! SOURCE-NO-CLONE (`tests/source_command_registry.rs`) refuses any remap
//! call, and every `std::fs::copy` and `std::io::copy` that is not a
//! registered, argued site. And this module observes the effect: the bench
//! refuses a fixture that shares an extent before a sample, and after every
//! arm (OI-1003-Q165 (b)), and P84's kernel leg
//! (`tests/source_lock_trace.rs`) holds a real `copy`'s source to zero shared
//! extents on a reflink file system.
//!
//! The probe is read-only: no `FIEMAP_FLAG_SYNC`, so it starts no write-back
//! of the file it looks at, and it reads no byte of it. It is Linux only;
//! elsewhere every call returns [`std::io::ErrorKind::Unsupported`].
//!
//! **What it can and cannot say** (review, 2026-10-15). `FIEMAP` reports
//! sharing that exists *now*: a clone whose target was then overwritten in
//! full, or unlinked and freed, leaves the source unshared again, though the
//! clone call did write the source inode (on xfs its reflink flag stays). So
//! a `0` here means "currently shares nothing", not "was never cloned"; the
//! static half (P84) is what rules out the call. And only some file systems
//! report sharing at all: a CIFS/SMB3 mount can clone (duplicate extents)
//! and answers `FIEMAP`, but never sets `FIEMAP_EXTENT_SHARED`. So the file
//! system is read first (`fstatfs`): `FIEMAP`'s flag is trusted only on xfs,
//! btrfs, bcachefs and ocfs2 ([`Basis::Fiemap`]); ext2/3/4 and tmpfs cannot
//! reflink at all, so a file there is unshared by construction
//! ([`Basis::CannotReflink`]); every other file system is
//! [`io::ErrorKind::Unsupported`], never a silent `0`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The sharing of one tree's regular files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Regular files probed.
    pub files: u64,
    /// Their mapped extents.
    pub extents: u64,
    /// Their extents flagged shared.
    pub shared_extents: u64,
    /// Every file with a shared extent, in walk order.
    pub shared_files: Vec<PathBuf>,
    /// Of `files`, those on a file system that cannot reflink
    /// ([`Basis::CannotReflink`]): unshared by construction, not by a flag.
    pub no_reflink_files: u64,
}

/// Why a file's shared count can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The file system reflinks and reports sharing as
    /// `FIEMAP_EXTENT_SHARED`: xfs, btrfs, bcachefs, ocfs2.
    Fiemap,
    /// The file system cannot reflink (ext2/3/4, tmpfs): no clone can share
    /// the file's blocks, so its shared count is 0 by construction.
    CannotReflink,
}

/// One regular file's sharing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sharing {
    /// Mapped extents (0 where the file system has no extent map).
    pub extents: u64,
    /// Extents flagged `FIEMAP_EXTENT_SHARED`.
    pub shared: u64,
    /// Why `shared` can be trusted.
    pub basis: Basis,
}

use crate::io::durable::{
    BTRFS_SUPER_MAGIC as BTRFS, EXT4_SUPER_MAGIC as EXT, TMPFS_MAGIC as TMPFS,
    XFS_SUPER_MAGIC as XFS,
};
/// `fstatfs`'s `f_type` of bcachefs.
const BCACHEFS: u32 = 0xCA45_1A4E;
/// `fstatfs`'s `f_type` of ocfs2.
const OCFS2: u32 = 0x7461_636F;

/// How far `FIEMAP` can be trusted on the file system of type `magic`
/// (`fstatfs`'s `f_type`): `None` where it cannot tell sharing apart (any
/// file system not named here, CIFS/SMB3 and NFS among them).
#[must_use]
pub const fn basis(magic: u32) -> Option<Basis> {
    match magic {
        XFS | BTRFS | BCACHEFS | OCFS2 => Some(Basis::Fiemap),
        EXT | TMPFS => Some(Basis::CannotReflink),
        _ => None,
    }
}

/// The sharing of the regular file at `path`. The path is opened
/// read-only, without following a final symlink, and never read.
///
/// # Errors
/// The failed open, `fstatfs` or probe; [`io::ErrorKind::Unsupported`] off
/// Linux and on a file system whose sharing `FIEMAP` cannot show
/// ([`basis`]).
pub fn file_sharing(path: &Path) -> io::Result<Sharing> {
    file_sharing_impl(path)
}

#[cfg(target_os = "linux")]
fn file_sharing_impl(path: &Path) -> io::Result<Sharing> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let magic = crate::io::sys::fs_type(&file)?;
    let Some(basis) = basis(magic) else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{}: file system {magic:#x} does not report extent sharing \
                 (OI-1003-Q165)",
                path.display()
            ),
        ));
    };
    match (crate::io::sys::extent_sharing(&file), basis) {
        (Ok(sharing), _) => Ok(Sharing {
            extents: sharing.extents,
            shared: sharing.shared,
            basis,
        }),
        // tmpfs has no extent map; it cannot reflink either.
        (Err(error), Basis::CannotReflink)
            if matches!(error.raw_os_error(), Some(libc::EOPNOTSUPP | libc::ENOTTY)) =>
        {
            Ok(Sharing {
                extents: 0,
                shared: 0,
                basis,
            })
        }
        (Err(error), _) => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn file_sharing_impl(_path: &Path) -> io::Result<Sharing> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The sharing of every regular file under `root`.
///
/// Symlinks are not followed (a symlink and every other non-regular node is
/// skipped). An unreadable directory fails the census; it is never skipped.
///
/// # Errors
/// The first failed listing, `lstat`, open or probe, as [`file_sharing`].
pub fn tree_sharing(root: &Path) -> io::Result<Census> {
    let mut census = Census::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        for path in entries {
            let kind = fs::symlink_metadata(&path)?.file_type();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let sharing = file_sharing(&path)?;
                census.files += 1;
                census.extents += sharing.extents;
                census.shared_extents += sharing.shared;
                if sharing.basis == Basis::CannotReflink {
                    census.no_reflink_files += 1;
                }
                if sharing.shared > 0 {
                    census.shared_files.push(path);
                }
            }
        }
    }
    Ok(census)
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// tmpfs cannot reflink, so a tree on it is unshared by construction:
    /// counted, not refused as "no FIEMAP" (review, 2026-10-15).
    #[test]
    fn a_tree_on_tmpfs_is_unshared_by_construction() {
        let shm = Path::new("/dev/shm");
        if !shm.is_dir() {
            return;
        }
        let root = tempfile::tempdir_in(shm).unwrap();
        fs::write(root.path().join("a"), b"bytes").unwrap();
        let census = tree_sharing(root.path()).unwrap();
        assert_eq!(census.files, 1);
        assert_eq!(census.shared_extents, 0);
        assert_eq!(census.no_reflink_files, 1);
    }

    /// `FIEMAP`'s shared flag is trusted only where the file system both
    /// reflinks and reports it; a CIFS/SMB3 or NFS mount is unsupported,
    /// never a silent 0.
    #[test]
    fn only_file_systems_that_report_sharing_are_trusted() {
        for magic in [XFS, BTRFS, BCACHEFS, OCFS2] {
            assert_eq!(basis(magic), Some(Basis::Fiemap), "{magic:#x}");
        }
        for magic in [EXT, TMPFS] {
            assert_eq!(basis(magic), Some(Basis::CannotReflink), "{magic:#x}");
        }
        // CIFS, SMB2, NFS, overlayfs, ZFS, f2fs, FUSE.
        for magic in [
            0xFF53_4D42,
            0xFE53_4D42,
            0x6969,
            0x794C_7630,
            0x2FC1_2FC1,
            0xF2F5_2010,
            0x6573_5546,
        ] {
            assert_eq!(basis(magic), None, "{magic:#x}");
        }
    }

    /// An unreadable directory fails the census instead of hiding what it
    /// holds (review, 2026-10-15).
    #[test]
    fn an_unreadable_directory_fails_the_census() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let hidden = root.path().join("hidden");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("x"), b"x").unwrap();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        let result = tree_sharing(root.path());
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            result.map_err(|error| error.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
    }
}
