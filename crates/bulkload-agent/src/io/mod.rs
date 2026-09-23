//! The engine's raw I/O layer (M2 W4, plan D3; R-N54, R-N58, R-N88).
//!
//! Every `unsafe` block in this module lives in one of four files, and each
//! carries a `// SAFETY:` comment (R-N54):
//!
//! - `sys_posix.rs`: the calls Darwin and Linux share (`openat`, `fstat`,
//!   `pread`/`pwrite`, `fchmod`, `unlinkat`, `mkdirat`, `linkat`,
//!   `setsockopt`);
//! - `sys_darwin.rs`: `F_BARRIERFSYNC`, `F_FULLFSYNC`,
//!   `renameatx_np(RENAME_EXCL)`, `F_PREALLOCATE`, `F_RDADVISE` and thread
//!   `QoS`;
//! - `sys_linux.rs`: `fdatasync`, `sync_file_range`,
//!   `renameat2(RENAME_NOREPLACE)`, `O_TMPFILE` plus `linkat` through
//!   `/proc/self/fd`, `fallocate(KEEP_SIZE)` and `posix_fadvise`;
//! - `buf.rs`: the aligned slab allocation.
//!
//! The platform file is mounted as [`sys`]; `sys_posix` is re-exported through
//! it, so callers name one module on both platforms. There is no `mmap`
//! anywhere: a live writer truncating a source file would turn a mapped read
//! into `SIGBUS`, where `pread` returns a short count.
//!
//! # Contract for `durable.rs`
//!
//! Group commit (W3, `durable.rs`) is built on these calls. Each mutating call
//! records one trace event when the `io-trace` feature is on (see `trace`),
//! with the sync kind the crash checker models:
//!
//! | call | Darwin | Linux | trace kind |
//! |---|---|---|---|
//! | `sys::barrier` | `F_BARRIERFSYNC` | `fdatasync` | `Barrier` / `DataSync` |
//! | `sys::barrier_dir` | `F_BARRIERFSYNC` (falls back to `F_FULLFSYNC`) | `fsync` | `Barrier` / `Fsync` |
//! | `sys::full_flush` | `F_FULLFSYNC` | `fsync` | `FullFlush` / `Fsync` |
//! | `sys::kick` | `fsync` (no cache flush) | `sync_file_range(WRITE)` | `Kick` |
//! | `sys::rename_noreplace` | `renameatx_np(RENAME_EXCL)` | `renameat2(RENAME_NOREPLACE)` | `Rename` |
//! | [`TempFile::create`] + [`TempFile::publish`] | named temp + rename | `O_TMPFILE` + `linkat` (named fallback) | `Create`, `Link`/`Rename` |
//!
//! The names and argument shapes of `barrier`, `barrier_dir`, `full_flush`,
//! `rename_noreplace` and `set_socket_buffers` match the W3 lane's `sys`
//! module, so `durable.rs` moves onto this layer without edits to its call
//! sites.

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

pub mod buf;
pub mod chunker;
#[cfg(test)]
pub mod crash_check;
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

use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};

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

    /// Permission bits only.
    pub const fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// How the last component of a confined path is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub enum Qos {
    /// `QOS_CLASS_USER_INITIATED`: the plan's class for the largest file, so
    /// it lands on a P-core.
    UserInitiated,
    /// `QOS_CLASS_UTILITY`.
    Utility,
    /// `QOS_CLASS_BACKGROUND`: E-cores only.
    Background,
}

/// A staged file that is not yet visible under its final name.
///
/// On Linux it is an `O_TMPFILE` inode with no name at all, so a crash leaves
/// no orphan; a file system without `O_TMPFILE` gets a named temporary
/// instead. On Darwin it is always a named temporary (`O_EXCL`, private
/// mode). [`TempFile::publish`] never replaces an existing name.
#[derive(Debug)]
pub enum TempFile {
    /// Linux `O_TMPFILE`: published with `linkat` through `/proc/self/fd`.
    Anonymous(OwnedFd),
    /// A named temporary in the destination directory: published with
    /// rename-no-replace.
    Named { fd: OwnedFd, name: CString },
}

impl TempFile {
    /// Stage a new file in `dir` with permission bits `mode`.
    ///
    /// # Errors
    /// Returns the `openat` failure.
    pub fn create(dir: impl AsFd, mode: u32) -> std::io::Result<Self> {
        let dir = dir.as_fd();
        if let Some(fd) = sys::open_tmpfile(dir, mode)? {
            return Ok(Self::Anonymous(fd));
        }
        let (fd, name) = sys::create_temp_named(dir, mode)?;
        Ok(Self::Named { fd, name })
    }

    /// The staged file's descriptor, for `pwrite`, sync and `fchmod`.
    pub const fn fd(&self) -> &OwnedFd {
        match self {
            Self::Anonymous(fd) | Self::Named { fd, .. } => fd,
        }
    }

    /// Whether this is an unnamed `O_TMPFILE` inode.
    pub const fn is_anonymous(&self) -> bool {
        matches!(self, Self::Anonymous(_))
    }

    /// Give the staged file the name `name` in `dir`; an existing `name` is
    /// `EEXIST` and is left untouched. The caller seals the directory
    /// afterwards: the new name is durable only after a directory sync.
    ///
    /// # Errors
    /// Returns the link or rename failure; an occupied name is `EEXIST`.
    pub fn publish(&self, dir: impl AsFd, name: &std::ffi::CStr) -> std::io::Result<()> {
        match self {
            Self::Anonymous(fd) => sys::link_tmpfile(fd, dir, name),
            Self::Named { name: temp, .. } => sys::rename_noreplace(dir, temp, name),
        }
    }
}

/// Convert a Rust path component to a C string for an `*at` call.
///
/// # Errors
/// `InvalidInput` when the bytes contain a NUL.
pub fn c_name(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

#[cfg(test)]
mod tests;
