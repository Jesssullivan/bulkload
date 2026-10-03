//! The engine's raw I/O layer (M2 W4, plan D3; R-N54, R-N58, R-N88).
//!
//! Every `unsafe` block in this module lives in one of four files, and each
//! carries a `// SAFETY:` comment (R-N54):
//!
//! - `sys_posix.rs`: the calls Darwin and Linux share (`openat`, `fstat`,
//!   `pread`/`pwrite`, `fchmod`, `unlinkat`, `mkdirat`, `linkat`,
//!   `symlinkat`/`readlinkat`, directory listing through `fdopendir` and
//!   `readdir`, `geteuid`, `flock`, `setsockopt`);
//! - `sys_darwin.rs`: `F_BARRIERFSYNC`, `F_FULLFSYNC`,
//!   `renameatx_np(RENAME_EXCL)` and thread `QoS`;
//! - `sys_linux.rs`: `fdatasync`, `fsync` and `renameat2(RENAME_NOREPLACE)`;
//! - `buf.rs`: the aligned slab allocation.
//!
//! The platform file is mounted as [`sys`]; `sys_posix` is re-exported through
//! it, so callers name one module on both platforms. There is no `mmap`
//! anywhere: a live writer truncating a source file would turn a mapped read
//! into `SIGBUS`, where `pread` returns a short count.
//!
//! The destination's materializer (`materialize.rs`), the source pack writes
//! and the counted syncs (`counters.rs`) all go through `sys`, so the R-N88
//! trace sees every destination syscall; none of them holds raw `libc`.
//!
//! # Contract for `durable.rs`
//!
//! Group commit (W3, `durable.rs`) is built on these calls. With the
//! `io-trace` feature each mutating call records its trace events (see
//! `trace`), with the sync kind the crash checker models. That is one event
//! per call, except the Linux `rename_noreplace_at` fallback, which records its
//! `linkat` and its `unlinkat`:
//!
//! | call | Darwin | Linux | trace |
//! |---|---|---|---|
//! | `sys::barrier` | `F_BARRIERFSYNC` | `fdatasync` | `Sync(Barrier)` / `Sync(DataSync)` |
//! | `sys::barrier_dir` | `F_BARRIERFSYNC` (falls back to `F_FULLFSYNC`) | `fsync` | `Sync(Barrier)` / `Sync(Fsync)` |
//! | `sys::full_flush` | `F_FULLFSYNC` | `fsync` | `Sync(FullFlush)` / `Sync(Fsync)` |
//! | `sys::rename_exclusive` | `renameatx_np(RENAME_EXCL)` | `renameat2(RENAME_NOREPLACE)`, no fallback | `Rename` |
//! | `sys::rename_noreplace_at` | as `rename_exclusive_at` | `rename_exclusive`, then `linkat` + `unlinkat` on `EINVAL`/`ENOSYS` (files only) | `Rename`, or `Link` + `Unlink` |
//! | `sys::create_excl_at`, `sys::mkdirat`, `sys::symlinkat` | | | `Create`, `Mkdir`, `Symlink` |
//! | `sys::pwrite_all`, `sys::fchmod`, `sys::unlinkat`, `sys::linkat` | | | `Write`, `SetMode`, `Unlink`, `Link` |
//!
//! The stores add an `Event::Commit` when a `SQLite` commit returns, naming
//! the records it made durable.
//!
//! `durable.rs` seals a file with `sys::barrier` on Darwin and with
//! `sys::full_flush` (`fsync`) on Linux, not `fdatasync`: a mode set with
//! `fchmod` after the last write must be durable with the data. Directories
//! are sealed with `sys::barrier_dir` in group mode and `sys::full_flush` in
//! strict mode. The source pack is sealed as a file. On Linux, `durable.rs`
//! never calls `sys::barrier`.
//!
//! Directory creation and file publication use `sys::rename_exclusive`
//! (through [`rename_exclusive`] and [`publish_noreplace`]), never the
//! sys-internal Linux `linkat` fallback of `sys::rename_noreplace_at`: `linkat`
//! on a directory fails with `EPERM` and would hide the R-N119 path taken.
//! [`publish_noreplace`] has its own io-level `linkat` + `unlinkat` fallback,
//! for files only, and counts it; a directory takes the `mkdirat` fallback
//! with an intent record first (R-N119).

// R-N54: every unsafe block names its obligations, one unsafe operation per
// block, so each SAFETY comment covers exactly one call.
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]

/// Hold the recorder's serial lock for the rest of the enclosing block when
/// the `io-trace` feature is on and a recorder is attached to this thread, so
/// traced syscalls and their events happen in one global order. Removed by
/// `cfg` with the feature off.
macro_rules! trace_serial {
    () => {
        #[cfg(feature = "io-trace")]
        let _serial = $crate::io::trace::serialize();
    };
}

/// Record a trace event when the `io-trace` feature is on.
///
/// `$call` names the syscall; `$make` is an expression of type
/// `std::io::Result<trace::Event>`. It is evaluated only while a recorder is
/// attached to the current thread, after the syscall succeeded. A failure to
/// build the event (an `fstat` for the node identity) is itself recorded as
/// `Event::Untraced`, which the crash checker refuses: nothing is dropped
/// silently. With the feature off the whole statement is removed by `cfg`, so
/// the tracing sites compile to nothing.
macro_rules! trace_event {
    ($call:literal, $make:expr) => {{
        #[cfg(feature = "io-trace")]
        $crate::io::trace::record($call, || $make);
    }};
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "unwired; wire or delete on the gate (a) evidence (#88, WP10 PR 3)"
    )
)]
pub mod buf;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "unwired; wire or delete on the gate (a) evidence (#88, WP10 PR 3)"
    )
)]
pub mod chunker;
#[cfg(any(test, feature = "io-trace"))]
pub mod crash_check;
pub mod durable;
pub mod limits;
#[cfg(any(test, feature = "io-trace"))]
pub mod trace;

mod sys_posix;

#[cfg(target_vendor = "apple")]
#[path = "sys_darwin.rs"]
pub mod sys;

#[cfg(target_os = "linux")]
#[path = "sys_linux.rs"]
pub mod sys;

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
compile_error!("bulkload's io layer supports Darwin and Linux only");

use std::ffi::{CStr, CString};
use std::os::fd::AsFd;

/// Identity of an inode as the kernel reports it (`st_dev`, `st_ino`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    pub dev: u64,
    pub ino: u64,
}

/// The `fstat` fields the engine reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub node: NodeId,
    /// Full `st_mode`, file type bits included.
    pub mode: u32,
    pub nlink: u64,
    /// Owner (`st_uid`).
    pub uid: u32,
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
}

// File type bits of `st_mode`. The values are the traditional Unix ones and
// are identical on Darwin and Linux; `mode_t` itself is `u16` on Darwin and
// `u32` on Linux, so the layer carries modes as `u32`.
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFSOCK: u32 = 0o140_000;
#[cfg_attr(
    target_vendor = "apple",
    allow(dead_code, reason = "only Linux sizes pipes")
)]
const S_IFIFO: u32 = 0o010_000;

impl Stat {
    pub const fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub const fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub const fn is_socket(&self) -> bool {
        self.mode & S_IFMT == S_IFSOCK
    }

    #[cfg_attr(
        target_vendor = "apple",
        allow(dead_code, reason = "only Linux sizes pipes")
    )]
    pub const fn is_fifo(&self) -> bool {
        self.mode & S_IFMT == S_IFIFO
    }

    /// Permission bits only: the effective mode the R-N88 trace records.
    #[cfg(any(test, feature = "io-trace"))]
    pub const fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// How the last component of a confined path is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the source reads with `Read` (W4 PR 3); tests use the others"
    )
)]
pub enum OpenMode {
    /// Read only, `O_NONBLOCK` so a FIFO swapped in cannot block the reader.
    Read,
    /// A directory, for use as the base of further `*at` calls.
    Directory,
    /// Create a new regular file, `O_RDWR | O_CREAT | O_EXCL`, with these
    /// permission bits. An existing name is `EEXIST`, never reused.
    CreateExcl(u32),
}

/// A thread quality-of-service class (Darwin `QoS`; a no-op on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "wired in by W4 PR 2/3 (R-N127); tests use it now")
)]
pub enum Qos {
    /// `QOS_CLASS_USER_INITIATED`: the plan's class for the largest file, so
    /// it lands on a P-core.
    UserInitiated,
    /// `QOS_CLASS_UTILITY`.
    Utility,
    /// `QOS_CLASS_BACKGROUND`: E-cores only.
    Background,
}

/// Convert a Rust path component to a C string for an `*at` call.
///
/// # Errors
/// `InvalidInput` when the bytes contain a NUL.
pub fn c_name(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

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

/// Test hook: while `on`, this thread's exclusive renames report `EINVAL` at
/// the syscall, as on a file system without them (see `sys_posix`).
#[cfg(any(test, feature = "fault-injection"))]
pub use sys_posix::force_rename_unsupported;

/// An exclusive (no-replace) rename with no fallback of any kind: the bare
/// `renameatx_np(RENAME_EXCL)` / `renameat2(RENAME_NOREPLACE)`. A file system
/// without it reports an error [`rename_unsupported`] recognizes, so a
/// directory can take the `mkdirat` fallback and a file the link fallback
/// (R-N119). `sys::rename_noreplace_at`, which falls back to `linkat` inside
/// `sys` on Linux, is for files only: `linkat` on a directory is `EPERM`.
///
/// # Errors
/// Returns the rename failure; see [`rename_unsupported`].
pub fn rename_exclusive(directory: &std::fs::File, from: &CStr, to: &CStr) -> std::io::Result<()> {
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
            sys::linkat(directory, from, directory, to)?;
            sys::unlinkat(directory, from, false)?;
            crate::counters::bump(crate::counters::Counter::PublishLinkFallback);
            Ok(Published::Linked)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod transport_tests {
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
