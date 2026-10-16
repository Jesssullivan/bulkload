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
/// The ref ledger's and the ref intent's prefix since stash-order-v2
/// (OI-1003-Q169): their stash rows and ops carry each entry's message.
/// The same records under [`MAGIC`] are the stash-order-v1 layout and
/// decode only through their v1 types, never as these (postcard is not
/// self-describing: one layout read as the other would be misread).
const REFS_MAGIC: &[u8; 16] = b"bulkload-refs-v2";
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

/// One stash reflog entry, the line `git stash list` shows
/// (`stash@{n}: <message>`): the stash commit and the entry's reflog
/// message, as Git stored it (any bytes but NUL and newline; Git folds
/// whitespace when it writes one). Since stash-order-v2 (OI-1003-Q169)
/// both are the entry's identity: in the capture key, in the ledger's
/// stash row and in the stash table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StashEntry {
    pub oid: String,
    pub message: Vec<u8>,
}

impl StashEntry {
    /// An entry of `oid` with `message`.
    pub fn new(oid: impl Into<String>, message: impl Into<Vec<u8>>) -> Self {
        Self {
            oid: oid.into(),
            message: message.into(),
        }
    }
}

/// `git reflog show` format of a stash list in stash-order-v2: one line
/// per entry, newest first, its commit, a NUL and its reflog message. The
/// source's capture and the destination's read use it alike.
pub const STASH_LIST_FORMAT: &str = "--format=%H%x00%gs";

/// Parse a stash list in [`STASH_LIST_FORMAT`] (or a stash-order-v2
/// metadata value, the same bytes). Strict: every line is a full object
/// name, one NUL and a message without NUL; nothing else.
///
/// # Errors
/// `GIT_INVENTORY_MALFORMED` for any other line.
pub fn parse_stash_list(bytes: &[u8]) -> Result<Vec<StashEntry>> {
    let mut entries = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let (oid, message) = line
            .iter()
            .position(|byte| *byte == 0)
            .map(|at| line.split_at(at))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let message = message.get(1..).unwrap_or_default();
        let oid = std::str::from_utf8(oid).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        if !matches!(oid.len(), 40 | 64)
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            || message.contains(&0)
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        entries.push(StashEntry::new(oid, message));
    }
    Ok(entries)
}

/// A reflog message as Git stores it (`refs.c` `copy_reflog_msg`): leading
/// whitespace dropped, each run of spaces, tabs, CRs and LFs folded to one
/// space, trailing whitespace dropped. A message read from a reflog is
/// already folded; a commit subject a stash-order-v1 capture names may not
/// be, and the destination's reflog would not read back equal to it.
#[must_use]
pub fn reflog_message(bytes: &[u8]) -> Vec<u8> {
    let space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
    let mut out = Vec::with_capacity(bytes.len());
    let mut was_space = true;
    for &byte in bytes {
        if was_space && space(byte) {
            continue;
        }
        was_space = space(byte);
        out.push(if was_space { b' ' } else { byte });
    }
    while out.last().is_some_and(|byte| space(*byte)) {
        out.pop();
    }
    out
}

/// `entries` with each message folded as Git folds a reflog message it
/// writes ([`reflog_message`]), and how many folding changed: the stash
/// bulkload can land. A source message Git would fold (written by a
/// libgit2 tool, or a hand-edited log) is stored folded whatever bulkload
/// asks, so it is folded before it becomes a target (review finding 5).
#[must_use]
pub fn folded(entries: Vec<StashEntry>) -> (Vec<StashEntry>, u64) {
    let mut changed = 0;
    let entries = entries
        .into_iter()
        .map(|entry| {
            let message = reflog_message(&entry.message);
            if message != entry.message {
                changed += 1;
            }
            StashEntry::new(entry.oid, message)
        })
        .collect();
    (entries, changed)
}

/// What a ledger's stash row identifies its entries by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StashIdentity {
    /// stash-order-v1: the commit alone. A row a ledger recorded before
    /// OI-1003-Q169 decodes so; its messages are unknown and left empty.
    Commit,
    /// stash-order-v2 (OI-1003-Q169): the commit and its reflog message.
    CommitAndMessage,
}

/// The stash reflog bulkload built, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedStash {
    pub order: Vec<StashEntry>,
    pub observed_ns: i128,
    pub identity: StashIdentity,
}

impl LandedStash {
    /// A stash-order-v2 row of `order` at `observed_ns`.
    #[must_use]
    pub const fn of(order: Vec<StashEntry>, observed_ns: i128) -> Self {
        Self {
            order,
            observed_ns,
            identity: StashIdentity::CommitAndMessage,
        }
    }

    /// Whether the destination's reflog `dst` is the one this row names:
    /// entry for entry, by commit and message; by commit alone for a
    /// stash-order-v1 row, the only identity it recorded.
    #[must_use]
    pub fn names(&self, dst: &[StashEntry]) -> bool {
        match self.identity {
            StashIdentity::CommitAndMessage => self.order == dst,
            StashIdentity::Commit => {
                self.order.len() == dst.len()
                    && self
                        .order
                        .iter()
                        .zip(dst)
                        .all(|(row, now)| row.oid == now.oid)
            }
        }
    }

    /// This row as stash-order-v2 against the destination's reflog `dst`
    /// (OI-1003-Q169's one-time upgrade): a v1 row that names `dst` by
    /// commit names exactly the entries bulkload stored, so it becomes
    /// `dst` itself, messages included. Any other row is returned as it is
    /// (a v1 row that does not name `dst` stays v1: the destination's
    /// stash moved since, and is kept).
    #[must_use]
    pub fn upgraded(&self, dst: &[StashEntry]) -> Self {
        if self.identity == StashIdentity::Commit && self.names(dst) {
            Self::of(dst.to_vec(), self.observed_ns)
        } else {
            self.clone()
        }
    }
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

/// A planned stash rebuild: the reflog before and after, newest first,
/// each entry with its message (stash-order-v2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashOp {
    pub old: Vec<StashEntry>,
    pub new: Vec<StashEntry>,
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
    pub stash_at: Option<Vec<StashEntry>>,
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

/// A record type and the prefix it is written under.
pub trait Record: Serialize + serde::de::DeserializeOwned {
    /// The prefix [`write`] puts before the record.
    const MAGIC: &'static [u8; 16];

    /// The record from one written under an earlier prefix `magic`, decoded
    /// strictly through that layout's own type; `None` when `magic` is not
    /// one of this record's earlier prefixes (the read refuses
    /// `FRAME_CODEC`).
    fn earlier(_magic: &[u8], _body: &[u8]) -> Option<Result<Self>> {
        None
    }
}

impl Record for LandedWorkspace {
    const MAGIC: &'static [u8; 16] = MAGIC;
}

impl Record for WorkspaceIntent {
    const MAGIC: &'static [u8; 16] = MAGIC;
}

impl Record for LandingIntent {
    const MAGIC: &'static [u8; 16] = MAGIC;
}

impl Record for RefLedger {
    const MAGIC: &'static [u8; 16] = REFS_MAGIC;

    // A stash-order-v1 ledger: its stash row identifies entries by commit.
    fn earlier(magic: &[u8], body: &[u8]) -> Option<Result<Self>> {
        (magic == MAGIC.as_slice()).then(|| decode::<RefLedgerV1>(body).map(Self::from))
    }
}

/// A ref intent is settled from the destination as it stands, so a
/// stash-order-v1 intent is read with [`read_ref_intent`], which leaves it
/// to be resolved against the destination's stash; the plain [`read`]
/// refuses it `FRAME_CODEC`.
impl Record for RefIntent {
    const MAGIC: &'static [u8; 16] = REFS_MAGIC;
}

/// The stash-order-v1 layouts (before OI-1003-Q169), field for field as
/// they were encoded: a stash row and a stash op named commits only, the op
/// with the messages it would store beside them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedStashV1 {
    pub order: Vec<String>,
    pub observed_ns: i128,
}

/// The stash-order-v1 ref ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefLedgerV1 {
    pub created: bool,
    pub refs: BTreeMap<String, LandedRef>,
    pub stash: Option<LandedStashV1>,
    pub exclude: Option<LandedExclude>,
}

/// The stash-order-v1 stash op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashOpV1 {
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub messages: Vec<String>,
    pub observed_ns: i128,
}

/// The stash-order-v1 ref intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefIntentV1 {
    pub ops: Vec<RefOp>,
    pub stash: Option<StashOpV1>,
    pub ledger: RefLedgerV1,
    pub stash_at: Option<Vec<String>>,
}

impl From<RefLedgerV1> for RefLedger {
    fn from(v1: RefLedgerV1) -> Self {
        Self {
            created: v1.created,
            refs: v1.refs,
            stash: v1.stash.map(|row| LandedStash {
                order: row
                    .order
                    .into_iter()
                    .map(|oid| StashEntry::new(oid, Vec::new()))
                    .collect(),
                observed_ns: row.observed_ns,
                identity: StashIdentity::Commit,
            }),
            exclude: v1.exclude,
        }
    }
}

impl RefIntentV1 {
    /// This intent in stash-order-v2 terms, against the destination's stash
    /// reflog `dst` as it stands under the lock. The new order is exact: a
    /// v1 store wrote each message it carried (`bulkload stash` for an
    /// empty one). The old order named commits only; when `dst` names the
    /// same commits it is `dst` (the stash the op found, nothing stored
    /// yet); for an append it is the new order's tail (the entries the
    /// append kept); otherwise its messages are unknown and it can match no
    /// state `dst` is in. Each recorded progress state is the old order, the
    /// empty reflog, or a suffix of the new order, as `stash_path` made
    /// them.
    ///
    /// # Errors
    /// `FRAME_CODEC` when the op's messages do not pair with its new order
    /// or a progress state is none of those: never a guess.
    pub fn resolve(self, dst: &[StashEntry]) -> Result<RefIntent> {
        let Some(op) = self.stash else {
            if self.stash_at.is_some() {
                return Err(BulkloadRefusal::FrameCodec);
            }
            return Ok(RefIntent {
                ops: self.ops,
                stash: None,
                ledger: self.ledger.into(),
                stash_at: None,
            });
        };
        if op.messages.len() != op.new.len() {
            return Err(BulkloadRefusal::FrameCodec);
        }
        let new: Vec<StashEntry> = op
            .new
            .iter()
            .zip(&op.messages)
            .map(|(oid, message)| {
                let stored = if message.is_empty() {
                    "bulkload stash"
                } else {
                    message.as_str()
                };
                StashEntry::new(oid.clone(), reflog_message(stored.as_bytes()))
            })
            .collect();
        let commits = |entries: &[StashEntry]| -> Vec<String> {
            entries.iter().map(|entry| entry.oid.clone()).collect()
        };
        // An append (the new order ends with the old one, by commit) kept
        // the old entries as they were, so their messages are the new
        // order's tail: with them, a destination one store past the old
        // order (a crash between a store and its record) is a state of the
        // op's path again and is settled forward, as the v1 code settled
        // it (review finding 6).
        let old: Vec<StashEntry> = if commits(dst) == op.old {
            dst.to_vec()
        } else if op.new.ends_with(&op.old) {
            new.get(new.len() - op.old.len()..)
                .unwrap_or_default()
                .to_vec()
        } else {
            op.old
                .iter()
                .map(|oid| StashEntry::new(oid.clone(), Vec::new()))
                .collect()
        };
        let state = |at: Vec<String>| -> Result<Vec<StashEntry>> {
            if at == op.old {
                Ok(old.clone())
            } else if at.is_empty() {
                Ok(Vec::new())
            } else if op.new.ends_with(&at) {
                Ok(new.get(new.len() - at.len()..).unwrap_or_default().to_vec())
            } else {
                Err(BulkloadRefusal::FrameCodec)
            }
        };
        let stash_at = self.stash_at.map(state).transpose()?;
        let mut ledger = RefLedger::from(self.ledger);
        // The ledger the v1 step planned names the op's new order.
        if let Some(row) = &mut ledger.stash {
            if commits(&row.order) == op.new {
                *row = LandedStash::of(new.clone(), row.observed_ns);
            }
        }
        Ok(RefIntent {
            ops: self.ops,
            stash: Some(StashOp {
                old,
                new,
                observed_ns: op.observed_ns,
            }),
            ledger,
            stash_at,
        })
    }
}

/// A pending ref intent as read: current, or stash-order-v1 to be resolved
/// against the destination ([`RefIntentV1::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingRefIntent {
    Current(RefIntent),
    V1(RefIntentV1),
}

/// Read a pending ref intent of either layout, `None` when there is none.
///
/// # Errors
/// As [`read`].
pub fn read_ref_intent(path: &Path) -> Result<Option<PendingRefIntent>> {
    let Some(bytes) = read_bytes(path)? else {
        return Ok(None);
    };
    if let Some(body) = bytes.strip_prefix(REFS_MAGIC.as_slice()) {
        return decode(body).map(|intent| Some(PendingRefIntent::Current(intent)));
    }
    let body = bytes
        .strip_prefix(MAGIC.as_slice())
        .ok_or(BulkloadRefusal::FrameCodec)?;
    decode(body).map(|intent| Some(PendingRefIntent::V1(intent)))
}

// A record's body, strictly: the whole of it, nothing after.
fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    let (value, rest) = postcard::take_from_bytes(body).refuse_at("git_carry::landed::decode")?;
    if !rest.is_empty() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    Ok(value)
}

/// Read a record, `None` when there is none.
pub fn read<T: Record>(path: &Path) -> Result<Option<T>> {
    let Some(bytes) = read_bytes(path)? else {
        return Ok(None);
    };
    if let Some(body) = bytes.strip_prefix(T::MAGIC.as_slice()) {
        return decode(body).map(Some);
    }
    let (magic, body) = bytes.split_at(bytes.len().min(MAGIC.len()));
    T::earlier(magic, body).map_or_else(
        || Err(BulkloadRefusal::FrameCodec),
        |record| record.map(Some),
    )
}

// A record file's bytes under the size cap, `None` when there is none.
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
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
    Ok(Some(bytes))
}

/// The encoded bytes of a record; `BUDGET_EXCEEDED` over [`LEDGER_CAP`].
pub fn encode<T: Record>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = T::MAGIC.to_vec();
    bytes.extend(postcard::to_allocvec(value).refuse_at("git_carry::landed::encode")?);
    if bytes.len() > LEDGER_CAP {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    Ok(bytes)
}

/// Write a record atomically: temporary, fsync, rename, directory fsync.
pub fn write<T: Record>(path: &Path, value: &T) -> Result<()> {
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
