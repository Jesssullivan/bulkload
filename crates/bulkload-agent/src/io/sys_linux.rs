//! Linux (and other non-Darwin Unix) system calls for the durable-write and
//! transport paths.
//!
//! Every `unsafe` block states why its preconditions hold (R-N54). Callers
//! see only safe functions over owned descriptors and C strings.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{AsRawFd as _, BorrowedFd};

/// `fsync`: the file's data and all of its metadata are durable on return.
///
/// That includes a mode set with `fchmod` after the last write, which
/// `fdatasync` would not carry, so `fdatasync` is not used as a seal.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// `fsync` on a directory descriptor, making its entries durable.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier_dir(directory: &File) -> std::io::Result<()> {
    directory.sync_all()
}

/// `fsync`: data and metadata durable, device cache flushed.
///
/// # Errors
/// Returns the flush failure.
pub fn full_flush(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// Rename `from` to `to` inside `directory` without replacing an existing `to`.
///
/// Uses `renameat2(RENAME_NOREPLACE)`; a file system without it gets
/// `linkat` followed by `unlinkat`, which is also no-clobber.
///
/// # Errors
/// Returns the rename failure; an occupied target is `EEXIST`.
pub fn rename_noreplace(directory: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let fd = directory.as_raw_fd();
        // SAFETY: `fd` is owned by `directory` for the duration of the call,
        // and both names are NUL-terminated C strings that outlive it.
        let renamed =
            unsafe { libc::renameat2(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_NOREPLACE) };
        if renamed == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(error);
        }
    }
    link_then_unlink(directory, from, to)
}

fn link_then_unlink(directory: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    let fd = directory.as_raw_fd();
    // SAFETY: `fd` is owned by `directory` for the duration of the call, both
    // names are NUL-terminated and outlive it, and flag 0 never follows links.
    if unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above; `from` is the private temporary name just linked.
    if unsafe { libc::unlinkat(fd, from.as_ptr(), 0) } != 0 {
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

/// Raise a pipe's capacity with `F_SETPIPE_SZ`. Returns `false` when `fd` is
/// not a pipe or the platform has no such control.
///
/// # Errors
/// Returns a failed `fstat` or `fcntl`.
pub fn set_pipe_buffer(fd: BorrowedFd<'_>, bytes: libc::c_int) -> std::io::Result<bool> {
    if !super::is_fifo(fd)? {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `fd` is an open pipe the caller owns for the duration of the
        // call, and F_SETPIPE_SZ takes one integer argument.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETPIPE_SZ, bytes) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = bytes;
        Ok(false)
    }
}
