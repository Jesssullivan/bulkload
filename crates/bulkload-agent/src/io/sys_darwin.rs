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
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd};

pub use super::sys_posix::*;
use super::sys_posix::{fsync_raw, to_off_t};
use super::{NodeId, Qos, Stat};

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
        size: u64::try_from(raw.st_size).unwrap_or(0),
        mtime_ns: i128::from(raw.st_mtime) * 1_000_000_000 + i128::from(raw.st_mtime_nsec),
        ctime_ns: i128::from(raw.st_ctime) * 1_000_000_000 + i128::from(raw.st_ctime_nsec),
    }
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

/// Background write-back kick for a large file (plan D1: every 16 MiB). On
/// Darwin that is a plain `fsync`, which sends the data to the drive without a
/// cache flush or a barrier. The range is advisory and ignored here.
///
/// # Errors
/// Returns the `fsync` failure.
pub fn kick(file: impl AsFd, _offset: u64, _len: u64) -> io::Result<()> {
    trace_serial!();
    let fd = file.as_fd();
    fsync_raw(fd)?;
    trace_event!(
        "fsync",
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

/// `renameatx_np(RENAME_EXCL)` between two directories: one directory
/// operation, no-clobber.
///
/// # Errors
/// Returns the rename failure; an occupied `to` is `EEXIST`.
pub fn rename_noreplace_at(
    from_dir: impl AsFd,
    from: &CStr,
    to_dir: impl AsFd,
    to: &CStr,
) -> io::Result<()> {
    trace_serial!();
    let (from_dir, to_dir) = (from_dir.as_fd(), to_dir.as_fd());
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

/// Darwin has no `O_TMPFILE`; staging always uses a named temporary.
///
/// # Errors
/// Never fails on Darwin.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches the Linux O_TMPFILE call"
)]
pub const fn open_tmpfile(_dir: BorrowedFd<'_>, _mode: u32) -> io::Result<Option<OwnedFd>> {
    Ok(None)
}

/// Unreachable on Darwin: [`open_tmpfile`] never yields an anonymous file.
///
/// # Errors
/// Always `Unsupported`.
pub fn link_tmpfile(_file: impl AsFd, _dir: impl AsFd, _name: &CStr) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Reserve `len` bytes past the file's physical end with `F_PREALLOCATE`,
/// contiguous if possible, without changing its size. Returns `false` when
/// the file system declines.
///
/// # Errors
/// Returns an unexpected `fcntl` failure.
pub fn preallocate(file: impl AsFd, len: u64) -> io::Result<bool> {
    let fd = file.as_fd();
    let length = to_off_t(len)?;
    for flags in [
        libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
        libc::F_ALLOCATEALL,
    ] {
        let mut store = libc::fstore_t {
            fst_flags: flags,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: length,
            fst_bytesalloc: 0,
        };
        // SAFETY: the descriptor is live for the call and `store` is a live,
        // exclusively borrowed `fstore_t`, the argument F_PREALLOCATE reads
        // and updates.
        let ret = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) };
        if ret != -1 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::ENOTSUP)) {
            return Err(error);
        }
    }
    Ok(false)
}

/// Read-ahead advice for `[offset, offset + len)` (`F_RDADVISE`). The count
/// is clamped to `c_int`.
///
/// # Errors
/// Returns the `fcntl` failure.
pub fn read_advise(file: impl AsFd, offset: u64, len: u64) -> io::Result<()> {
    let fd = file.as_fd();
    let mut advice = libc::radvisory {
        ra_offset: to_off_t(offset)?,
        ra_count: libc::c_int::try_from(len).unwrap_or(libc::c_int::MAX),
    };
    // SAFETY: the descriptor is live for the call and `advice` is a live
    // `radvisory`, the argument F_RDADVISE reads.
    let ret = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_RDADVISE, &raw mut advice) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

const fn qos_class(class: Qos) -> libc::qos_class_t {
    match class {
        Qos::UserInitiated => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
        Qos::Utility => libc::qos_class_t::QOS_CLASS_UTILITY,
        Qos::Background => libc::qos_class_t::QOS_CLASS_BACKGROUND,
    }
}

/// Set the calling thread's `QoS` class. Returns `true` when applied.
///
/// # Errors
/// Returns the error number `pthread_set_qos_class_self_np` reports.
pub fn set_thread_qos(class: Qos) -> io::Result<bool> {
    // SAFETY: the call acts on the calling thread only and takes no pointers.
    let ret = unsafe { libc::pthread_set_qos_class_self_np(qos_class(class), 0) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }
    Ok(true)
}

/// The calling thread's current `QoS` class, if it is one of [`Qos`].
///
/// # Errors
/// Returns the error number `pthread_get_qos_class_np` reports.
pub fn thread_qos() -> io::Result<Option<Qos>> {
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
    let class = class as u32;
    Ok([Qos::UserInitiated, Qos::Utility, Qos::Background]
        .into_iter()
        .find(|candidate| qos_class(*candidate) as u32 == class))
}
