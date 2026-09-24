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

use std::ffi::CString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::io::{AsRawFd as _, FromRawFd as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{BulkloadRefusal, Result};

/// Bytes of a child's stderr kept in memory, for classification only.
pub const CLASSIFY_LIMIT: usize = 1 << 20;

/// The opened, checked `DIR/stderr` directory and its digest key.
#[derive(Debug)]
pub struct StderrStore {
    root: PathBuf,
    directory: File,
    shown: PathBuf,
    key: [u8; 32],
}

/// One child's stderr being streamed into a private temporary.
pub struct Capture {
    file: File,
    temporary: CString,
    hasher: blake3::Hasher,
}

impl StderrStore {
    /// Open `state_dir` and its `stderr/` directory, creating `stderr/` and
    /// the digest key when absent.
    ///
    /// # Errors
    /// Refuses a state dir or `stderr/` that is a symlink, is not owned by
    /// the effective uid, has group or other permission bits, or carries an
    /// extended ACL; a key file that fails the same checks; any I/O failure.
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
        for component in parent.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => directory = open_dir(Some(&directory), Path::new(part))?,
                _ => return Err(BulkloadRefusal::PathEscapesRoot),
            }
        }
        let state = open_dir(Some(&directory), Path::new(name))?;
        private_directory(&state)?;
        let leaf = cstring(b"stderr")?;
        // SAFETY: `state` is an open directory and `leaf` is NUL-terminated.
        if unsafe { libc::mkdirat(state.as_raw_fd(), leaf.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error.into());
            }
        }
        let stderr = open_dir(Some(&state), Path::new("stderr"))?;
        private_directory(&stderr)?;
        let key = key(&stderr)?;
        let root = parent.join(name);
        Ok(Self {
            shown: root.join("stderr"),
            root,
            directory: stderr,
            key,
        })
    }

    /// Whether the state dir lies at or under `path` (resolved).
    #[must_use]
    pub fn is_inside(&self, path: &Path) -> bool {
        std::fs::canonicalize(path).is_ok_and(|resolved| self.root.starts_with(resolved))
    }

    /// Start a capture: a new private temporary in `stderr/`.
    ///
    /// # Errors
    /// Refuses when the temporary cannot be created privately.
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
        let file = create_private(&self.directory, &temporary)?;
        Ok(Capture {
            file,
            temporary,
            hasher: blake3::Hasher::new_keyed(&self.key),
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
        let outcome = self.link(&capture);
        self.discard(capture);
        outcome
    }

    fn link(&self, capture: &Capture) -> Result<(String, PathBuf)> {
        capture.file.sync_all()?;
        let digest = capture.hasher.finalize().to_hex().to_string();
        let leaf = format!("{digest}.log");
        let name = cstring(leaf.as_bytes())?;
        // SAFETY: both names are NUL-terminated and relative to the open
        // `stderr` directory; `linkat` never replaces an existing name.
        let linked = unsafe {
            libc::linkat(
                self.directory.as_raw_fd(),
                capture.temporary.as_ptr(),
                self.directory.as_raw_fd(),
                name.as_ptr(),
                0,
            )
        };
        if linked != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error.into());
            }
            let existing = open_existing(&self.directory, &name)?;
            private_file(&existing)?;
            let mut hasher = blake3::Hasher::new_keyed(&self.key);
            hasher.update_reader(&existing)?;
            if hasher.finalize().to_hex().as_str() != digest {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        }
        self.directory.sync_all()?;
        Ok((digest, self.shown.join(leaf)))
    }

    /// Drop a capture's temporary (a probe that succeeded, or stderr that
    /// was empty). The capture is consumed so it cannot be committed after.
    pub fn discard(&self, capture: Capture) {
        let Capture {
            file, temporary, ..
        } = capture;
        drop(file);
        // SAFETY: the name is NUL-terminated and relative to `stderr`.
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), temporary.as_ptr(), 0) };
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

fn cstring(bytes: &[u8]) -> Result<CString> {
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

fn create_private(directory: &File, name: &CString) -> Result<File> {
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

fn open_existing(directory: &File, name: &CString) -> Result<File> {
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

fn private_file(file: &File) -> Result<()> {
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
