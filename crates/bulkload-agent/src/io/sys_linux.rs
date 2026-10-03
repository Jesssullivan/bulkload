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
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd};

use super::sys_posix::fsync_raw;
pub use super::sys_posix::*;
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
        uid: raw.st_uid,
        size: u64::try_from(raw.st_size).unwrap_or(0),
        mtime_ns: i128::from(raw.st_mtime) * 1_000_000_000 + i128::from(raw.st_mtime_nsec),
        ctime_ns: i128::from(raw.st_ctime) * 1_000_000_000 + i128::from(raw.st_ctime_nsec),
    }
}

/// Zero this thread's `errno`, so a NULL from `readdir` tells the end of the
/// stream apart from an error.
pub(super) fn clear_errno() {
    // SAFETY: `__errno_location` takes no arguments and returns this thread's
    // errno slot.
    let slot = unsafe { libc::__errno_location() };
    // SAFETY: `slot` is this thread's errno, a valid, aligned `c_int` that
    // lives as long as the thread.
    unsafe { *slot = 0 };
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
    trace_serial!();
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
    trace_serial!();
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

/// [`rename_exclusive_at`], then on `EINVAL`/`ENOSYS` `linkat` and `unlinkat`
/// (no-clobber, two directory operations). For files only: `linkat` on a
/// directory is `EPERM`.
///
/// # Errors
/// Returns the rename, link or unlink failure; an occupied `to` is `EEXIST`.
pub fn rename_noreplace_at(
    from_dir: impl AsFd,
    from: &CStr,
    to_dir: impl AsFd,
    to: &CStr,
) -> io::Result<()> {
    let (from_dir, to_dir) = (from_dir.as_fd(), to_dir.as_fd());
    match rename_exclusive_at(from_dir, from, to_dir, to) {
        Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) => {
            linkat(from_dir, from, to_dir, to)?;
            unlinkat(from_dir, from, false)
        }
        other => other,
    }
}

/// [`rename_exclusive_at`] within one directory.
///
/// # Errors
/// Returns the rename failure.
pub fn rename_exclusive(directory: impl AsFd, from: &CStr, to: &CStr) -> io::Result<()> {
    let directory = directory.as_fd();
    rename_exclusive_at(directory, from, directory, to)
}

/// The bare `renameat2(RENAME_NOREPLACE)`, with no fallback: a kernel or file
/// system without it reports `EINVAL` or `ENOSYS`.
///
/// # Errors
/// Returns the rename failure; an occupied `to` is `EEXIST`.
pub fn rename_exclusive_at(
    from_dir: impl AsFd,
    from: &CStr,
    to_dir: impl AsFd,
    to: &CStr,
) -> io::Result<()> {
    trace_serial!();
    let (from_dir, to_dir) = (from_dir.as_fd(), to_dir.as_fd());
    #[cfg(any(test, feature = "fault-injection"))]
    if super::sys_posix::rename_forced_unsupported() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
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
    if renamed != 0 {
        return Err(io::Error::last_os_error());
    }
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
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "wired in by W4 PR 2/3 (R-N127); tests use it now")
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
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "wired in by W4 PR 2/3 (R-N127); tests use it now")
)]
pub const fn thread_qos() -> io::Result<Option<Qos>> {
    Ok(None)
}

/// Raise a pipe's capacity with `F_SETPIPE_SZ`. Returns `false` without
/// changing anything when `fd` is not a pipe.
///
/// # Errors
/// Returns a failed `fstat` or `fcntl`.
pub fn set_pipe_buffer(fd: BorrowedFd<'_>, bytes: libc::c_int) -> io::Result<bool> {
    if !fstat(fd)?.is_fifo() {
        return Ok(false);
    }
    // SAFETY: the descriptor is a live pipe for the call, and F_SETPIPE_SZ
    // takes one integer argument.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETPIPE_SZ, bytes) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}
