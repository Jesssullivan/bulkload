//! Private storage for a child's raw stderr (R-N121).
//!
//! A receipt never carries stderr; with `--state-dir DIR` the raw bytes go to
//! `DIR/stderr/<keyed-blake3>.log`, and nowhere else. Every step works on
//! file descriptors, never on re-resolved paths:
//! - `DIR`'s parent is canonicalized once, then walked from `/` one component
//!   at a time with `O_DIRECTORY|O_NOFOLLOW`; `DIR` itself and `stderr/` are
//!   opened the same way, so a symlink anywhere after canonicalization, or at
//!   `DIR` itself, refuses.
//! - `DIR` and `stderr/` must be directories owned by the effective uid, with
//!   no group or other permission bits and no extended ACL.
//! - Each capture streams into a temporary created with
//!   `O_CREAT|O_EXCL|O_NOFOLLOW` and set to 0600 with `fchmod` (umask cannot
//!   widen or narrow it), then is `linkat`-ed to its digest name, which never
//!   replaces an existing file. An existing file is reused only if the opened
//!   descriptor is a regular file owned by the effective uid, mode 0600,
//!   `nlink == 1`, with no ACL, and holds bytes with the same keyed digest.
//! - The digest is BLAKE3 in keyed mode, with a random 32-byte key created
//!   once as `DIR/stderr/key` under the same checks, so a receipt's digest
//!   cannot confirm a guessed secret without the key.
//! - Opening a store writes nothing: `stderr/` and the key are created by the
//!   first capture (DF1), after the caller has refused a state dir inside a
//!   repository. That containment check compares directory identities
//!   (device and inode) along the state dir's ancestors, never path prefixes
//!   (DF2).
//!
//! [`PrivateState`] and the private-file helpers are shared with git carry
//! v2's list store (`git_carry::carry_v2`).

use std::ffi::CString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::io::{AsRawFd as _, FromRawFd as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::{BulkloadRefusal, Result};

/// Bytes of a child's stderr kept in memory, for classification only.
pub const CLASSIFY_LIMIT: usize = 1 << 20;

/// A private state directory, opened by descriptor and checked: owned by the
/// effective uid, no group or other bits, no extended ACL. It also remembers
/// the device and inode of every directory from `/` down to itself, so
/// containment is decided by identity, never by comparing path spellings
/// (DF2: a case alias, a firmlink or a bind mount names the same directory
/// under another prefix). Opening it creates nothing (DF1).
#[derive(Debug)]
pub struct PrivateState {
    directory: File,
    root: PathBuf,
    chain: Vec<(u64, u64)>,
}

impl PrivateState {
    /// Open `state_dir`: its parent is canonicalized once, then walked from
    /// `/` one component at a time with `O_DIRECTORY|O_NOFOLLOW`.
    ///
    /// # Errors
    /// Refuses a state dir that is a symlink, is not owned by the effective
    /// uid, has group or other permission bits or carries an extended ACL,
    /// and any I/O failure.
    pub fn open(state_dir: &Path) -> Result<Self> {
        let absolute = if state_dir.is_absolute() {
            state_dir.to_path_buf()
        } else {
            std::env::current_dir()?.join(state_dir)
        };
        let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
            return Err(BulkloadRefusal::PathNotAbsolute);
        };
        let parent = std::fs::canonicalize(parent)?;
        let mut directory = open_dir(None, Path::new("/"))?;
        let mut chain = vec![identity(&directory)?];
        for component in parent.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    directory = open_dir(Some(&directory), Path::new(part))?;
                    chain.push(identity(&directory)?);
                }
                _ => return Err(BulkloadRefusal::PathEscapesRoot),
            }
        }
        let state = open_dir(Some(&directory), Path::new(name))?;
        private_directory(&state)?;
        chain.push(identity(&state)?);
        Ok(Self {
            directory: state,
            root: parent.join(name),
            chain,
        })
    }

    /// Whether the state dir is `path`'s directory or lies under it: `path`
    /// (followed) has the device and inode of the state dir or of one of its
    /// ancestors (DF2).
    #[must_use]
    pub fn is_inside(&self, path: &Path) -> bool {
        std::fs::metadata(path)
            .is_ok_and(|metadata| self.chain.contains(&(metadata.dev(), metadata.ino())))
    }

    /// The state dir as its canonical parent and name spell it.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The open state dir.
    #[must_use]
    pub const fn directory(&self) -> &File {
        &self.directory
    }
}

/// Open `name` under `parent` as a private directory, creating it at 0700
/// first when `create` is set. Without `create`, an absent directory is
/// `None`; anything else at the name must pass the private checks.
///
/// # Errors
/// Refuses a symlink, a non-directory, a directory that is not private, and
/// any I/O failure.
pub fn private_subdirectory(parent: &File, name: &str, create: bool) -> Result<Option<File>> {
    if create {
        let leaf = cstring(name.as_bytes())?;
        // SAFETY: `parent` is an open directory and `leaf` is NUL-terminated.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error.into());
            }
        }
    }
    match open_dir(Some(parent), Path::new(name)) {
        Ok(directory) => {
            private_directory(&directory)?;
            Ok(Some(directory))
        }
        Err(BulkloadRefusal::Io(Some(libc::ENOENT))) if !create => Ok(None),
        Err(error) => Err(error),
    }
}

fn identity(directory: &File) -> Result<(u64, u64)> {
    let metadata = directory.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// The opened, checked `DIR/stderr` directory and its digest key.
///
/// Both are created on the first capture (DF1): opening a store writes nothing, so a state dir
/// that a later check refuses (inside a repository) is left untouched.
pub struct StderrStore {
    state: PrivateState,
    shown: PathBuf,
    opened: Mutex<Option<Opened>>,
}

/// `DIR/stderr` and the key, once a capture needed them.
struct Opened {
    directory: File,
    key: [u8; 32],
}

impl std::fmt::Debug for StderrStore {
    // The key never reaches a `Debug` rendering.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StderrStore")
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

/// One child's stderr being streamed into a private temporary.
pub struct Capture {
    file: File,
    temporary: CString,
    hasher: blake3::Hasher,
}

impl StderrStore {
    /// Open `state_dir`, checking an existing `stderr/` directory. Nothing is
    /// created: `stderr/` and the digest key are made by the first
    /// [`StderrStore::capture`] (DF1).
    ///
    /// # Errors
    /// Refuses a state dir or an existing `stderr/` that is a symlink, is not
    /// owned by the effective uid, has group or other permission bits, or
    /// carries an extended ACL; any I/O failure.
    pub fn open(state_dir: &Path) -> Result<Self> {
        let state = PrivateState::open(state_dir)?;
        private_subdirectory(state.directory(), "stderr", false)?;
        Ok(Self {
            shown: state.root().join("stderr"),
            state,
            opened: Mutex::new(None),
        })
    }

    /// Whether the state dir lies at or under `path`, by directory identity
    /// (DF2).
    #[must_use]
    pub fn is_inside(&self, path: &Path) -> bool {
        self.state.is_inside(path)
    }

    /// Run `step` with `stderr/` and the key, creating them on first use.
    fn with_opened<T>(&self, step: impl FnOnce(&Opened) -> Result<T>) -> Result<T> {
        let mut guard = self.opened.lock().map_err(|_| BulkloadRefusal::Io(None))?;
        if guard.is_none() {
            let directory = private_subdirectory(self.state.directory(), "stderr", true)?
                .ok_or(BulkloadRefusal::Io(None))?;
            let key = key(&directory)?;
            *guard = Some(Opened { directory, key });
        }
        step(guard.as_ref().ok_or(BulkloadRefusal::Io(None))?)
    }

    /// Start a capture: a new private temporary in `stderr/`, which (with
    /// the key) is created here on first use.
    ///
    /// # Errors
    /// Refuses when `stderr/` or the key fail the private checks, or the
    /// temporary cannot be created privately.
    pub fn capture(&self) -> Result<Capture> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let temporary = cstring(
            format!(
                ".capture-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
        self.with_opened(|opened| {
            let file = create_private(&opened.directory, &temporary)?;
            Ok(Capture {
                file,
                temporary,
                hasher: blake3::Hasher::new_keyed(&opened.key),
            })
        })
    }

    /// Finish a capture: link it to `<digest>.log` and return the digest and
    /// the shown path. An existing file of that name is accepted only if it
    /// passes every private-file check and holds the same keyed digest.
    ///
    /// # Errors
    /// Refuses an existing file that fails those checks, or an I/O failure.
    /// The temporary is removed either way.
    pub fn commit(&self, capture: Capture) -> Result<(String, PathBuf)> {
        let outcome = self.with_opened(|opened| self.link(opened, &capture));
        self.discard(capture);
        outcome
    }

    fn link(&self, opened: &Opened, capture: &Capture) -> Result<(String, PathBuf)> {
        capture.file.sync_all()?;
        let digest = capture.hasher.finalize().to_hex().to_string();
        let leaf = format!("{digest}.log");
        let name = cstring(leaf.as_bytes())?;
        // SAFETY: both names are NUL-terminated and relative to the open
        // `stderr` directory; `linkat` never replaces an existing name.
        let linked = unsafe {
            libc::linkat(
                opened.directory.as_raw_fd(),
                capture.temporary.as_ptr(),
                opened.directory.as_raw_fd(),
                name.as_ptr(),
                0,
            )
        };
        if linked != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error.into());
            }
            let existing = open_existing(&opened.directory, &name)?;
            private_file(&existing)?;
            let mut hasher = blake3::Hasher::new_keyed(&opened.key);
            hasher.update_reader(&existing)?;
            if hasher.finalize().to_hex().as_str() != digest {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        }
        opened.directory.sync_all()?;
        Ok((digest, self.shown.join(leaf)))
    }

    /// Drop a capture's temporary (a probe that succeeded, or stderr that
    /// was empty). The capture is consumed so it cannot be committed after.
    pub fn discard(&self, capture: Capture) {
        let Capture {
            file, temporary, ..
        } = capture;
        drop(file);
        // A capture exists only once `stderr/` was opened, so this never
        // creates it.
        let _ = self.with_opened(|opened| {
            // SAFETY: the name is NUL-terminated and relative to `stderr`.
            unsafe { libc::unlinkat(opened.directory.as_raw_fd(), temporary.as_ptr(), 0) };
            Ok(())
        });
    }
}

impl Capture {
    /// Append `bytes` to the temporary and the keyed digest.
    ///
    /// # Errors
    /// Any write failure.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        Ok(())
    }
}

pub fn cstring(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| BulkloadRefusal::PathNotPortable)
}

fn open_dir(parent: Option<&File>, name: &Path) -> Result<File> {
    let name = cstring(name.as_os_str().as_bytes())?;
    let at = parent.map_or(libc::AT_FDCWD, File::as_raw_fd);
    // SAFETY: `at` is AT_FDCWD or an open directory; `name` is NUL-terminated;
    // no create flag, so no mode argument.
    let fd = unsafe {
        libc::openat(
            at,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn create_private(directory: &File, name: &CString) -> Result<File> {
    // SAFETY: `directory` is open, `name` is NUL-terminated, and the mode
    // accompanies O_CREAT.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just created and is owned by nothing else.
    let file = unsafe { File::from_raw_fd(fd) };
    // SAFETY: `file` is open; fchmod sets 0600 whatever the umask removed.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: as in `create_private`'s open.
        unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        return Err(error.into());
    }
    if let Err(error) = private_file(&file) {
        // SAFETY: as above.
        unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        return Err(error);
    }
    Ok(file)
}

pub fn open_existing(directory: &File, name: &CString) -> Result<File> {
    // SAFETY: `directory` is open and `name` NUL-terminated; O_NONBLOCK keeps
    // a FIFO planted at the name from blocking the open.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// The digest key: created once at 0600 with 32 random bytes, or read back
/// after the private-file checks.
fn key(directory: &File) -> Result<[u8; 32]> {
    let name = cstring(b"key")?;
    let mut key = [0_u8; 32];
    match create_private(directory, &name) {
        Ok(mut file) => {
            File::open("/dev/urandom")?.read_exact(&mut key)?;
            file.write_all(&key)?;
            file.sync_all()?;
            directory.sync_all()?;
        }
        Err(BulkloadRefusal::Io(Some(libc::EEXIST))) => {
            let mut file = open_existing(directory, &name)?;
            private_file(&file)?;
            if file.metadata()?.len() != 32 {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            file.read_exact(&mut key)?;
        }
        Err(error) => return Err(error),
    }
    Ok(key)
}

fn private_directory(directory: &File) -> Result<()> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if !metadata.is_dir()
        || metadata.uid() != euid
        || metadata.mode() & 0o077 != 0
        || extended_acl(directory)?
    {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(())
}

pub fn private_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != euid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || extended_acl(file)?
    {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(())
}

/// Whether the open file carries an extended ACL (macOS: any entry of its
/// `ACL_TYPE_EXTENDED` ACL, which `chmod +a` sets and children inherit).
#[cfg(target_os = "macos")]
fn extended_acl(file: &File) -> Result<bool> {
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            entry_id: libc::c_int,
            entry: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }
    // SAFETY: `file` is open; the call only reads its ACL.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            _ => Err(error.into()),
        };
    }
    let mut entry = std::ptr::null_mut();
    // SAFETY: `acl` is a live ACL from acl_get_fd_np; `entry` is writable.
    let found = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &raw mut entry) } == 0;
    // SAFETY: `acl` came from acl_get_fd_np and is freed exactly once.
    unsafe { acl_free(acl) };
    Ok(found)
}

/// Whether the open file carries a POSIX ACL (Linux: either ACL xattr).
#[cfg(target_os = "linux")]
fn extended_acl(file: &File) -> Result<bool> {
    for name in [c"system.posix_acl_access", c"system.posix_acl_default"] {
        // SAFETY: `file` is open and `name` NUL-terminated; a null buffer of
        // size 0 only asks for the attribute's size.
        let size =
            unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size >= 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENODATA | libc::EOPNOTSUPP) => {}
            _ => return Err(error.into()),
        }
    }
    Ok(false)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn extended_acl(_file: &File) -> Result<bool> {
    Ok(false)
}
