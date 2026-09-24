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

use std::ffi::CStr;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::FileTypeExt as _;

/// Socket buffer size for transfer streams. With Darwin's 8 KiB `AF_UNIX`
/// default a socketpair moves 0.26–0.71 GB/s; at 4 MiB it moves
/// 8.6–13.3 GB/s (`docs/evidence/m0-2026-09-23.md`).
pub const SOCKET_BUFFER_BYTES: libc::c_int = 4 * 1024 * 1024;

/// Pipe capacity requested where the platform allows it (the Linux default
/// `fs.pipe-max-size` for an unprivileged process).
pub const PIPE_BUFFER_BYTES: libc::c_int = 1024 * 1024;

/// Raise the kernel buffers of one transfer stream; return whether any changed.
///
/// A socket gets [`SOCKET_BUFFER_BYTES`], a Linux pipe [`PIPE_BUFFER_BYTES`],
/// anything else nothing.
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

/// How a no-replace publish was carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Published {
    /// One atomic exclusive rename.
    Renamed,
    /// `linkat` then `unlinkat`, because this file system offers no
    /// exclusive rename (R-N119).
    Linked,
}

/// Environment variable that makes a fault-harness child's exclusive renames
/// report `EINVAL`, forcing the no-replace fallback (R-N119).
#[cfg(feature = "fault-injection")]
pub const RENAME_UNSUPPORTED_ENV: &str = "BULKLOAD_FAULT_RENAME_UNSUPPORTED";

#[cfg(any(test, feature = "fault-injection"))]
std::thread_local! {
    static RENAME_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, this thread's exclusive renames report `EINVAL`,
/// as on a file system without them.
#[cfg(any(test, feature = "fault-injection"))]
pub fn force_rename_unsupported(on: bool) {
    RENAME_UNSUPPORTED.with(|forced| forced.set(on));
}

/// An exclusive (no-replace) rename behind the test hook, with no fallback.
///
/// # Errors
/// Returns the rename failure; see [`rename_unsupported`].
pub fn rename_exclusive(directory: &std::fs::File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        #[cfg(feature = "fault-injection")]
        let from_env = std::env::var_os(RENAME_UNSUPPORTED_ENV).is_some();
        #[cfg(not(feature = "fault-injection"))]
        let from_env = false;
        if from_env || RENAME_UNSUPPORTED.with(std::cell::Cell::get) {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
    }
    sys::rename_exclusive(directory, from, to)
}

/// Whether an exclusive rename failed because the file system or kernel does
/// not offer it, rather than because of the names involved.
#[must_use]
pub fn rename_unsupported(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
        || error.raw_os_error().is_some_and(|code| {
            [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&code)
        })
}

/// Publish `from` as `to` inside `directory` without replacing an existing `to`.
///
/// An exclusive rename, or where the file system offers none, `linkat` then
/// `unlinkat` (R-N119). The path taken is returned and counted
/// (`publish_link_fallback`).
///
/// # Errors
/// Returns the failure; an occupied target is `EEXIST` either way.
pub fn publish_noreplace(
    directory: &std::fs::File,
    from: &CStr,
    to: &CStr,
) -> std::io::Result<Published> {
    match rename_exclusive(directory, from, to) {
        Ok(()) => Ok(Published::Renamed),
        Err(error) if rename_unsupported(&error) => {
            sys::link_then_unlink(directory, from, to)?;
            crate::counters::bump(crate::counters::Counter::PublishLinkFallback);
            Ok(Published::Linked)
        }
        Err(error) => Err(error),
    }
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
