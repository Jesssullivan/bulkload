//! System calls shared by Darwin and Linux.
//!
//! Callers reach these through the platform module (`io::sys`), which
//! re-exports them. Every `unsafe` block states why its preconditions hold
//! (R-N54). Each mutating call records one trace event under `io-trace`.
//!
//! Descriptors come in as `impl AsFd`, so a borrowed descriptor is live for the
//! whole call; names come in as `&CStr`, so they are NUL-terminated and outlive
//! the call. Those two facts discharge most of the `SAFETY` obligations below.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{OpenMode, Stat};

/// Map a `-1` return to the thread's `errno`.
fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn is_interrupted(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
}

fn invalid_input() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidInput)
}

/// `u64` byte offset to `off_t`, refusing offsets past `i64::MAX`.
pub(super) fn to_off_t(offset: u64) -> io::Result<libc::off_t> {
    libc::off_t::try_from(offset).map_err(|_| invalid_input())
}

/// `openat` with `EINTR` retried. `mode` is used only with `O_CREAT` or
/// `O_TMPFILE`; otherwise pass 0.
pub(super) fn openat_raw(
    dir: BorrowedFd<'_>,
    name: &CStr,
    flags: libc::c_int,
    mode: libc::c_uint,
) -> io::Result<OwnedFd> {
    loop {
        // SAFETY: `dir` is a live descriptor borrowed for the call, `name` is
        // NUL-terminated and outlives it, and the variadic mode argument is
        // passed as the `c_uint` the C ABI promotes `mode_t` to.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, mode) };
        match check(fd) {
            // SAFETY: `fd` was just returned by `openat`, is open, and nothing
            // else owns it.
            Ok(fd) => return Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            Err(error) if is_interrupted(&error) => {}
            Err(error) => return Err(error),
        }
    }
}

/// Open the operator-supplied root directory. The root path itself may pass
/// through symlinks (on Darwin `/var` and `/tmp` are symlinks); everything
/// beneath it is opened with [`openat_beneath`], which follows none.
///
/// # Errors
/// `InvalidInput` for a path with a NUL, otherwise the `open` failure.
pub fn open_root(path: &Path) -> io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid_input())?;
    loop {
        // SAFETY: `path` is NUL-terminated and outlives the call; the flags
        // carry no O_CREAT, so the two-argument form is correct.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        match check(fd) {
            // SAFETY: `fd` was just returned by `open` and nothing else owns it.
            Ok(fd) => return Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            Err(error) if is_interrupted(&error) => {}
            Err(error) => return Err(error),
        }
    }
}

/// Open `rel` beneath `root` one component at a time: every intermediate
/// component with `O_DIRECTORY | O_NOFOLLOW`, and the last with `O_NOFOLLOW`
/// plus the flags `mode` asks for, all `O_CLOEXEC`. A symlink anywhere in
/// `rel` fails the open (`ELOOP` or `ENOTDIR`), so swapping an intermediate
/// directory for a symlink cannot move the open outside `root`.
///
/// # Errors
/// `InvalidInput` for an empty path, an absolute path, a `..` component or a
/// NUL byte; otherwise the failing `openat`.
pub fn openat_beneath(root: impl AsFd, rel: &Path, mode: OpenMode) -> io::Result<OwnedFd> {
    let mut names: Vec<&OsStr> = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_input());
            }
        }
    }
    let Some((last, parents)) = names.split_last() else {
        return Err(invalid_input());
    };
    let root = root.as_fd();
    let mut held: Option<OwnedFd> = None;
    for name in parents {
        let name = super::c_name(name.as_bytes())?;
        let base = held.as_ref().map_or(root, AsFd::as_fd);
        let next = openat_raw(
            base,
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
        held = Some(next);
    }
    let base = held.as_ref().map_or(root, AsFd::as_fd);
    let name = super::c_name(last.as_bytes())?;
    match mode {
        OpenMode::Read => openat_raw(
            base,
            &name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0,
        ),
        OpenMode::Directory => openat_raw(
            base,
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        ),
        OpenMode::CreateExcl(bits) => create_excl_at(base, &name, bits),
    }
}

/// Create `name` in `dir` as a new regular file (`O_EXCL`, never follows).
pub(super) fn create_excl_at(dir: BorrowedFd<'_>, name: &CStr, mode: u32) -> io::Result<OwnedFd> {
    trace_serial!();
    let fd = openat_raw(
        dir,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        mode,
    )?;
    trace_event!(
        "create",
        Ok(super::trace::Event::Create {
            dir: Some(fstat(dir)?.node),
            name: Some(name.to_bytes().to_vec()),
            node: fstat(&fd)?.node,
            // The effective mode, after the umask.
            mode: fstat(&fd)?.permissions(),
        })
    );
    Ok(fd)
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Create a private named temporary in `dir` (`O_EXCL`), retrying on a name
/// collision. Returns the descriptor and the name.
///
/// The name is `.bulkload-<tag>-<pid>-<n>`, the materializer's file-temporary
/// grammar (`materialize::temporary_name`), carrying the destination store's
/// `tag`. The store's sweep therefore recognizes and removes a W4 temporary
/// that a crash left behind, exactly as it does its own.
///
/// # Errors
/// `InvalidInput` for a tag that is not 16 lowercase hex digits; otherwise
/// the `openat` failure, or `AlreadyExists` after 64 collisions.
pub fn create_temp_named(
    dir: impl AsFd,
    mode: u32,
    tag: &super::TempTag,
) -> io::Result<(OwnedFd, CString)> {
    if !tag
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(invalid_input());
    }
    let dir = dir.as_fd();
    for _ in 0..64 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = crate::materialize::TEMPORARY_PREFIX.to_vec();
        name.extend_from_slice(tag);
        name.extend_from_slice(format!("-{}-{sequence}", std::process::id()).as_bytes());
        let name = super::c_name(&name)?;
        match create_excl_at(dir, &name, mode) {
            Ok(fd) => return Ok((fd, name)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// `fstat`.
///
/// # Errors
/// Returns the `fstat` failure.
pub fn fstat(fd: impl AsFd) -> io::Result<Stat> {
    let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor is live for the call and `raw` points to writable
    // storage of exactly `struct stat`'s size.
    check(unsafe { libc::fstat(fd.as_fd().as_raw_fd(), raw.as_mut_ptr()) })?;
    // SAFETY: `fstat` returned 0, so the kernel filled the whole struct.
    let raw = unsafe { raw.assume_init() };
    Ok(super::sys::stat_from_raw(&raw))
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
///
/// # Errors
/// Returns the `fstatat` failure.
pub fn fstatat_nofollow(dir: impl AsFd, name: &CStr) -> io::Result<Stat> {
    let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor is live for the call, `name` is NUL-terminated
    // and outlives it, and `raw` points to writable `struct stat` storage.
    check(unsafe {
        libc::fstatat(
            dir.as_fd().as_raw_fd(),
            name.as_ptr(),
            raw.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    // SAFETY: `fstatat` returned 0, so the kernel filled the whole struct.
    let raw = unsafe { raw.assume_init() };
    Ok(super::sys::stat_from_raw(&raw))
}

/// Read into all of `buf` from `offset`, looping over short reads and
/// retrying `EINTR`. Returns the bytes read, which is less than `buf.len()`
/// only at end of file; a file truncated by a live writer yields a short
/// count, never a signal.
///
/// # Errors
/// Returns the `pread` failure, or `InvalidInput` for an offset past
/// `i64::MAX`.
pub fn pread_full(fd: impl AsFd, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let raw = fd.as_fd().as_raw_fd();
    let mut done = 0_usize;
    while let Some(rest) = buf.get_mut(done..) {
        if rest.is_empty() {
            break;
        }
        let at =
            to_off_t(offset.saturating_add(u64::try_from(done).map_err(|_| invalid_input())?))?;
        // SAFETY: `rest` is a live, exclusively borrowed slice, so its pointer
        // is valid for writes of `rest.len()` bytes; the descriptor is live.
        let read = unsafe { libc::pread(raw, rest.as_mut_ptr().cast(), rest.len(), at) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if is_interrupted(&error) {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            break;
        }
        done = done.saturating_add(usize::try_from(read).map_err(|_| invalid_input())?);
    }
    Ok(done)
}

/// Write all of `buf` at `offset`, looping over short writes and retrying
/// `EINTR`.
///
/// # Errors
/// Returns the `pwrite` failure, `WriteZero` if the kernel accepts nothing, or
/// `InvalidInput` for an offset past `i64::MAX`.
pub fn pwrite_all(fd: impl AsFd, buf: &[u8], offset: u64) -> io::Result<()> {
    trace_serial!();
    let fd = fd.as_fd();
    let mut done = 0_usize;
    let result = pwrite_loop(fd, buf, offset, &mut done);
    // Bytes the kernel accepted are traced even when a later chunk fails, so
    // the trace never under-reports what reached the page cache.
    if done > 0 {
        trace_event!(
            "pwrite",
            buf.get(..done)
                .ok_or_else(invalid_input)
                .and_then(|written| {
                    Ok(super::trace::Event::Write {
                        node: fstat(fd)?.node,
                        offset,
                        data: written.to_vec(),
                        digest: *blake3::hash(written).as_bytes(),
                    })
                })
        );
    }
    result
}

fn pwrite_loop(fd: BorrowedFd<'_>, buf: &[u8], offset: u64, done: &mut usize) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    while let Some(rest) = buf.get(*done..) {
        if rest.is_empty() {
            break;
        }
        let at =
            to_off_t(offset.saturating_add(u64::try_from(*done).map_err(|_| invalid_input())?))?;
        // SAFETY: `rest` is a live borrowed slice, so its pointer is valid for
        // reads of `rest.len()` bytes; the descriptor is live.
        let wrote = unsafe { libc::pwrite(raw, rest.as_ptr().cast(), rest.len(), at) };
        if wrote < 0 {
            let error = io::Error::last_os_error();
            if is_interrupted(&error) {
                continue;
            }
            return Err(error);
        }
        if wrote == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        *done = done.saturating_add(usize::try_from(wrote).map_err(|_| invalid_input())?);
    }
    Ok(())
}

/// `fchmod`: set the permission bits of an open file.
///
/// # Errors
/// Returns the `fchmod` failure.
pub fn fchmod(fd: impl AsFd, mode: u32) -> io::Result<()> {
    trace_serial!();
    let fd = fd.as_fd();
    let bits = super::sys::to_mode_t(mode & 0o7777)?;
    // SAFETY: the descriptor is live for the call; `fchmod` takes no pointers.
    check(unsafe { libc::fchmod(fd.as_raw_fd(), bits) })?;
    trace_event!(
        "fchmod",
        Ok(super::trace::Event::SetMode {
            node: fstat(fd)?.node,
            mode: mode & 0o7777,
        })
    );
    Ok(())
}

/// `unlinkat`; `remove_dir` selects `AT_REMOVEDIR`.
///
/// # Errors
/// Returns the `unlinkat` failure.
pub fn unlinkat(dir: impl AsFd, name: &CStr, remove_dir: bool) -> io::Result<()> {
    trace_serial!();
    let dir = dir.as_fd();
    let flags = if remove_dir { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: the descriptor is live for the call and `name` is NUL-terminated
    // and outlives it.
    check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) })?;
    trace_event!(
        "unlinkat",
        Ok(super::trace::Event::Unlink {
            dir: fstat(dir)?.node,
            name: name.to_bytes().to_vec(),
        })
    );
    Ok(())
}

/// `mkdirat`.
///
/// # Errors
/// Returns the `mkdirat` failure; an existing name is `EEXIST`.
pub fn mkdirat(dir: impl AsFd, name: &CStr, mode: u32) -> io::Result<()> {
    trace_serial!();
    let dir = dir.as_fd();
    let bits = super::sys::to_mode_t(mode & 0o7777)?;
    // SAFETY: the descriptor is live for the call and `name` is NUL-terminated
    // and outlives it.
    check(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), bits) })?;
    trace_event!(
        "mkdirat",
        Ok(super::trace::Event::Mkdir {
            dir: fstat(dir)?.node,
            name: name.to_bytes().to_vec(),
            node: fstatat_nofollow(dir, name)?.node,
            // The effective mode, after the umask.
            mode: fstatat_nofollow(dir, name)?.permissions(),
        })
    );
    Ok(())
}

/// `linkat` without following symlinks: a second name for an existing file.
/// An occupied `to` is `EEXIST`.
///
/// # Errors
/// Returns the `linkat` failure.
pub fn linkat(from_dir: impl AsFd, from: &CStr, to_dir: impl AsFd, to: &CStr) -> io::Result<()> {
    trace_serial!();
    let (from_dir, to_dir) = (from_dir.as_fd(), to_dir.as_fd());
    // SAFETY: both descriptors are live for the call, both names are
    // NUL-terminated and outlive it, and flag 0 never follows a symlink.
    check(unsafe {
        libc::linkat(
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            0,
        )
    })?;
    trace_event!(
        "linkat",
        Ok(super::trace::Event::Link {
            node: fstatat_nofollow(to_dir, to)?.node,
            dir: fstat(to_dir)?.node,
            name: to.to_bytes().to_vec(),
        })
    );
    Ok(())
}

/// Plain `fsync` with `EINTR` retried. Not traced here: the platform module
/// records it with the sync kind it has on that platform.
pub(super) fn fsync_raw(fd: BorrowedFd<'_>) -> io::Result<()> {
    loop {
        // SAFETY: the descriptor is live for the call; `fsync` takes no
        // pointers.
        match check(unsafe { libc::fsync(fd.as_raw_fd()) }) {
            Ok(_) => return Ok(()),
            Err(error) if is_interrupted(&error) => {}
            Err(error) => return Err(error),
        }
    }
}

fn int_len() -> io::Result<libc::socklen_t> {
    libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).map_err(|_| invalid_input())
}

/// Raise `SO_SNDBUF` and `SO_RCVBUF` to `bytes`. Returns `false` without
/// changing anything when `fd` is not a socket.
///
/// # Errors
/// Returns a failed `fstat` or `setsockopt`.
pub fn set_socket_buffers(fd: impl AsFd, bytes: libc::c_int) -> io::Result<bool> {
    let fd = fd.as_fd();
    if !fstat(fd)?.is_socket() {
        return Ok(false);
    }
    let len = int_len()?;
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: the descriptor is a live socket for the call; the option
        // value points at a live `c_int` and `len` is exactly its size.
        check(unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::from_ref(&bytes).cast(),
                len,
            )
        })?;
    }
    Ok(true)
}

/// Current `(SO_SNDBUF, SO_RCVBUF)` of a socket.
///
/// # Errors
/// Returns the `getsockopt` failure.
pub fn socket_buffers(fd: impl AsFd) -> io::Result<(libc::c_int, libc::c_int)> {
    let fd = fd.as_fd();
    let mut values = [0 as libc::c_int; 2];
    for (value, option) in values.iter_mut().zip([libc::SO_SNDBUF, libc::SO_RCVBUF]) {
        let mut len = int_len()?;
        // SAFETY: the descriptor is live for the call; `value` points at a
        // live, exclusively borrowed `c_int` and `len` holds its exact size,
        // which the kernel may only shrink.
        check(unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::from_mut(value).cast(),
                &raw mut len,
            )
        })?;
    }
    Ok(values.into())
}

/// Test support: a process-wide `RLIMIT_FSIZE` soft limit with `SIGXFSZ`
/// ignored, so a write past the limit is accepted up to it and then fails
/// with `EFBIG` instead of ending the process. Dropping it restores the old
/// limit and disposition. Process-wide: a test using it must run alone.
#[cfg(all(test, feature = "io-trace"))]
pub struct FileSizeLimit {
    old_limit: libc::rlimit,
    old_action: libc::sigaction,
}

#[cfg(all(test, feature = "io-trace"))]
impl FileSizeLimit {
    /// Set the soft file-size limit to `bytes` and ignore `SIGXFSZ`.
    ///
    /// # Errors
    /// Returns the `getrlimit`, `sigaction` or `setrlimit` failure.
    pub fn set(bytes: u64) -> io::Result<Self> {
        let mut old_limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `old_limit` is a live, exclusively borrowed `rlimit`.
        check(unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &raw mut old_limit) })?;
        let zeroed = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: an all-zero `sigaction` is a valid value: no handler
        // pointer, an empty flag set and a zeroed mask.
        let mut ignore = unsafe { zeroed.assume_init() };
        ignore.sa_sigaction = libc::SIG_IGN;
        // SAFETY: `ignore.sa_mask` is a live, exclusively borrowed mask.
        check(unsafe { libc::sigemptyset(&raw mut ignore.sa_mask) })?;
        let mut old_action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: both pointers are live locals of type `sigaction` for the
        // call; the kernel fills `old_action`.
        check(unsafe {
            libc::sigaction(libc::SIGXFSZ, &raw const ignore, old_action.as_mut_ptr())
        })?;
        // SAFETY: `sigaction` returned 0, so it filled `old_action`, which
        // was zero-initialized in any case.
        let old_action = unsafe { old_action.assume_init() };
        let limit = libc::rlimit {
            rlim_cur: bytes,
            rlim_max: old_limit.rlim_max,
        };
        let guard = Self {
            old_limit,
            old_action,
        };
        // SAFETY: `limit` is a live `rlimit` for the call.
        check(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit) })?;
        Ok(guard)
    }

    /// The current `RLIMIT_FSIZE` as `(soft, hard)`.
    ///
    /// # Errors
    /// Returns the `getrlimit` failure.
    pub fn current() -> io::Result<(u64, u64)> {
        let mut now = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `now` is a live, exclusively borrowed `rlimit`.
        check(unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &raw mut now) })?;
        Ok((now.rlim_cur, now.rlim_max))
    }

    /// Whether `SIGXFSZ` is currently ignored.
    ///
    /// # Errors
    /// Returns the `sigaction` failure.
    pub fn sigxfsz_ignored() -> io::Result<bool> {
        let mut now = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: a null new-action pointer only queries; `now` is a live
        // local of type `sigaction` that the kernel fills.
        check(unsafe { libc::sigaction(libc::SIGXFSZ, std::ptr::null(), now.as_mut_ptr()) })?;
        // SAFETY: `sigaction` returned 0, so it filled `now`, which was
        // zero-initialized in any case.
        let now = unsafe { now.assume_init() };
        Ok(now.sa_sigaction == libc::SIG_IGN)
    }
}

#[cfg(all(test, feature = "io-trace"))]
impl Drop for FileSizeLimit {
    fn drop(&mut self) {
        // SAFETY: `old_limit` is a live `rlimit` read by `getrlimit`.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &raw const self.old_limit) };
        // SAFETY: `old_action` is the disposition `sigaction` returned, and a
        // null old-action pointer is allowed.
        let _ = unsafe {
            libc::sigaction(
                libc::SIGXFSZ,
                &raw const self.old_action,
                std::ptr::null_mut(),
            )
        };
    }
}

/// `(soft, hard)` `RLIMIT_NOFILE`.
///
/// # Errors
/// Returns a failed `getrlimit`.
pub fn nofile_limit() -> io::Result<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live, writable `rlimit` for the duration of the
    // call, and RLIMIT_NOFILE is a valid resource.
    check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) })?;
    Ok((limit.rlim_cur, limit.rlim_max))
}

/// Set `RLIMIT_NOFILE` to `(soft, hard)`.
///
/// # Errors
/// Returns a failed `setrlimit`, e.g. `soft` above `hard`.
pub fn set_nofile_limit(soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: `limit` is a live, initialized `rlimit` for the duration of the
    // call, and RLIMIT_NOFILE is a valid resource.
    check(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) })?;
    Ok(())
}

#[cfg(any(test, feature = "fault-injection"))]
std::thread_local! {
    static RENAME_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, this thread's exclusive renames report `EINVAL`.
///
/// The error replaces the syscall's result, as on a file system without the
/// call. It sits at the syscall, below every fallback, so tests see what a
/// real unsupported file system does to each caller (PR #59 round 4, B1).
#[cfg(any(test, feature = "fault-injection"))]
pub fn force_rename_unsupported(on: bool) {
    RENAME_UNSUPPORTED.with(|forced| forced.set(on));
}

/// Whether the test hook, or `RENAME_UNSUPPORTED_ENV` in a fault-injection
/// build, forces exclusive renames to report `EINVAL`.
#[cfg(any(test, feature = "fault-injection"))]
pub(super) fn rename_forced_unsupported() -> bool {
    #[cfg(feature = "fault-injection")]
    let from_env = std::env::var_os(super::RENAME_UNSUPPORTED_ENV).is_some();
    #[cfg(not(feature = "fault-injection"))]
    let from_env = false;
    from_env || RENAME_UNSUPPORTED.with(std::cell::Cell::get)
}
