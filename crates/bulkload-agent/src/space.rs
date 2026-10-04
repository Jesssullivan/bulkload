//! Destination space preflight (OI-1001-Q2).
//!
//! Before the engine commits to writing planned bytes it asks the destination
//! filesystem (`statvfs`) how much room is left, and refuses
//! [`BulkloadRefusal::DestinationSpaceInsufficient`] when the write would
//! leave less than the configured share of the filesystem free. The floor is
//! process-wide: the binary sets it from `--min-free-percent` (default
//! [`DEFAULT_MIN_FREE_PERCENT`]); a library caller that never sets it gets 0,
//! which still refuses a write larger than the space available.
//!
//! The check is advisory against concurrent writers: another process can fill
//! the filesystem between the probe and the write. It exists so a planned
//! cohort cannot, by itself, run a destination out of space.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::{BulkloadRefusal, Result};

/// The binary's floor when `--min-free-percent` is not given.
pub const DEFAULT_MIN_FREE_PERCENT: u8 = 25;

static MIN_FREE_PERCENT: AtomicU8 = AtomicU8::new(0);

/// Set the process-wide free-space floor, in percent of the filesystem.
pub fn set_min_free_percent(percent: u8) {
    MIN_FREE_PERCENT.store(percent.min(100), Ordering::Relaxed);
}

/// The process-wide free-space floor, in percent of the filesystem.
#[must_use]
pub fn min_free_percent() -> u8 {
    MIN_FREE_PERCENT.load(Ordering::Relaxed)
}

/// Parse a `--min-free-percent` value: an integer from 0 to 100.
///
/// # Errors
/// Refuses anything else with `FIELD_DOMAIN_VIOLATION`.
pub fn parse_percent(value: &str) -> Result<u8> {
    match value.parse::<u8>() {
        Ok(percent) if percent <= 100 => Ok(percent),
        _ => Err(BulkloadRefusal::FieldDomainViolation),
    }
}

/// One filesystem's size and the space an unprivileged writer may still use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// Filesystem size in bytes (`f_blocks * f_frsize`).
    pub total: u64,
    /// Bytes available to an unprivileged writer (`f_bavail * f_frsize`).
    pub available: u64,
}

/// Decide whether `planned` bytes fit with `min_free_percent` of the
/// filesystem still free afterwards. Exactly at the floor passes; one byte
/// below it refuses.
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT` when `planned` exceeds the available
/// space, or when the free space left after it would be under the floor.
pub fn check(planned: u64, space: Space, min_free_percent: u8) -> Result<()> {
    let after = space
        .available
        .checked_sub(planned)
        .ok_or(BulkloadRefusal::DestinationSpaceInsufficient)?;
    // after / total >= percent / 100, in integers that cannot overflow.
    if u128::from(after) * 100 < u128::from(space.total) * u128::from(min_free_percent.min(100)) {
        return Err(BulkloadRefusal::DestinationSpaceInsufficient);
    }
    Ok(())
}

/// The nearest existing path at or above `path`: a destination that does not
/// exist yet will be created on its closest existing ancestor's filesystem.
///
/// # Errors
/// Refuses a relative path that has no existing ancestor, and stat failures
/// other than `ENOENT`.
pub fn existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => return Ok(current.to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
            }
            Err(error) => return Err(crate::refuse::io(&error, "space::existing_ancestor")),
        }
    }
}

/// `(blocks, available blocks, block size)` from `statvfs`.
#[cfg(not(target_os = "macos"))]
fn counts(name: &std::ffi::CStr) -> Result<(u128, u128, u128)> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `name` is a valid NUL-terminated string that outlives the call,
    // and `stats` points to writable storage of the right size and alignment.
    if unsafe { libc::statvfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(crate::refuse::io(
            &std::io::Error::last_os_error(),
            "space::counts",
        ));
    }
    // SAFETY: a successful statvfs initialized the whole structure.
    let stats = unsafe { stats.assume_init() };
    // Widths vary across platforms; widen before multiplying.
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    Ok((
        u128::from(stats.f_blocks as u64),
        u128::from(stats.f_bavail as u64),
        u128::from(stats.f_frsize as u64),
    ))
}

/// `(blocks, available blocks, block size)` from `statfs`: Darwin's
/// `statvfs` carries 32-bit block counts (`fsblkcnt_t`), which wrap on a
/// large volume; its `statfs` counts are 64-bit.
#[cfg(target_os = "macos")]
fn counts(name: &std::ffi::CStr) -> Result<(u128, u128, u128)> {
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `name` is a valid NUL-terminated string that outlives the call,
    // and `stats` points to writable storage of the right size and alignment.
    if unsafe { libc::statfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(crate::refuse::io(
            &std::io::Error::last_os_error(),
            "space::counts",
        ));
    }
    // SAFETY: a successful statfs initialized the whole structure.
    let stats = unsafe { stats.assume_init() };
    Ok((
        u128::from(stats.f_blocks),
        u128::from(stats.f_bavail),
        u128::from(stats.f_bsize),
    ))
}

/// Probe the filesystem holding `path`, which must exist (`statvfs`;
/// `statfs` on Darwin).
///
/// # Errors
/// Refuses a path with an interior NUL, and the OS error of a failed call.
pub fn probe(path: &Path) -> Result<Space> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| BulkloadRefusal::PathNotPortable)?;
    let (blocks, available, fragment) = counts(&name)?;
    Ok(Space {
        total: u64::try_from(blocks * fragment).unwrap_or(u64::MAX),
        available: u64::try_from(available * fragment).unwrap_or(u64::MAX),
    })
}

/// Probe the filesystem that will hold `destination` and [`check`] `planned`
/// bytes against the process-wide floor. Zero planned bytes always pass
/// without a probe.
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT`, or the probe's refusal.
pub fn preflight(destination: &Path, planned: u64) -> Result<()> {
    if planned == 0 {
        return Ok(());
    }
    let space = probe(&existing_ancestor(destination)?)?;
    check(planned, space, min_free_percent())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn exactly_at_the_floor_passes_and_one_byte_under_refuses() {
        // 100 GiB filesystem, 40 GiB available, 25% floor = 25 GiB.
        let space = Space {
            total: 100 * GIB,
            available: 40 * GIB,
        };
        assert_eq!(check(15 * GIB, space, 25), Ok(()));
        assert_eq!(
            check(15 * GIB + 1, space, 25),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        assert_eq!(check(0, space, 25), Ok(()));
    }

    #[test]
    fn a_floor_that_does_not_divide_evenly_rounds_against_the_write() {
        // 25% of 1001 bytes is 250.25: 250 bytes left is under it.
        let space = Space {
            total: 1001,
            available: 300,
        };
        assert_eq!(check(49, space, 25), Ok(()));
        assert_eq!(
            check(50, space, 25),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
    }

    #[test]
    fn already_under_the_floor_refuses_any_write() {
        let space = Space {
            total: 100 * GIB,
            available: 20 * GIB,
        };
        assert_eq!(
            check(1, space, 25),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
    }

    #[test]
    fn zero_floor_still_refuses_more_than_is_available() {
        let space = Space {
            total: 10 * GIB,
            available: GIB,
        };
        assert_eq!(check(GIB, space, 0), Ok(()));
        assert_eq!(
            check(GIB + 1, space, 0),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
    }

    #[test]
    fn full_floor_refuses_every_write_and_huge_values_do_not_overflow() {
        let space = Space {
            total: u64::MAX,
            available: u64::MAX,
        };
        assert_eq!(
            check(1, space, 100),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        assert_eq!(check(0, space, 100), Ok(()));
        assert_eq!(check(u64::MAX / 2, space, 50), Ok(()));
        assert_eq!(
            check(u64::MAX / 2 + 1, space, 50),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
    }

    #[test]
    fn percent_parse_is_closed() {
        assert_eq!(parse_percent("0"), Ok(0));
        assert_eq!(parse_percent("25"), Ok(25));
        assert_eq!(parse_percent("100"), Ok(100));
        for bad in ["101", "-1", "", "25%", "2.5", "256"] {
            assert_eq!(
                parse_percent(bad),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{bad}"
            );
        }
    }

    #[test]
    fn probe_reads_a_real_filesystem_through_a_missing_destination() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent").join("deeper");
        let ancestor = existing_ancestor(&missing).unwrap();
        assert_eq!(ancestor, root.path());
        let space = probe(&ancestor).unwrap();
        assert!(space.total > 0);
        assert!(space.available <= space.total);
        // More than the whole filesystem never fits, whatever the floor.
        assert_eq!(
            check(space.total.saturating_add(1), space, 0),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        assert_eq!(preflight(&missing, 0), Ok(()));
    }
}
