//! Linux system calls for the durable-write, read and transport paths.
//!
//! The calls Linux shares with Darwin come from `sys_posix` and are
//! re-exported here. Every `unsafe` block states why its preconditions hold
//! (R-N54). Each mutating call records one trace event under `io-trace`.
//!
//! Linux's durability ladder: `sync_file_range(WRITE)` starts write-back and
//! is neither durable nor ordered (it is never used alone; plan D3 "Linux");
//! `fdatasync` makes a file's data durable, cache flush included;
//! `fsync` also makes its metadata durable, and on a directory makes its
//! entries durable.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd};

pub use super::sys_posix::*;
use super::sys_posix::{fsync_raw, to_off_t};
use super::{NodeId, Qos, Stat};

/// Translate a Linux `struct stat`.
#[allow(
    clippy::useless_conversion,
    reason = "st_nlink is u64 on x86_64 but u32 on aarch64 Linux"
)]
pub(super) fn stat_from_raw(raw: &libc::stat) -> Stat {
    Stat {
        node: NodeId {
            dev: raw.st_dev,
            ino: raw.st_ino,
        },
        mode: raw.st_mode,
        nlink: u64::from(raw.st_nlink),
        size: u64::try_from(raw.st_size).unwrap_or(0),
        mtime_ns: i128::from(raw.st_mtime) * 1_000_000_000 + i128::from(raw.st_mtime_nsec),
        ctime_ns: i128::from(raw.st_ctime) * 1_000_000_000 + i128::from(raw.st_ctime_nsec),
    }
}

/// Permission bits as Linux's 32-bit `mode_t`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches Darwin's 16-bit mode_t conversion"
)]
pub(super) const fn to_mode_t(mode: u32) -> io::Result<libc::mode_t> {
    Ok(mode)
}

fn retry_eintr(mut call: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if call() != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `fdatasync`: the file's data, and the metadata needed to read it back, are
/// durable when this returns.
///
/// # Errors
/// Returns the flush failure.
pub fn data_sync(file: impl AsFd) -> io::Result<()> {
    let fd = file.as_fd();
    // SAFETY: the descriptor is live for every call of the closure;
    // `fdatasync` takes no pointers.
    retry_eintr(|| unsafe { libc::fdatasync(fd.as_raw_fd()) })?;
    trace_event!(
        "fdatasync",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::DataSync,
        })
    );
    Ok(())
}

/// The per-file seal of the group protocol. On Linux there is no barrier
/// without a flush, so this is [`data_sync`] (the W3 `sys::barrier` shape).
///
/// # Errors
/// Returns the flush failure.
pub fn barrier(file: impl AsFd) -> io::Result<()> {
    data_sync(file)
}

/// `fsync`: data and metadata durable; on a directory, its entries.
///
/// # Errors
/// Returns the flush failure.
pub fn full_flush(file: impl AsFd) -> io::Result<()> {
    let fd = file.as_fd();
    fsync_raw(fd)?;
    trace_event!(
        "fsync",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::Fsync,
        })
    );
    Ok(())
}

/// `fsync` on a directory descriptor, making its entries durable.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier_dir(directory: impl AsFd) -> io::Result<()> {
    full_flush(directory)
}

/// Start write-back of `[offset, offset + len)` with
/// `sync_file_range(SYNC_FILE_RANGE_WRITE)`. Not durable and not ordered; a
/// later [`data_sync`] is what makes the range durable.
///
/// # Errors
/// Returns the `sync_file_range` failure.
pub fn kick(file: impl AsFd, offset: u64, len: u64) -> io::Result<()> {
    let fd = file.as_fd();
    let (offset, len) = (to_off_t(offset)?, to_off_t(len)?);
    // SAFETY: the descriptor is live for every call of the closure;
    // `sync_file_range` takes no pointers.
    retry_eintr(|| unsafe {
        libc::sync_file_range(fd.as_raw_fd(), offset, len, libc::SYNC_FILE_RANGE_WRITE)
    })?;
    trace_event!(
        "sync_file_range",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::Kick,
        })
    );
    Ok(())
}

/// Rename `from` to `to` inside `directory`; an existing `to` is `EEXIST` and
/// is left untouched.
///
/// # Errors
/// Returns the rename failure.
pub fn rename_noreplace(directory: impl AsFd, from: &CStr, to: &CStr) -> io::Result<()> {
    let directory = directory.as_fd();
    rename_noreplace_at(directory, from, directory, to)
}

/// `renameat2(RENAME_NOREPLACE)` between two directories. A file system
/// without it (`EINVAL`, `ENOSYS`) gets `linkat` then `unlinkat`, which is
/// also no-clobber but is two directory operations.
///
/// # Errors
/// Returns the rename failure; an occupied `to` is `EEXIST`.
pub fn rename_noreplace_at(
    from_dir: impl AsFd,
    from: &CStr,
    to_dir: impl AsFd,
    to: &CStr,
) -> io::Result<()> {
    let (from_dir, to_dir) = (from_dir.as_fd(), to_dir.as_fd());
    // SAFETY: both descriptors are live for the call and both names are
    // NUL-terminated and outlive it.
    let renamed = unsafe {
        libc::renameat2(
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if renamed == 0 {
        trace_event!(
            "renameat2",
            Ok(super::trace::Event::Rename {
                node: fstatat_nofollow(to_dir, to)?.node,
                from_dir: fstat(from_dir)?.node,
                from: from.to_bytes().to_vec(),
                to_dir: fstat(to_dir)?.node,
                to: to.to_bytes().to_vec(),
            })
        );
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
        return Err(error);
    }
    linkat(from_dir, from, to_dir, to)?;
    unlinkat(from_dir, from, false)
}

/// Stage an unnamed file in `dir` with `O_TMPFILE`. Returns `None` when the
/// kernel or file system does not support it (`EOPNOTSUPP`, `EISDIR`,
/// `EINVAL`), and the caller falls back to a named temporary.
///
/// # Errors
/// Returns any other `openat` failure.
pub fn open_tmpfile(dir: BorrowedFd<'_>, mode: u32) -> io::Result<Option<OwnedFd>> {
    let here = c".";
    match openat_raw(
        dir,
        here,
        libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
        mode,
    ) {
        Ok(fd) => {
            trace_event!(
                "openat(O_TMPFILE)",
                Ok(super::trace::Event::Create {
                    dir: Some(fstat(dir)?.node),
                    name: None,
                    node: fstat(&fd)?.node,
                    mode: mode & 0o7777,
                })
            );
            Ok(Some(fd))
        }
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EOPNOTSUPP | libc::EISDIR | libc::EINVAL)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Give an `O_TMPFILE` inode the name `name` in `dir`:
/// `linkat(AT_FDCWD, "/proc/self/fd/N", dir, name, AT_SYMLINK_FOLLOW)`, which
/// needs no capability. Without `/proc` it tries `linkat(fd, "", …,
/// AT_EMPTY_PATH)`, which needs `CAP_DAC_READ_SEARCH`. An occupied `name` is
/// `EEXIST`.
///
/// # Errors
/// Returns the `linkat` failure.
pub fn link_tmpfile(file: impl AsFd, dir: impl AsFd, name: &CStr) -> io::Result<()> {
    let (fd, dir) = (file.as_fd(), dir.as_fd());
    let proc_path = super::c_name(format!("/proc/self/fd/{}", fd.as_raw_fd()).as_bytes())?;
    // SAFETY: `dir` is live for the call, both paths are NUL-terminated and
    // outlive it, and AT_FDCWD with an absolute path reads no descriptor.
    let linked = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            proc_path.as_ptr(),
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if linked != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error);
        }
        // SAFETY: both descriptors are live for the call, the empty path and
        // `name` are NUL-terminated and outlive it.
        let linked = unsafe {
            libc::linkat(
                fd.as_raw_fd(),
                c"".as_ptr(),
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        if linked != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    trace_event!(
        "linkat(/proc/self/fd)",
        Ok(super::trace::Event::Link {
            node: fstat(fd)?.node,
            dir: fstat(dir)?.node,
            name: name.to_bytes().to_vec(),
        })
    );
    Ok(())
}

/// Reserve `[0, len)` without changing the file size
/// (`fallocate(FALLOC_FL_KEEP_SIZE)`). Returns `false` when the file system
/// declines (`EOPNOTSUPP`).
///
/// # Errors
/// Returns any other `fallocate` failure.
pub fn preallocate(file: impl AsFd, len: u64) -> io::Result<bool> {
    let fd = file.as_fd();
    let len = to_off_t(len)?;
    // SAFETY: the descriptor is live for every call of the closure;
    // `fallocate` takes no pointers.
    match retry_eintr(|| unsafe {
        libc::fallocate(fd.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, len)
    }) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Read-ahead advice for `[offset, offset + len)`
/// (`posix_fadvise(POSIX_FADV_WILLNEED)`).
///
/// # Errors
/// Returns the error number `posix_fadvise` reports.
pub fn read_advise(file: impl AsFd, offset: u64, len: u64) -> io::Result<()> {
    let fd = file.as_fd();
    // SAFETY: the descriptor is live for the call; `posix_fadvise` takes no
    // pointers and returns an error number instead of setting errno.
    let ret = unsafe {
        libc::posix_fadvise(
            fd.as_raw_fd(),
            to_off_t(offset)?,
            to_off_t(len)?,
            libc::POSIX_FADV_WILLNEED,
        )
    };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }
    Ok(())
}

/// Linux has no `QoS` classes; nothing is applied.
///
/// # Errors
/// Never fails on Linux.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches Darwin's pthread_set_qos_class_self_np"
)]
pub const fn set_thread_qos(_class: Qos) -> io::Result<bool> {
    Ok(false)
}

/// Linux has no `QoS` classes.
///
/// # Errors
/// Never fails on Linux.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches Darwin's pthread_get_qos_class_np"
)]
pub const fn thread_qos() -> io::Result<Option<Qos>> {
    Ok(None)
}
