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
