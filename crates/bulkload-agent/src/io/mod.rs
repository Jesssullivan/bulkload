//! The engine's raw I/O layer (R-N54).
//!
//! All `unsafe` for durable writes and transport tuning lives in the platform
//! modules (`sys_darwin.rs`, `sys_linux.rs`); each block carries a `SAFETY`
//! comment. [`durable`] builds group commit on top of them.

pub mod durable;
pub mod limits;

#[cfg(target_vendor = "apple")]
#[path = "sys_darwin.rs"]
pub mod sys;

#[cfg(not(target_vendor = "apple"))]
#[path = "sys_linux.rs"]
pub mod sys;

use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::FileTypeExt as _;

/// Socket buffer size for transfer streams. With Darwin's 8 KiB `AF_UNIX`
/// default a socketpair moves 0.26–0.71 GB/s; at 4 MiB it moves
/// 8.6–13.3 GB/s (`docs/evidence/m0-2026-09-23.md`).
pub const SOCKET_BUFFER_BYTES: libc::c_int = 4 * 1024 * 1024;

/// Pipe capacity requested where the platform allows it (the Linux default
/// `fs.pipe-max-size` for an unprivileged process).
pub const PIPE_BUFFER_BYTES: libc::c_int = 1024 * 1024;

/// Raise the kernel buffers of one transfer stream: [`SOCKET_BUFFER_BYTES`] on
/// a socket, [`PIPE_BUFFER_BYTES`] on a Linux pipe, nothing otherwise. Returns
/// whether anything changed.
///
/// # Errors
/// Returns a failed `fstat`, `setsockopt` or `fcntl`.
pub fn tune_transport(stream: &impl AsFd) -> std::io::Result<bool> {
    let fd = stream.as_fd();
    if sys::set_socket_buffers(fd, SOCKET_BUFFER_BYTES)? {
        return Ok(true);
    }
    sys::set_pipe_buffer(fd, PIPE_BUFFER_BYTES)
}

fn file_type(fd: BorrowedFd<'_>) -> std::io::Result<std::fs::FileType> {
    Ok(std::fs::File::from(fd.try_clone_to_owned()?)
        .metadata()?
        .file_type())
}

fn is_socket(fd: BorrowedFd<'_>) -> std::io::Result<bool> {
    Ok(file_type(fd)?.is_socket())
}

#[cfg_attr(target_vendor = "apple", allow(dead_code))]
fn is_fifo(fd: BorrowedFd<'_>) -> std::io::Result<bool> {
    Ok(file_type(fd)?.is_fifo())
}

#[allow(clippy::cast_possible_truncation)]
const fn c_int_len() -> libc::socklen_t {
    // A `c_int` is 4 bytes on every supported target.
    std::mem::size_of::<libc::c_int>() as libc::socklen_t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socketpair_buffers_are_raised_and_files_are_left_alone() -> std::io::Result<()> {
        let (left, right) = std::os::unix::net::UnixStream::pair()?;
        assert!(tune_transport(&left)?);
        assert!(tune_transport(&right)?);
        let file = std::fs::File::open(std::env::temp_dir())?;
        assert!(!tune_transport(&file)?);
        Ok(())
    }
}
