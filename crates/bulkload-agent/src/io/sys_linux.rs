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
//! entries durable; `syncfs` makes every file of the file system durable.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd};

use super::sys_posix::fsync_raw;
pub use super::sys_posix::*;
use super::{NodeId, Stat};

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

/// `syncfs` through `through`, then `fsync` of `file`: the group seal of the batched
/// protocol (S1, OI-1003-Q107), one call pair per device in place of a
/// flush per file and per directory. The engine calls it only on the file
/// systems `io::durable::syncfs_seals` allows; elsewhere `syncfs` can be
/// weaker than `fsync`.
///
/// `syncfs` writes back every file of the file system and runs its
/// `->sync_fs` (the ext4 journal commit, the xfs log force, the btrfs
/// transaction commit). That alone does not always flush the device cache
/// or report a failure:
///
/// - xfs: with an idle log `xfs_log_force` returns 0 without any I/O, so
///   data written back in place (no log write after it) can stay in the
///   drive's volatile cache. `xfs_file_fsync` then calls
///   `blkdev_issue_flush` itself (v6.12 `xfs_file.c`; not for a realtime
///   file, which the engine never batches).
/// - ext4: on a forced shutdown `ext4_sync_fs` returns 0 (v5.17, v6.12),
///   while `ext4_sync_file` returns `EIO`.
///
/// The `fsync` that follows covers both. Write-back errors are reported by
/// `syncfs` (Linux 5.8, errseq) only against the cursor of `through`, which
/// the kernel sampled when `through` was opened, so an error another
/// `syncfs` already reported is not reported again ([`wait_writeback`]
/// checks each file's own), and one raised before `through` was opened is
/// not reported at all. The caller opens `through` for the seal (S1 stack
/// review, 2026-10-15, finding 1). The file system's own `->sync_fs`
/// failure is returned directly, from 5.17 (5679897eb104, and for xfs
/// 2d86293c7075), whatever the cursor.
///
/// `through` and `file` must be on one file system. Traced as the `fsync`
/// of `file`, then one `FsSync` naming `file`, recorded only once both
/// returned: the device seal is complete at the `FsSync`.
///
/// # Errors
/// Returns the first failure.
pub fn sync_fs(through: impl AsFd, file: impl AsFd) -> io::Result<()> {
    trace_serial!();
    let through = through.as_fd();
    let fd = file.as_fd();
    // SAFETY: the descriptor is live for every call of the closure; `syncfs`
    // takes no pointers.
    retry_eintr(|| unsafe { libc::syncfs(through.as_raw_fd()) })?;
    full_flush(fd)?;
    trace_event!(
        "syncfs",
        Ok(super::trace::Event::Sync {
            node: fstat(fd)?.node,
            kind: super::trace::SyncKind::FsSync,
        })
    );
    Ok(())
}

/// `sync_file_range(SYNC_FILE_RANGE_WRITE)` over the whole file: start
/// write-back of its dirty pages now, so a later [`sync_fs`] finds less to
/// write. Neither durable nor ordered; traced as a kick.
///
/// # Errors
/// Returns the failed call.
pub fn start_writeback(file: impl AsFd) -> io::Result<()> {
    trace_serial!();
    let fd = file.as_fd();
    // SAFETY: the descriptor is live for every call of the closure;
    // `sync_file_range` takes no pointers.
    retry_eintr(|| unsafe {
        libc::sync_file_range(fd.as_raw_fd(), 0, 0, libc::SYNC_FILE_RANGE_WRITE)
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

/// `sync_file_range(WAIT_BEFORE | WRITE | WAIT_AFTER)` over the whole file:
/// write any dirty page and wait for its write-back, then return the file's
/// own write-back error, checked against this descriptor's errseq cursor
/// (`file_fdatawait_range` returns `file_check_and_advance_wb_err`; errseq
/// since Linux 4.14). It flushes no device cache. Traced as a kick: it makes
/// nothing durable.
///
/// # Errors
/// Returns the file's write-back error.
pub fn wait_writeback(file: impl AsFd) -> io::Result<()> {
    trace_serial!();
    let fd = file.as_fd();
    // SAFETY: the descriptor is live for every call of the closure;
    // `sync_file_range` takes no pointers.
    retry_eintr(|| unsafe {
        libc::sync_file_range(
            fd.as_raw_fd(),
            0,
            0,
            libc::SYNC_FILE_RANGE_WAIT_BEFORE
                | libc::SYNC_FILE_RANGE_WRITE
                | libc::SYNC_FILE_RANGE_WAIT_AFTER,
        )
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

/// `fstatfs`: the type (`f_type`, a file system magic) of the file system
/// holding `file`, as its low 32 bits (every magic fits in them).
///
/// # Errors
/// Returns the failed call.
#[allow(
    clippy::useless_conversion,
    reason = "f_type is i64 on x86_64 and aarch64 but u32 on s390x Linux"
)]
pub fn fs_type(file: impl AsFd) -> io::Result<u32> {
    let fd = file.as_fd();
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is live; `stats` is a writable `statfs` that
    // outlives the call.
    retry_eintr(|| unsafe { libc::fstatfs(fd.as_raw_fd(), stats.as_mut_ptr()) })?;
    // SAFETY: a successful fstatfs initialized the whole structure.
    let stats = unsafe { stats.assume_init() };
    u32::try_from(i64::from(stats.f_type) & 0xFFFF_FFFF)
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

/// `struct fsxattr` of `<linux/fs.h>`, the argument of `FS_IOC_FSGETXATTR`.
#[repr(C)]
#[allow(
    clippy::struct_field_names,
    reason = "the field names of the kernel's struct fsxattr"
)]
struct FsXattr {
    fsx_xflags: u32,
    fsx_extsize: u32,
    fsx_nextents: u32,
    fsx_projid: u32,
    fsx_cowextsize: u32,
    fsx_pad: [u8; 8],
}

/// `FS_IOC_FSGETXATTR`, `_IOR('X', 31, struct fsxattr)`.
const FS_IOC_FSGETXATTR: libc::Ioctl = libc::_IOR::<FsXattr>(b'X' as u32, 31);

/// `FS_IOC_FSGETXATTR`: the inode flags (`fsx_xflags`, such as
/// `FS_XFLAG_REALTIME`) of `file`.
///
/// # Errors
/// Returns the failed call (`ENOTTY` where the file system has no such
/// flags).
pub fn fs_xflags(file: impl AsFd) -> io::Result<u32> {
    let fd = file.as_fd();
    let mut attributes = FsXattr {
        fsx_xflags: 0,
        fsx_extsize: 0,
        fsx_nextents: 0,
        fsx_projid: 0,
        fsx_cowextsize: 0,
        fsx_pad: [0; 8],
    };
    // SAFETY: the descriptor is live; `attributes` is a writable
    // `struct fsxattr` that outlives the call.
    retry_eintr(|| unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            FS_IOC_FSGETXATTR,
            std::ptr::addr_of_mut!(attributes),
        )
    })?;
    Ok(attributes.fsx_xflags)
}

/// `struct fiemap` of `<linux/fiemap.h>` without its trailing extent array:
/// the size `FS_IOC_FIEMAP` encodes.
#[repr(C)]
#[allow(
    clippy::struct_field_names,
    reason = "the field names of the kernel's struct fiemap"
)]
struct FiemapHeader {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
}

/// `struct fiemap_extent` of `<linux/fiemap.h>`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "the field names of the kernel's struct fiemap_extent"
)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

/// How many extents one `FS_IOC_FIEMAP` call asks for, as the array length
/// and as `fm_extent_count` (the same number).
const FIEMAP_BATCH: usize = 64;
const FIEMAP_BATCH_COUNT: u32 = 64;

/// One `FS_IOC_FIEMAP` argument: the header, then room for
/// [`FIEMAP_BATCH`] extents, as the kernel lays it out.
#[repr(C)]
struct FiemapRequest {
    header: FiemapHeader,
    extents: [FiemapExtent; FIEMAP_BATCH],
}

/// `FS_IOC_FIEMAP`, `_IOWR('f', 11, struct fiemap)`.
const FS_IOC_FIEMAP: libc::Ioctl = libc::_IOWR::<FiemapHeader>(b'f' as u32, 11);
/// `FIEMAP_EXTENT_LAST`: the file's last extent.
const FIEMAP_EXTENT_LAST: u32 = 0x1;
/// `FIEMAP_EXTENT_SHARED`: the extent's blocks also belong to another file.
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;

/// A file's mapped extents, and how many of them share their blocks with
/// another file (a reflink or clone, OI-1003-Q165).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtentSharing {
    /// Mapped extents, delayed allocations included.
    pub extents: u64,
    /// Extents flagged `FIEMAP_EXTENT_SHARED`.
    pub shared: u64,
}

/// `FS_IOC_FIEMAP` over the whole of `file`: how many extents it maps and how
/// many of them are shared with another file (OI-1003-Q165). Read-only: no
/// `FIEMAP_FLAG_SYNC`, so the probe never starts write-back of the file it
/// looks at, and it moves no timestamp.
///
/// # Errors
/// Returns the failed call (`EOPNOTSUPP` where the file system has no
/// extent map, such as tmpfs).
pub fn extent_sharing(file: impl AsFd) -> io::Result<ExtentSharing> {
    let fd = file.as_fd();
    let mut sharing = ExtentSharing::default();
    let mut start = 0_u64;
    loop {
        let mut request = FiemapRequest {
            header: FiemapHeader {
                fm_start: start,
                fm_length: u64::MAX,
                fm_flags: 0,
                fm_mapped_extents: 0,
                fm_extent_count: FIEMAP_BATCH_COUNT,
                fm_reserved: 0,
            },
            extents: [FiemapExtent {
                fe_logical: 0,
                fe_physical: 0,
                fe_length: 0,
                fe_reserved64: [0; 2],
                fe_flags: 0,
                fe_reserved: [0; 3],
            }; FIEMAP_BATCH],
        };
        // SAFETY: the descriptor is live; `request` is a writable `struct
        // fiemap` followed by room for `fm_extent_count` extents, and it
        // outlives the call.
        retry_eintr(|| unsafe {
            libc::ioctl(
                fd.as_raw_fd(),
                FS_IOC_FIEMAP,
                std::ptr::addr_of_mut!(request),
            )
        })?;
        let mapped = usize::try_from(request.header.fm_mapped_extents).unwrap_or(FIEMAP_BATCH);
        let mut last = mapped == 0;
        let mut next = start;
        for extent in request.extents.iter().take(mapped) {
            sharing.extents += 1;
            if extent.fe_flags & FIEMAP_EXTENT_SHARED != 0 {
                sharing.shared += 1;
            }
            last |= extent.fe_flags & FIEMAP_EXTENT_LAST != 0;
            next = extent.fe_logical.saturating_add(extent.fe_length);
        }
        // A batch that ends short of the last extent continues after it; a
        // map that does not advance ends, so the walk always terminates.
        if last || mapped < FIEMAP_BATCH || next <= start {
            return Ok(sharing);
        }
        start = next;
    }
}

/// The running kernel's release string (`uname -r`), if it can be read.
#[must_use]
pub fn kernel_release() -> Option<String> {
    // SAFETY: `utsname` is plain old data; all-zero bytes are a valid value.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a writable `utsname` that outlives the call.
    if unsafe { libc::uname(&raw mut name) } != 0 {
        return None;
    }
    // SAFETY: on success `release` holds a NUL-terminated string within the
    // array, which lives as long as `name`.
    let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
    release.to_str().ok().map(str::to_owned)
}

/// `fsync` on a directory descriptor, making its entries durable.
///
/// # Errors
/// Returns the flush failure.
pub fn barrier_dir(directory: impl AsFd) -> io::Result<()> {
    full_flush(directory)
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

/// `renameat2(RENAME_EXCHANGE)` between two directories: `a` in `a_dir` and `b` in
/// `b_dir`, both of which must exist, atomically trade the files they name
/// (`estate-apply --side-root`, M2r2 item e: a live symlink and the
/// side-root directory it names). Nothing is replaced or removed. Not
/// traced: no transfer publish uses it. A file system without it reports
/// `EINVAL`, `ENOTSUP` or `ENOSYS` ([`super::rename_unsupported`]); two
/// file systems, `EXDEV`.
///
/// # Errors
/// Returns the rename failure; a missing name is `ENOENT`.
pub fn exchange_at(a_dir: impl AsFd, a: &CStr, b_dir: impl AsFd, b: &CStr) -> io::Result<()> {
    trace_serial!();
    let (a_dir, b_dir) = (a_dir.as_fd(), b_dir.as_fd());
    #[cfg(any(test, feature = "fault-injection"))]
    if super::sys_posix::rename_forced_unsupported() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: both descriptors are live for the call and both names are
    // NUL-terminated and outlive it.
    let exchanged = unsafe {
        libc::renameat2(
            a_dir.as_raw_fd(),
            a.as_ptr(),
            b_dir.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if exchanged != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `renameat2(RENAME_EXCHANGE)` within one directory: `a` and `b`, both of
/// which must exist, atomically trade the files they name (WP0(d), #187).
/// Nothing is replaced or removed. A kernel or file system without it
/// reports `EINVAL` or `ENOSYS` ([`super::rename_unsupported`]).
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
        libc::renameat2(
            directory.as_raw_fd(),
            a.as_ptr(),
            directory.as_raw_fd(),
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if exchanged != 0 {
        return Err(io::Error::last_os_error());
    }
    trace_event!(
        "renameat2",
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

/// `ioprio_set`/`ioprio_get` `which`: one thread (or process) by id, 0 the
/// caller (`linux/ioprio.h`).
const IOPRIO_WHO_PROCESS: libc::c_int = 1;
/// The idle IO class: served only when no other class has IO pending.
const IOPRIO_CLASS_IDLE: libc::c_int = 3;
/// `IOPRIO_PRIO_VALUE(class, data)`: the class sits above 13 data bits.
const IOPRIO_CLASS_SHIFT: libc::c_int = 13;

/// Enter background priority (WP0(f), OI-1003-Q17): nice 19 and the idle IO
/// class. Called first in `main`, before any thread exists, so every thread
/// and every child (Git, ssh) inherits both: Linux keeps nice and the IO
/// priority per thread and copies them on `clone`.
///
/// # Errors
/// Returns the `setpriority` or `ioprio_set` failure; neither needs
/// privilege when lowering the caller's own priority.
pub fn enter_background() -> io::Result<()> {
    super::sys_posix::nice_background()?;
    // SAFETY: `ioprio_set` takes three integers and no pointers; `who` 0
    // names the calling thread.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            0,
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        )
    };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `IOPRIO_CLASS_NONE`: no class of its own, so the thread's IO priority is
/// derived from its nice value (best effort, level 7 at nice 19): what a
/// process that never called `ioprio_set` has.
const IOPRIO_CLASS_NONE: libc::c_int = 0;

/// A process directory under `/proc`: this process's own through
/// `/proc/self` (right in any PID namespace), a descendant's by PID.
fn proc_dir(pid: Option<u32>) -> String {
    pid.map_or_else(|| "/proc/self".to_owned(), |pid| format!("/proc/{pid}"))
}

/// The thread IDs of a process, or `None` when it is gone.
fn tasks(pid: Option<u32>) -> Option<Vec<libc::c_int>> {
    Some(
        std::fs::read_dir(format!("{}/task", proc_dir(pid)))
            .ok()?
            .filter_map(|task| {
                task.ok()?
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<libc::c_int>().ok())
            })
            .collect(),
    )
}

/// The children of thread `tid` of process `pid` whose parent is still
/// `parent` (the process's own PID as its children see it): a PID reused
/// since the `children` file was read names another parent and is left
/// out, so the walk never leaves this process's descendants.
fn children(pid: Option<u32>, tid: libc::c_int, parent: u32) -> Vec<u32> {
    let Ok(listed) = std::fs::read_to_string(format!("{}/task/{tid}/children", proc_dir(pid)))
    else {
        return Vec::new();
    };
    listed
        .split_ascii_whitespace()
        .filter_map(|child| child.parse::<u32>().ok())
        .filter(|child| {
            std::fs::read_to_string(format!("/proc/{child}/stat"))
                .ok()
                .and_then(|stat| {
                    // `pid (comm) state ppid ...`; comm may hold anything.
                    let after = stat.get(stat.rfind(')')? + 1..)?;
                    after.split_ascii_whitespace().nth(1)?.parse::<u32>().ok()
                })
                == Some(parent)
        })
        .collect()
}

/// Bounded starvation (M2 item 3): give every thread of this process, and
/// every thread of each live descendant (its Git children and theirs), the
/// IO priority its nice value implies (`IOPRIO_CLASS_NONE`: best effort,
/// level 7 at nice 19) in place of the idle class. Nice stays 19: an
/// unprivileged process cannot raise it back, and the idle class, not the
/// CPU share, is what starves a reader behind a busy disk. Touches only
/// this process's own descendants: found through
/// `/proc/<pid>/task/<tid>/children`, each checked to name its parent; it
/// sends no signal (R-N11). A descendant that ended meanwhile is skipped; a
/// thread that cannot be moved is counted and the walk goes on.
///
/// # Errors
/// Returns a failure to list this process's own threads. Otherwise returns
/// the number of threads moved and the number that could not be.
pub fn escalate_io() -> io::Result<(u64, u64)> {
    let own = std::process::id();
    let (mut moved, mut failed) = (0, 0);
    let mut pending: Vec<(Option<u32>, u32)> = vec![(None, own)];
    let mut seen = std::collections::BTreeSet::new();
    while let Some((pid, as_parent)) = pending.pop() {
        if !seen.insert(as_parent) {
            continue;
        }
        let Some(threads) = tasks(pid) else {
            if pid.is_none() {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            continue;
        };
        for tid in threads {
            // SAFETY: `ioprio_set` takes three integers and no pointers;
            // `tid` names one thread of this process or of a descendant.
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_ioprio_set,
                    IOPRIO_WHO_PROCESS,
                    tid,
                    IOPRIO_CLASS_NONE << IOPRIO_CLASS_SHIFT,
                )
            };
            if ret == -1 {
                if io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    failed += 1;
                }
                continue;
            }
            moved += 1;
            // Read after the move, so a child forked since inherits it.
            for child in children(pid, tid, as_parent) {
                pending.push((Some(child), child));
            }
        }
    }
    Ok((moved, failed))
}

/// The bytes this process's live descendants have read and written so far
/// (`rchar` plus `wchar` of each `/proc/<pid>/io`, summed, wrapping): the
/// starvation watcher's measure of a child's progress, so a long but
/// healthy Git child is not taken for a starved one. 0 with none.
#[must_use]
pub fn descendant_io() -> u64 {
    let own = std::process::id();
    let mut total = 0_u64;
    let mut pending: Vec<(Option<u32>, u32)> = vec![(None, own)];
    let mut seen = std::collections::BTreeSet::new();
    while let Some((pid, as_parent)) = pending.pop() {
        if !seen.insert(as_parent) {
            continue;
        }
        if let Some(pid) = pid {
            if let Ok(io) = std::fs::read_to_string(format!("/proc/{pid}/io")) {
                for line in io.lines() {
                    if let Some(value) = line
                        .strip_prefix("rchar: ")
                        .or_else(|| line.strip_prefix("wchar: "))
                    {
                        total = total.wrapping_add(value.trim().parse::<u64>().unwrap_or(0));
                    }
                }
            }
        }
        for tid in tasks(pid).unwrap_or_default() {
            for child in children(pid, tid, as_parent) {
                pending.push((Some(child), child));
            }
        }
    }
    total
}

/// Whether the calling thread runs at background priority: nice 19 and the
/// idle IO class, exactly as [`enter_background`] leaves it.
///
/// # Errors
/// Returns the `getpriority` or `ioprio_get` failure.
pub fn in_background() -> io::Result<bool> {
    let nice = super::sys_posix::nice()?;
    // SAFETY: `ioprio_get` takes two integers and no pointers; `who` 0 names
    // the calling thread.
    let ioprio = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0) };
    if ioprio == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(nice == super::sys_posix::BACKGROUND_NICE
        && ioprio >> IOPRIO_CLASS_SHIFT == libc::c_long::from(IOPRIO_CLASS_IDLE))
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

/// The extended attribute that carries a published output's capture record
/// (#169): a `user.` attribute, which needs no privilege.
pub(super) const CAPTURE_RECORD: &CStr = c"user.bulkload.capture";

/// `errno` for an attribute the file does not have.
pub(super) const NO_ATTRIBUTE: libc::c_int = libc::ENODATA;

/// `fsetxattr(fd, name, value, 0)`: create or replace the attribute.
pub(super) fn set_xattr_raw(fd: BorrowedFd<'_>, name: &CStr, value: &[u8]) -> io::Result<()> {
    // SAFETY: the descriptor is live for every call of the closure, `name` is
    // NUL-terminated and outlives it, and `value` is a live slice whose
    // length is passed with its pointer; the kernel only reads it.
    retry_eintr(|| unsafe {
        libc::fsetxattr(
            fd.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    })
}

/// `fgetxattr(fd, name, buffer)`: the attribute's length, read into
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

