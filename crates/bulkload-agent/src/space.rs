//! Destination space preflight (OI-1001-Q2).
//!
//! Before the engine commits to writing planned bytes it asks the destination
//! how much room is left, and refuses
//! [`BulkloadRefusal::DestinationSpaceInsufficient`] when the write would
//! leave less than the configured share free. The floor is process-wide: the
//! binary sets it from `--min-free-percent` (default
//! [`DEFAULT_MIN_FREE_PERCENT`]); a library caller that never sets it gets 0,
//! which still refuses a write larger than the space available.
//!
//! Two limits can bind (M2 round 2, item 1). On XFS (and ext4) with project
//! quotas, `statvfs` of a directory inside a project-quota tree reports the
//! PROJECT's limit and use, not the filesystem's. [`probe_limits`] reads the
//! path's view and, on Linux, the view at the root of the same filesystem
//! (the mount of the path's device whose root is the filesystem's own root,
//! from `/proc/self/mountinfo`); when the two report different sizes, the
//! path's view is a quota and the root's is the filesystem. The floor applies
//! to each limit on its own: the write must leave `--min-free-percent` of the
//! quota's limit free under the quota, and the same percent of the
//! filesystem's size free on the filesystem. A refusal records which limit
//! bound (`filesystem`, `project-quota` or `both`) and both limits' numbers
//! ([`last_refusal`]), counted as `space_refused_filesystem` /
//! `space_refused_quota` and printed by the binary as a `space_refusal` line.
//! Where the root cannot be found or read (no mountinfo, a device with no
//! mount of its root, Darwin), the path's view is the filesystem, as before.
//!
//! The check is advisory against concurrent writers: another process can fill
//! the filesystem between the probe and the write. It exists so a planned
//! cohort cannot, by itself, run a destination out of space.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use crate::counters::{self, Counter};
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

/// The variable a debug build takes its free-space reading from instead of
/// the filesystem: `TOTAL:AVAILABLE`, in bytes (BL2-F5, operator
/// 2026-10-28, Linear TIN-3692 `952e2fb6`: "Hermetic test floor").
///
/// The test fixtures set it, so no test result depends on the free space of
/// the host's disk. The floor, its default, its flag and the refusal are
/// unchanged: only the reading is faked. A release build never reads it
/// (the seam is compiled out), as with the hand-off's
/// `TRACKED_TARGETS_ENV`.
pub const TEST_SPACE_ENV: &str = "BULKLOAD_TEST_SPACE";

/// A reading with room for every test fixture: 1 TiB, three quarters free,
/// over any floor up to 75%.
pub const TEST_SPACE_ROOMY: Space = Space {
    total: 1 << 40,
    available: 3 << 38,
};

impl Space {
    /// The [`TEST_SPACE_ENV`] value that fakes this reading.
    #[must_use]
    pub fn test_env_value(self) -> String {
        format!("{}:{}", self.total, self.available)
    }
}

/// The faked reading, when [`TEST_SPACE_ENV`] is set (debug builds only).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for a value that is not `TOTAL:AVAILABLE` with
/// AVAILABLE at most TOTAL.
#[cfg(debug_assertions)]
fn test_space() -> Result<Option<Space>> {
    std::env::var_os(TEST_SPACE_ENV)
        .map(|value| parse_test_space(&value))
        .transpose()
}

/// Parse a [`TEST_SPACE_ENV`] value.
///
/// # Errors
/// As [`test_space`].
#[cfg(debug_assertions)]
fn parse_test_space(value: &std::ffi::OsStr) -> Result<Space> {
    let (total, available) = value
        .to_str()
        .and_then(|value| value.split_once(':'))
        .and_then(|(total, available)| Some((total.parse().ok()?, available.parse().ok()?)))
        .filter(|(total, available)| available <= total)
        .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    Ok(Space { total, available })
}

/// Probe the filesystem holding `path`, which must exist (`statvfs`;
/// `statfs` on Darwin). A debug build with [`TEST_SPACE_ENV`] set returns
/// that reading instead.
///
/// # Errors
/// Refuses a path with an interior NUL, and the OS error of a failed call.
pub fn probe(path: &Path) -> Result<Space> {
    use std::os::unix::ffi::OsStrExt as _;
    #[cfg(debug_assertions)]
    if let Some(space) = test_space()? {
        return Ok(space);
    }
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| BulkloadRefusal::PathNotPortable)?;
    let (blocks, available, fragment) = counts(&name)?;
    Ok(Space {
        total: u64::try_from(blocks * fragment).unwrap_or(u64::MAX),
        available: u64::try_from(available * fragment).unwrap_or(u64::MAX),
    })
}

/// Probe the limits that will hold `destination` and [`check_at`] `planned`
/// bytes against the process-wide floor on each. Zero planned bytes always pass
/// without a probe.
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT`, or the probe's refusal.
pub fn preflight(destination: &Path, planned: u64) -> Result<()> {
    if planned == 0 {
        return Ok(());
    }
    let existing = existing_ancestor(destination)?;
    let limits = probe_limits(&existing)?;
    check_at(&existing, (planned, planned), limits, min_free_percent())
}

/// The limits that bind writes under one path: the filesystem's, and a
/// project quota's when `statvfs` of the path reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The filesystem holding the path.
    pub filesystem: Space,
    /// The project quota the path is under, when its view differs from the
    /// filesystem's.
    pub quota: Option<Space>,
}

impl Limits {
    /// Limits from the path's own view and, when it could be read, the view
    /// at its filesystem's root. A smaller size at the path means its view is
    /// a project quota; any other difference is not a quota (review A1).
    #[must_use]
    pub const fn from_views(path: Space, root: Option<Space>) -> Self {
        match root {
            Some(root) if path.total < root.total => Self {
                filesystem: root,
                quota: Some(path),
            },
            _ => Self {
                filesystem: path,
                quota: None,
            },
        }
    }

    /// One limit only: a filesystem with no quota.
    #[must_use]
    pub const fn filesystem(space: Space) -> Self {
        Self {
            filesystem: space,
            quota: None,
        }
    }
}

/// Which limit refused a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// The filesystem's free space under the floor.
    Filesystem,
    /// The project quota's free space under the floor.
    ProjectQuota,
    /// Both.
    Both,
}

impl Bound {
    /// The word printed for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::ProjectQuota => "project-quota",
            Self::Both => "both",
        }
    }
}

/// Which limit, if any, refuses the write. `None`: it fits.
///
/// `planned` bytes go under the quota and `filesystem_planned` bytes on the
/// filesystem (they differ when several quota trees share one filesystem),
/// each judged under `min_free_percent` of that limit's own size.
#[must_use]
pub fn bound(
    planned: u64,
    filesystem_planned: u64,
    limits: Limits,
    min_free_percent: u8,
) -> Option<Bound> {
    let filesystem = check(filesystem_planned, limits.filesystem, min_free_percent).is_err();
    let quota = limits
        .quota
        .is_some_and(|quota| check(planned, quota, min_free_percent).is_err());
    match (filesystem, quota) {
        (false, false) => None,
        (true, false) => Some(Bound::Filesystem),
        (false, true) => Some(Bound::ProjectQuota),
        (true, true) => Some(Bound::Both),
    }
}

/// A refused space check: where, which limit bound, what was planned and
/// both limits' numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The existing path that was probed.
    pub path: PathBuf,
    /// The limit that refused.
    pub bound: Bound,
    /// Bytes planned under the quota (or the filesystem, without one).
    pub planned: u64,
    /// Bytes planned on the whole filesystem.
    pub filesystem_planned: u64,
    /// The floor, in percent of each limit's size.
    pub min_free_percent: u8,
    /// The limits read.
    pub limits: Limits,
}

impl Refused {
    /// `key=value` fields (the path last, escaped), as the binary's
    /// `space_refusal` line prints them.
    #[must_use]
    pub fn fields(&self) -> String {
        let quota = self.limits.quota.map_or_else(
            || "quota=none".to_owned(),
            |quota| {
                format!(
                    "quota=project quota_total_bytes={} quota_available_bytes={}",
                    quota.total, quota.available
                )
            },
        );
        format!(
            "bound={} planned_bytes={} filesystem_planned_bytes={} min_free_percent={} \
             filesystem_total_bytes={} filesystem_available_bytes={} {quota} path={}",
            self.bound.word(),
            self.planned,
            self.filesystem_planned,
            self.min_free_percent,
            self.limits.filesystem.total,
            self.limits.filesystem.available,
            self.path.as_os_str().as_encoded_bytes().escape_ascii(),
        )
    }
}

static LAST_REFUSAL: Mutex<Option<Refused>> = Mutex::new(None);

/// The process's most recent refused space check, if any.
#[must_use]
pub fn last_refusal() -> Option<Refused> {
    LAST_REFUSAL.lock().ok().and_then(|last| last.clone())
}

/// [`last_refusal`], cleared: the binary prints it once, after a verb
/// that ended `DESTINATION_SPACE_INSUFFICIENT` (review A3).
#[must_use]
pub fn take_refusal() -> Option<Refused> {
    LAST_REFUSAL.lock().ok().and_then(|mut last| last.take())
}

/// Forget the recorded refusal (review A3).
///
/// A later `DESTINATION_SPACE_INSUFFICIENT` converted from a write's
/// `ENOSPC` or `EDQUOT` (or a store's `SQLITE_FULL`) was judged by no
/// limit, so no earlier check's numbers are printed for it.
pub fn forget_refusal() {
    if let Ok(mut last) = LAST_REFUSAL.lock() {
        *last = None;
    }
}

/// Judge `planned` bytes (quota) and `filesystem_planned` bytes
/// (filesystem) at `path` under `limits`: count the check, and on a refusal
/// count which limit bound and record it for [`last_refusal`].
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT`.
pub fn check_at(
    path: &Path,
    (planned, filesystem_planned): (u64, u64),
    limits: Limits,
    min_free_percent: u8,
) -> Result<()> {
    counters::bump(Counter::SpaceChecks);
    let Some(bound) = bound(planned, filesystem_planned, limits, min_free_percent) else {
        return Ok(());
    };
    if matches!(bound, Bound::Filesystem | Bound::Both) {
        counters::bump(Counter::SpaceRefusedFilesystem);
    }
    if matches!(bound, Bound::ProjectQuota | Bound::Both) {
        counters::bump(Counter::SpaceRefusedQuota);
    }
    if let Ok(mut last) = LAST_REFUSAL.lock() {
        *last = Some(Refused {
            path: path.to_owned(),
            bound,
            planned,
            filesystem_planned,
            min_free_percent,
            limits,
        });
    }
    Err(BulkloadRefusal::DestinationSpaceInsufficient)
}

/// The view at the root of the filesystem holding `path` (an existing
/// path), when it can be found and read: Linux only, from
/// `/proc/self/mountinfo`. The root must still resolve to the path's device
/// when it is read (a mount stacked on it hides it, review A1). Any failure
/// is `None` (the path's own view is then the filesystem's).
#[cfg(target_os = "linux")]
fn root_view(path: &Path) -> Option<Space> {
    use std::os::unix::fs::MetadataExt as _;
    let dev = std::fs::metadata(path).ok()?.dev();
    let root = filesystem_root(dev)?;
    probe(&root).ok()
}

#[cfg(not(target_os = "linux"))]
const fn root_view(_path: &Path) -> Option<Space> {
    None
}

/// The mount point of device `dev`'s filesystem root that resolves to
/// `dev` now. A root found is cached for the process and checked again on
/// every use; a device with none is looked up again each time.
#[cfg(target_os = "linux")]
fn filesystem_root(dev: u64) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt as _;
    static ROOTS: Mutex<BTreeMap<u64, PathBuf>> = Mutex::new(BTreeMap::new());
    let resolves = |root: &Path| std::fs::metadata(root).is_ok_and(|meta| meta.dev() == dev);
    let cached = ROOTS.lock().ok()?.get(&dev).cloned();
    if let Some(root) = cached.filter(|root| resolves(root)) {
        return Some(root);
    }
    let table = std::fs::read("/proc/self/mountinfo").ok()?;
    let found = mounts_of_root(&table, libc::major(dev), libc::minor(dev))
        .into_iter()
        .find(|root| resolves(root))?;
    ROOTS.lock().ok()?.insert(dev, found.clone());
    Some(found)
}

/// The mount points of device `major:minor`'s filesystem root, in table
/// order.
///
/// Read from a `/proc/self/mountinfo` table: the mounts whose root is the
/// filesystem's own root (`/`). A device mounted only below its root (a
/// bind of a subtree) has none. Whether each still resolves to the device
/// (nothing stacked on it) is the caller's check.
#[must_use]
pub fn mounts_of_root(table: &[u8], major: u32, minor: u32) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let device = format!("{major}:{minor}");
    table
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            // id parent major:minor root mount-point ...
            let mut fields = line.split(|byte| *byte == b' ');
            let (_, _, dev, root, point) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (dev == device.as_bytes() && root == b"/")
                .then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&unescape_mountinfo(point))))
        })
        .collect()
}

/// Undo mountinfo's octal escapes (`\040` for a space, `\011`, `\012`,
/// `\134`).
fn unescape_mountinfo(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut index = 0;
    while let Some(&byte) = field.get(index) {
        let octal = field.get(index + 1..index + 4).and_then(|digits| {
            digits
                .iter()
                .all(|digit| (b'0'..=b'7').contains(digit))
                .then(|| {
                    digits
                        .iter()
                        .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'))
                })
                .and_then(|value| u8::try_from(value).ok())
        });
        if let (b'\\', Some(value)) = (byte, octal) {
            out.push(value);
            index += 4;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    out
}

/// Probe the limits that bind writes under `path`, which must exist: the
/// path's view, and the filesystem root's view where it differs (a project
/// quota).
///
/// # Errors
/// As [`probe`] of `path`; failing to read the root's view is not an error.
pub fn probe_limits(path: &Path) -> Result<Limits> {
    let view = probe(path)?;
    Ok(Limits::from_views(view, root_view(path)))
}

/// One destination's planned bytes, grouped for a multi-path preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The existing path probed.
    pub path: PathBuf,
    /// Its device.
    pub device: u64,
    /// Its limits.
    pub limits: Limits,
    /// Bytes planned under it.
    pub bytes: u64,
}

/// Check bytes planned at several existing paths, probed already.
///
/// Paths on one device and under one limit (the same quota, or none) are
/// summed against that limit, and every path on one device is summed
/// against its filesystem. The first group that does not fit refuses.
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT` (recorded, see [`check_at`]).
pub fn check_planned(planned: &[Planned], min_free_percent: u8) -> Result<()> {
    let mut devices: BTreeMap<u64, u64> = BTreeMap::new();
    // A quota is told apart by its limit as probed: two trees on one
    // device under quotas of the same size are summed together and judged
    // by the least room any of them reported (review A2), which errs
    // towards refusing.
    let mut groups: BTreeMap<(u64, Option<u64>), (PathBuf, Limits, u64)> = BTreeMap::new();
    for want in planned.iter().filter(|want| want.bytes > 0) {
        let device = devices.entry(want.device).or_insert(0);
        *device = device.saturating_add(want.bytes);
        let key = (want.device, want.limits.quota.map(|quota| quota.total));
        let group = groups
            .entry(key)
            .or_insert_with(|| (want.path.clone(), want.limits, 0));
        group.2 = group.2.saturating_add(want.bytes);
        if want.limits.filesystem.available < group.1.filesystem.available {
            group.1.filesystem = want.limits.filesystem;
        }
        if let (Some(least), Some(quota)) = (group.1.quota.as_mut(), want.limits.quota) {
            if quota.available < least.available {
                *least = quota;
                group.0.clone_from(&want.path);
            }
        }
    }
    for ((device, _), (path, limits, bytes)) in &groups {
        let on_device = devices.get(device).copied().unwrap_or(*bytes);
        check_at(path, (*bytes, on_device), *limits, min_free_percent)?;
    }
    Ok(())
}

/// Probe one destination for [`check_planned`]: its nearest existing
/// ancestor, that ancestor's device (followed) and limits.
///
/// # Errors
/// A path with no existing ancestor, a stat or probe failure.
pub fn locate(path: &Path, bytes: u64) -> Result<Planned> {
    use std::os::unix::fs::MetadataExt as _;
    let existing = existing_ancestor(path)?;
    let device = std::fs::metadata(&existing)
        .map_err(|error| crate::refuse::io(&error, "space::locate"))?
        .dev();
    let limits = probe_limits(&existing)?;
    Ok(Planned {
        path: existing,
        device,
        limits,
        bytes,
    })
}

/// Probe each of `wants`' destinations ([`locate`]) and [`check_planned`]
/// them. Zero planned bytes are never probed.
///
/// # Errors
/// `DESTINATION_SPACE_INSUFFICIENT`, or a probe's refusal.
pub fn preflight_many(wants: &[(&Path, u64)], min_free_percent: u8) -> Result<()> {
    let planned = wants
        .iter()
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(path, bytes)| locate(path, *bytes))
        .collect::<Result<Vec<_>>>()?;
    check_planned(&planned, min_free_percent)
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

    // BL2-F5: the faked reading round-trips, and a malformed one is a
    // refusal, never a silent real probe.
    #[cfg(debug_assertions)]
    #[test]
    fn the_test_space_value_round_trips_and_a_malformed_one_is_refused() {
        let value = TEST_SPACE_ROOMY.test_env_value();
        assert_eq!(
            parse_test_space(std::ffi::OsStr::new(&value)),
            Ok(TEST_SPACE_ROOMY)
        );
        assert_eq!(
            parse_test_space(std::ffi::OsStr::new("100:100")),
            Ok(Space {
                total: 100,
                available: 100
            })
        );
        for bad in ["", "100", "100:", ":100", "100:101", "-1:0", "1:x", "1:2:3"] {
            assert_eq!(
                parse_test_space(std::ffi::OsStr::new(bad)),
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

    const QUOTA: Space = Space {
        total: 1_400 * GIB,
        available: 74 * GIB,
    };
    const FILESYSTEM: Space = Space {
        total: 1_600 * GIB,
        available: 174 * GIB,
    };

    #[test]
    fn a_root_view_of_another_size_makes_the_path_view_a_quota() {
        let limits = Limits::from_views(QUOTA, Some(FILESYSTEM));
        assert_eq!(limits.filesystem, FILESYSTEM);
        assert_eq!(limits.quota, Some(QUOTA));
        // The same size (no quota, or the path is the root) or no root view:
        // the path's view is the filesystem's.
        for root in [Some(QUOTA), None] {
            assert_eq!(Limits::from_views(QUOTA, root), Limits::filesystem(QUOTA));
        }
        // A larger view at the path is no quota (review A1: a root that
        // reads another, smaller filesystem).
        assert_eq!(
            Limits::from_views(FILESYSTEM, Some(QUOTA)),
            Limits::filesystem(FILESYSTEM)
        );
    }

    #[test]
    fn each_limit_keeps_the_floor_of_its_own_size_and_the_bound_is_named() {
        let limits = Limits::from_views(QUOTA, Some(FILESYSTEM));
        // 5%: the quota keeps 70 of 1400 (5%) after 4 GiB; the filesystem
        // 170 of 1600.
        assert_eq!(bound(4 * GIB, 4 * GIB, limits, 5), None);
        // The quota binds first: 69 left of 1400 is under 5%.
        assert_eq!(
            bound(5 * GIB, 5 * GIB, limits, 5),
            Some(Bound::ProjectQuota)
        );
        // 10%: the filesystem (174 - 15 = 159 < 160) binds while the quota
        // (floor 140, 74 free) already did.
        assert_eq!(bound(15 * GIB, 15 * GIB, limits, 10), Some(Bound::Both));
        // Bytes planned elsewhere on the filesystem count there only.
        let roomy = Limits {
            filesystem: FILESYSTEM,
            quota: Some(Space {
                total: 100 * GIB,
                available: 90 * GIB,
            }),
        };
        assert_eq!(bound(GIB, 20 * GIB, roomy, 10), Some(Bound::Filesystem));
        assert_eq!(bound(GIB, GIB, roomy, 10), None);
        // Without a quota only the filesystem is judged, as before.
        assert_eq!(
            bound(15 * GIB, 15 * GIB, Limits::filesystem(FILESYSTEM), 10),
            Some(Bound::Filesystem)
        );
    }

    #[test]
    fn a_refusal_records_the_bound_and_both_numbers() {
        let path = Path::new("/srv/fast-local/jess/git");
        let limits = Limits::from_views(QUOTA, Some(FILESYSTEM));
        let refused_quota = crate::counters::Counters::snapshot();
        assert_eq!(check_at(path, (GIB, GIB), limits, 5), Ok(()));
        assert_eq!(
            check_at(path, (5 * GIB, 5 * GIB), limits, 5),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        let after = crate::counters::Counters::snapshot();
        assert!(
            after.get(Counter::SpaceRefusedQuota) > refused_quota.get(Counter::SpaceRefusedQuota)
        );
        assert!(after.get(Counter::SpaceChecks) >= refused_quota.get(Counter::SpaceChecks) + 2);
        // Other tests in this process may record a refusal too; this one's
        // fields are checked directly.
        let refused = Refused {
            path: path.to_owned(),
            bound: Bound::ProjectQuota,
            planned: 5 * GIB,
            filesystem_planned: 5 * GIB,
            min_free_percent: 5,
            limits,
        };
        assert_eq!(
            refused.fields(),
            format!(
                "bound=project-quota planned_bytes={} filesystem_planned_bytes={} \
                 min_free_percent=5 filesystem_total_bytes={} filesystem_available_bytes={} \
                 quota=project quota_total_bytes={} quota_available_bytes={} \
                 path=/srv/fast-local/jess/git",
                5 * GIB,
                5 * GIB,
                1_600 * GIB,
                174 * GIB,
                1_400 * GIB,
                74 * GIB
            )
        );
        assert!(last_refusal().is_some());
        let plain = Refused {
            limits: Limits::filesystem(FILESYSTEM),
            bound: Bound::Filesystem,
            ..refused
        };
        assert!(
            plain.fields().contains("bound=filesystem "),
            "{}",
            plain.fields()
        );
        assert!(
            plain.fields().contains(" quota=none "),
            "{}",
            plain.fields()
        );
    }

    #[test]
    fn the_root_mount_of_a_device_is_found_and_a_subtree_bind_is_not_it() {
        let table = b"22 1 253:0 / / rw,relatime shared:1 - xfs /dev/mapper/rl-root rw\n\
            97 22 253:5 /jess/git /home/jess/git rw,relatime shared:2 - xfs /dev/mapper/fast rw,prjquota\n\
            98 22 253:5 / /srv/fast\\040local rw,relatime shared:3 - xfs /dev/mapper/fast rw,prjquota\n\
            99 22 253:7 /sub /mnt/sub rw - xfs /dev/mapper/other rw\n";
        assert_eq!(
            mounts_of_root(table, 253, 5),
            vec![PathBuf::from("/srv/fast local")]
        );
        assert_eq!(mounts_of_root(table, 253, 0), vec![PathBuf::from("/")]);
        // Only a subtree of 253:7 is mounted: no root view.
        assert!(mounts_of_root(table, 253, 7).is_empty());
        assert!(mounts_of_root(table, 8, 1).is_empty());
        assert!(mounts_of_root(b"", 253, 5).is_empty());
        // Two mounts of one root (one may be hidden under a stacked mount):
        // both are candidates, in table order; the caller keeps the first
        // that still resolves to the device.
        let twice = b"1 0 253:5 / /a rw - xfs x rw\n2 0 253:5 / /b rw - xfs x rw\n";
        assert_eq!(
            mounts_of_root(twice, 253, 5),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert_eq!(unescape_mountinfo(br"a\011b\134c\012"), b"a\tb\\c\n");
        // Not an escape: kept as it is.
        assert_eq!(unescape_mountinfo(br"a\9b\0"), br"a\9b\0");
    }

    #[test]
    fn planned_bytes_sum_per_quota_and_per_filesystem() {
        let quota_a = Space {
            total: 100 * GIB,
            available: 30 * GIB,
        };
        let quota_b = Space {
            total: 200 * GIB,
            available: 150 * GIB,
        };
        let filesystem = Space {
            total: 1_000 * GIB,
            available: 300 * GIB,
        };
        let at = |path: &str, quota, bytes| Planned {
            path: PathBuf::from(path),
            device: 5,
            limits: Limits::from_views(quota, Some(filesystem)),
            bytes,
        };
        // Two paths under quota A share its 30 GiB: 10 + 10 leaves 10 of
        // 100 (10%) free.
        let fits = [
            at("/a/one", quota_a, 10 * GIB),
            at("/a/two", quota_a, 10 * GIB),
            at("/b", quota_b, 40 * GIB),
        ];
        assert_eq!(check_planned(&fits, 10), Ok(()));
        let over = [
            at("/a/one", quota_a, 10 * GIB),
            at("/a/two", quota_a, 10 * GIB + 1),
        ];
        assert_eq!(
            check_planned(&over, 10),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        // Each quota fits on its own, but together they take the filesystem
        // (300 free, floor 100) under its floor.
        let together = [
            at("/a", quota_a, 20 * GIB),
            at("/b", quota_b, 130 * GIB),
            at(
                "/c",
                Space {
                    total: 500 * GIB,
                    available: 400 * GIB,
                },
                60 * GIB,
            ),
        ];
        assert_eq!(
            check_planned(&together, 10),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        // Zero bytes are never judged.
        assert_eq!(check_planned(&[at("/a", quota_a, 0)], 100), Ok(()));
        // Review A2: two trees under quotas of one size are judged by the
        // least room either reported, whichever path came first.
        let roomy = Space {
            total: 100 * GIB,
            available: 90 * GIB,
        };
        let tight = Space {
            total: 100 * GIB,
            available: 5 * GIB,
        };
        let alike = [at("/a", roomy, 1), at("/b", tight, 10 * GIB)];
        assert_eq!(
            check_planned(&alike, 0),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        let reversed = [at("/b", tight, 10 * GIB), at("/a", roomy, 1)];
        assert_eq!(
            check_planned(&reversed, 0),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_hosts_limits_read_through_mountinfo() {
        let root = tempfile::tempdir().unwrap();
        let limits = probe_limits(root.path()).unwrap();
        assert!(limits.filesystem.total > 0);
        if let Some(quota) = limits.quota {
            // Only a project quota reads differently from its filesystem.
            assert_ne!(quota.total, limits.filesystem.total);
        }
    }

    /// A live reading of a real project-quota tree, read-only: set
    /// `BULKLOAD_QUOTA_PROBE` to a directory inside one (on sting,
    /// `/srv/cache/jess`, read as a 386.5 GB (360 GiB) quota on a 429.3 GB
    /// filesystem, 2026-10-17) and run it ignored.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs BULKLOAD_QUOTA_PROBE: a directory inside a project quota"]
    fn a_real_project_quota_is_told_from_its_filesystem() {
        let path = PathBuf::from(std::env::var_os("BULKLOAD_QUOTA_PROBE").unwrap());
        let limits = probe_limits(&path).unwrap();
        assert!(
            limits
                .quota
                .is_some_and(|quota| quota.total < limits.filesystem.total),
            "{limits:?}"
        );
        println!("{limits:?}");
    }

    /// Review A4: a destination under a dangling symlink cannot be located;
    /// the caller decides (the apply's space plan skips that item, as it
    /// did, and lets it refuse on its own).
    #[test]
    fn a_destination_under_a_dangling_link_is_not_located() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("dangling");
        std::os::unix::fs::symlink(root.path().join("gone"), &link).unwrap();
        assert!(locate(&link.join("repository"), 1).is_err());
        let found = locate(&root.path().join("absent/repository"), 7).unwrap();
        assert_eq!((found.path.as_path(), found.bytes), (root.path(), 7));
    }
}
