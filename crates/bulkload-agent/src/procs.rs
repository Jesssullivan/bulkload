//! Which processes are inside a tree: their working directory, or a file
//! they hold open, under it (review of #289, finding 2; operator ruling
//! 2026-10-19, "Switch when clean", condition c).
//!
//! On Linux every `/proc/<pid>/cwd` and `/proc/<pid>/fd/*` link this user
//! may read is resolved and compared, by path component, with each root
//! (canonical) and everything under it. A process whose links cannot be
//! read (another user's) is not seen: the estate's checkouts are this
//! user's own, and a process of another user cannot write them. This
//! process itself is never counted (it holds nothing there between its own
//! steps). Elsewhere nothing can be proved: [`inside`] answers `None`, and
//! a caller refuses (or keeps) rather than assume no process is there.

use std::path::{Path, PathBuf};

use crate::Result;

/// The processes (by pid) whose working directory or an open file is under
/// one of `roots`, sorted; `None` when this platform cannot say.
///
/// A root that does not exist is skipped. Read-only.
///
/// # Errors
/// IO reading `/proc` itself (an exiting process's links are skipped).
pub fn inside(roots: &[&Path]) -> Result<Option<Vec<u32>>> {
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect();
    scan(&roots)
}

// Whether `link` (a resolved `/proc` link) names a path under one of
// `roots`; a deleted file's link (`... (deleted)`) names nothing there.
#[cfg(target_os = "linux")]
fn under(link: &Path, roots: &[PathBuf]) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    !link.as_os_str().as_bytes().ends_with(b" (deleted)")
        && roots.iter().any(|root| link.starts_with(root))
}

#[cfg(target_os = "linux")]
fn scan(roots: &[PathBuf]) -> Result<Option<Vec<u32>>> {
    use crate::refuse::RefuseAt as _;
    const SITE: &str = "procs::inside";
    let mut found = Vec::new();
    if roots.is_empty() {
        return Ok(Some(found));
    }
    let own = std::process::id();
    for entry in std::fs::read_dir("/proc").refuse_at(SITE)? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let base = entry.path();
        let cwd = std::fs::read_link(base.join("cwd")).is_ok_and(|cwd| under(&cwd, roots));
        let open = cwd
            || std::fs::read_dir(base.join("fd")).is_ok_and(|fds| {
                fds.filter_map(std::result::Result::ok).any(|fd| {
                    std::fs::read_link(fd.path()).is_ok_and(|target| under(&target, roots))
                })
            });
        if open {
            found.push(pid);
        }
    }
    found.sort_unstable();
    Ok(Some(found))
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unnecessary_wraps)]
const fn scan(_roots: &[PathBuf]) -> Result<Option<Vec<u32>>> {
    Ok(None)
}

/// The processes (by pid) that hold one of `files` open, or work in or hold
/// a regular file open under one of `dirs`.
///
/// Sorted; `None` where this platform cannot say (anything but Linux).
///
/// The bytes a write would race (#294 gate, 2026-10-21):
/// - **Files by identity.** A file is matched by its device and inode, never
///   by path, so a renamed or linked name is still seen.
/// - **The caller's own handles, exactly.** `own` are the descriptors the
///   caller opened for this check's files. A descriptor that is the very
///   same open file description as one of them (Linux `kcmp(KCMP_FILE)`) is
///   not a holder: a child forked from this process, before its exec, holds
///   copies of every descriptor. Any other handle on the file is a holder,
///   in this process too (another thread of a long-lived process is a
///   writer like any other). Where `kcmp` cannot say, the handle counts.
/// - **Deterministic.** What this process opened is told apart by identity,
///   never by timing, so parallel tests (forking children of their own) do
///   not see each other's copies as writers.
///
/// # Errors
/// IO reading `/proc` itself, or the identity of one of `files`.
pub fn holders(
    files: &[&std::fs::File],
    dirs: &[&Path],
    own: &[std::os::fd::RawFd],
) -> Result<Option<Vec<u32>>> {
    let dirs: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    scan_holders(files, &dirs, own)
}

/// Whether descriptor `theirs` of process `pid` is the very open file
/// description this process holds as `mine` (Linux `kcmp(KCMP_FILE)`, a
/// read-only compare of two descriptor tables). `None`: the compare did not
/// answer; `Some(false)` with the descriptor or the process gone (`EBADF`,
/// `ESRCH`) is told apart by the caller re-reading it.
#[cfg(target_os = "linux")]
fn same_description(pid: u32, mine: i32, theirs: i32) -> std::result::Result<bool, i32> {
    let me = libc::pid_t::try_from(std::process::id()).unwrap_or(-1);
    // SAFETY: `kcmp` reads two processes' descriptor tables and writes
    // nothing; every argument is a plain integer.
    let answer = unsafe {
        libc::syscall(
            libc::SYS_kcmp,
            me,
            libc::pid_t::try_from(pid).unwrap_or(-1),
            0, // KCMP_FILE
            libc::c_long::from(mine),
            libc::c_long::from(theirs),
        )
    };
    match answer {
        0 => Ok(true),
        -1 => Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)),
        _ => Ok(false),
    }
}

#[cfg(target_os = "linux")]
fn scan_holders(
    files: &[&std::fs::File],
    dirs: &[PathBuf],
    own: &[std::os::fd::RawFd],
) -> Result<Option<Vec<u32>>> {
    use crate::refuse::RefuseAt as _;
    use std::os::unix::fs::MetadataExt as _;
    const SITE: &str = "procs::holders";
    let mut ids = Vec::with_capacity(files.len());
    for file in files {
        let meta = file.metadata().refuse_at(SITE)?;
        ids.push((meta.dev(), meta.ino()));
    }
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").refuse_at(SITE)? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let base = entry.path();
        let working =
            pid != me && std::fs::read_link(base.join("cwd")).is_ok_and(|cwd| under(&cwd, dirs));
        // Each descriptor on one of the files (or a regular file under a
        // dir): what it refers to, read now.
        let matches = |fd: &std::path::Path| {
            std::fs::metadata(fd).is_ok_and(|meta| {
                ids.contains(&(meta.dev(), meta.ino()))
                    || (!dirs.is_empty()
                        && meta.is_file()
                        && std::fs::read_link(fd).is_ok_and(|target| under(&target, dirs)))
            })
        };
        let mut copy_of_ours = false;
        let mut other = false;
        for fd in std::fs::read_dir(base.join("fd"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = fd.path();
            let Some(number) = fd.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
                continue;
            };
            if !matches(&path) {
                continue;
            }
            let mut ours = false;
            let mut unknown = false;
            for mine in own {
                match same_description(pid, *mine, number) {
                    Ok(true) => ours = true,
                    Ok(false) => {}
                    // Gone since it was listed (a forked child's exec closed
                    // it, or the process ended): judged by re-reading below.
                    Err(code) if code == libc::EBADF || code == libc::ESRCH => {}
                    Err(_) => unknown = true,
                }
            }
            if ours {
                copy_of_ours = true;
            } else if matches(&path) || unknown {
                // Still the file, another description of it (or the compare
                // could not say): a holder, unless the process proves to be
                // a fork of this one.
                other = true;
            }
        }
        // A process holding this process's very description is a fork of it
        // (before its exec): what it holds is a copy of what this process
        // holds, and this process's own entry is judged on its own.
        let holds = working || (other && (pid == me || !copy_of_ours));
        if holds {
            found.push(pid);
        }
    }
    found.sort_unstable();
    Ok(Some(found))
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unnecessary_wraps)]
const fn scan_holders(
    _files: &[&std::fs::File],
    _dirs: &[PathBuf],
    _own: &[std::os::fd::RawFd],
) -> Result<Option<Vec<u32>>> {
    Ok(None)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_process_whose_working_directory_is_inside_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("checkout");
        std::fs::create_dir(&inner).unwrap();
        assert_eq!(inside(&[&inner]).unwrap(), Some(Vec::new()));
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&inner)
            .spawn()
            .unwrap();
        // The child's cwd is set before it execs.
        let mut seen = None;
        for _ in 0..200 {
            seen = inside(&[&inner]).unwrap();
            if seen.as_ref().is_some_and(|pids| !pids.is_empty()) {
                break;
            }
            std::thread::yield_now();
        }
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(seen, Some(vec![pid]));
        // A sibling whose name shares a prefix is not inside.
        let sibling = dir.path().join("checkout.neo");
        std::fs::create_dir(&sibling).unwrap();
        assert!(!under(&sibling, &[std::fs::canonicalize(&inner).unwrap()]));
    }
}
