//! Group commit for the transfer engine (M2 W3).
//!
//! A committer thread collects work items and makes them durable in groups.
//! A group closes at [`GROUP_FILES`] files, [`GROUP_BYTES`] bytes,
//! [`GROUP_IDLE`] without a new item, or, when its limits set one, an age
//! since its first item ([`Limits::group_age`]); a caller's
//! [`Committer::sync`] closes it at once. For each group the sink:
//!
//! 1. seals every file's data with [`seal_file`]: `F_BARRIERFSYNC` on Darwin,
//!    `fsync` elsewhere, or a full flush under [`Durability::Strict`];
//! 2. publishes and seals each touched directory once with [`seal_dir`];
//! 3. fully flushes each touched device other than the store's own;
//! 4. commits the group's records in one `SQLite` WAL transaction. With
//!    `synchronous=FULL` and `fullfsync=ON` that commit's `F_FULLFSYNC`
//!    drains the store's device, which is the only device-cache flush a
//!    group needs when its files share that device.
//!
//! On Linux 5.17 (rc3) or later in group mode, a group of
//! [`BATCH_MIN_FILES`] or more outputs whose devices are all on the
//! [`syncfs_seals`] allowlist is sealed in two device-wide steps instead
//! (S1, OI-1003-Q107): each output's write-back starts as it is queued
//! ([`start_writeback`]); the group then runs one device seal per device
//! number its outputs live on ([`seal_device`]: `syncfs`, then `fsync` of
//! the same descriptor), which makes every temporary's data durable under
//! its temporary name, and checks each file's own write-back error
//! ([`check_writeback`]); renames every output into place (or, superseding
//! one, exchanges it in: WP0(d)); and runs one device seal per touched
//! device number again, which makes the new entries durable, before the
//! records commit. Each btrfs subvolume has its own device number, so a
//! group spread over N subvolumes of one btrfs pays N transaction commits
//! per step. The
//! renames never precede the first seal: a name must not survive a power
//! loss that its data did not.
//!
//! The load-bearing order is data before record: a record never commits
//! before the bytes it describes are sealed and, on another device, flushed.
//! A sink that fails stops the committer's callers at their next
//! [`Committer::submit`] or [`Committer::sync`]. Dropping a [`Committer`]
//! closes and commits whatever is pending, so an interrupted transfer keeps
//! the work it finished.

use crate::refuse::RefuseAt as _;
use std::fs::File;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// Close a group once it holds this many files.
pub const GROUP_FILES: u64 = 64;
/// Close a group once it holds this many payload bytes.
pub const GROUP_BYTES: u64 = 256 * 1024 * 1024;
/// Close a group after this long without a new item.
pub const GROUP_IDLE: Duration = Duration::from_millis(20);
/// Queued items before [`Committer::submit`] blocks the producer.
pub const QUEUE_DEPTH: usize = 64;

/// How each file is made durable before its record commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Durability {
    /// A barrier per file and per touched directory; the group's `SQLite`
    /// commit is the one full flush.
    #[default]
    Group,
    /// A full flush (`F_FULLFSYNC` / `fsync`) per file and per directory, for
    /// A/B comparison against [`Durability::Group`].
    Strict,
}

impl std::str::FromStr for Durability {
    type Err = BulkloadRefusal;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "group" => Ok(Self::Group),
            "strict" => Ok(Self::Strict),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }
}

impl std::fmt::Display for Durability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Group => "group",
            Self::Strict => "strict",
        })
    }
}

/// How the SOURCE ledger's row commits reach disk (WP0(g): OI-1003-Q20,
/// adopted with conditions by OI-1003-Q37, built on OI-1003-Q104).
///
/// The source ledger is a cache of what the source already holds: a row is
/// a seat's row key and its chunk manifest, or a remembered refusal (#186).
/// R25 is carried by the destination's committed rows, so losing a ledger
/// row costs at most one more read (or sniff) of its seat, and only where
/// the destination no longer holds that seat.
///
/// The mode covers the ledger's ROW commits and nothing else
/// (`StorePublisher::commit_captures` on a source publisher). The commit
/// that creates a store (its schema and its authority, in `Store::open`),
/// the state root's seals (#161) and every destination commit are
/// `synchronous=FULL`, `fullfsync=ON` in both modes
/// ([`configure_sqlite`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LedgerSync {
    /// `synchronous=NORMAL`, `fullfsync=OFF` on the source publisher's
    /// connection ([`relax_ledger_rows`]). A row commit appends to the WAL
    /// without syncing it, so a power loss may roll back the most recent
    /// row commits; a process crash loses none. A failed row commit is
    /// counted (`source_ledger_commit_failed`) and the transfer goes on
    /// (#163). The default (OI-1003-Q37).
    #[default]
    Relaxed,
    /// `synchronous=FULL`, `fullfsync=ON`: every row commit is durable when
    /// it returns, and the first failed one fails the session, as before
    /// WP0(g). For A/B comparison against [`LedgerSync::Relaxed`].
    Full,
}

impl std::str::FromStr for LedgerSync {
    type Err = BulkloadRefusal;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "relaxed" => Ok(Self::Relaxed),
            "full" => Ok(Self::Full),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }
}

impl std::fmt::Display for LedgerSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Relaxed => "relaxed",
            Self::Full => "full",
        })
    }
}

static MODE: AtomicU8 = AtomicU8::new(0);
static LEDGER_SYNC: AtomicU8 = AtomicU8::new(0);

/// Set the process-wide source-ledger sync mode (`--source-ledger-sync`).
pub fn set_ledger_sync(mode: LedgerSync) {
    LEDGER_SYNC.store(u8::from(mode == LedgerSync::Full), Ordering::Relaxed);
}

/// The process-wide source-ledger sync mode.
#[must_use]
pub fn ledger_sync() -> LedgerSync {
    if LEDGER_SYNC.load(Ordering::Relaxed) == 0 {
        LedgerSync::Relaxed
    } else {
        LedgerSync::Full
    }
}

/// Set the process-wide durability mode (`--durability`).
pub fn set_durability(mode: Durability) {
    MODE.store(u8::from(mode == Durability::Strict), Ordering::Relaxed);
}

/// The process-wide durability mode.
#[must_use]
pub fn durability() -> Durability {
    if MODE.load(Ordering::Relaxed) == 0 {
        Durability::Group
    } else {
        Durability::Strict
    }
}

/// The smallest group sealed with [`seal_device`] rather than file by file:
/// for one file a `syncfs` costs no less than its own flush, and it also
/// flushes whatever else is dirty on that file system.
pub const BATCH_MIN_FILES: usize = 2;

/// The first Linux release whose `syncfs` reports the file system's own
/// sync failure (its rc1 and rc2 do not: [`release_meets_floor`]).
///
/// From 5.8 `syncfs` reports a data write-back error of the file system
/// (errseq on the superblock, checked against the calling descriptor's
/// cursor). Until 5.17 `sync_filesystem` still threw away the return value of
/// the file system's `->sync_fs` (ext4's journal commit or cache flush, xfs's
/// log force, btrfs's transaction commit), so `syncfs` could return 0 after
/// those failed. Fixed by torvalds/linux commit 5679897eb104 ("vfs: make
/// `sync_filesystem` return errors from `->sync_fs`", Darrick J. Wong,
/// 2022-01-30); xfs needed 2d86293c7075 ("xfs: return errors in
/// `xfs_fs_sync_fs`") as well, since before it `xfs_fs_sync_fs` ignored its
/// own log force's result. Both are first tagged in v5.17-rc3 (neither is in
/// v5.17-rc2) and released in v5.17 (checked against the upstream tree on
/// 2026-10-08). An older kernel keeps the per-file seals (OI-1003-Q113 set
/// 5.8; OI-1003-Q141, ruled 2026-10-08 (Linear TIN-4543 comment 7734678e),
/// moved it to 5.17 on these commits, from the OI-1003-Q107 review).
///
/// A distribution kernel below 5.17 that carries the fix as a backport is
/// still treated as too old: only the release number is read.
pub const SYNCFS_REPORTS_ERRORS_SINCE: (u32, u32) = (5, 17);

/// Whether a group of `files` outputs may be sealed device-wide.
///
/// Linux 5.17 or later, group mode, at least [`BATCH_MIN_FILES`]. The group
/// is batched only if, in addition, every device it touches passes
/// [`syncfs_seals_handle`].
#[must_use]
pub fn batched(files: usize) -> bool {
    cfg!(target_os = "linux")
        && durability() == Durability::Group
        && files >= BATCH_MIN_FILES
        && syncfs_reports_errors()
}

/// Whether this kernel's `syncfs` reports write-back and `->sync_fs`
/// errors, read once. An unreadable release counts as too old.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::missing_const_for_fn,
        reason = "the body is Linux-only; a const fn on the other platforms alone would be no API"
    )
)]
fn syncfs_reports_errors() -> bool {
    #[cfg(target_os = "linux")]
    {
        static REPORTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *REPORTS.get_or_init(|| {
            super::sys::kernel_release()
                .as_deref()
                .is_some_and(release_meets_floor)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// `f_type` of ext4 (shared with ext2 and ext3: see [`syncfs_seals`]).
pub const EXT4_SUPER_MAGIC: u32 = 0xEF53;
/// `f_type` of xfs.
pub const XFS_SUPER_MAGIC: u32 = 0x5846_5342;
/// `f_type` of btrfs.
pub const BTRFS_SUPER_MAGIC: u32 = 0x9123_683E;
/// `f_type` of tmpfs.
pub const TMPFS_MAGIC: u32 = 0x0102_1994;
/// `f_type` of FUSE (not on the allowlist; named for tests).
pub const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// Whether a file system of type `magic` may take the group's device seal
/// ([`seal_device`]) in place of the per-file and per-directory seals.
///
/// `magic` is `fstatfs`'s `f_type`; `fstype` is the mount's type name,
/// needed only for the ext magic. `syncfs` alone is not as strong as those
/// seals even here, and does not report every failure they would: with an
/// idle log xfs's sends no device-cache flush, and a shut-down ext4's
/// returns 0 (`ext4_sync_fs`, v5.17 and v6.12) where its `fsync` returns
/// `EIO`. The batch is sound on the allowlist only together with the
/// `fsync` that ends each device seal and the per-file
/// [`check_writeback`] (kernel floor: [`SYNCFS_REPORTS_ERRORS_SINCE`]). The
/// allowlist (OI-1003-Q107 review):
///
/// - ext4 (and ext3, which ext4 serves): `ext4_sync_fs` commits the journal
///   and flushes the device, or flushes it alone without a journal. The
///   ext2 driver shares the magic, but `ext2_sync_fs` never flushes the
///   device cache while its `fsync` does, so the ext magic is allowed only
///   when the mount's type is `ext4` or `ext3`.
/// - xfs (`xfs_fs_sync_fs` forces the log; the seal's `fsync` flushes the
///   data device when the log force was a no-op) and btrfs (`btrfs_sync_fs`
///   commits the transaction). An xfs realtime file is refused by
///   [`syncfs_seals_handle`]: its data is on the realtime device, which
///   neither flushes.
/// - tmpfs: there is nothing to make durable; its `fsync` is a no-op too,
///   so the batch is exactly as strong as the per-file path.
///
/// Everything else keeps the per-file seals, among them FUSE other than
/// virtiofs (`fuse_sync_fs` sends nothing unless the connection opted in,
/// while `fsync` sends `FUSE_FSYNC`), CIFS/SMB and NFS (no `->sync_fs` that
/// reaches the server, while `fsync` flushes there), 9p, vfat and exFAT
/// (`syncfs` sends no device-cache flush, their `fsync` does), overlayfs
/// (upper-layer write-back errors are not reported through the overlay's
/// superblock), and f2fs (`f2fs_sync_fs` returns 0 without a checkpoint
/// when checkpointing is disabled or has failed).
#[must_use]
pub fn syncfs_seals(magic: u32, fstype: Option<&str>) -> bool {
    match magic {
        XFS_SUPER_MAGIC | BTRFS_SUPER_MAGIC | TMPFS_MAGIC => true,
        EXT4_SUPER_MAGIC => matches!(fstype, Some("ext4" | "ext3")),
        _ => false,
    }
}

/// The type name (`ext4`, `ext2`, ...) of the mount of device
/// `(major, minor)` in a `/proc/self/mountinfo` listing, if it has one.
#[must_use]
pub fn mount_fstype(mountinfo: &str, device: (u32, u32)) -> Option<&str> {
    let wanted = format!("{}:{}", device.0, device.1);
    mountinfo.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.nth(2)? != wanted {
            return None;
        }
        fields.skip_while(|field| *field != "-").nth(1)
    })
}

/// Whether the device holding `handle` may be sealed device-wide
/// ([`syncfs_seals`]).
///
/// Its type is read with `fstatfs` on every call; nothing is cached across
/// groups, because an anonymous device number (tmpfs, a btrfs subvolume)
/// can be handed to another file system after an unmount. For the ext magic
/// the mount's type is looked up by device number in `mountinfo`, which is
/// filled from `/proc/self/mountinfo` on first use (a group shares one).
/// On xfs a file flagged `FS_XFLAG_REALTIME` (read with
/// `FS_IOC_FSGETXATTR`) answers `false`: its data is on the realtime
/// device, which the group's seal does not flush. Anything that cannot be
/// read answers `false`, the per-file path.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::missing_const_for_fn,
        reason = "the body is Linux-only; a const fn on the other platforms alone would be no API"
    )
)]
#[must_use]
pub fn syncfs_seals_handle(handle: &File, mountinfo: &std::cell::OnceCell<Option<String>>) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Some(magic) = fs_magic(handle) else {
            return false;
        };
        // A realtime file's data lives on the realtime device, which neither
        // the log force nor the seal's `fsync` of another file flushes.
        if magic == XFS_SUPER_MAGIC
            && xflags(handle).is_none_or(|flags| flags & FS_XFLAG_REALTIME != 0)
        {
            return false;
        }
        if magic != EXT4_SUPER_MAGIC {
            return syncfs_seals(magic, None);
        }
        let Ok(metadata) = handle.metadata() else {
            return false;
        };
        let device = std::os::unix::fs::MetadataExt::dev(&metadata);
        let Some(mountinfo) = mountinfo
            .get_or_init(|| std::fs::read_to_string("/proc/self/mountinfo").ok())
            .as_deref()
        else {
            return false;
        };
        syncfs_seals(
            magic,
            mount_fstype(mountinfo, (libc::major(device), libc::minor(device))),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (handle, mountinfo);
        false
    }
}

/// `fstatfs`'s `f_type` of the file system holding `handle` (or the
/// [`force_fs_magic`] override in tests), if it can be read.
#[cfg(target_os = "linux")]
fn fs_magic(handle: &File) -> Option<u32> {
    #[cfg(test)]
    if let Some(forced) = FORCE_FS_MAGIC.with(std::cell::Cell::get) {
        return Some(forced);
    }
    super::sys::fs_type(handle).ok()
}

/// The `FS_IOC_FSGETXATTR` flags of `handle` (or the [`force_xflags`]
/// override in tests), if they can be read.
#[cfg(target_os = "linux")]
fn xflags(handle: &File) -> Option<u32> {
    #[cfg(test)]
    if let Some(forced) = FORCE_XFLAGS.with(std::cell::Cell::get) {
        return Some(forced);
    }
    super::sys::fs_xflags(handle).ok()
}

/// `FS_XFLAG_REALTIME`: an xfs file whose data lives on the realtime device.
pub const FS_XFLAG_REALTIME: u32 = 0x1;

#[cfg(test)]
thread_local! {
    static FORCE_FS_MAGIC: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static FORCE_XFLAGS: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static FAIL_WRITEBACK: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
    static DEVICE_SEALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SUPERBLOCK_ERROR: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static SUPERBLOCK_REPORTED: std::cell::RefCell<std::collections::HashSet<(u64, u64, i32)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Test hook: while `Some(dev)`, the superblock of device `dev` holds a
/// write-back error raised after every descriptor this test opened, and
/// before any [`seal_device`] opened its own.
///
/// It models `syncfs`'s errseq check (Linux 5.8,
/// `errseq_check_and_advance(&sb->s_wb_err, &file->f_sb_err)`, the cursor
/// sampled when the descriptor was opened): this thread's [`seal_device`]
/// fails with `EIO` once per descriptor that predates the error, and never
/// on a descriptor the seal opened itself.
#[cfg(test)]
pub fn superblock_error(device: Option<u64>) {
    SUPERBLOCK_ERROR.with(|error| error.set(device));
    SUPERBLOCK_REPORTED.with(|reported| reported.borrow_mut().clear());
}

/// The [`superblock_error`] model's answer for a `syncfs` on `through`,
/// which the seal opened itself when `fresh`.
#[cfg(all(test, target_os = "linux"))]
fn superblock_reports(through: &File, fresh: bool) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let Some(device) = SUPERBLOCK_ERROR.with(std::cell::Cell::get) else {
        return Ok(());
    };
    let metadata = through.metadata()?;
    let descriptor = (
        std::os::unix::fs::MetadataExt::dev(&metadata),
        std::os::unix::fs::MetadataExt::ino(&metadata),
        through.as_raw_fd(),
    );
    if fresh
        || descriptor.0 != device
        || !SUPERBLOCK_REPORTED.with(|reported| reported.borrow_mut().insert(descriptor))
    {
        return Ok(());
    }
    Err(std::io::Error::from_raw_os_error(libc::EIO))
}

/// Test hook: while `Some`, this thread's [`syncfs_seals_handle`] takes
/// that `f_type` in place of `fstatfs`'s answer.
#[cfg(test)]
pub fn force_fs_magic(magic: Option<u32>) {
    FORCE_FS_MAGIC.with(|forced| forced.set(magic));
}

/// Test hook: while `Some`, this thread reads those `FS_IOC_FSGETXATTR`
/// flags for every handle in place of the ioctl's answer.
#[cfg(test)]
pub fn force_xflags(flags: Option<u32>) {
    FORCE_XFLAGS.with(|forced| forced.set(flags));
}

/// Test hook: while `Some((dev, ino))`, this thread's [`check_writeback`]
/// of that file fails with `EIO` after the real call.
///
/// It stands in for a write-back error an earlier `syncfs` already
/// consumed, which only this per-file check still reports.
#[cfg(test)]
pub fn fail_writeback_of(node: Option<(u64, u64)>) {
    FAIL_WRITEBACK.with(|failing| failing.set(node));
}

/// Test hook: how many [`seal_device`] calls this thread has made.
#[cfg(test)]
#[must_use]
pub fn device_seals() -> usize {
    DEVICE_SEALS.with(std::cell::Cell::get)
}

/// Whether the kernel release string `release` is at or above
/// [`SYNCFS_REPORTS_ERRORS_SINCE`].
///
/// A 5.17 release candidate before rc3 is below it: both commits the floor
/// rests on, 5679897eb104 (VFS) and 2d86293c7075 (xfs), are first in
/// v5.17-rc3. The candidate number is read from an `rcN` that follows a
/// `-` or `.` (`5.17.0-rc2`, Fedora's `5.17.0-0.rc2.<date>git...`). An
/// unreadable release is below the floor.
#[must_use]
pub fn release_meets_floor(release: &str) -> bool {
    /// The first 5.17 release candidate that carries both commits.
    const FIRST_CANDIDATE: u32 = 3;
    let Some(version) = release_version(release) else {
        return false;
    };
    if version != SYNCFS_REPORTS_ERRORS_SINCE {
        return version > SYNCFS_REPORTS_ERRORS_SINCE;
    }
    let candidate = release
        .split(['-', '.'])
        .filter_map(|part| part.strip_prefix("rc"))
        .find_map(|digits| {
            let end = digits
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(digits.len());
            digits[..end].parse::<u32>().ok()
        });
    candidate.is_none_or(|number| number >= FIRST_CANDIDATE)
}

/// `(major, minor)` of a kernel release string such as `6.12.0-211.el10`.
#[must_use]
pub fn release_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Start write-back of a fully written output as it is queued for its group.
///
/// Linux, group mode only, so the group's [`seal_device`] finds less to
/// write. Not a durability step: a failure is ignored, and the group seal
/// still makes the data durable.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::missing_const_for_fn,
        reason = "the body is Linux-only; a const fn on the other platforms alone would be no API"
    )
)]
pub fn start_writeback(file: &File) {
    #[cfg(target_os = "linux")]
    if durability() == Durability::Group {
        let _ = super::sys::start_writeback(file);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = file;
}

/// Start write-back of a staged file while its data still streams
/// (OI-1003-Q143 item 1a).
///
/// `sync_file_range(SYNC_FILE_RANGE_WRITE)` over the whole file queues only
/// dirty pages not already under write-back. Linux, group mode only, like
/// [`start_writeback`]; a no-op elsewhere.
///
/// Nothing is durable by it, and the file's seal is unchanged. Without
/// `WAIT_AFTER` the call never reaches `file_check_and_advance_wb_err`, so
/// it leaves the descriptor's errseq cursor alone, and a later write-back
/// error is still reported by the file's own `fsync`, or by a batched
/// group's write-back check.
///
/// # Errors
/// Returns the failed call: the caller refuses the entry with it.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::missing_const_for_fn,
        reason = "the body is Linux-only; a const fn on the other platforms alone would be no API"
    )
)]
pub fn kick_writeback(file: &File) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    if durability() == Durability::Group {
        counters::bump(Counter::WritebackKicks);
        return super::sys::start_writeback(file);
    }
    let _ = file;
    Ok(())
}

/// Seal every file of the file system holding `handle`: its data, metadata
/// and directory entries.
///
/// Linux `syncfs` on a descriptor of `directory` (a directory on the same
/// file system) opened for this seal, then `fsync` of `handle` itself,
/// which flushes the device cache where the `syncfs` did not (an idle xfs
/// log) and fails where it returned 0 (a shut-down ext4); see
/// `sys::sync_fs`. Only a [`batched`] group calls it, on a regular file of
/// the group and its parent in its first step, and on a touched directory
/// (as both) in its second.
///
/// `syncfs` reports the superblock's write-back error against the cursor of
/// the descriptor it runs on, sampled when that descriptor was opened. A
/// long-lived descriptor (a staged temporary, a destination directory) would
/// report an error raised since its open again for every later group whose
/// seal happens to run on another such descriptor, so one bad file, or
/// another process's error on a shared file system, could refuse every
/// later healthy group (S1 stack review, 2026-10-15, finding 1). The fresh
/// descriptor reports only what is raised during the seal. The group's own
/// data errors are each member's [`check_writeback`]; the file system's own
/// `->sync_fs` failure is returned by `syncfs` whatever the cursor. If the
/// directory cannot be opened again (no read permission), `syncfs` runs on
/// `directory` itself: that can only report more, never less.
///
/// # Errors
/// Returns the flush failure; `Unsupported` off Linux.
pub fn seal_device(handle: &File, directory: &File) -> std::io::Result<()> {
    #[cfg(test)]
    DEVICE_SEALS.with(|seals| seals.set(seals.get() + 1));
    #[cfg(target_os = "linux")]
    {
        let fresh = super::sys::open_dir_at(directory, c".").map(File::from);
        let through = fresh.as_ref().unwrap_or(directory);
        #[cfg(test)]
        superblock_reports(through, fresh.is_ok())?;
        counters::timed(Counter::FlushFs, Counter::FlushFsNs, || {
            super::sys::sync_fs(through, handle)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (handle, directory);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// Check one file's own write-back after its group's [`seal_device`].
///
/// Linux `sync_file_range(WAIT_BEFORE | WRITE | WAIT_AFTER)` over the whole
/// file, which waits for its pages and returns its own write-back error
/// (`file_check_and_advance_wb_err` against this descriptor's cursor), with
/// no device-cache flush of its own.
///
/// `syncfs` reports a write-back error only against the cursor of the one
/// descriptor it is called on, so an error an earlier `syncfs` elsewhere has
/// already consumed is not reported by the group's. The per-file path's
/// `fsync` checks each file's own cursor; this restores that check for the
/// batched path (OI-1003-Q107 review).
///
/// # Errors
/// Returns the file's write-back error; `Unsupported` off Linux.
pub fn check_writeback(file: &File) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        super::sys::wait_writeback(file)?;
        #[cfg(test)]
        if let Some(node) = FAIL_WRITEBACK.with(std::cell::Cell::get) {
            let metadata = file.metadata()?;
            let found = (
                std::os::unix::fs::MetadataExt::dev(&metadata),
                std::os::unix::fs::MetadataExt::ino(&metadata),
            );
            if found == node {
                return Err(std::io::Error::from_raw_os_error(libc::EIO));
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// Seal one file's data ahead of any record that describes it.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_file(file: &File) -> std::io::Result<()> {
    match durability() {
        Durability::Group => {
            counters::timed(Counter::FlushBarrier, Counter::FlushBarrierNs, || {
                file_seal(file)
            })
        }
        Durability::Strict => counters::timed(Counter::FlushFull, Counter::FlushFullNs, || {
            super::sys::full_flush(file)
        }),
    }
}

/// The group-mode seal for one file: `F_BARRIERFSYNC` on Darwin. Elsewhere
/// `fsync`, not the io layer's `fdatasync` barrier: a mode set with `fchmod`
/// after the last write must be durable with the data (PR #59 review, F4).
#[cfg(target_vendor = "apple")]
fn file_seal(file: &File) -> std::io::Result<()> {
    super::sys::barrier(file)
}

#[cfg(not(target_vendor = "apple"))]
fn file_seal(file: &File) -> std::io::Result<()> {
    super::sys::full_flush(file)
}

#[cfg(test)]
thread_local! {
    static FAIL_DIR_SEALS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, this thread's [`seal_dir`] calls fail with `EIO`
/// before any syscall, as a device error would.
#[cfg(test)]
pub fn fail_dir_seals(on: bool) {
    FAIL_DIR_SEALS.with(|flag| flag.set(on));
}

/// Seal one directory's entries ahead of any record that depends on them.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_dir(directory: &File) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DIR_SEALS.with(std::cell::Cell::get) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    match durability() {
        Durability::Group => {
            counters::timed(Counter::FlushDirBarrier, Counter::FlushDirBarrierNs, || {
                super::sys::barrier_dir(directory)
            })
        }
        Durability::Strict => counters::timed(Counter::FlushDir, Counter::FlushDirNs, || {
            super::sys::full_flush(directory)
        }),
    }
}

/// Make a private state root durable now, whatever the [`Durability`] mode.
///
/// #161, R25: the root's entry in `parent` and the entries it holds (a
/// store's database and WAL). A store's authority leaves the host at Start
/// and its records are committed inside the root, so ordering is not
/// enough: both are on stable media when this returns.
///
/// `parent` is sealed with [`seal_dir`] and `root` with a full flush
/// (counted as `flush_dir`). On Darwin that full flush also drains the
/// drive, and with it the parent's barrier; a parent on another device (the
/// root is a mount point) is fully flushed as well.
///
/// # Errors
/// Returns the flush failure.
pub fn seal_state_root(parent: &File, root: &File) -> std::io::Result<()> {
    seal_dir(parent)?;
    counters::sync_dir(root)?;
    if super::sys::fstat(parent)?.node.dev != super::sys::fstat(root)?.node.dev {
        counters::sync_dir(parent)?;
    }
    Ok(())
}

/// Configure a bulkload-owned `SQLite` store for durable commits: WAL,
/// `synchronous=FULL`, and `fullfsync=ON` with `checkpoint_fullfsync=ON`.
///
/// Bundled `SQLite` on Darwin issues a plain `fsync` unless `fullfsync` is on,
/// and a plain Darwin `fsync` neither drains the device cache nor orders
/// writes, so a commit could be lost or torn by power loss. With these
/// settings each commit syncs the WAL with `F_FULLFSYNC`; a commit that runs
/// an automatic checkpoint (at 1,000 WAL pages) also syncs the WAL and the
/// database file. Checkpoint-on-close is off:
/// the WAL is durable, so closing a store never adds a flush of its own, and
/// automatic checkpoints happen inside later commits.
///
/// Never apply this to a provider database; those are not bulkload's to
/// reconfigure.
///
/// # Errors
/// Refuses if `SQLite` rejects a setting or will not enter WAL mode.
pub fn configure_sqlite(conn: &rusqlite::Connection) -> Result<()> {
    let refuse = |_| BulkloadRefusal::SqliteIntegrityCheckFailed;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(refuse)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    conn.execute_batch(
        "PRAGMA synchronous=FULL;
        PRAGMA fullfsync=ON;
        PRAGMA checkpoint_fullfsync=ON;",
    )
    .map_err(refuse)?;
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(refuse)?;
    Ok(())
}

/// Bytes of page 1 of a store created directly in WAL mode
/// ([`wal_database_template`]).
pub const WAL_TEMPLATE_BYTES: usize = 4096;

/// Where the template keeps the version number of the `SQLite` that wrote
/// it (`SQLITE_VERSION_NUMBER`, big-endian): any value is a valid header.
const TEMPLATE_VERSION: std::ops::Range<usize> = 96..100;

/// Page 1 of an empty `SQLite` database in WAL mode.
///
/// Byte for byte what `SQLite` writes when `PRAGMA journal_mode=WAL`
/// switches a new, empty database (4096-byte pages, read and write
/// versions 2, one page, change counter 1, an empty `sqlite_schema` leaf),
/// with this build's `SQLite` version number. `io::durable::tests` checks
/// it against `SQLite`'s own.
#[must_use]
pub fn wal_database_template() -> [u8; WAL_TEMPLATE_BYTES] {
    let mut page = [0_u8; WAL_TEMPLATE_BYTES];
    let page_size = u16::try_from(WAL_TEMPLATE_BYTES).unwrap_or(u16::MAX);
    let version = u32::try_from(rusqlite::version_number()).unwrap_or(0);
    let fields: [(usize, &[u8]); 10] = [
        (0, b"SQLite format 3\0"),
        (16, &page_size.to_be_bytes()),
        // Write and read versions 2: WAL. Reserved bytes 0; payload
        // fractions 64, 32, 32.
        (18, &[2, 2, 0, 64, 32, 32]),
        // File change counter, then the database's size in pages.
        (24, &1_u32.to_be_bytes()),
        (28, &1_u32.to_be_bytes()),
        // The change counter the version number below is valid for.
        (92, &1_u32.to_be_bytes()),
        (TEMPLATE_VERSION.start, &version.to_be_bytes()),
        // `sqlite_schema`'s root: an empty table leaf whose cell content
        // area starts at the end of the page.
        (100, &[0x0d, 0, 0, 0, 0]),
        (105, &page_size.to_be_bytes()),
        (107, &[0]),
    ];
    for (at, bytes) in fields {
        if let Some(slot) = page.get_mut(at..at + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
    }
    page
}

/// Make a new, empty bulkload store file a database in WAL mode before
/// `SQLite` opens it.
///
/// OI-1003-Q154 lever 3, stores created directly in WAL mode: write
/// [`wal_database_template`] and sync it.
///
/// `SQLite` would otherwise switch the new database to WAL with a commit of
/// its own in rollback-journal mode, which syncs a `-journal` twice and
/// the database once (3 `fsync`s per store, 2026-10-08 trace). Opening a
/// file whose header already says WAL, it writes nothing to the main file
/// and makes no journal: the store's first write is its creation commit,
/// in the WAL, at `synchronous=FULL` as before.
///
/// The page must reach stable media no later than the store's first WAL
/// frame does: `SQLite` discards the WAL of a database file that reads as
/// empty. [`template_seal`] gives that order with the cheapest call that
/// does. On Linux it is an `fsync`, the same call each of `SQLite`'s three
/// syncs was. On Darwin it is `F_BARRIERFSYNC`, never `F_FULLFSYNC`:
/// `SQLite`'s three syncs there were plain `fsync`s (`fullfsync` is set
/// only after the switch, [`configure_sqlite`]), which drain no device
/// cache, so an `F_FULLFSYNC` here would add a device-cache flush per
/// store that the old path never paid (OI-1003-Q154 review). The barrier orders the page ahead of every later write,
/// and the store's state-root seal ([`seal_state_root`], a full flush of
/// the root) and its creation commit (`F_FULLFSYNC` of the WAL) each drain
/// the device after it, so the page is on stable media before the store's
/// first commit returns, as `BulkloadTransfer.tla` assumes. Like the
/// `SQLite` syncs it replaces, this seal is the database file's own I/O,
/// outside the flush counters and the R-N88 trace. Under
/// [`Durability::Strict`] it is a full flush.
///
/// # Errors
/// Returns the write or sync failure.
pub fn init_wal_database(file: &File) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, &wal_database_template(), 0)?;
    template_seal(file)
}

/// [`init_wal_database`]'s seal: a full flush under
/// [`Durability::Strict`]; otherwise `F_BARRIERFSYNC` on Darwin (untraced)
/// and `fsync` elsewhere.
fn template_seal(file: &File) -> std::io::Result<()> {
    if durability() == Durability::Strict {
        return file.sync_all();
    }
    #[cfg(target_vendor = "apple")]
    {
        super::sys::barrier_untraced(file)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_all()
    }
}

/// Whether `bytes`, a store file's whole content, are what a crash can
/// leave of [`init_wal_database`].
///
/// At most one page, every byte zero or the template's (the version number
/// any value). A file like that holds no store and is made whole again; any
/// other is left to `SQLite`.
#[must_use]
pub fn interrupted_wal_init(bytes: &[u8]) -> bool {
    bytes.len() <= WAL_TEMPLATE_BYTES
        && bytes
            .iter()
            .zip(wal_database_template())
            .enumerate()
            .all(|(at, (found, template))| {
                *found == 0 || *found == template || TEMPLATE_VERSION.contains(&at)
            })
}

/// Relax the row commits of a SOURCE ledger connection (WP0(g),
/// [`LedgerSync::Relaxed`]): `synchronous=NORMAL` and `fullfsync=OFF`.
///
/// Call it only on the source publisher's own connection, and only after
/// `Store::open` has returned on it: the store's creation commit (schema
/// and authority) and its root seals are then already durable, and nothing
/// this connection commits afterwards can undo them. In WAL mode a NORMAL
/// commit appends its frames without syncing the WAL. A power loss can
/// then cut the WAL's tail, and recovery stops at the last commit whose
/// frame checksums hold: commits are lost newest first, and a surviving
/// row is never a torn one.
///
/// `checkpoint_fullfsync` stays ON ([`configure_sqlite`] set it): a
/// checkpoint still syncs the WAL and then the database with a full flush
/// (`F_FULLFSYNC` on Darwin), so what a checkpoint moved is durable and
/// the WAL header a later commit rewrites is synced before its frames.
///
/// # Errors
/// Refuses if `SQLite` rejects a setting or does not report it back.
pub fn relax_ledger_rows(conn: &rusqlite::Connection) -> Result<()> {
    let refuse = |_| BulkloadRefusal::SqliteIntegrityCheckFailed;
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
        PRAGMA fullfsync=OFF;",
    )
    .map_err(refuse)?;
    let read = |pragma: &str| -> Result<i64> {
        conn.query_row(pragma, [], |row| row.get(0)).map_err(refuse)
    };
    // 1 is NORMAL. A connection that did not take the settings would commit
    // rows under a mode the counters do not report.
    if read("PRAGMA synchronous")? != 1
        || read("PRAGMA fullfsync")? != 0
        || read("PRAGMA checkpoint_fullfsync")? != 1
    {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    Ok(())
}

/// Which store a sink commits to, for per-side test limits (F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupSide {
    /// The source store (the digest-only capture ledger).
    Source,
    /// The destination store (outputs).
    Destination,
    /// Neither (tests of the committer itself).
    Other,
}

/// The work a [`Committer`] makes durable.
pub trait GroupSink: Send + 'static {
    /// The store this sink commits to.
    const SIDE: GroupSide = GroupSide::Other;

    /// One unit of work.
    type Item: Send + 'static;
    /// What [`Committer::finish`] hands back.
    type Report: Send + 'static;
    /// Work the session's last group carries beside its items
    /// ([`Committer::sync_last`]); `()` for a sink that has none.
    type Last: Send + 'static;

    /// `(files, bytes)` this item adds to the open group.
    fn weight(item: &Self::Item) -> (u64, u64);

    /// Seal, publish and commit one closed group, in that order.
    fn commit(&mut self, items: Vec<Self::Item>);

    /// Commit `items` as the session's last group, with `last`. Called only
    /// for a group that holds items: a [`Committer::sync_last`] that finds
    /// the group empty drops `last` unused. By default `last` is dropped
    /// and the group commits as any other.
    fn commit_last(&mut self, items: Vec<Self::Item>, last: Self::Last) {
        drop(last);
        self.commit(items);
    }

    /// The failure that stops this sink, once one group has failed.
    fn failure(&self) -> Option<BulkloadRefusal>;

    /// Called once after the last group has committed.
    fn finish(self) -> Self::Report;
}

/// Group-close and queue limits for one [`Committer`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Close a group once it holds this many files.
    pub group_files: u64,
    /// Close a group once it holds this many payload bytes.
    pub group_bytes: u64,
    /// Close a group after this long without a new item.
    pub group_idle: Duration,
    /// Queued items before [`Committer::submit`] blocks the producer.
    pub queue_depth: usize,
    /// Close a group once this long has passed since its first item, idle
    /// or not (`None`: no such bound). A group that waits for idleness
    /// longer than [`GROUP_IDLE`] still commits at least this often.
    pub group_age: Option<Duration>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            group_files: GROUP_FILES,
            group_bytes: GROUP_BYTES,
            group_idle: GROUP_IDLE,
            queue_depth: QUEUE_DEPTH,
            group_age: None,
        }
    }
}

type Failure = std::sync::Arc<std::sync::Mutex<Option<BulkloadRefusal>>>;

enum Message<T, L> {
    Item(T),
    Sync(SyncSender<()>),
    /// [`Committer::sync_last`]: behind every item submitted before it.
    SyncLast(SyncSender<()>, L),
}

/// A running committer thread. Dropping it commits pending work and joins.
pub struct Committer<S: GroupSink> {
    sender: Option<SyncSender<Message<S::Item, S::Last>>>,
    handle: Option<JoinHandle<S::Report>>,
    failure: Failure,
}

impl<S: GroupSink> Committer<S> {
    /// Start the committer thread for `sink`.
    ///
    /// # Errors
    /// Refuses if the thread cannot be spawned.
    pub fn spawn(sink: S) -> Result<Self> {
        Self::spawn_with(sink, Limits::default())
    }

    /// Start the committer thread for `sink` with explicit `limits`.
    ///
    /// # Errors
    /// Refuses if the thread cannot be spawned.
    pub fn spawn_with(sink: S, limits: Limits) -> Result<Self> {
        #[cfg(feature = "fault-injection")]
        let limits = {
            let mut limits = crate::fault::group_files().map_or(limits, |group_files| Limits {
                group_files,
                ..limits
            });
            if let Some((files, idle)) = crate::fault::side_group(S::SIDE) {
                limits.group_files = files;
                if let Some(idle) = idle {
                    // A harness row's own idle, as before any age bound.
                    limits.group_idle = idle;
                    limits.group_age = None;
                }
            }
            limits
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(limits.queue_depth.max(1));
        let failure = Failure::default();
        let shared = Failure::clone(&failure);
        let handle = std::thread::Builder::new()
            .name("bulkload-commit".to_owned())
            .spawn(move || run(sink, &receiver, limits, &shared))
            .refuse_at("io::durable::spawn_with")?;
        Ok(Self {
            sender: Some(sender),
            handle: Some(handle),
            failure,
        })
    }

    fn failed(&self) -> Result<()> {
        self.failure
            .lock()
            .map_or(Err(BulkloadRefusal::Io(None)), |failure| {
                failure.clone().map_or(Ok(()), Err)
            })
    }

    /// Queue one item. Blocks while the queue is full.
    ///
    /// # Errors
    /// Returns the sink's failure once a group has failed, or refuses if the
    /// committer thread has stopped.
    pub fn submit(&self, item: S::Item) -> Result<()> {
        self.failed()?;
        self.sender
            .as_ref()
            .ok_or(BulkloadRefusal::Io(None))?
            .send(Message::Item(item))
            .map_err(|_| BulkloadRefusal::Io(None))
    }

    /// Close the open group now and wait until it has committed.
    ///
    /// # Errors
    /// Returns the sink's failure if this or an earlier group failed, or
    /// refuses if the committer thread has stopped.
    pub fn sync(&self) -> Result<()> {
        self.round_trip(Message::Sync)
    }

    /// [`Committer::sync`] for the session's last group: the open group,
    /// which holds every item submitted before this call, commits through
    /// [`GroupSink::commit_last`] with `last`. When that group is empty
    /// (an idle or a bound already closed it) nothing commits and `last` is
    /// dropped unused; the caller learns which from `last` itself.
    ///
    /// # Errors
    /// As [`Committer::sync`].
    pub fn sync_last(&self, last: S::Last) -> Result<()> {
        self.round_trip(|ack| Message::SyncLast(ack, last))
    }

    /// Send `message` behind every item submitted so far and wait for the
    /// committer to answer it.
    fn round_trip(
        &self,
        message: impl FnOnce(SyncSender<()>) -> Message<S::Item, S::Last>,
    ) -> Result<()> {
        self.failed()?;
        let (ack, done) = std::sync::mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or(BulkloadRefusal::Io(None))?
            .send(message(ack))
            .map_err(|_| BulkloadRefusal::Io(None))?;
        done.recv().map_err(|_| BulkloadRefusal::Io(None))?;
        self.failed()
    }

    /// Commit everything pending, stop the thread and return its report.
    ///
    /// # Errors
    /// Refuses [`BulkloadRefusal::WorkerLost`] if the committer thread
    /// panicked.
    pub fn finish(mut self) -> Result<S::Report> {
        drop(self.sender.take());
        self.handle
            .take()
            .ok_or(BulkloadRefusal::WorkerLost)?
            .join()
            .map_err(|_| BulkloadRefusal::WorkerLost)
    }
}

impl<S: GroupSink> Drop for Committer<S> {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct OpenGroup<T> {
    items: Vec<T>,
    files: u64,
    bytes: u64,
    /// When its first item arrived.
    opened: Option<std::time::Instant>,
}

impl<T> OpenGroup<T> {
    const fn new() -> Self {
        Self {
            items: Vec::new(),
            files: 0,
            bytes: 0,
            opened: None,
        }
    }

    /// How long the committer may wait for the next item before this group
    /// closes: its idle time, cut short by what is left of its age.
    fn wait(&self, limits: &Limits) -> Duration {
        match (limits.group_age, self.opened) {
            (Some(age), Some(opened)) => {
                limits.group_idle.min(age.saturating_sub(opened.elapsed()))
            }
            _ => limits.group_idle,
        }
    }

    /// Whether the group has reached its age bound.
    fn aged(&self, limits: &Limits) -> bool {
        matches!(
            (limits.group_age, self.opened),
            (Some(age), Some(opened)) if opened.elapsed() >= age
        )
    }

    fn close<S: GroupSink<Item = T>>(&mut self, sink: &mut S, failure: &Failure) {
        if let Some(items) = self.take() {
            sink.commit(items);
            Self::note_failure(sink, failure);
        }
    }

    /// [`OpenGroup::close`] through [`GroupSink::commit_last`]; an empty
    /// group drops `last`.
    fn close_last<S: GroupSink<Item = T>>(
        &mut self,
        sink: &mut S,
        failure: &Failure,
        last: S::Last,
    ) {
        if let Some(items) = self.take() {
            sink.commit_last(items, last);
            Self::note_failure(sink, failure);
        }
    }

    /// The group's items, emptied for the next group; `None` when empty.
    fn take(&mut self) -> Option<Vec<T>> {
        if self.items.is_empty() {
            return None;
        }
        counters::bump(Counter::DurableGroups);
        self.files = 0;
        self.bytes = 0;
        self.opened = None;
        Some(std::mem::take(&mut self.items))
    }

    fn note_failure<S: GroupSink<Item = T>>(sink: &S, failure: &Failure) {
        if let Some(refusal) = sink.failure() {
            if let Ok(mut shared) = failure.lock() {
                shared.get_or_insert(refusal);
            }
        }
    }
}

fn run<S: GroupSink>(
    mut sink: S,
    receiver: &Receiver<Message<S::Item, S::Last>>,
    limits: Limits,
    failure: &Failure,
) -> S::Report {
    let mut group = OpenGroup::new();
    loop {
        let message = if group.items.is_empty() {
            match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(group.wait(&limits)) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    group.close(&mut sink, failure);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        match message {
            Message::Item(item) => {
                let (files, bytes) = S::weight(&item);
                group.files = group.files.saturating_add(files);
                group.bytes = group.bytes.saturating_add(bytes);
                group.opened.get_or_insert_with(std::time::Instant::now);
                group.items.push(item);
                if group.files >= limits.group_files
                    || group.bytes >= limits.group_bytes
                    || group.aged(&limits)
                {
                    group.close(&mut sink, failure);
                }
            }
            Message::Sync(ack) => {
                group.close(&mut sink, failure);
                let _ = ack.send(());
            }
            Message::SyncLast(ack, last) => {
                group.close_last(&mut sink, failure, last);
                let _ = ack.send(());
            }
        }
    }
    group.close(&mut sink, failure);
    sink.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// OI-1003-Q113, floor moved to 5.17 by OI-1003-Q141 (ruled 2026-10-08,
    /// Linear TIN-4543): a release is read as `(major, minor)`, and only 5.17-rc3 or
    /// later seals a group device-wide. 5.8 to 5.16, and 5.17-rc1 and rc2,
    /// report data write-back errors from `syncfs` but drop the file
    /// system's own `->sync_fs` failure (journal commit, log force, cache
    /// flush).
    #[test]
    fn the_syncfs_floor_reads_the_release_major_and_minor() {
        assert_eq!(
            release_version("6.12.0-211.51.1.el10_2.x86_64"),
            Some((6, 12))
        );
        assert_eq!(release_version("5.8.0"), Some((5, 8)));
        assert_eq!(release_version("5.7.19-arch1"), Some((5, 7)));
        assert_eq!(release_version("4.19"), Some((4, 19)));
        assert_eq!(release_version("garbage"), None);
        assert_eq!(release_version("6"), None);
        assert_eq!(SYNCFS_REPORTS_ERRORS_SINCE, (5, 17));
        for below in [
            "5.7.19",
            "5.8.0",
            "5.10.1",
            "5.14.0-503.el9",
            "5.15.0-91-generic",
            "5.16.20",
            "5.17.0-rc1",
            "5.17.0-rc2",
            "5.17.0-0.rc2.20220208git555f3d7be914.85.fc36",
        ] {
            assert!(
                !release_meets_floor(below),
                "{below} must keep the per-file seals"
            );
        }
        for at_or_above in [
            "5.17.0",
            "5.17.0-rc3",
            "5.17.0-rc8",
            "5.17.0-0.rc3.89.fc36.x86_64",
            "5.17.15-arch1-1",
            "6.1.0-18-amd64",
            "6.12.0",
            "6.12.0-211.51.1.el10_2.x86_64",
            "6.0.0-rc1",
        ] {
            assert!(
                release_meets_floor(at_or_above),
                "{at_or_above} may seal a group device-wide"
            );
        }
        assert!(!release_meets_floor("garbage"));
    }

    struct Recorder(Arc<Mutex<Vec<Vec<u64>>>>);

    impl GroupSink for Recorder {
        type Item = u64;
        type Report = usize;
        type Last = ();

        fn weight(item: &u64) -> (u64, u64) {
            (1, *item)
        }

        fn commit(&mut self, items: Vec<u64>) {
            if let Ok(mut groups) = self.0.lock() {
                groups.push(items);
            }
        }

        fn failure(&self) -> Option<BulkloadRefusal> {
            None
        }

        fn finish(self) -> usize {
            self.0.lock().map_or(0, |groups| groups.len())
        }
    }

    #[test]
    fn groups_close_on_count_bytes_sync_and_drop() -> Result<()> {
        let groups = Arc::new(Mutex::new(Vec::new()));
        let committer = Committer::spawn(Recorder(Arc::clone(&groups)))?;
        for _ in 0..GROUP_FILES {
            committer.submit(1)?;
        }
        committer.submit(GROUP_BYTES)?;
        committer.submit(7)?;
        committer.sync()?;
        committer.submit(9)?;
        let closed = committer.finish()?;
        let sizes: Vec<_> = groups
            .lock()
            .map_err(|_| BulkloadRefusal::Io(None))?
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(closed, sizes.len());
        // 64 files close the first group; one oversized item closes the
        // second; sync closes the third; finish commits the last.
        assert_eq!(sizes, [64, 1, 1, 1]);
        Ok(())
    }

    #[test]
    fn an_idle_group_closes_without_sync() -> Result<()> {
        let groups = Arc::new(Mutex::new(Vec::new()));
        let committer = Committer::spawn(Recorder(Arc::clone(&groups)))?;
        committer.submit(1)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while groups.lock().map_or(0, |groups| groups.len()) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "idle group never closed"
            );
            std::thread::yield_now();
        }
        drop(committer);
        Ok(())
    }

    fn group_sizes(groups: &Arc<Mutex<Vec<Vec<u64>>>>) -> Vec<usize> {
        groups
            .lock()
            .map(|groups| groups.iter().map(Vec::len).collect())
            .unwrap_or_default()
    }

    /// OI-1003-Q154 lever 3: items that land less than the idle apart share
    /// a group; the age bound still closes a group that never goes idle,
    /// and a 20 ms idle (the bound before the lever) splits the same items.
    #[test]
    fn a_group_waits_out_its_idle_and_closes_at_its_age() -> Result<()> {
        // `closes`: before each next item, wait (bounded) until the committer
        // has closed every group so far by itself, on its idle.
        let gapped = |idle: Duration,
                      age: Option<Duration>,
                      gaps: usize,
                      (gap, closes): (Duration, bool)|
         -> Result<Vec<usize>> {
            let groups = Arc::new(Mutex::new(Vec::new()));
            let committer = Committer::spawn_with(
                Recorder(Arc::clone(&groups)),
                Limits {
                    group_idle: idle,
                    group_age: age,
                    ..Limits::default()
                },
            )?;
            committer.submit(1)?;
            for submitted in 1..=gaps {
                std::thread::sleep(gap);
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while closes && group_sizes(&groups).len() < submitted {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "an idle group never closed by itself"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                committer.submit(1)?;
            }
            committer.sync()?;
            drop(committer);
            Ok(group_sizes(&groups))
        };
        let gap = Duration::from_millis(60);
        // Five items 60 ms apart under a 2 s idle: one group, closed by sync.
        assert_eq!(gapped(Duration::from_secs(2), None, 4, (gap, false))?, [5]);
        // The same five items under a 20 ms idle: each item alone, its group
        // closed on its idle with no sync. The committer judges idleness by
        // what it sees: a committer thread descheduled past the gap (a loaded
        // host, nice 19) finds the next item already queued and groups the
        // two, whatever the gap (a 60 ms gap, and then a 200 ms one, grouped
        // two items at load 45 to 60 on 32 CPUs: `[2, 1, 1, 1]`). That is
        // scheduling, not the idle rule, and a group closing late is only
        // later durability, never a record before its data. So each next
        // item waits until the committer has closed the group before it.
        assert_eq!(
            gapped(Duration::from_millis(20), None, 4, (gap, true))?,
            [1; 5]
        );
        // Twenty items 60 ms apart (over 1.1 s) under a 2 s idle and a 300 ms
        // age: closed by age, more than once, nothing lost.
        let aged = gapped(
            Duration::from_secs(2),
            Some(Duration::from_millis(300)),
            19,
            (gap, false),
        )?;
        assert!(aged.len() >= 3, "{aged:?}");
        assert_eq!(aged.iter().sum::<usize>(), 20, "{aged:?}");
        Ok(())
    }

    /// OI-1003-Q154 lever 3: the template is the page `SQLite` itself writes
    /// when it switches a new, empty database to WAL.
    #[test]
    fn the_wal_template_is_sqlites_own_first_page() -> Result<()> {
        let scratch =
            std::env::temp_dir().join(format!("bulkload-wal-template-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir(&scratch).map_err(|_| BulkloadRefusal::Io(None))?;
        let path = scratch.join("own.sqlite");
        {
            let conn = rusqlite::Connection::open(&path).map_err(|_| BulkloadRefusal::Io(None))?;
            let mode: String = conn
                .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
                .map_err(|_| BulkloadRefusal::Io(None))?;
            assert_eq!(mode, "wal");
        }
        let own = std::fs::read(&path).map_err(|_| BulkloadRefusal::Io(None))?;
        assert_eq!(own.as_slice(), wal_database_template().as_slice());

        // A database made from the template opens in WAL mode without a
        // write to its main file, and takes a schema.
        let made = scratch.join("made.sqlite");
        let file = std::fs::File::create(&made).map_err(|_| BulkloadRefusal::Io(None))?;
        init_wal_database(&file).map_err(|_| BulkloadRefusal::Io(None))?;
        drop(file);
        {
            let conn = rusqlite::Connection::open(&made).map_err(|_| BulkloadRefusal::Io(None))?;
            configure_sqlite(&conn)?;
            conn.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES (1);")
                .map_err(|_| BulkloadRefusal::Io(None))?;
            assert!(!scratch.join("made.sqlite-journal").exists());
            assert_eq!(
                std::fs::read(&made).map_err(|_| BulkloadRefusal::Io(None))?,
                wal_database_template(),
                "nothing written to the main file before a checkpoint"
            );
            let check: String = conn
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .map_err(|_| BulkloadRefusal::Io(None))?;
            assert_eq!(check, "ok");
        }
        let _ = std::fs::remove_dir_all(&scratch);
        Ok(())
    }

    /// What a crash can leave of the template's write is recognised; any
    /// other content is not.
    #[test]
    fn an_interrupted_wal_init_is_recognised() {
        let template = wal_database_template();
        assert!(interrupted_wal_init(&[]));
        assert!(interrupted_wal_init(&template));
        assert!(interrupted_wal_init(&template[..512]));
        assert!(interrupted_wal_init(&[0; WAL_TEMPLATE_BYTES]));
        let mut torn = template;
        torn[..512].fill(0);
        assert!(interrupted_wal_init(&torn));
        let mut other_version = template;
        other_version[96..100].copy_from_slice(&3_046_001_u32.to_be_bytes());
        assert!(interrupted_wal_init(&other_version));
        let mut foreign = template;
        foreign[200] = 7;
        assert!(!interrupted_wal_init(&foreign));
        assert!(!interrupted_wal_init(&[template.as_slice(), &[0]].concat()));
    }

    #[test]
    fn durability_parses_and_round_trips() {
        assert_eq!("group".parse(), Ok(Durability::Group));
        assert_eq!("strict".parse(), Ok(Durability::Strict));
        assert!("fast".parse::<Durability>().is_err());
        assert_eq!(Durability::Strict.to_string(), "strict");
    }
}
