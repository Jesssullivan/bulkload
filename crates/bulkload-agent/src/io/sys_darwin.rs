//! Darwin system calls for the durable-write, read and transport paths.
//!
//! The calls Darwin shares with Linux come from `sys_posix` and are
//! re-exported here. Every `unsafe` block states why its preconditions hold
//! (R-N54). Each mutating call records one trace event under `io-trace`.
//!
//! Darwin's durability ladder (Apple, `fcntl(2)`; plan D1 "Durability"):
//! plain `fsync` pushes a file's data to the drive but neither flushes the
//! drive cache nor orders anything; `F_BARRIERFSYNC` does the same and then
//! issues a barrier to the drive; `F_FULLFSYNC` also drains the drive cache,
//! so everything already sent to the drive is on stable media when it returns.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd};

pub use super::sys_posix::*;
use super::{NodeId, Stat};

/// Translate a Darwin `struct stat`.
pub(super) fn stat_from_raw(raw: &libc::stat) -> Stat {
    Stat {
        node: NodeId {
            // Sign-extended, as std's `MetadataExt::dev` does, so identities
            // from `fstat` and from `std::fs::metadata` agree.
            dev: i64::from(raw.st_dev).cast_unsigned(),
            ino: raw.st_ino,
        },
        mode: u32::from(raw.st_mode),
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
    // SAFETY: `__error` takes no arguments and returns this thread's errno
    // slot.
    let slot = unsafe { libc::__error() };
    // SAFETY: `slot` is this thread's errno, a valid, aligned `c_int` that
    // lives as long as the thread.
    unsafe { *slot = 0 };
}

/// Permission bits as Darwin's 16-bit `mode_t`.
pub(super) fn to_mode_t(mode: u32) -> io::Result<libc::mode_t> {
    libc::mode_t::try_from(mode).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `fcntl(fd, command)` for the argument-free flush commands, `EINTR`
/// retried.
fn fcntl_flush(fd: BorrowedFd<'_>, command: libc::c_int) -> io::Result<()> {
    loop {
        // SAFETY: the descriptor is live for the call and the flush commands
        // this is called with (F_BARRIERFSYNC, F_FULLFSYNC) take no third
        // argument.
        if unsafe { libc::fcntl(fd.as_raw_fd(), command) } != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `fcntl(F_BARRIERFSYNC)`: the file's writes are sent to the drive and ordered
/// ahead of any write issued later. It does not drain the drive cache, so the
/// data is not yet durable.
///
/// # Errors
/// Returns the `fcntl` failure.
pub fn barrier(file: impl AsFd) -> io::Result<()> {
    trace_serial!();
    let fd = file.as_fd();
    fcntl_flush(fd, libc::F_BARRIERFSYNC)?;
    trace_event!(
        "F_BARRIERFSYNC",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::Barrier,
        })
    );
    Ok(())
}

/// A directory barrier. A file system that rejects `F_BARRIERFSYNC` on a
/// directory gets `F_FULLFSYNC`, which is strictly stronger.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier_dir(directory: impl AsFd) -> io::Result<()> {
    let fd = directory.as_fd();
    match barrier(fd) {
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) => {
            full_flush(fd)
        }
        other => other,
    }
}

/// `fcntl(F_FULLFSYNC)`: the file's writes, and everything else already sent
/// to the drive, are on stable media when this returns.
///
/// # Errors
/// Returns the `fcntl` failure.
pub fn full_flush(file: impl AsFd) -> io::Result<()> {
    trace_serial!();
    let fd = file.as_fd();
    fcntl_flush(fd, libc::F_FULLFSYNC)?;
    trace_event!(
        "F_FULLFSYNC",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::FullFlush,
        })
    );
    Ok(())
}

/// [`rename_exclusive_at`] within one directory.
///
/// # Errors
/// Returns the rename failure.
pub fn rename_exclusive(directory: impl AsFd, from: &CStr, to: &CStr) -> io::Result<()> {
    let directory = directory.as_fd();
    rename_exclusive_at(directory, from, directory, to)
}

/// The bare `renameatx_np(RENAME_EXCL)` between two directories: one
/// directory operation, no-clobber, no fallback. A file system without it
/// reports `ENOTSUP` or `EINVAL`.
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
        libc::renameatx_np(
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if renamed != 0 {
        return Err(io::Error::last_os_error());
    }
    trace_event!(
        "renameatx_np",
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

/// `renameatx_np(RENAME_SWAP)` within one directory: `a` and `b`, both of
/// which must exist, atomically trade the files they name (WP0(d), #187).
/// Nothing is replaced or removed. A file system without it reports
/// `ENOTSUP` or `EINVAL` ([`super::rename_unsupported`]).
///
/// # Errors
/// Returns the rename failure; a missing name is `ENOENT`.
pub fn exchange(directory: impl AsFd, a: &CStr, b: &CStr) -> io::Result<()> {
    trace_serial!();
    let directory = directory.as_fd();
    #[cfg(any(test, feature = "fault-injection"))]
    if super::sys_posix::rename_forced_unsupported() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: the descriptor is live for the call and both names are
    // NUL-terminated and outlive it.
    let exchanged = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            a.as_ptr(),
            directory.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if exchanged != 0 {
        return Err(io::Error::last_os_error());
    }
    trace_event!(
        "renameatx_np",
        Ok(super::trace::Event::Exchange {
            dir: fstat(directory)?.node,
            a: a.to_bytes().to_vec(),
            a_node: fstatat_nofollow(directory, a)?.node,
            b: b.to_bytes().to_vec(),
            b_node: fstatat_nofollow(directory, b)?.node,
        })
    );
    Ok(())
}

// <sys/resource.h>: the disk IO policy calls libc does not bind.
extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
}

/// `IOPOL_TYPE_DISK`: the disk IO policy.
const IOPOL_TYPE_DISK: libc::c_int = 0;
/// `IOPOL_SCOPE_PROCESS`: every thread of the process, inherited by children.
const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
/// `IOPOL_THROTTLE`: the process's IO yields to every unthrottled IO.
const IOPOL_THROTTLE: libc::c_int = 3;

/// Enter background priority (WP0(f), OI-1003-Q17): the process-wide
/// `IOPOL_THROTTLE` disk policy, `QOS_CLASS_BACKGROUND` on the calling
/// thread, and nice 19. Called first in `main`, before any thread exists:
/// threads inherit their creator's `QoS` class, and children (Git, ssh)
/// inherit the process IO policy and the nice value.
///
/// # Errors
/// Returns the `setiopolicy_np`, `pthread_set_qos_class_self_np` or
/// `setpriority` failure.
pub fn enter_background() -> io::Result<()> {
    // SAFETY: `setiopolicy_np` takes three integers and no pointers.
    if unsafe { setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call acts on the calling thread only and takes no pointers.
    let ret =
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }
    super::sys_posix::nice_background()
}

/// Whether the process runs at background priority: the `IOPOL_THROTTLE`
/// disk policy, the calling thread at `QOS_CLASS_BACKGROUND`, and nice 19.
///
/// # Errors
/// Returns the `getiopolicy_np`, `pthread_get_qos_class_np` or
/// `getpriority` failure.
pub fn in_background() -> io::Result<bool> {
    // SAFETY: `getiopolicy_np` takes two integers and no pointers.
    let policy = unsafe { getiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS) };
    if policy == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
    let mut priority: libc::c_int = 0;
    // SAFETY: `pthread_self` takes no arguments and only names the calling
    // thread.
    let thread = unsafe { libc::pthread_self() };
    // SAFETY: `thread` is the live calling thread, and both out pointers are
    // live, exclusively borrowed locals of the expected types.
    let ret = unsafe { libc::pthread_get_qos_class_np(thread, &raw mut class, &raw mut priority) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }
    Ok(policy == IOPOL_THROTTLE
        && class as u32 == libc::qos_class_t::QOS_CLASS_BACKGROUND as u32
        && super::sys_posix::nice()? == super::sys_posix::BACKGROUND_NICE)
}

/// Darwin pipes size themselves; there is nothing to raise.
///
/// # Errors
/// Never fails on Darwin.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches the Linux F_SETPIPE_SZ call"
)]
pub const fn set_pipe_buffer(_fd: BorrowedFd<'_>, _bytes: libc::c_int) -> io::Result<bool> {
    Ok(false)
}

/// The extended attribute that carries a published output's capture record
/// (#169), in reverse-DNS form as Darwin's own attributes are named.
pub(super) const CAPTURE_RECORD: &CStr = c"dev.tinyland.bulkload.capture";

/// `errno` for an attribute the file does not have.
pub(super) const NO_ATTRIBUTE: libc::c_int = libc::ENOATTR;

/// `fsetxattr(fd, name, value, 0, 0)`: create or replace the attribute.
pub(super) fn set_xattr_raw(fd: BorrowedFd<'_>, name: &CStr, value: &[u8]) -> io::Result<()> {
    loop {
        // SAFETY: the descriptor is live for the call, `name` is
        // NUL-terminated and outlives it, and `value` is a live slice whose
        // length is passed with its pointer; the kernel only reads it.
        // Position 0 and no options are what a regular file's attribute takes.
        let ret = unsafe {
            libc::fsetxattr(
                fd.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        if ret != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `fgetxattr(fd, name, buffer, 0, 0)`: the attribute's length, read into
/// `buffer`.
pub(super) fn get_xattr_raw(
    fd: BorrowedFd<'_>,
    name: &CStr,
    buffer: &mut [u8],
) -> io::Result<usize> {
    loop {
        // SAFETY: the descriptor is live for the call, `name` is
        // NUL-terminated and outlives it, and `buffer` is a live, exclusively
        // borrowed slice whose length is passed with its pointer, so the
        // kernel writes at most that many bytes.
        let got = unsafe {
            libc::fgetxattr(
                fd.as_raw_fd(),
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
                0,
            )
        };
        if let Ok(length) = usize::try_from(got) {
            return Ok(length);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
