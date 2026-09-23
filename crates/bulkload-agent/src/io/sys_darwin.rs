//! Darwin system calls for the durable-write and transport paths.
//!
//! Every `unsafe` block states why its preconditions hold (R-N54). Callers
//! see only safe functions over owned descriptors and C strings.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{AsRawFd as _, BorrowedFd};

/// `fcntl(F_BARRIERFSYNC)`: the file's writes reach the device ahead of any
/// write issued later, without draining the device cache.
///
/// # Errors
/// Returns the `fcntl` failure.
pub fn barrier(file: &File) -> std::io::Result<()> {
    // SAFETY: the descriptor is owned by `file`, which outlives the call, and
    // F_BARRIERFSYNC takes no argument beyond the command.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A directory barrier.
///
/// `F_BARRIERFSYNC` applies to directory descriptors on APFS; a file system
/// that rejects it gets `F_FULLFSYNC` instead, which is strictly stronger.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier_dir(directory: &File) -> std::io::Result<()> {
    match barrier(directory) {
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) => {
            full_flush(directory)
        }
        other => other,
    }
}

/// `fcntl(F_FULLFSYNC)`: the file's writes are on stable media when this
/// returns, and the device cache has been drained.
///
/// # Errors
/// Returns the `fcntl` failure.
pub fn full_flush(file: &File) -> std::io::Result<()> {
    // SAFETY: the descriptor is owned by `file`, which outlives the call, and
    // F_FULLFSYNC takes no argument beyond the command.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Rename `from` to `to` inside `directory`, failing with `EEXIST` rather than
/// replacing an existing `to` (`renameatx_np(RENAME_EXCL)`).
///
/// # Errors
/// Returns the rename failure; an occupied target is `EEXIST`.
pub fn rename_noreplace(directory: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    let fd = directory.as_raw_fd();
    // SAFETY: `fd` is owned by `directory` for the duration of the call, and
    // both names are NUL-terminated C strings that outlive it.
    let renamed =
        unsafe { libc::renameatx_np(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_EXCL) };
    if renamed != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Raise `SO_SNDBUF` and `SO_RCVBUF` on a socket descriptor. Returns `false`
/// without changing anything when `fd` is not a socket.
///
/// # Errors
/// Returns a failed `fstat` or `setsockopt`.
pub fn set_socket_buffers(fd: BorrowedFd<'_>, bytes: libc::c_int) -> std::io::Result<bool> {
    if !super::is_socket(fd)? {
        return Ok(false);
    }
    let fd = fd.as_raw_fd();
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: `fd` is an open socket the caller owns for the duration of
        // the call; the option value is a live `c_int` and its exact size is
        // passed as the option length.
        let set = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                std::ptr::from_ref(&bytes).cast(),
                super::c_int_len(),
            )
        };
        if set != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(true)
}

/// Darwin pipes size themselves; there is nothing to raise.
///
/// # Errors
/// Never fails on Darwin.
pub const fn set_pipe_buffer(_fd: BorrowedFd<'_>, _bytes: libc::c_int) -> std::io::Result<bool> {
    Ok(false)
}
