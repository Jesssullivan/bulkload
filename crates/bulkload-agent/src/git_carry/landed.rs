//! What bulkload landed, recorded next to what it landed (estate converge,
//! `docs/agent-notes/2026-10-08-estate-converge-design.md`).
//!
//! Two ledgers and their intents, each keyed by SOURCE label:
//!
//! - the **ref ledger**, `$GIT_COMMON_DIR/bulkload/v1/<SOURCE>/refs.landed`:
//!   the native refs, the stash order and the common `info/exclude` this
//!   label's applies set, each with the capture instant (`observed_ns`) it
//!   came from. Its `created` flag is the repository marker: only a
//!   repository bulkload itself created takes native refs (finding 9);
//! - the **workspace ledger**, `$GIT_DIR/bulkload/v1/<SOURCE>/workspace.landed`
//!   (the worktree's own git directory, so `git worktree remove` removes it):
//!   the capture a landed checkout holds, its HEAD, index entries and the
//!   stat print of every seat.
//!
//! Every record is written atomically (temporary, fsync, rename, directory
//! fsync), after the Git writes it describes were made durable by the
//! destination writer's `core.fsync` (finding 7). A record is read with
//! `O_NOFOLLOW` under a size cap and decoded strictly.

use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{BulkloadRefusal, Result};

/// The directory, relative to a git directory, that holds a label's ledgers.
pub const LEDGER_ROOT: &str = "bulkload/v1";
/// The ref ledger's file name.
pub const REFS_LANDED: &str = "refs.landed";
/// The ref intent's file name.
pub const REFS_INTENT: &str = "refs.intent";
/// The workspace ledger's file name.
pub const WORKSPACE_LANDED: &str = "workspace.landed";
/// The workspace intent's file name.
pub const WORKSPACE_INTENT: &str = "workspace.intent";
/// A first linked landing's intent: `landing-<key>.intent` in the common
/// ledger directory, `<key>` naming the destination (#216's residual).
pub const LANDING_INTENT_PREFIX: &str = "landing-";
/// The destination lock's file name.
const LOCK: &str = "lock";
/// The largest record read or written: a 200k-seat ledger is about 16 MiB.
pub const LEDGER_CAP: usize = 256 * 1024 * 1024;
/// Each record starts with this prefix, so a foreign file is never decoded.
const MAGIC: &[u8; 16] = b"bulkload-land-v1";
/// A tombstone older than this, relative to the newest capture the ledger
/// holds, is forgotten (finding 8: retention is bounded by age).
pub const TOMBSTONE_RETENTION_NS: i128 = 90 * 24 * 3600 * 1_000_000_000;

/// One native ref bulkload set, or a tombstone for one it deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedRef {
    /// The value bulkload set; `None` is a tombstone (the source deleted
    /// the ref and bulkload deleted it here).
    pub oid: Option<String>,
    /// The `started_ns` of the capture the value came from.
    pub observed_ns: i128,
}

/// The stash reflog bulkload built, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedStash {
    pub order: Vec<String>,
    pub observed_ns: i128,
}

/// The common `info/exclude` a standalone item set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedExclude {
    pub digest: [u8; 32],
    pub observed_ns: i128,
}

/// A repository's ref ledger for one SOURCE label.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefLedger {
    /// The repository marker: bulkload created this repository (`git init`
    /// in its private stage). Native refs are restored only then.
    pub created: bool,
    pub refs: BTreeMap<String, LandedRef>,
    pub stash: Option<LandedStash>,
    pub exclude: Option<LandedExclude>,
}

impl RefLedger {
    /// The newest capture instant any row holds.
    pub fn newest(&self) -> i128 {
        self.refs
            .values()
            .map(|row| row.observed_ns)
            .chain(self.stash.iter().map(|stash| stash.observed_ns))
            .max()
            .unwrap_or(0)
    }

    /// Forget tombstones past their retention.
    pub fn prune_tombstones(&mut self) {
        let horizon = self.newest().saturating_sub(TOMBSTONE_RETENTION_NS);
        self.refs
            .retain(|_, row| row.oid.is_some() || row.observed_ns >= horizon);
    }
}

/// One planned native ref write: `old` and `new` are the compare-and-swap
/// values (`None` = absent), `before` the ledger row before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefOp {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub before: Option<LandedRef>,
    pub after: Option<LandedRef>,
}

/// A planned stash rebuild: the reflog before and after, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashOp {
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub messages: Vec<String>,
    pub observed_ns: i128,
}

/// The ref step's intent: durable before any native ref or stash write,
/// removed after the ledger it produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefIntent {
    pub ops: Vec<RefOp>,
    pub stash: Option<StashOp>,
    /// The ledger to write once every op is settled, before the
    /// adjustments for ops a third party overtook.
    pub ledger: RefLedger,
    /// The stash reflog the stash op has reached, made durable after each
    /// store (`None`: its old order, no store made). A settle continues the
    /// stash only from here, or from one store past it (the earlier
    /// report's finding 4: an operator's drop or clear is never undone).
    pub stash_at: Option<Vec<String>>,
}

/// A first linked landing, durable before `git worktree add` and removed
/// after the worktree's ledger (#216's residual, the earlier report's
/// finding 7): a crash between leaves a worktree without a ledger that this
/// record names as bulkload's, which the rerun finishes forward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingIntent {
    /// `estate::id` of the item.
    pub item: String,
    /// The new worktree's path, as `worktree add` was given it.
    pub destination: Vec<u8>,
    /// The capture landed, its instant and its provenance instance.
    pub capture: [u8; 32],
    pub observed_ns: i128,
    pub instance: String,
    /// HEAD as the landing plans it, and the branch recorded when detached.
    pub head: Head,
    pub intended_branch: Option<String>,
}

/// The landing intent of a linked worktree at `destination`, in the common
/// ledger directory `ledger_dir`.
pub fn landing_intent_path(ledger_dir: &Path, destination: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    let key = blake3::hash(destination.as_os_str().as_bytes()).to_hex();
    ledger_dir.join(format!(
        "{LANDING_INTENT_PREFIX}{}.intent",
        key.get(..32).unwrap_or_default()
    ))
}

/// A worktree's HEAD: its symbolic ref (`None` = detached) and its commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub symbolic: Option<String>,
    pub oid: String,
}

/// A seat's kind, as a ledger records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Directory,
    Regular,
    Symlink,
}

/// The stat print of one landed seat. Identity is `(ino, size, mtime,
/// ctime)` on the ledger's file system (`fsid`), never `st_dev`, which a
/// reboot or a device renumbering moves (finding 19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatPrint {
    pub path: Vec<u8>,
    pub kind: Kind,
    /// Permission bits (`& 0o7777`).
    pub mode: u32,
    pub size: u64,
    pub ino: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
}

impl SeatPrint {
    /// The print of `meta` at `path`; `None` for a kind no seat has.
    pub fn of(path: &[u8], meta: &fs::Metadata) -> Option<Self> {
        let kind = if meta.file_type().is_symlink() {
            Kind::Symlink
        } else if meta.is_file() {
            Kind::Regular
        } else if meta.is_dir() {
            Kind::Directory
        } else {
            return None;
        };
        let identity = crate::freshness::StatIdentity::from_metadata(meta);
        Some(Self {
            path: path.to_vec(),
            kind,
            mode: meta.mode() & 0o7777,
            size: identity.size,
            ino: identity.ino,
            mtime_ns: identity.mtime_ns,
            ctime_ns: identity.ctime_ns,
        })
    }

    /// Whether `other` is the same seat, unchanged. A directory compares by
    /// kind and mode only: its times move with every entry.
    pub fn same(&self, other: &Self) -> bool {
        if self.kind == Kind::Directory || other.kind == Kind::Directory {
            return self.kind == other.kind && self.mode == other.mode;
        }
        self == other
    }
}

/// A landed checkout, as its workspace ledger records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedWorkspace {
    /// `estate::id` of the item that landed it (empty for a CLI restore).
    pub item: String,
    /// The bundle digest of the capture the checkout holds.
    pub capture: [u8; 32],
    /// That capture's `started_ns`.
    pub observed_ns: i128,
    /// Its provenance instance, `refs/carry/v1/<SOURCE>/<instance>/`.
    pub instance: String,
    /// HEAD as bulkload left it.
    pub head: Head,
    /// The source's branch when bulkload had to detach (finding 11, 17).
    pub intended_branch: Option<String>,
    /// The digest of the index entries (path, mode, oid, stage, flags).
    pub index: [u8; 32],
    /// The digest of the common `info/exclude` at landing.
    pub exclude: [u8; 32],
    /// When the prints were taken (the racy window).
    pub recorded_ns: i128,
    /// The file system the prints were taken on (`statvfs` `f_fsid`).
    pub fsid: u64,
    /// Every carried seat, sorted by path.
    pub seats: Vec<SeatPrint>,
}

/// A pending workspace converge: durable before the converge's first
/// destination write, removed after the ledger it produces. It names both
/// captures; the seat operations are recomputed from their provenance
/// (both instances are in the destination after the import), so every
/// settle decision is made from the destination itself (finding 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIntent {
    pub from: [u8; 32],
    pub to: [u8; 32],
    pub to_instance: String,
    pub to_observed_ns: i128,
    /// The temporary-name tag of this converge's seats and index.
    pub tag: String,
    /// The index entries digest the converge replaces.
    pub index_old: [u8; 32],
    /// HEAD before and after.
    pub head: (Head, Head),
    /// The HEAD branch moved with HEAD (W1): name, old (`None` = created), new.
    pub branch: Option<(String, Option<String>, String)>,
    pub intended_branch: Option<String>,
    /// The standalone item's `info/exclude`, before and after.
    pub exclude: Option<([u8; 32], [u8; 32])>,
    /// HEAD is at its new value: written right after `set_head`, so a
    /// settle never moves HEAD again (an operator's switch back to the old
    /// HEAD while the pass still ran is theirs).
    pub head_done: bool,
}

/// Directory of `label`'s ledgers under the git directory `git_dir`.
pub fn ledger_dir(git_dir: &Path, label: &str) -> PathBuf {
    git_dir.join(LEDGER_ROOT).join(label)
}

/// Create the ledger directory (0700 each level).
pub fn ensure_dir(git_dir: &Path, label: &str) -> Result<PathBuf> {
    let directory = ledger_dir(git_dir, label);
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&directory)
        .refuse_at("git_carry::landed::ensure_dir")?;
    Ok(directory)
}

/// Read a record, `None` when there is none.
pub fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    const SITE: &str = "git_carry::landed::read";
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(LEDGER_CAP)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .refuse_at(SITE)?;
    if bytes.len() > LEDGER_CAP {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let body = bytes
        .strip_prefix(MAGIC.as_slice())
        .ok_or(BulkloadRefusal::FrameCodec)?;
    let (value, rest) = postcard::take_from_bytes(body).refuse_at(SITE)?;
    if !rest.is_empty() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    Ok(Some(value))
}

/// The encoded bytes of a record; `BUDGET_EXCEEDED` over [`LEDGER_CAP`].
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(postcard::to_allocvec(value).refuse_at("git_carry::landed::encode")?);
    if bytes.len() > LEDGER_CAP {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    Ok(bytes)
}

/// Write a record atomically: temporary, fsync, rename, directory fsync.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_bytes(path, &encode(value)?)
}

/// [`write`] of a record already encoded.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_mode(path, bytes, 0o600)
}

/// [`write_bytes`] at `mode` (a Git file such as `info/exclude`).
pub fn write_bytes_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    const SITE: &str = "git_carry::landed::write";
    let parent = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let mut generation = 0u64;
    let (temporary, mut file) = loop {
        let temporary = path.with_extension(format!("pending-{generation}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                generation = generation
                    .checked_add(1)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            }
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    };
    file.write_all(bytes).refuse_at(SITE)?;
    file.sync_file_counted().refuse_at(SITE)?;
    drop(file);
    fs::rename(&temporary, path).refuse_at(SITE)?;
    sync_dir(parent)
}

/// Remove a record and seal its directory; absent is done.
pub fn remove(path: &Path) -> Result<()> {
    const SITE: &str = "git_carry::landed::remove";
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    sync_dir(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
}

/// fsync a directory.
pub fn sync_dir(directory: &Path) -> Result<()> {
    fs::File::open(directory)
        .refuse_at("git_carry::landed::sync_dir")?
        .sync_dir_counted()
        .refuse_at("git_carry::landed::sync_dir")
}

/// The destination-side lock of one repository and label. It closes the gap
/// the in-process mutex leaves: two applies with different STATE
/// directories landing into one repository. S2 forbids locks on the source
/// only.
pub struct DestinationLock(fs::File);

impl Drop for DestinationLock {
    fn drop(&mut self) {
        // SAFETY: this guard owns a live descriptor for the acquired flock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Take the destination lock of `common` (a common git directory) for
/// `label`, waiting for another holder.
pub fn lock(common: &Path, label: &str) -> Result<DestinationLock> {
    const SITE: &str = "git_carry::landed::lock";
    let directory = ensure_dir(common, label)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join(LOCK))
        .refuse_at(SITE)?;
    loop {
        // SAFETY: the owned descriptor stays open for the lock's lifetime.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(DestinationLock(file));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(crate::refuse::io(&error, SITE));
        }
    }
}

/// The `statvfs` file-system id of `path`'s file system.
pub fn fsid(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| BulkloadRefusal::PathNotPortable)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `name` is NUL-terminated and outlives the call; `stat` is a
    // writable statvfs the call fills on success.
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(crate::refuse::io(
            &std::io::Error::last_os_error(),
            "git_carry::landed::fsid",
        ));
    }
    // SAFETY: statvfs returned 0, so it initialised `stat`.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    Ok(stat.f_fsid as u64)
}

/// The git directory of a worktree at `worktree`: `.git` itself, or the
/// directory a `.git` gitfile names. `None` when there is neither.
pub fn worktree_git_dir(worktree: &Path) -> Result<Option<PathBuf>> {
    const SITE: &str = "git_carry::landed::worktree_git_dir";
    let dotgit = worktree.join(".git");
    let meta = match fs::symlink_metadata(&dotgit) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    if meta.is_dir() {
        return Ok(Some(dotgit));
    }
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return Ok(None);
    }
    let text = fs::read_to_string(&dotgit).refuse_at(SITE)?;
    let Some(target) = text.strip_prefix("gitdir: ").map(str::trim_end) else {
        return Ok(None);
    };
    let target = Path::new(target);
    Ok(Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree.join(target)
    }))
}

/// The common git directory of the git directory `git_dir`: the one its
/// `commondir` file names, or itself.
pub fn common_dir(git_dir: &Path) -> Result<PathBuf> {
    const SITE: &str = "git_carry::landed::common_dir";
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let target = Path::new(text.trim_end());
            let joined = if target.is_absolute() {
                target.to_path_buf()
            } else {
                git_dir.join(target)
            };
            fs::canonicalize(joined).refuse_at(SITE)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::canonicalize(git_dir).refuse_at(SITE)
        }
        Err(error) => Err(crate::refuse::io(&error, SITE)),
    }
}

/// Whether a record exists at `path` (metadata only).
pub fn exists(path: &Path) -> Result<bool> {
    path.try_exists().refuse_at("git_carry::landed::exists")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_refuse_foreign_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("refs.landed");
        let mut ledger = RefLedger {
            created: true,
            ..RefLedger::default()
        };
        ledger.refs.insert(
            "refs/heads/main".into(),
            LandedRef {
                oid: Some("a".repeat(40)),
                observed_ns: 7,
            },
        );
        write(&path, &ledger).unwrap();
        assert_eq!(read::<RefLedger>(&path).unwrap(), Some(ledger));
        fs::write(&path, b"not a ledger").unwrap();
        assert_eq!(
            read::<RefLedger>(&path).unwrap_err(),
            BulkloadRefusal::FrameCodec
        );
        assert_eq!(
            read::<RefLedger>(&directory.path().join("absent")).unwrap(),
            None
        );
    }

    #[test]
    fn tombstones_are_forgotten_past_retention() {
        let mut ledger = RefLedger::default();
        let now = TOMBSTONE_RETENTION_NS * 3;
        ledger.refs.insert(
            "refs/remotes/o/old".into(),
            LandedRef {
                oid: None,
                observed_ns: now - TOMBSTONE_RETENTION_NS - 1,
            },
        );
        ledger.refs.insert(
            "refs/remotes/o/new".into(),
            LandedRef {
                oid: None,
                observed_ns: now - 1,
            },
        );
        ledger.refs.insert(
            "refs/heads/main".into(),
            LandedRef {
                oid: Some("b".repeat(40)),
                observed_ns: now,
            },
        );
        ledger.prune_tombstones();
        assert!(!ledger.refs.contains_key("refs/remotes/o/old"));
        assert!(ledger.refs.contains_key("refs/remotes/o/new"));
    }

    #[test]
    fn a_directory_print_ignores_times() {
        let a = SeatPrint {
            path: b"d".to_vec(),
            kind: Kind::Directory,
            mode: 0o755,
            size: 1,
            ino: 2,
            mtime_ns: 3,
            ctime_ns: 4,
        };
        let b = SeatPrint {
            size: 9,
            ino: 9,
            mtime_ns: 9,
            ctime_ns: 9,
            ..a.clone()
        };
        assert!(a.same(&b));
        let c = SeatPrint {
            kind: Kind::Regular,
            ..a.clone()
        };
        assert!(!a.same(&c));
    }
}
