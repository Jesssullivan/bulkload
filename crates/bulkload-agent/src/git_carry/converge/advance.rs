//! Estate advance (operator ruling 2026-10-19, interview, "Yes, advance
//! those"; quoted in full in `docs/agent-notes/2026-10-19-adopt-ancestor.md`).
//!
//! A clean live checkout whose HEAD is an ancestor of the source's is
//! advanced to the source's HEAD plus its working-tree changes, so
//! sessions resume on the source's actual work; the destination's own
//! branches, stashes and worktrees stay untouched, and the beside copy is
//! retired only after bulkload proves the live tree equals it.
//!
//! `estate-apply --adopt --beside SUFFIX --advance-ancestor` (and so
//! `estate-move --advance-ancestor`). A live checkout bulkload did not
//! land, whose adopt proof finds it differs from the capture
//! (`GIT_DESTINATION_DIVERGED` without the flag), is advanced in place
//! only when every one of these holds, each read before any write and
//! proved again under the destination lock:
//!
//! 1. its HEAD is born and an ancestor of (or equal to) the captured HEAD
//!    (`GIT_ADVANCE_NOT_ANCESTOR`);
//! 2. its branch is the captured one: both on the same branch, or both
//!    detached (`GIT_ADVANCE_BRANCH_MISMATCH`; detaching or switching the
//!    live checkout is never the answer);
//! 3. that branch is a plain ref whose tip is the live HEAD, no other
//!    worktree has it checked out (or held by a rebase or bisect) and no
//!    pending bulkload intent names it (`GIT_ADVANCE_BRANCH_BUSY`);
//! 4. its index and tracked files are clean: no staged or unstaged change,
//!    no merge, rebase, cherry-pick, revert or bisect in progress, no
//!    assume-unchanged or skip-worktree entry, and every seat the advance
//!    replaces or removes holds its HEAD's bytes (`GIT_ADVANCE_NOT_CLEAN`);
//! 5. its untracked files that are not ignored (the coordinator's
//!    refinement of the ruling: sting's own untracked work stays, byte for
//!    byte) collide with no path the capture holds, in its HEAD's tree, its
//!    index or its working-tree changes: the same path, or one a directory
//!    of the other (`GIT_ADVANCE_UNTRACKED_COLLIDES`, naming them). An
//!    ignored entry collides only where the capture tracks the path (or
//!    one under it); elsewhere it is left as it is;
//! 6. under the lock, HEAD, the branch, the index entries and every proved
//!    seat's print are as the proof found them (`GIT_ADVANCE_RACED`).
//!
//! Each refusal writes nothing at the live checkout; the caller lands the
//! capture beside it in the same pass, as without the flag.
//!
//! The advance, under the lock, after an additive import of the capture's
//! objects and provenance:
//!
//! 1. the intent ([`AdvanceIntent`]: the proved HEAD, the live HEAD tree's
//!    seats with their live modes, the entries left to the live checkout)
//!    (`estate.advance.after_intent`);
//! 2. the branch (or a detached HEAD) by compare-and-swap from the proved
//!    tip to the captured HEAD; a branch moved since leaves nothing written
//!    but the intent, which is removed, and refuses `GIT_ADVANCE_RACED`
//!    (`estate.advance.after_branch`);
//! 3. the seats, from the live HEAD's tree to the captured worktree
//!    (tracked content, dirty and untracked files, as a landing lays them
//!    down), each checked through the print the proof took, replaced by
//!    `RENAME_EXCHANGE` with the displaced file checked (the converge's
//!    seat engine; `estate.advance.after_seat`);
//! 4. the captured index (`estate.advance.after_index`: tree, working
//!    changes and index all in place, no ledger);
//! 5. the advance record ([`Advanced`], the swap record: the live tip
//!    before and after), then the workspace ledger, as a landing records
//!    one (`estate.advance.after_ledger`), then the intent removed.
//!
//! What the advance never touches: an untracked or ignored entry of the
//! live checkout. Its untracked files (proved apart from every captured
//! path) are recorded as its own (`untracked`). A captured seat the
//! capture does not track whose path the live checkout holds with an
//! ignored entry is left to it, with its subtree (`kept`), and a captured
//! directory the live checkout holds as an untracked directory keeps its
//! own mode (`kept_dirs`); the capture's bytes for them stay in its
//! provenance in the live repository. Nor are other branches, the
//! stash, other worktrees, configuration or `info/exclude` touched: no
//! native ref step runs (the repository marker is never set), and the
//! captured branches, tags and stash entries go in under `refs/<ns>/`
//! (`--refs-in`, the caller).
//!
//! Every refusal after the intent is `GIT_CONVERGE_INTERRUPTED` (the
//! converge's rule): the rerun settles the intent forward ([`settle`]),
//! each seat classified by its content (old, new or a third party's), the
//! index by compare-and-swap on its entries; an intent whose branch never
//! moved is dropped and the checkout proved afresh.
//!
//! Later passes converge the advanced checkout as any landing, with three
//! rules from its record ([`Advanced`]): the entries it left to the live
//! checkout are never touched, its `info/exclude` is never replaced, and
//! its branch moves by compare-and-swap from the value bulkload left it at
//! ([`branch_plan`]), never detached for a foreign repository's.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::batch_objects::BatchObjects;
use super::super::landed::{self, Head, Kind, LandedWorkspace, SeatPrint};
use super::super::native_refs::{self, Custody};
use super::super::{git, output, safe_destination};
use super::adopt::{ancestor, Scratch};
use super::{
    after_intent, ancestors, checked_out_by, delta, exclude_digest, head_of, holds, holds_unlanded,
    index_digest, intent_branches, interrupted, lstat, probe_exchange, publish_index, read_exclude,
    same_path, set_head, sides, Action, Applied, Engine, HeadPlan, Landing, Meta, Op, Place, Side,
};
use crate::{BulkloadRefusal, Result};

/// Seats by path.
type Sides = BTreeMap<Vec<u8>, Side>;
/// Seat prints by path.
type Prints = BTreeMap<Vec<u8>, SeatPrint>;

/// The advance intent's file name, in the live checkout's workspace ledger
/// directory (`$GIT_DIR/bulkload/v1/<SOURCE>/`).
pub const INTENT: &str = "advance.intent";

/// The advance record's file name, beside the workspace ledger.
pub const ADVANCED: &str = "advanced";

/// Git's in-progress markers in a worktree's git directory: a merge,
/// cherry-pick, revert, rebase, bisect or sequencer run.
const IN_PROGRESS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

/// The captured entries an advance left to the live checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    /// Captured seats whose path the live checkout held with an entry its
    /// HEAD does not track (an ignored file, link or directory, or one under
    /// it): left as they were, with everything under them. Sorted.
    pub kept: Vec<Vec<u8>>,
    /// Captured directories the live checkout held as untracked
    /// directories (ignored ones: its untracked files collide with no
    /// captured path): the directory itself keeps its mode, what the
    /// capture tracks under it lands, and what it does not track is left
    /// (in `kept`). Sorted.
    pub kept_dirs: Vec<Vec<u8>>,
    /// The live checkout's own untracked files (not ignored) when it was
    /// advanced, apart from every captured path: never touched, and never
    /// a difference from its beside landing. Sorted.
    pub untracked: Vec<Vec<u8>>,
}

impl Left {
    /// Whether `path` (or a directory above it) was left to the live
    /// checkout, with its subtree.
    #[must_use]
    pub fn keeps(&self, path: &[u8]) -> bool {
        std::iter::once(path).chain(ancestors(path)).any(|at| {
            self.kept
                .binary_search_by(|kept| kept.as_slice().cmp(at))
                .is_ok()
        })
    }

    /// Whether `path` (or a directory above it) was one of the live
    /// checkout's own untracked files when it was advanced.
    #[must_use]
    pub fn own_untracked(&self, path: &[u8]) -> bool {
        std::iter::once(path).chain(ancestors(path)).any(|at| {
            self.untracked
                .binary_search_by(|own| own.as_slice().cmp(at))
                .is_ok()
        })
    }

    /// Whether no seat operation may touch `path`: kept, or a kept
    /// directory itself.
    #[must_use]
    pub fn leaves(&self, path: &[u8]) -> bool {
        self.keeps(path)
            || self
                .kept_dirs
                .binary_search_by(|kept| kept.as_slice().cmp(path))
                .is_ok()
    }

    /// The sides a converge's mode pass seals: none left to the live
    /// checkout.
    fn mode_sides(&self, new: &BTreeMap<Vec<u8>, Side>) -> BTreeMap<Vec<u8>, Side> {
        new.iter()
            .filter(|(path, _)| !self.leaves(path))
            .map(|(path, side)| (path.clone(), side.clone()))
            .collect()
    }
}

/// One seat of the live checkout as the advance proved it: an entry of
/// its HEAD's tree, with the mode the live seat has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OldSeat {
    pub path: Vec<u8>,
    pub kind: Kind,
    pub mode: u32,
    pub blob: Option<String>,
    pub link: Option<Vec<u8>>,
}

/// A pending advance: durable before its first write to the live checkout
/// (the branch), removed after the ledger it produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceIntent {
    /// `estate::id` of the item.
    pub item: String,
    /// The capture advanced to, its instant and its provenance instance.
    pub capture: [u8; 32],
    pub observed_ns: i128,
    pub instance: String,
    /// The temporary-name tag of this advance's seats and index.
    pub tag: String,
    /// HEAD as proved, and as the advance leaves it.
    pub from: Head,
    pub to: Head,
    /// The index entries digest the advance replaces.
    pub index_old: [u8; 32],
    /// The live HEAD tree's seats, as proved.
    pub old: Vec<OldSeat>,
    /// Their prints, as proved: a settle records these for a seat no
    /// operation touches, never one it reads afterwards (so an edit made
    /// since a crash is a difference the next converge proves, never
    /// bulkload's output).
    pub prints: Vec<SeatPrint>,
    /// What the advance leaves to the live checkout.
    pub left: Left,
    /// The branch (or the detached HEAD) is at `to`: written right after
    /// its compare-and-swap.
    pub branch_done: bool,
}

impl landed::Record for AdvanceIntent {
    const MAGIC: &'static [u8; 16] = b"bulkload-advnc-1";
}

/// The record of a finished advance, beside the workspace ledger it made.
///
/// The swap record (the live tip before and after) and what it left to
/// the live checkout, which every later converge of the checkout honours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advanced {
    /// `estate::id` of the item.
    pub item: String,
    /// The capture advanced to, and its instant.
    pub capture: [u8; 32],
    pub observed_ns: i128,
    /// HEAD before (the live tip, an ancestor of `to`) and after.
    pub from: Head,
    pub to: Head,
    /// When the advance was recorded.
    pub recorded_ns: i128,
    /// What the advance left to the live checkout.
    pub left: Left,
}

impl landed::Record for Advanced {
    const MAGIC: &'static [u8; 16] = b"bulkload-advncd1";
}

impl Advanced {
    /// See [`Left::keeps`].
    #[must_use]
    pub fn keeps(&self, path: &[u8]) -> bool {
        self.left.keeps(path)
    }

    /// See [`Left::leaves`].
    #[must_use]
    pub fn leaves(&self, path: &[u8]) -> bool {
        self.left.leaves(path)
    }
}

/// What STATE records of one item's advance.
///
/// `{item}-{blake3(SOURCE)}.advance`, for receipts and closure: the
/// capture it was decided for, the live HEAD it found, whether it advanced
/// or which `GIT_ADVANCE_*` code refused it, and what became of the item's
/// beside landing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceRecord {
    /// `estate::id` of the item.
    pub item: String,
    /// The capture the advance was decided for.
    pub capture: [u8; 32],
    /// The live HEAD's commit when it was decided (empty when unborn).
    pub live: String,
    /// The live HEAD's branch (empty when detached).
    pub branch: String,
    /// `advanced`, or the `GIT_ADVANCE_*` code that refused it.
    pub outcome: String,
    /// The beside landing, after an advance: `pending` (not decided yet),
    /// `absent`, `retired` or `kept`; empty after a refusal.
    pub beside: String,
    /// Why a beside landing was kept (`not-owned`, `differs:<what>`,
    /// `refused:<CODE>`, ...).
    pub reason: String,
    /// How many captured entries the advance left to the live checkout.
    pub kept: u64,
    /// A refusal's detail (its `Display`: the colliding paths of
    /// `GIT_ADVANCE_UNTRACKED_COLLIDES`).
    pub detail: String,
    /// The digest of the live checkout's untracked file list when it was
    /// decided ([`untracked_digest`]).
    pub untracked: [u8; 32],
    /// The paths a `GIT_ADVANCE_UNTRACKED_COLLIDES` named.
    pub paths: Vec<Vec<u8>>,
}

impl landed::Record for AdvanceRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-advrec1";
}

/// The advance record STATE holds at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<AdvanceRecord>> {
    landed::read::<AdvanceRecord>(path)
}

/// Write `record` at `path` (atomically, durably).
///
/// # Errors
/// IO.
pub fn write_record(path: &Path, record: &AdvanceRecord) -> Result<()> {
    landed::write(path, record)
}

/// The advance record in the workspace ledger directory `dir`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_advanced(dir: &Path) -> Result<Option<Advanced>> {
    landed::read::<Advanced>(&dir.join(ADVANCED))
}

/// Whether an advance a crash cut short is pending at `place`.
pub(super) fn pending(place: &Place<'_>) -> Result<bool> {
    landed::exists(&place.workspace_dir().join(INTENT))
}

/// The live checkout's HEAD at `workspace`: its commit (empty when unborn)
/// and its branch (empty when detached).
///
/// # Errors
/// Whatever reading HEAD refuses.
pub fn live_head(workspace: &Path) -> Result<(String, String)> {
    let head = head_of(workspace)?;
    Ok((head.oid, head.symbolic.unwrap_or_default()))
}

/// What a rerun whose beside journal already closed the current capture
/// does about `--advance-ancestor` ([`gate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// The live path is no checkout: the journal stands.
    Leave,
    /// An advance would refuse now, with this code: the journal stands.
    Refused(BulkloadRefusal),
    /// The item is proved again: an advance may hold now, or one a crash
    /// cut short (its intent, its ledger) is finished. The digest of the
    /// live checkout's untracked files ([`untracked_digest`]; zero when
    /// not read).
    Try([u8; 32]),
}

/// `--advance-ancestor`'s preconditions that need no capture ([`Gate`]).
///
/// Read-only, for the live checkout at `workspace`. A rerun whose beside
/// journal already closed the current capture asks this before it proves
/// the item again, so an unchanged rerun stays a metadata walk.
///
/// # Errors
/// Whatever reading the checkout refuses.
pub fn gate(workspace: &Path, source: &str) -> Result<Gate> {
    if lstat(workspace)?.is_none_or(|meta| !meta.is_dir()) {
        return Ok(Gate::Leave);
    }
    let Some(git_dir) = landed::worktree_git_dir(workspace)?.filter(|dir| dir.is_dir()) else {
        return Ok(Gate::Leave);
    };
    let dir = landed::ledger_dir(&git_dir, source);
    if landed::exists(&dir.join(landed::WORKSPACE_LANDED))? || landed::exists(&dir.join(INTENT))? {
        return Ok(Gate::Try([0; 32]));
    }
    let head = head_of(workspace)?;
    if head.oid.is_empty() {
        return Ok(Gate::Refused(BulkloadRefusal::GitAdvanceNotAncestor));
    }
    if let Some(branch) = &head.symbolic {
        if busy(workspace, branch, &head.oid, source)? {
            return Ok(Gate::Refused(BulkloadRefusal::GitAdvanceBranchBusy));
        }
    }
    Ok(status(workspace, &git_dir)?.map_or(
        Gate::Refused(BulkloadRefusal::GitAdvanceNotClean),
        |untracked| Gate::Try(digest(&untracked)),
    ))
}

// The digest of an untracked file list.
fn digest(untracked: &[Vec<u8>]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-advance-untracked-v1\0");
    for path in untracked {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
    }
    *hasher.finalize().as_bytes()
}

/// The digest of the untracked files (not ignored) of the live checkout at
/// `workspace`, sorted; `None` when it is not clean, or no checkout.
///
/// # Errors
/// Whatever reading the checkout refuses.
pub fn untracked_digest(workspace: &Path) -> Result<Option<[u8; 32]>> {
    let Some(git_dir) = landed::worktree_git_dir(workspace)?.filter(|dir| dir.is_dir()) else {
        return Ok(None);
    };
    Ok(status(workspace, &git_dir)?.map(|untracked| digest(&untracked)))
}

/// The advance record of the checkout at `workspace` for `source`, if an
/// advance made it bulkload's.
///
/// # Errors
/// IO; a record that does not decode.
pub fn advanced_at(workspace: &Path, source: &str) -> Result<Option<Advanced>> {
    landed::worktree_git_dir(workspace)?.map_or(Ok(None), |git_dir| {
        read_advanced(&landed::ledger_dir(&git_dir, source))
    })
}

// The untracked entries (not ignored) of the worktree at `worktree` (git
// directory `git_dir`), sorted, when its index and tracked files are
// clean; `None` when they are not: an operation in progress, an
// assume-unchanged or skip-worktree entry (Git's status does not see their
// changes), or a status that lists a staged or unstaged change. Each entry
// is as Git lists it by default: an untracked file, or a directory that
// holds nothing tracked (`.serena`, not each file under it), a nested
// repository included. Git runs with no fsmonitor (`git_env`), so it walks
// the worktree itself (an untracked cache it keeps is checked against the
// directories' times).
fn status(worktree: &Path, git_dir: &Path) -> Result<Option<Vec<Vec<u8>>>> {
    for name in IN_PROGRESS {
        if lstat(&git_dir.join(name))?.is_some() {
            return Ok(None);
        }
    }
    let tagged = output(git(worktree).args(["ls-files", "-v", "-z"]))?;
    if tagged
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry.first() != Some(&b'H'))
    {
        return Ok(None);
    }
    let listed = output(git(worktree).args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignore-submodules=none",
        "--no-renames",
    ]))?;
    let mut untracked = Vec::new();
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(path) = entry.strip_prefix(b"?? ") else {
            return Ok(None);
        };
        let path = path.strip_suffix(b"/").unwrap_or(path);
        untracked.push(path.to_vec());
    }
    untracked.sort();
    untracked.dedup();
    Ok(Some(untracked))
}

/// Every path the capture holds: in its HEAD's tree, its index (the staged
/// tree and the intent-to-add entries) and its working-tree changes (the
/// carried seats).
struct Captured {
    /// Every path the capture tracks: its HEAD's and its index's entries.
    tracked: BTreeSet<Vec<u8>>,
    /// Every path that is no directory: the tracked ones and the carried
    /// seats that are not directories.
    files: BTreeSet<Vec<u8>>,
    /// Every carried directory.
    dirs: BTreeSet<Vec<u8>>,
}

impl Captured {
    fn read(scratch: &Scratch, meta: &Meta) -> Result<Self> {
        let mut tracked = BTreeSet::new();
        for tree in [meta.get("head")?, meta.get("staged")?] {
            let listed = output(git(&scratch.repo).args(["ls-tree", "-r", "-z", tree]))?;
            for entry in listed
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
            {
                let tab = entry
                    .iter()
                    .position(|byte| *byte == b'\t')
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
                // A gitlink is a nested repository's place, its directory
                // the live checkout's (as its HEAD's tree leaves it to the
                // seats), never a seat that collides.
                if entry.starts_with(b"160000 ") {
                    continue;
                }
                tracked.insert(entry.get(tab + 1..).unwrap_or_default().to_vec());
            }
        }
        for entry in super::super::carried_intent_to_add(&scratch.repo, &meta.heads())? {
            tracked.insert(entry.rel_path);
        }
        let mut files = tracked.clone();
        let mut dirs = BTreeSet::new();
        for (path, side) in sides(&scratch.repo, meta)? {
            if side.kind == Kind::Directory {
                dirs.insert(path);
            } else {
                files.insert(path);
            }
        }
        Ok(Self {
            tracked,
            files,
            dirs,
        })
    }

    // Whether the untracked entry `path` collides with a captured path:
    // the same path, a captured path under it (an untracked directory the
    // capture holds anything in), or a captured file above it. A captured
    // directory above it is no collision: an untracked directory inside a
    // tracked one is the live checkout's own.
    fn collides(&self, path: &[u8]) -> bool {
        let mut under = path.to_vec();
        under.push(b'/');
        let mut past = path.to_vec();
        past.push(b'/' + 1);
        self.files.contains(path)
            || self.dirs.contains(path)
            || self
                .files
                .range(under.clone()..past.clone())
                .next()
                .is_some()
            || self.dirs.range(under..past).next().is_some()
            || ancestors(path).any(|above| self.files.contains(above))
    }

    // Whether the capture tracks `path` or a path under it.
    fn tracks(&self, path: &[u8]) -> bool {
        let mut under = path.to_vec();
        under.push(b'/');
        let mut past = path.to_vec();
        past.push(b'/' + 1);
        self.tracked.contains(path) || self.tracked.range(under..past).next().is_some()
    }
}

// The collision refusal naming `paths` (at most 16, then how many more).
fn collides(paths: &[Vec<u8>]) -> BulkloadRefusal {
    let mut detail = Vec::new();
    for path in paths.iter().take(16) {
        if !detail.is_empty() {
            detail.push(b'\n');
        }
        detail.extend_from_slice(path);
    }
    if paths.len() > 16 {
        detail.extend_from_slice(format!("\n+{}", paths.len() - 16).as_bytes());
    }
    BulkloadRefusal::GitAdvanceUntrackedCollides(detail)
}

// Whether the live `branch` (at `oid`, the live HEAD) cannot be moved by
// compare-and-swap alone: a symbolic ref, a tip that is not the live HEAD,
// checked out (or held by a rebase or bisect) in another worktree, or
// named by a pending bulkload workspace intent of `source`.
fn busy(worktree: &Path, branch: &str, oid: &str, source: &str) -> Result<bool> {
    let (native, symbolic) = native_refs::native_refs(worktree)?;
    if symbolic.contains(branch) || native.get(branch).map(String::as_str) != Some(oid) {
        return Ok(true);
    }
    if checked_out_by(worktree)?
        .iter()
        .any(|(path, held)| held == branch && !same_path(path, worktree))
    {
        return Ok(true);
    }
    Ok(intent_branches(worktree, source)?.contains(branch))
}

/// What the read-only proof found.
struct Proven {
    /// HEAD as proved, and the captured HEAD.
    from: Head,
    to: Head,
    /// The live HEAD tree's seats, with their live modes, and their prints.
    old: BTreeMap<Vec<u8>, Side>,
    prints: BTreeMap<Vec<u8>, SeatPrint>,
    /// The live index entries digest.
    index: [u8; 32],
    /// The live checkout's untracked files, apart from the capture.
    untracked: Vec<Vec<u8>>,
    /// Every path the capture holds.
    captured: Captured,
}

// The preconditions, read-only (module docs, 1 to 5).
fn prove(place: &Place<'_>, scratch: &Scratch, meta: &Meta) -> Result<Proven> {
    let worktree = place.worktree;
    let from = head_of(worktree)?;
    if from.oid.is_empty() {
        return Err(BulkloadRefusal::GitAdvanceNotAncestor);
    }
    let to = Head {
        symbolic: meta.symbolic(&scratch.repo)?,
        oid: meta.get("head")?.to_owned(),
    };
    // The scratch repository holds the capture and borrows the live
    // repository's objects, so both histories are readable there.
    if !ancestor(&scratch.repo, &from.oid, &to.oid)? {
        return Err(BulkloadRefusal::GitAdvanceNotAncestor);
    }
    if from.symbolic != to.symbolic {
        return Err(BulkloadRefusal::GitAdvanceBranchMismatch);
    }
    if let Some(branch) = &from.symbolic {
        if busy(worktree, branch, &from.oid, place.source)? {
            return Err(BulkloadRefusal::GitAdvanceBranchBusy);
        }
    }
    let (old, prints) = tree_sides(worktree, &from.oid)?;
    let untracked = status(worktree, &place.git_dir)?.ok_or(BulkloadRefusal::GitAdvanceNotClean)?;
    if !held(worktree, &prints)? {
        return Err(BulkloadRefusal::GitAdvanceNotClean);
    }
    let captured = Captured::read(scratch, meta)?;
    let colliding: Vec<Vec<u8>> = untracked
        .iter()
        .filter(|path| captured.collides(path))
        .cloned()
        .collect();
    if !colliding.is_empty() {
        return Err(collides(&colliding));
    }
    let index = index_digest(worktree, None)?;
    if head_of(worktree)? != from {
        return Err(BulkloadRefusal::GitAdvanceRaced);
    }
    Ok(Proven {
        from,
        to,
        old,
        prints,
        index,
        untracked,
        captured,
    })
}

// The live checkout's seats: every entry of `commit`'s tree (gitlinks
// aside, as a capture's rows leave them), each with the kind its HEAD
// names, the mode the live seat has and its print. A tracked seat that is
// missing or of another kind is not clean.
#[allow(clippy::type_complexity)]
fn tree_sides(
    worktree: &Path,
    commit: &str,
) -> Result<(BTreeMap<Vec<u8>, Side>, BTreeMap<Vec<u8>, SeatPrint>)> {
    const SITE: &str = "git_carry::converge::advance::tree_sides";
    let not_clean = || BulkloadRefusal::GitAdvanceNotClean;
    let listed = output(git(worktree).args(["ls-tree", "-r", "-t", "-z", "--full-tree", commit]))?;
    let mut sides = BTreeMap::new();
    let mut prints = BTreeMap::new();
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(entry.get(..tab).unwrap_or_default())
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let (Some(mode), Some(_), Some(value)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        let kind = match mode {
            "040000" => Kind::Directory,
            "100644" | "100755" => Kind::Regular,
            "120000" => Kind::Symlink,
            "160000" => continue,
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        let path = entry.get(tab + 1..).unwrap_or_default().to_vec();
        let at = safe_destination(worktree, Path::new(OsStr::from_bytes(&path)))
            .map_err(|_| not_clean())?;
        let meta = lstat(&at)?.ok_or_else(not_clean)?;
        let print = SeatPrint::of(&path, &meta).ok_or_else(not_clean)?;
        if print.kind != kind {
            return Err(not_clean());
        }
        let link = if kind == Kind::Symlink {
            Some(
                std::fs::read_link(&at)
                    .refuse_at(SITE)?
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            )
        } else {
            None
        };
        sides.insert(
            path.clone(),
            Side {
                kind,
                mode: meta.mode() & 0o777,
                blob: (kind != Kind::Directory).then(|| value.to_owned()),
                link,
                prefix: Vec::new(),
            },
        );
        prints.insert(path, print);
    }
    Ok((sides, prints))
}

// Whether every proved print still holds (metadata only).
fn held(worktree: &Path, prints: &BTreeMap<Vec<u8>, SeatPrint>) -> Result<bool> {
    for print in prints.values() {
        let Ok(path) = safe_destination(worktree, Path::new(OsStr::from_bytes(&print.path))) else {
            return Ok(false);
        };
        let now = lstat(&path)?.and_then(|meta| SeatPrint::of(&print.path, &meta));
        if !now.is_some_and(|now| now.same(print)) {
            return Ok(false);
        }
    }
    Ok(true)
}

// Whether the proof still holds under the lock: HEAD, its branch (a plain
// ref at the live HEAD, free to move), the index entries and every seat's
// print. Metadata only; nothing has been written.
fn still(place: &Place<'_>, proven: &Proven) -> Result<bool> {
    let worktree = place.worktree;
    if head_of(worktree)? != proven.from {
        return Ok(false);
    }
    if let Some(branch) = &proven.from.symbolic {
        if busy(worktree, branch, &proven.from.oid, place.source)? {
            return Ok(false);
        }
    }
    if index_digest(worktree, None)? != proven.index {
        return Ok(false);
    }
    held(worktree, &proven.prints)
}

/// The seat operations of an advance, and what it leaves to the live
/// checkout.
struct Plan {
    ops: Vec<Op>,
    left: Left,
}

// Plan the seats from the live HEAD tree (`old`) to the capture (`new`):
// a captured seat whose path the live checkout holds with an entry its
// HEAD does not track (an ignored one: its untracked files collide with
// no captured path) is left to it with its subtree, unless the capture
// tracks that path (or one under it), and a captured directory it holds
// as one keeps its own mode. A tracked directory the capture turns into a
// file while it holds such an entry cannot be freed without touching it.
// Each of those two is `GIT_ADVANCE_UNTRACKED_COLLIDES`, naming the
// paths. Read-only.
fn plan(
    worktree: &Path,
    (old, new): (&Sides, &Sides),
    captured: &Captured,
    untracked: &[Vec<u8>],
) -> Result<Plan> {
    let mut kept = BTreeSet::<Vec<u8>>::new();
    let mut kept_dirs = BTreeSet::<Vec<u8>>::new();
    let mut colliding = Vec::new();
    // Ascending: a directory is decided before what it holds.
    for (path, side) in new {
        if old.contains_key(path) || ancestors(path).any(|at| kept.contains(at)) {
            continue;
        }
        // Under a directory the live checkout holds untracked (an ignored
        // one), only what the capture tracks lands: the rest is left, never
        // mixed into the live checkout's ignored tree.
        if ancestors(path).any(|at| kept_dirs.contains(at)) && !captured.tracks(path) {
            kept.insert(path.clone());
            continue;
        }
        let at = match safe_destination(worktree, Path::new(OsStr::from_bytes(path))) {
            Ok(at) => at,
            // A parent the advance creates first.
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => continue,
            // A parent that is not a directory: a tracked seat the advance
            // removes first, else the live checkout's own entry.
            Err(_) => {
                if !ancestors(path).any(|at| old.contains_key(at)) {
                    kept.insert(path.clone());
                }
                continue;
            }
        };
        match lstat(&at)? {
            None => {}
            Some(meta) if side.kind == Kind::Directory && meta.is_dir() => {
                kept_dirs.insert(path.clone());
            }
            Some(_) => {
                kept.insert(path.clone());
            }
        }
    }
    // An ignored entry collides where the capture tracks the path (or one
    // under it), whether or not its working tree holds the file.
    colliding.extend(kept.iter().filter(|path| captured.tracks(path)).cloned());
    for path in &captured.tracked {
        if old.contains_key(path) {
            continue;
        }
        let occupied = match safe_destination(worktree, Path::new(OsStr::from_bytes(path))) {
            Ok(at) => lstat(&at)?.is_some(),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => false,
            // A directory above it is a file, a link or another entry: the
            // live checkout's own unless it is a tracked seat the advance
            // removes first.
            Err(_) => !ancestors(path).any(|at| old.contains_key(at)),
        };
        if occupied {
            colliding.push(path.clone());
        }
    }
    for (path, side) in new {
        if side.kind == Kind::Directory
            || !old
                .get(path)
                .is_some_and(|side| side.kind == Kind::Directory)
        {
            continue;
        }
        let unlanded = match safe_destination(worktree, Path::new(OsStr::from_bytes(path))) {
            Ok(at) => holds_unlanded(&at, path, old)?,
            Err(_) => true,
        };
        if unlanded {
            colliding.push(path.clone());
        }
    }
    if !colliding.is_empty() {
        colliding.sort();
        colliding.dedup();
        return Err(collides(&colliding));
    }
    let left = Left {
        kept: kept.into_iter().collect(),
        kept_dirs: kept_dirs.into_iter().collect(),
        untracked: untracked.to_vec(),
    };
    let ops = delta(old, new)
        .into_iter()
        .filter(|op| !left.leaves(&op.path))
        .collect();
    Ok(Plan { ops, left })
}

// Whether every seat the advance replaces or removes holds its HEAD's
// bytes (a clean status trusts the index's stat data; these are the bytes
// the advance displaces). Reads only those seats, counted.
fn displaced_hold(worktree: &Path, ops: &[Op]) -> Result<bool> {
    let mut objects = BatchObjects::new(worktree)?;
    let mut all = true;
    for op in ops
        .iter()
        .filter(|op| matches!(op.action, Action::Remove | Action::Replace))
    {
        let Some(side) = op.old.as_ref().filter(|side| side.kind != Kind::Directory) else {
            continue;
        };
        let held = match safe_destination(worktree, Path::new(OsStr::from_bytes(&op.path))) {
            Ok(at) => holds(&mut objects, &at, side)?,
            Err(_) => false,
        };
        if !held {
            all = false;
            break;
        }
    }
    objects.finish()?;
    Ok(all)
}

// The proved seats as the intent records them.
fn old_seats(old: &BTreeMap<Vec<u8>, Side>) -> Vec<OldSeat> {
    old.iter()
        .map(|(path, side)| OldSeat {
            path: path.clone(),
            kind: side.kind,
            mode: side.mode,
            blob: side.blob.clone(),
            link: side.link.clone(),
        })
        .collect()
}

// The intent's seats as sides again.
fn old_sides(old: &[OldSeat]) -> BTreeMap<Vec<u8>, Side> {
    old.iter()
        .map(|seat| {
            (
                seat.path.clone(),
                Side {
                    kind: seat.kind,
                    mode: seat.mode,
                    blob: seat.blob.clone(),
                    link: seat.link.clone(),
                    prefix: Vec::new(),
                },
            )
        })
        .collect()
}

// Whether the branch (or the detached HEAD) the intent moves is at its new
// value.
fn moved(worktree: &Path, intent: &AdvanceIntent) -> Result<bool> {
    Ok(match &intent.from.symbolic {
        Some(branch) => {
            let (native, _) = native_refs::native_refs(worktree)?;
            native.get(branch) == Some(&intent.to.oid)
        }
        None => head_of(worktree)? == intent.to,
    })
}

/// Advance the live checkout at `place` to the capture `staged` (its
/// scratch repository `scratch`, metadata `meta`): module docs.
///
/// # Errors
/// `GIT_ADVANCE_NOT_ANCESTOR`, `GIT_ADVANCE_BRANCH_MISMATCH`,
/// `GIT_ADVANCE_BRANCH_BUSY`, `GIT_ADVANCE_NOT_CLEAN`,
/// `GIT_ADVANCE_UNTRACKED_COLLIDES` and `GIT_ADVANCE_RACED` with nothing
/// written at the live checkout; `GIT_DESTINATION_OCCUPIED` when another
/// apply landed, adopted or advanced it since; whatever the import refuses;
/// `GIT_CONVERGE_INTERRUPTED` (or `DESTINATION_EXCHANGE_UNSUPPORTED`) after
/// the intent, kept for the rerun's settle.
#[allow(clippy::too_many_lines)]
pub(super) fn workspace(
    staged: &super::super::StagedBundle,
    landing: &Landing<'_>,
    place: &Place<'_>,
    scratch: &Scratch,
    meta: &Meta,
) -> Result<Applied> {
    let worktree = place.worktree;
    let source = place.source;
    let proven = prove(place, scratch, meta)?;
    let planned = plan(
        worktree,
        (&proven.old, &sides(&scratch.repo, meta)?),
        &proven.captured,
        &proven.untracked,
    )?;
    if !displaced_hold(worktree, &planned.ops)? {
        return Err(BulkloadRefusal::GitAdvanceNotClean);
    }
    #[cfg(test)]
    super::super::mid_pass::fire(worktree, super::super::mid_pass::Stage::AdvanceBeforeLock);
    let lock = landed::lock(&place.common, source)?;
    // Another apply (another STATE or plan) may have landed, adopted or
    // advanced the checkout since the proof.
    let dir = place.workspace_dir();
    if landed::exists(&dir.join(landed::WORKSPACE_LANDED))? || landed::exists(&dir.join(INTENT))? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let mut custody = super::settle_repository(worktree, &place.common, source)?;
    custody.add("probe-removed", super::sweep_probes(worktree)?);
    // The capture's objects and provenance first (additive; the live
    // checkout's worktree, index and refs untouched), so the proof below
    // is the last read before the intent and the branch's compare-and-swap.
    if let Some(base) = place.base {
        super::super::import_verified(worktree, base.path(), source)?;
    }
    let (_, instance) = super::super::import_verified_instance(worktree, staged.path(), source)?;
    if instance != scratch.instance {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let meta1 = Meta::read(worktree, source, &instance)?;
    let new = sides(worktree, &meta1)?;
    // The proof again, under the lock: HEAD, its branch, the index and
    // every proved seat as they were, and the untracked files (one added
    // since must collide with no captured path either).
    if !still(place, &proven)? {
        return Err(BulkloadRefusal::GitAdvanceRaced);
    }
    let untracked = status(worktree, &place.git_dir)?.ok_or(BulkloadRefusal::GitAdvanceRaced)?;
    let colliding: Vec<Vec<u8>> = untracked
        .iter()
        .filter(|path| proven.captured.collides(path))
        .cloned()
        .collect();
    if !colliding.is_empty() {
        return Err(collides(&colliding));
    }
    // Planned again under the lock, from the import: an entry the live
    // checkout gained since the proof where the capture lands one is left
    // to it (or collides), never written over.
    let planned = plan(worktree, (&proven.old, &new), &proven.captured, &untracked)?;
    let tag = super::tag(landing);
    if planned.ops.iter().any(|op| op.action == Action::Replace) {
        probe_exchange(worktree, &tag)?;
    }
    let intent_path = dir.join(INTENT);
    let mut intent = AdvanceIntent {
        item: landing.item.to_owned(),
        capture: landing.capture,
        observed_ns: landing.observed_ns,
        instance,
        tag: tag.clone(),
        from: proven.from.clone(),
        to: proven.to.clone(),
        index_old: proven.index,
        old: old_seats(&proven.old),
        prints: proven.prints.values().cloned().collect(),
        left: planned.left.clone(),
        branch_done: false,
    };
    landed::ensure_dir(&place.git_dir, source)?;
    landed::write(&intent_path, &intent)?;
    fault_point!(EstateAdvanceAfterIntent);
    #[cfg(test)]
    super::super::mid_pass::fire(worktree, super::super::mid_pass::Stage::AdvanceBeforeBranch);
    // The branch first, by compare-and-swap from the proved tip: one moved
    // since (a commit, a reset) leaves nothing written but this intent,
    // which goes. A Git child that failed after its transaction committed
    // leaves the branch moved, and the advance goes on.
    let branch = proven.from.symbolic.as_ref().map(|name| {
        (
            name.clone(),
            Some(proven.from.oid.clone()),
            proven.to.oid.clone(),
        )
    });
    if set_head(worktree, &proven.from, &proven.to, branch.as_ref()).is_err()
        && !moved(worktree, &intent)?
    {
        landed::remove(&intent_path)?;
        return Err(BulkloadRefusal::GitAdvanceRaced);
    }
    // HEAD must still be on the branch the compare-and-swap moved (Git
    // cannot verify HEAD's target in the transaction that moves it, as
    // moving it logs HEAD too): a switch in the instant since puts the
    // branch back by compare-and-swap and refuses, nothing else written.
    if head_of(worktree)? != proven.to {
        if let (Some((name, _, _)), true) = (&branch, proven.from.oid != proven.to.oid) {
            let back = format!("update {name}\0{}\0{}\0", proven.from.oid, proven.to.oid);
            let _ = native_refs::update_refs(worktree, None, back.as_bytes(), &[name.as_str()]);
        }
        if !moved(worktree, &intent)? {
            landed::remove(&intent_path)?;
        }
        return Err(BulkloadRefusal::GitAdvanceRaced);
    }
    intent.branch_done = true;
    landed::write(&intent_path, &intent)?;
    fault_point!(EstateAdvanceAfterBranch);
    let after = || -> Result<Custody> {
        let mut engine = Engine {
            worktree,
            tag: &tag,
            proof: Some(&proven.prints),
            objects: BatchObjects::new(worktree)?,
            touched: BTreeMap::new(),
            written: BTreeMap::new(),
            custody: Custody::default(),
        };
        for op in &planned.ops {
            engine.run(op)?;
            fault_point!(EstateAdvanceAfterSeat);
        }
        engine.modes(&planned.left.mode_sides(&new))?;
        engine.objects.finish()?;
        super::super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
        let index = publish_index(
            worktree,
            &place.git_dir,
            (&format!("{}^{{tree}}", proven.from.oid), &meta1),
            proven.index,
            &tag,
        )?;
        fault_point!(EstateAdvanceAfterIndex);
        let mut custody = finish(
            place,
            &intent,
            index,
            &new,
            (&engine.written, &proven.prints),
        )?;
        custody.merge(engine.custody);
        Ok(custody)
    };
    custody.merge(after().map_err(after_intent)?);
    drop(lock);
    // Every custody word, the routine ones too, as an adoption's receipt
    // says what it created and kept.
    let mut applied = Applied::adopted(&custody);
    applied.advanced = true;
    Ok(applied)
}

// The advance record, then the workspace ledger (every captured seat but
// those left to the live checkout, each with its print: the one written,
// the one the proof took for a seat no operation touched, else read now),
// then the intent removed.
fn finish(
    place: &Place<'_>,
    intent: &AdvanceIntent,
    index: [u8; 32],
    new: &BTreeMap<Vec<u8>, Side>,
    (written, proved): (&Prints, &Prints),
) -> Result<Custody> {
    let mut custody = Custody::default();
    let dir = landed::ensure_dir(&place.git_dir, place.source)?;
    let record = Advanced {
        item: intent.item.clone(),
        capture: intent.capture,
        observed_ns: intent.observed_ns,
        from: intent.from.clone(),
        to: intent.to.clone(),
        recorded_ns: super::super::pass_start_ns(),
        left: intent.left.clone(),
    };
    landed::write(&dir.join(ADVANCED), &record)?;
    let mut seats = Vec::with_capacity(new.len());
    for (path, side) in new {
        if record.keeps(path) {
            continue;
        }
        let print = if let Some(print) = written.get(path) {
            print.clone()
        } else if let (Some(print), false) = (proved.get(path), side.kind == Kind::Directory) {
            print.clone()
        } else {
            let at = safe_destination(place.worktree, Path::new(OsStr::from_bytes(path)))?;
            let meta = lstat(&at)?.ok_or_else(interrupted)?;
            SeatPrint::of(path, &meta).ok_or_else(interrupted)?
        };
        seats.push(print);
    }
    let ledger = LandedWorkspace {
        item: intent.item.clone(),
        capture: intent.capture,
        observed_ns: intent.observed_ns,
        instance: intent.instance.clone(),
        head: intent.to.clone(),
        intended_branch: None,
        index,
        exclude: exclude_digest(&read_exclude(&place.common)?),
        recorded_ns: super::super::pass_start_ns(),
        fsid: landed::fsid(place.worktree)?,
        seats,
    };
    match landed::encode(&ledger) {
        Ok(bytes) => landed::write_bytes(&dir.join(landed::WORKSPACE_LANDED), &bytes)?,
        Err(BulkloadRefusal::BudgetExceeded) => custody.count("not-convergeable"),
        Err(other) => return Err(other),
    }
    fault_point!(EstateAdvanceAfterLedger);
    landed::remove(&dir.join(INTENT))?;
    custody.count("advanced");
    if intent.from.oid != intent.to.oid {
        custody.count("advance-branch-moved");
    }
    custody.add(
        "advance-kept",
        u64::try_from(intent.left.kept.len()).unwrap_or(u64::MAX),
    );
    custody.add(
        "advance-kept-dirs",
        u64::try_from(intent.left.kept_dirs.len()).unwrap_or(u64::MAX),
    );
    Ok(custody)
}

/// Settle an advance a crash cut short, under the destination lock: its
/// intent's branch never moved (nothing of it but the intent written), so
/// the intent is dropped and `None` returned (the checkout is proved
/// afresh); else every component is brought forward, each seat by its
/// content (the live HEAD's, the capture's, or a third party's:
/// `GIT_CONVERGE_INTERRUPTED`, the intent kept), the index by
/// compare-and-swap on its entries, then the record, the ledger, the
/// intent removed.
///
/// # Errors
/// `GIT_DESTINATION_OCCUPIED` for another item's intent;
/// `GIT_CONVERGE_INTERRUPTED` as above; IO.
pub(super) fn settle(place: &Place<'_>, landing: &Landing<'_>) -> Result<Option<Custody>> {
    let worktree = place.worktree;
    let intent_path = place.workspace_dir().join(INTENT);
    let Some(mut intent) = landed::read::<AdvanceIntent>(&intent_path)? else {
        return Ok(None);
    };
    if intent.item != landing.item {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    if !intent.branch_done {
        if !moved(worktree, &intent)? {
            landed::remove(&intent_path)?;
            return Ok(None);
        }
        intent.branch_done = true;
        landed::write(&intent_path, &intent)?;
    }
    let settled = || -> Result<Custody> {
        // HEAD (or its branch) moved by anyone since the advance moved it
        // is theirs: refused, never moved again.
        if head_of(worktree)? != intent.to {
            return Err(interrupted());
        }
        let meta1 = Meta::read(worktree, place.source, &intent.instance)?;
        let new = sides(worktree, &meta1)?;
        let old = old_sides(&intent.old);
        let mut engine = Engine {
            worktree,
            tag: &intent.tag,
            proof: None,
            objects: BatchObjects::new(worktree)?,
            touched: BTreeMap::new(),
            written: BTreeMap::new(),
            custody: Custody::default(),
        };
        for op in delta(&old, &new)
            .into_iter()
            .filter(|op| !intent.left.leaves(&op.path))
        {
            engine.run(&op)?;
        }
        engine.modes(&intent.left.mode_sides(&new))?;
        engine.objects.finish()?;
        super::super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
        let index = publish_index(
            worktree,
            &place.git_dir,
            (&format!("{}^{{tree}}", intent.from.oid), &meta1),
            intent.index_old,
            &intent.tag,
        )?;
        let proved: Prints = intent
            .prints
            .iter()
            .map(|print| (print.path.clone(), print.clone()))
            .collect();
        let mut custody = finish(place, &intent, index, &new, (&engine.written, &proved))?;
        custody.merge(engine.custody);
        custody.count("advance-settled");
        Ok(custody)
    };
    settled().map(Some).map_err(after_intent)
}

/// The HEAD plan of a later converge of an advanced checkout (`place`,
/// workspace ledger `ledger`): its branch, when the capture's (`symbolic`)
/// is still the one HEAD is on, no other worktree has it checked out
/// (`others`) and it still holds the value bulkload left it at (`native`),
/// moves to the captured HEAD `h1` with HEAD, by compare-and-swap, as the
/// advance moved it (LIVE-30: bulkload's own unchanged output). `None`
/// leaves the attach rule's plan.
pub(super) fn branch_plan(
    place: &Place<'_>,
    ledger: &LandedWorkspace,
    symbolic: Option<&str>,
    (native, others): (&BTreeMap<String, String>, &BTreeSet<String>),
    h1: &str,
) -> Option<HeadPlan> {
    place.advanced.as_ref()?;
    let branch = symbolic.filter(|_| !place.beside)?;
    if ledger.head.symbolic.as_deref() != Some(branch)
        || others.contains(branch)
        || native.get(branch) != Some(&ledger.head.oid)
        || ledger.head.oid == h1
    {
        return None;
    }
    Some(HeadPlan {
        head: Head {
            symbolic: Some(branch.to_owned()),
            oid: h1.to_owned(),
        },
        branch: Some((
            branch.to_owned(),
            Some(ledger.head.oid.clone()),
            h1.to_owned(),
        )),
        intended: None,
        line: None,
        row: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_kept_entry_leaves_its_subtree_and_a_kept_directory_only_itself() {
        let left = Left {
            kept: vec![b".env".to_vec(), b"build/out".to_vec()],
            kept_dirs: vec![b".claude".to_vec()],
            untracked: vec![b"docs/review".to_vec(), b"notes.md".to_vec()],
        };
        assert!(left.own_untracked(b"docs/review"));
        assert!(left.own_untracked(b"docs/review/a.md"));
        assert!(!left.own_untracked(b"docs"));
        assert!(!left.own_untracked(b"notes.md.bak"));
        assert!(left.keeps(b".env"));
        assert!(left.keeps(b"build/out"));
        assert!(left.keeps(b"build/out/deep/file"));
        assert!(!left.keeps(b"build/outer"));
        assert!(!left.keeps(b"build"));
        assert!(!left.keeps(b".envrc"));
        assert!(left.leaves(b".claude"));
        assert!(!left.leaves(b".claude/settings.json"));
        assert!(!left.keeps(b".claude"));
    }

    /// The coordinator's rule: an untracked entry collides with a captured
    /// path that is the same, under it, or a captured file above it; a
    /// captured directory above it is the live checkout's business.
    #[test]
    fn an_untracked_entry_collides_only_by_path() {
        let set = |paths: &[&str]| -> BTreeSet<Vec<u8>> {
            paths.iter().map(|path| path.as_bytes().to_vec()).collect()
        };
        let captured = Captured {
            tracked: set(&["docs/agent-notes/n1.md", "tofu/modules/main.tf", "README"]),
            files: set(&[
                "docs/agent-notes/n1.md",
                "tofu/modules/main.tf",
                "README",
                ".serena/neo.yml",
            ]),
            dirs: set(&["docs", "docs/agent-notes", ".serena", "empty"]),
        };
        for (path, collides) in [
            ("docs/agent-notes/2026-08-28-convergence-review", false),
            ("docs/agent-notes/review/x.md", false),
            ("tofu", true),
            ("tofu/modules", true),
            (".serena", true),
            ("README", true),
            ("README/inner", true),
            ("empty", true),
            ("README.md", false),
            ("notes", false),
        ] {
            assert_eq!(captured.collides(path.as_bytes()), collides, "{path}");
        }
        assert!(captured.tracks(b"tofu"));
        assert!(captured.tracks(b"README"));
        assert!(!captured.tracks(b".serena"));
        assert!(!captured.tracks(b"docs/agent-notes/review"));
    }

    #[test]
    fn the_records_keep_their_own_prefixes() {
        use landed::Record as _;
        let prefixes = [
            AdvanceIntent::MAGIC,
            Advanced::MAGIC,
            AdvanceRecord::MAGIC,
            LandedWorkspace::MAGIC,
        ];
        for (n, a) in prefixes.iter().enumerate() {
            for b in prefixes.iter().skip(n + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
