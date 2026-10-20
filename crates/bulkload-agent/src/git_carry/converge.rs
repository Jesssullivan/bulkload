//! Estate convergence: a re-apply brings an unchanged landed checkout to a
//! newer capture.
//!
//! Design: `docs/agent-notes/2026-10-08-estate-converge-design.md`, as
//! amended by its 2026-10-08 review. A checkout bulkload landed, and nobody
//! changed since, is brought to the new capture.
//!
//! The order of one item (section 8, amended):
//!
//! 1. settle any pending intent forward (`refs.intent`, `workspace.intent`);
//! 2. prove the landed checkout against its ledger (read-only; a failure is
//!    `GIT_DESTINATION_MODIFIED` with nothing written);
//! 3. import the capture's objects and provenance (additive);
//! 4. refuse a changed shape (`GIT_CONVERGE_SHAPE_CHANGED`), plan the delta,
//!    refuse a third party's seat at a path the source added;
//! 5. write the workspace intent;
//! 6. the native ref step, every branch some worktree has checked out left
//!    to that worktree's own item (finding 1);
//! 7. the seats, each checked through the descriptor it was proved by
//!    (finding 10), replaced by `RENAME_EXCHANGE` and the displaced file
//!    checked;
//! 8. the index under Git's own `index.lock`, compare-and-swap on its entries
//!    (finding 3);
//! 9. HEAD and its branch in one `update-ref` transaction, after the index
//!    (finding 2);
//! 10. the new workspace ledger, then the intent removed.
//!
//! Every refusal after step 5 is `GIT_CONVERGE_INTERRUPTED`, the intent is
//! kept, and closure counts the item unaccounted until a rerun settles it
//! (finding 6).

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use bulkload_proto::FileKind;

use super::batch_objects::BatchObjects;
use super::landed::{
    self, Head, Kind, LandedRef, LandedWorkspace, RefLedger, SeatPrint, WorkspaceIntent,
};
use super::native_refs::{self, Custody};
use super::{git, oid, output, safe_destination, text};
use crate::{BulkloadRefusal, Result};

/// What an item's apply knows about the capture it applies.
#[derive(Debug, Clone, Copy)]
pub struct Landing<'a> {
    /// `estate::id` of the item; empty for a CLI restore.
    pub item: &'a str,
    /// The capture's `started_ns` (its `.parts` sidecar; 0 when legacy).
    pub observed_ns: i128,
    /// The capture's bundle digest.
    pub capture: [u8; 32],
    /// Where an estate apply records the parents a first landing creates
    /// (`{item}-{source}.parents` in STATE; M2 finding 1). `None` (a CLI
    /// restore) keeps `GIT_DESTINATION_PARENT_MISSING` for an absent
    /// parent.
    pub parents: Option<&'a Path>,
    /// `--exclude-union` (M2 round 2, item 2): where an estate apply
    /// records the lines a first linked landing appends to the common
    /// `info/exclude` (`{item}-{source}.exclude-union` in STATE,
    /// [`super::exclude_union`]). `None` keeps
    /// `GIT_IGNORE_POLICY_CONFLICT` for any exclude that is not the
    /// capture's.
    pub exclude_union: Option<&'a Path>,
    /// A beside landing (`estate-apply --beside`, M2 round 2 item c): a
    /// first linked landing into the planned repository at the beside path,
    /// detached at the captured head, that never steps the repository's
    /// native refs.
    pub beside: bool,
}

impl<'a> Landing<'a> {
    /// The record [`super::parents::FirstTarget::place`] appends to, with
    /// the item it names.
    #[must_use]
    pub fn parents_record(&self) -> Option<(&'a Path, &'a str)> {
        self.parents.map(|record| (record, self.item))
    }
}

/// What an apply did, for its receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// A converge (or a settle) ran, not a first landing.
    pub converged: bool,
    /// Custody counts, as `key=value` words.
    pub custody: Option<String>,
    /// `--advance-ancestor` (operator ruling 2026-10-19): this apply
    /// advanced a live checkout bulkload did not land to the capture
    /// ([`advance`]), or finished such an advance a crash cut short, or
    /// found the advance of this capture already done.
    pub advanced: bool,
}

/// Custody words that describe the ordinary work of a landing. A first
/// landing's receipt leaves them out (every first landing creates refs and
/// seats); a converge's receipt keeps them.
const ROUTINE: &[&str] = &[
    "ref-created",
    "ref-adopted",
    "ref-moved",
    "ref-pruned",
    "seats-added",
    "seats-replaced",
    "seats-removed",
    "seats-mode",
    "stash-restored",
    "stash-appended",
    "stash-rewritten",
];

impl Applied {
    /// A first landing's: its exceptional custody words only.
    pub(super) fn first(custody: &Custody) -> Self {
        Self::of(false, custody)
    }

    /// An adoption's (#226): every custody word, the routine ones too, so
    /// the receipt says what adopt created and kept.
    pub(super) fn adopted(custody: &Custody) -> Self {
        Self {
            converged: false,
            custody: custody.words(),
            advanced: false,
        }
    }

    fn of(converged: bool, custody: &Custody) -> Self {
        let mut shown = custody.clone();
        if !converged {
            shown.0.retain(|key, _| !ROUTINE.contains(key));
        }
        Self {
            converged,
            custody: shown.words(),
            advanced: false,
        }
    }
}

/// What one apply run may do beyond the default (estate adopt, #226,
/// OI-1003-Q171/Q172). Per run and explicit, recorded in every receipt it
/// changes: never inferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct Policy<'a> {
    /// `--adopt`: an existing checkout or repository bulkload did not land
    /// becomes bulkload's by proof (A11a, A11d), and a ledger naming
    /// another plan's item is re-keyed after its proof when the checkout is
    /// spelled as it is and the capture is the landed tree's (A11b).
    pub adopt: bool,
    /// `--activate-origin PREFIX`: a first landing activates an scp-style
    /// or `ssh://` origin that starts with one of these (#226 item 6).
    pub origins: &'a [String],
    /// Every item id of the run's plan: A11b never re-keys a ledger that
    /// names one of them (review of 2026-10-16, finding 2).
    pub items: &'a [String],
    /// `--advance-ancestor` (operator ruling 2026-10-19, "Yes, advance
    /// those"): under `adopt`, a live checkout bulkload did not land whose
    /// adopt proof finds it differs is advanced to the capture when it is
    /// clean, its HEAD an ancestor of the captured HEAD and its branch the
    /// captured one, free to move ([`advance`]); otherwise a typed
    /// `GIT_ADVANCE_*` refusal, with nothing written. It holds the beside
    /// suffix (`--beside SUFFIX`) a file-level beside takes.
    pub advance: Option<&'a str>,
}

/// Where an item lands.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// The repository: the workspace itself for a standalone item.
    pub repository: &'a Path,
    /// The workspace, `None` for a refs-only item.
    pub workspace: Option<&'a Path>,
    /// The SOURCE label.
    pub source: &'a str,
    /// A standalone item's plan base (a based capture), imported where the
    /// capture lands: the private stage, or the checkout before a converge.
    /// A linked item's, when its apply has not imported it into the
    /// repository yet: imported with the capture, after every refusal
    /// decidable without a write (dogfood of 2026-10-16, F2).
    pub base: Option<&'a super::StagedBundle>,
}

// ---- provenance metadata of one instance -----------------------------------

/// The metadata refs of a provenance instance, by suffix.
#[derive(Debug, Clone)]
pub(super) struct Meta {
    pub instance: String,
    refs: BTreeMap<String, String>,
}

impl Meta {
    /// Read `refs/carry/v1/<source>/<instance>/*` (not its native refs or
    /// stashes) from `repo`. An instance the destination does not hold is
    /// `SEALED_OBJECT_MISSING` (finding 14: settling needs only this).
    pub(super) fn read(repo: &Path, source: &str, instance: &str) -> Result<Self> {
        let prefix = format!("refs/carry/v1/{source}/{instance}/");
        let listed = text(git(repo).args([
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            &format!("--exclude={prefix}refs/"),
            &format!("--exclude={prefix}stashes/"),
            &prefix,
        ]))?;
        let mut refs = BTreeMap::new();
        for line in listed.lines() {
            let (value, name) = line
                .split_once(' ')
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            let suffix = name
                .strip_prefix(prefix.as_str())
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            if !oid(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            refs.insert(suffix.to_owned(), value.to_owned());
        }
        if !refs.contains_key(super::WORKTREE_REVISION) || !refs.contains_key("head") {
            return Err(BulkloadRefusal::SealedObjectMissing);
        }
        Ok(Self {
            instance: instance.to_owned(),
            refs,
        })
    }

    pub(super) fn get(&self, suffix: &str) -> Result<&str> {
        self.refs
            .get(suffix)
            .map(String::as_str)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    }

    fn optional(&self, suffix: &str) -> Option<&str> {
        self.refs.get(suffix).map(String::as_str)
    }

    /// The instance as bundle header lines (`<oid> refs/carry-export/<suffix>`),
    /// for the readers that take a capture's headers.
    pub(super) fn heads(&self) -> String {
        let mut heads = String::new();
        for (suffix, value) in &self.refs {
            heads.push_str(value);
            heads.push_str(" refs/carry-export/");
            heads.push_str(suffix);
            heads.push('\n');
        }
        heads
    }

    fn value(&self, repo: &Path, suffix: &str) -> Result<Vec<u8>> {
        output(git(repo).args(["show", &format!("{}:value", self.get(suffix)?)]))
    }

    /// The captured symbolic HEAD (`None` = detached).
    pub(super) fn symbolic(&self, repo: &Path) -> Result<Option<String>> {
        let value = String::from_utf8(self.value(repo, "head-symbolic")?)
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        if value.is_empty() {
            return Ok(None);
        }
        if !value.starts_with("refs/heads/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(repo).args(["check-ref-format", &value]))?;
        Ok(Some(value))
    }

    fn rows(&self, repo: &Path) -> Result<Vec<crate::RowSchema>> {
        postcard::from_bytes(&self.value(repo, "filesystem-v1")?)
            .map_err(|_| BulkloadRefusal::FrameCodec)
    }

    fn exclude(&self, repo: &Path) -> Result<Vec<u8>> {
        self.value(repo, "exclude")
    }
}

// ---- reading the destination -----------------------------------------------

/// The digest of an index's entries: path, mode, oid and stage, and the
/// assume-valid, intent-to-add and skip-worktree flags (design 3.4). Stat
/// data and extensions are not in it, so `git status` refreshing the stat
/// cache is not a change.
pub(super) fn index_digest(worktree: &Path, index: Option<&Path>) -> Result<[u8; 32]> {
    const FLAGS: u32 = 0x8000 | 0x2000_0000 | 0x4000_0000;
    let command = || {
        let mut command = git(worktree);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command
    };
    let stage = output(command().args(["ls-files", "--stage", "-z"]))?;
    let debug = String::from_utf8(output(command().args(["ls-files", "--debug"]))?)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let flags = debug
        .lines()
        .filter_map(|line| line.split_once("\tflags: ").map(|(_, flags)| flags))
        .map(|flags| u32::from_str_radix(flags.trim(), 16).map(|flags| flags & FLAGS))
        .collect::<std::result::Result<Vec<u32>, _>>()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let entries: Vec<&[u8]> = stage
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    if entries.len() != flags.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-index-entries-v1\0");
    for (entry, flags) in entries.into_iter().zip(flags) {
        hasher.update(entry);
        hasher.update(&[0]);
        hasher.update(&flags.to_le_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

/// A worktree's HEAD. An unborn one (an orphan switch, or the checked-out
/// branch deleted) has an empty oid, never a refusal of its own (review 3
/// finding 4): the proof then refuses `GIT_DESTINATION_MODIFIED` and a
/// settle `GIT_CONVERGE_INTERRUPTED`, as for any other HEAD.
pub(super) fn head_of(worktree: &Path) -> Result<Head> {
    let verified = git(worktree)
        .args(["rev-parse", "-q", "--verify", "HEAD"])
        .output()
        .refuse_at("git_carry::converge::head_of")?;
    let oid = match verified.status.code() {
        Some(0) => {
            let oid = String::from_utf8(verified.stdout)
                .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
                .trim_end()
                .to_owned();
            if !super::oid(&oid) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            oid
        }
        Some(1) => String::new(),
        _ => return Err(super::estimate::child_failed(&verified.stderr)),
    };
    let symbolic = git(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .refuse_at("git_carry::converge::head_of")?;
    let name = match symbolic.status.code() {
        Some(0) => Some(
            String::from_utf8(symbolic.stdout)
                .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
                .trim_end()
                .to_owned(),
        ),
        Some(1) => None,
        _ => return Err(super::estimate::child_failed(&symbolic.stderr)),
    };
    Ok(Head {
        symbolic: name,
        oid,
    })
}

fn exclude_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn read_exclude(common: &Path) -> Result<Vec<u8>> {
    match fs::read(common.join("info/exclude")) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(crate::refuse::io(
            &error,
            "git_carry::converge::read_exclude",
        )),
    }
}

/// Each worktree of `repo` as `worktree list --porcelain` lists it: its
/// path and the branch its HEAD names, if any.
fn worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found: Vec<(PathBuf, Option<String>)> = Vec::new();
    for field in listed.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            found.push((PathBuf::from(OsStr::from_bytes(path)), None));
        } else if let Some(name) = field.strip_prefix(b"branch ") {
            let name =
                std::str::from_utf8(name).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
            if let Some((_, branch)) = found.last_mut() {
                *branch = Some(name.to_owned());
            }
        }
    }
    Ok(found)
}

/// One worktree `git worktree list` names (`estate-move-source`'s
/// inventory, M2 item 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The worktree's path as Git records it (its real path).
    pub path: PathBuf,
    /// The repository itself is bare (only ever the first entry).
    pub bare: bool,
    /// Git calls the worktree prunable: its directory is gone.
    pub prunable: bool,
}

/// Every worktree `repo` registers, its main first, with whether the
/// repository is bare and whether Git calls a worktree prunable. Read-only
/// (the same `worktree list` [`worktrees`] reads).
///
/// # Errors
/// Whatever `git worktree list` refuses.
pub fn worktree_census(repo: &Path) -> Result<Vec<Listed>> {
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found: Vec<Listed> = Vec::new();
    for field in listed.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            found.push(Listed {
                path: PathBuf::from(OsStr::from_bytes(path)),
                bare: false,
                prunable: false,
            });
        } else if let Some(last) = found.last_mut() {
            if field == b"bare" {
                last.bare = true;
            } else if field == b"prunable" || field.starts_with(b"prunable ") {
                last.prunable = true;
            }
        }
    }
    Ok(found)
}

/// The git directory of every worktree of `repo` that has one (the main
/// worktree's `.git`, each linked worktree's `worktrees/<id>`).
pub(super) fn worktree_git_dirs(repo: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for (path, _) in worktrees(repo)? {
        if let Some(git_dir) = landed::worktree_git_dir(&path)?.filter(|dir| dir.is_dir()) {
            dirs.push(git_dir);
        }
    }
    Ok(dirs)
}

/// Whether `a` and `b` are one directory.
pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    same_path(a, b)
}

// A small Git state file of `git_dir`, trimmed; `None` when absent.
fn state_file(git_dir: &Path, name: &str) -> Result<Option<String>> {
    match fs::read(git_dir.join(name)) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).trim_end().to_owned())),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(crate::refuse::io(&error, "git_carry::converge::state_file")),
    }
}

/// The branch a rebase or a bisect in progress in the worktree whose git
/// directory is `git_dir` detached it from (review 3 finding 2): Git's own
/// `rebase-merge/head-name`, `rebase-apply/head-name` and `BISECT_START`,
/// which `is_worktree_being_rebased` and `_bisected` read so that `git
/// branch -f` refuses that branch.
fn held_branches(git_dir: &Path) -> Result<Vec<String>> {
    let mut held = Vec::new();
    for name in ["rebase-merge/head-name", "rebase-apply/head-name"] {
        if let Some(branch) = state_file(git_dir, name)?.filter(|b| b.starts_with("refs/heads/")) {
            held.push(branch);
        }
    }
    if let Some(start) =
        state_file(git_dir, "BISECT_START")?.filter(|start| !start.is_empty() && !oid(start))
    {
        held.push(if start.starts_with("refs/heads/") {
            start
        } else {
            format!("refs/heads/{start}")
        });
    }
    Ok(held)
}

/// Branches checked out by each worktree of `repo`: (worktree path,
/// branch): its HEAD's branch, and the branch a rebase or bisect in
/// progress holds (review 3 finding 2: a rebasing worktree's HEAD is
/// detached, but its branch is the rebase's to move).
pub(super) fn checked_out_by(repo: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut found = Vec::new();
    for (path, branch) in worktrees(repo)? {
        if let Some(branch) = branch {
            found.push((path.clone(), branch));
        }
        if let Some(git_dir) = landed::worktree_git_dir(&path)? {
            for held in held_branches(&git_dir)? {
                found.push((path.clone(), held));
            }
        }
    }
    Ok(found)
}

/// The branches a pending workspace intent of any worktree of `repo` (for
/// `source`) moves with HEAD or puts HEAD on: only that item's `set_head`
/// moves them, so every other ref step leaves them (the earlier report's
/// finding 2: a step that moved one made the item's settle replay a stale
/// compare-and-swap).
pub(super) fn intent_branches(repo: &Path, source: &str) -> Result<BTreeSet<String>> {
    let mut branches = BTreeSet::new();
    for git_dir in worktree_git_dirs(repo)? {
        let path = landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_INTENT);
        if let Some(intent) = landed::read::<WorkspaceIntent>(&path)? {
            branches.extend(intent.branch.map(|(name, _, _)| name));
            branches.extend(intent.head.1.symbolic);
        }
    }
    Ok(branches)
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

// ---- the landing ledger ----------------------------------------------------

/// The prints of every carried seat of `rows` at `worktree`.
fn census(worktree: &Path, rows: &[crate::RowSchema]) -> Result<Vec<SeatPrint>> {
    let mut seats = Vec::with_capacity(rows.len());
    for row in rows {
        let path = safe_destination(worktree, Path::new(OsStr::from_bytes(&row.rel_path)))?;
        let meta = fs::symlink_metadata(&path).refuse_at("git_carry::converge::census")?;
        seats.push(
            SeatPrint::of(&row.rel_path, &meta).ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        );
    }
    seats.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(seats)
}

/// The workspace ledger of a checkout just landed at `worktree` (git
/// directory `git_dir`, common `common`) from `meta`. `index` is the digest
/// of the index the landing installed (review 3 finding 3: a linked
/// landing, laid down in place, never re-reads it); `None` reads it, for a
/// private stage nobody else sees. `false` over the ledger cap: the
/// checkout is landed without one and reported `not-convergeable` (finding
/// 20.6).
#[allow(clippy::too_many_arguments)]
pub(super) fn record_landing(
    worktree: &Path,
    git_dir: &Path,
    common: &Path,
    target: &Target<'_>,
    landing: &Landing<'_>,
    meta: &Meta,
    head: (Head, Option<String>),
    index: Option<[u8; 32]>,
) -> Result<bool> {
    let index = match index {
        Some(index) => index,
        None => index_digest(worktree, None)?,
    };
    let ledger = LandedWorkspace {
        item: landing.item.to_owned(),
        capture: landing.capture,
        observed_ns: landing.observed_ns,
        instance: meta.instance.clone(),
        head: head.0,
        intended_branch: head.1,
        index,
        exclude: exclude_digest(&read_exclude(common)?),
        recorded_ns: super::pass_start_ns(),
        fsid: landed::fsid(worktree)?,
        seats: census(worktree, &meta.rows(worktree)?)?,
    };
    let bytes = match landed::encode(&ledger) {
        Ok(bytes) => bytes,
        Err(BulkloadRefusal::BudgetExceeded) => return Ok(false),
        Err(other) => return Err(other),
    };
    let directory = landed::ensure_dir(git_dir, target.source)?;
    landed::write_bytes(&directory.join(landed::WORKSPACE_LANDED), &bytes)?;
    Ok(true)
}

// ---- HEAD and its branch -----------------------------------------------------

/// Where HEAD goes, and the branch move that goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeadPlan {
    pub head: Head,
    /// `(name, old, new)`: the HEAD branch created or moved with HEAD.
    pub branch: Option<(String, Option<String>, String)>,
    pub intended: Option<String>,
    pub line: Option<&'static str>,
    /// The ref ledger row the branch gets.
    pub row: Option<(String, LandedRef)>,
}

/// The attach rule, for a first landing and a converge alike (findings 11,
/// 17): the source's branch when bulkload may put HEAD on it at `h1`,
/// otherwise detached at `h1` with the branch recorded as intended.
/// `others` are the branches other worktrees have checked out; `ledger` is
/// the ref ledger of a repository bulkload created (`None`: a foreign one,
/// where a branch is used only when it already names `h1`). `refs` are the
/// destination's native refs and the names of its symbolic ones (review 4:
/// a branch beside or at a symbolic ref can never be created).
pub(super) fn head_plan(
    symbolic: Option<&str>,
    h1: &str,
    refs: (&BTreeMap<String, String>, &BTreeSet<String>),
    ledger: Option<&RefLedger>,
    others: &BTreeSet<String>,
    t: i128,
) -> HeadPlan {
    let (native, symbolic_refs) = refs;
    let detached = |intended: Option<&str>, line: Option<&'static str>| HeadPlan {
        head: Head {
            symbolic: None,
            oid: h1.to_owned(),
        },
        branch: None,
        intended: intended.map(str::to_owned),
        line,
        row: None,
    };
    let Some(branch) = symbolic else {
        return detached(None, None);
    };
    if others.contains(branch) {
        return detached(Some(branch), Some("head-detached-ref-conflict"));
    }
    let attached = |branch_move, row| HeadPlan {
        head: Head {
            symbolic: Some(branch.to_owned()),
            oid: h1.to_owned(),
        },
        branch: branch_move,
        intended: None,
        line: None,
        row,
    };
    let current = native.get(branch).map(String::as_str);
    // Review 2 finding 3: a branch whose name is a path prefix of an
    // existing ref, or under one (`foo` kept by R7, `foo/bar` new), cannot
    // be created; it detaches with the branch recorded.
    if current.is_none()
        && (symbolic_refs.contains(branch)
            || native_refs::name_conflict(
                branch,
                native.keys().chain(symbolic_refs).map(String::as_str),
            ))
    {
        return detached(Some(branch), Some("head-detached-ref-conflict"));
    }
    // A foreign repository: its branches are not bulkload's. One that
    // names `h1` is used; an absent one is created (a new name overwrites
    // nothing, as the old `carry/*` branch did not); any other detaches.
    let Some(ledger) = ledger.filter(|ledger| ledger.created) else {
        return match current {
            Some(value) if value == h1 => attached(None, None),
            None => attached(Some((branch.to_owned(), None, h1.to_owned())), None),
            Some(_) => detached(Some(branch), Some("head-detached-ref-conflict")),
        };
    };
    let decision = native_refs::decide(native_refs::Input {
        native: current,
        row: ledger.refs.get(branch),
        carried: Some(h1),
        t,
        class: native_refs::Class::Local,
        checked_out: false,
    });
    let row = match decision.row {
        native_refs::Row::Set(row) => Some((branch.to_owned(), row)),
        native_refs::Row::Keep => None,
    };
    match decision.write {
        native_refs::Write::Create(new) => attached(Some((branch.to_owned(), None, new)), row),
        native_refs::Write::Update { old, new } => {
            attached(Some((branch.to_owned(), Some(old), new)), row)
        }
        _ if current == Some(h1) => attached(None, row),
        _ => detached(
            Some(branch),
            Some(if decision.line == native_refs::Line::Stale {
                "head-detached-ref-newer"
            } else {
                "head-detached-ref-conflict"
            }),
        ),
    }
}

/// Move HEAD from `old` to `new` by compare-and-swap, the branch move with
/// it. With both, the branch commits first in a transaction of its own,
/// under every worktree's HEAD lock with this worktree's HEAD verified as
/// `old` and the branch checked out nowhere, then HEAD in a second
/// transaction (review 3 finding 13, the earlier report's finding 3: one
/// transaction a killed Git child cut short could commit HEAD, which sorts
/// first, without its branch). Between the two, or when the HEAD line
/// loses its compare-and-swap, the branch is at its new value and HEAD at
/// its old one: a state the intent names, which the settle finishes.
///
/// A HEAD line that attaches HEAD to a branch it is not on already holds
/// every other worktree's HEAD lock from before the branch's transaction
/// to after HEAD's, and refuses `GIT_CONVERGE_INTERRUPTED` when another
/// worktree has the branch checked out (or held by a rebase or bisect),
/// read under them (review 4, the blocker: an operator's `git switch` or
/// `git worktree add` of the branch since the plan, or since a crash,
/// ended with two worktrees on one branch, Git's own invariant broken).
/// This worktree's own HEAD is left to the HEAD line's compare-and-swap.
fn set_head(
    worktree: &Path,
    old: &Head,
    new: &Head,
    branch: Option<&(String, Option<String>, String)>,
) -> Result<()> {
    // A branch already at its new value is done (a settle after the
    // branch's transaction, or after another item's step): HEAD alone.
    let branch = match branch {
        Some((name, _, after)) => {
            let (native, _) = native_refs::native_refs(worktree)?;
            branch.filter(|_| native.get(name) != Some(after))
        }
        None => None,
    };
    let branch_line = branch.map(|(name, before, after)| {
        before.as_ref().map_or_else(
            || format!("create {name}\0{after}\0"),
            |before| format!("update {name}\0{after}\0{before}\0"),
        )
    });
    let head_line = match (&old.symbolic, &new.symbolic) {
        (Some(a), Some(b)) if a == b => None,
        (_, Some(b)) => {
            let check = old
                .symbolic
                .as_ref()
                .map_or_else(|| format!("oid\0{}\0", old.oid), |a| format!("ref\0{a}\0"));
            Some(format!("symref-update HEAD\0{b}\0{check}"))
        }
        (_, None) => (old.symbolic.is_some() || old.oid != new.oid)
            .then(|| format!("update HEAD\0{}\0{}\0", new.oid, old.oid)),
    };
    let (git_dir, _) = native_refs::git_dirs(worktree)?;
    // The branch HEAD is attached to, when it is not on it already.
    let attach = new
        .symbolic
        .as_ref()
        .filter(|b| head_line.is_some() && old.symbolic.as_ref() != Some(*b));
    let others = match attach {
        Some(_) => Some(native_refs::HeadLocks::take_except(
            worktree,
            Some(&git_dir),
        )?),
        None => None,
    };
    let checked_elsewhere = |name: &str| -> Result<bool> {
        Ok(checked_out_by(worktree)?
            .iter()
            .any(|(path, checked)| checked == name && !same_path(path, worktree)))
    };
    let mut branch_first = false;
    if let (Some((name, _, _)), Some(line)) = (branch, &branch_line) {
        if head_line.is_some() {
            branch_first = true;
            let own = native_refs::HeadLocks::take_dirs([git_dir])?;
            if head_of(worktree)? != *old
                || checked_out_by(worktree)?
                    .iter()
                    .any(|(_, checked)| checked == name)
            {
                return Err(interrupted());
            }
            native_refs::update_refs(worktree, None, line.as_bytes(), &[name.as_str()])?;
            drop(own);
            fault_point!(EstateConvergeHeadBranch);
        } else {
            // HEAD stays on this branch: Git locks HEAD itself for its log.
            native_refs::update_refs(worktree, None, line.as_bytes(), &[name.as_str()])?;
        }
    }
    let Some(line) = head_line else {
        return Ok(());
    };
    if let Some(target) = attach {
        if checked_elsewhere(target)? {
            return Err(interrupted());
        }
    }
    let result = match native_refs::update_refs(worktree, None, line.as_bytes(), &["HEAD"]) {
        // After the branch's transaction, HEAD already at its new value (an
        // operator's `git switch` to the very branch) is done.
        Err(_) if branch_first && head_of(worktree)? == *new => Ok(()),
        other => other,
    };
    drop(others);
    result
}

/// Remove the HEAD and branch locks a killed `set_head` of `intent` left in
/// the worktree whose git directory is `git_dir` (their content is the
/// intent's new HEAD or branch value, so they are bulkload's; `common` is
/// the repository's common directory). Returns how many.
fn remove_intent_head_locks(
    git_dir: &Path,
    common: &Path,
    intent: &WorkspaceIntent,
) -> Result<u64> {
    let new = &intent.head.1;
    let head = new.symbolic.as_ref().map_or_else(
        || format!("{}\n", new.oid),
        |branch| format!("ref: {branch}\n"),
    );
    let mut removed = u64::from(native_refs::remove_owned_lock(
        &git_dir.join("HEAD.lock"),
        &[head.into_bytes()],
    )?);
    if let Some((name, _, after)) = &intent.branch {
        removed += u64::from(native_refs::remove_owned_lock(
            &common.join(format!("{name}.lock")),
            &[format!("{after}\n").into_bytes()],
        )?);
    }
    Ok(removed)
}

/// The crash leftovers of every pending converge of `repo` (for `source`),
/// under the destination lock, before anything else reads or writes a ref
/// (review 4: a `HEAD.lock` a killed `set_head` left made every later ref
/// transaction of the repository refuse, so a ref intent another item left
/// could never settle, and the item whose intent named the lock never
/// reached its own settle): each worktree's lock temporaries
/// ([`native_refs::sweep_lock_temps`]), the HEAD locks bulkload marks as
/// its own, and the HEAD and branch locks a killed `set_head` of each
/// pending workspace intent left. Then the pending ref intent is settled
/// ([`native_refs::settle`]).
///
/// # Errors
/// As [`native_refs::settle`].
pub(super) fn settle_repository(repo: &Path, common: &Path, source: &str) -> Result<Custody> {
    let mut removed = 0;
    for git_dir in worktree_git_dirs(repo)? {
        native_refs::sweep_lock_temps(&git_dir)?;
        let path = landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_INTENT);
        if let Some(intent) = landed::read::<WorkspaceIntent>(&path)? {
            removed += remove_intent_head_locks(&git_dir, common, &intent)?;
        }
    }
    removed += native_refs::remove_marked_head_locks(repo)?;
    let mut custody = native_refs::settle(repo, &landed::ledger_dir(common, source))?;
    custody.add("git-lock-removed", removed);
    Ok(custody)
}

// ---- the seat delta ----------------------------------------------------------

/// One side of a seat: what the capture holds at a path. `prefix` (the old
/// side only, [`handoff::landed_sides`]) lists the hand-off blocks the seat
/// may carry above its landed bytes (TIN-6569, OI-1003-Q183); it is not
/// part of the side's identity, so the delta never plans a write for it.
#[derive(Debug, Clone)]
struct Side {
    kind: Kind,
    mode: u32,
    blob: Option<String>,
    link: Option<Vec<u8>>,
    prefix: Vec<handoff::Prefix>,
}

impl PartialEq for Side {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.mode == other.mode
            && self.blob == other.blob
            && self.link == other.link
    }
}

impl Eq for Side {}

fn sides(repo: &Path, meta: &Meta) -> Result<BTreeMap<Vec<u8>, Side>> {
    let rows = meta.rows(repo)?;
    let listed =
        output(git(repo).args(["ls-tree", "-r", "-z", meta.get(super::WORKTREE_REVISION)?]))?;
    let mut blobs = BTreeMap::new();
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
        let value = header
            .split(' ')
            .nth(2)
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        blobs.insert(
            entry.get(tab + 1..).unwrap_or_default().to_vec(),
            value.to_owned(),
        );
    }
    let mut sides = BTreeMap::new();
    for row in rows {
        let kind = match row.kind {
            FileKind::Directory => Kind::Directory,
            FileKind::Regular => Kind::Regular,
            FileKind::Symlink => Kind::Symlink,
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        let blob = blobs.get(&row.rel_path).cloned();
        if kind != Kind::Directory && blob.is_none() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        sides.insert(
            row.rel_path.clone(),
            Side {
                kind,
                mode: row.mode & 0o777,
                blob,
                link: row.link_target.clone(),
                prefix: Vec::new(),
            },
        );
    }
    Ok(sides)
}

/// What one seat operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Remove,
    RmDir,
    MkDir,
    Replace,
    Add,
    Chmod,
    DirMode,
}

#[derive(Debug, Clone)]
struct Op {
    n: usize,
    path: Vec<u8>,
    action: Action,
    old: Option<Side>,
    new: Option<Side>,
    /// The path is a directory once the delta has run (a `Remove` of a file
    /// the source turned into a directory).
    to_dir: bool,
}

// One planned seat operation before it is numbered.
type Planned = (Action, Vec<u8>, Option<Side>, Option<Side>);

/// The seat operations from `old` to `new`, in execution order: removals
/// (deepest first), directory removals (deepest first), directory creations
/// (shallowest first), replacements, additions and mode changes, then the
/// directory modes (deepest first).
fn delta(old: &BTreeMap<Vec<u8>, Side>, new: &BTreeMap<Vec<u8>, Side>) -> Vec<Op> {
    let new_sides = new;
    let mut ops: Vec<Planned> = Vec::new();
    let paths: BTreeSet<&Vec<u8>> = old.keys().chain(new.keys()).collect();
    for path in paths {
        let (before, after) = (old.get(path), new.get(path));
        let dir = |side: Option<&Side>| side.is_some_and(|side| side.kind == Kind::Directory);
        let file = |side: Option<&Side>| side.is_some_and(|side| side.kind != Kind::Directory);
        let mut push = |action, before: Option<&Side>, after: Option<&Side>| {
            ops.push((action, path.clone(), before.cloned(), after.cloned()));
        };
        match (before, after) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) if a.kind == Kind::Directory && b.kind == Kind::Directory => {
                push(Action::DirMode, before, after);
            }
            (Some(a), Some(b)) if file(Some(a)) && file(Some(b)) => {
                if a.kind == b.kind && a.blob == b.blob && a.link == b.link {
                    push(Action::Chmod, before, after);
                } else {
                    push(Action::Replace, before, after);
                }
            }
            _ => {
                if file(before) {
                    push(Action::Remove, before, None);
                }
                if dir(before) {
                    push(Action::RmDir, before, None);
                }
                if dir(after) {
                    push(Action::MkDir, None, after);
                }
                if file(after) {
                    push(Action::Add, None, after);
                }
            }
        }
    }
    let phase = |action: Action| match action {
        Action::Remove => 0,
        Action::RmDir => 1,
        Action::MkDir => 2,
        Action::Replace | Action::Add | Action::Chmod => 3,
        Action::DirMode => 4,
    };
    ops.sort_by(|a, b| {
        phase(a.0).cmp(&phase(b.0)).then_with(|| match phase(a.0) {
            0 | 1 | 4 => b.1.cmp(&a.1),
            _ => a.1.cmp(&b.1),
        })
    });
    ops.into_iter()
        .enumerate()
        .map(|(n, (action, path, old, new))| Op {
            n,
            to_dir: new_sides
                .get(&path)
                .is_some_and(|side| side.kind == Kind::Directory),
            path,
            action,
            old,
            new,
        })
        .collect()
}

// The proper ancestors of a relative path, as relative paths.
fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .filter_map(|(at, _)| path.get(..at))
}

// ---- seat writes -----------------------------------------------------------

fn cname(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| BulkloadRefusal::PathNotPortable)
}

/// One seat's location: its parent directory (opened) and leaf name.
struct Seat {
    path: PathBuf,
    parent: PathBuf,
    leaf: CString,
    temp: CString,
    temp_path: PathBuf,
}

impl Seat {
    fn at(worktree: &Path, relative: &[u8], tag: &str, n: usize) -> Result<Self> {
        let path = safe_destination(worktree, Path::new(OsStr::from_bytes(relative)))?;
        let parent = path
            .parent()
            .ok_or(BulkloadRefusal::PathEscapesRoot)?
            .to_path_buf();
        let leaf = cname(path.file_name().ok_or(BulkloadRefusal::PathEscapesRoot)?)?;
        let temp_name = format!(".bulkload-converge-{tag}-{n}");
        Ok(Self {
            temp_path: parent.join(&temp_name),
            temp: cname(OsStr::new(&temp_name))?,
            path,
            parent,
            leaf,
        })
    }

    fn directory(&self) -> Result<fs::File> {
        fs::File::open(&self.parent).refuse_at("git_carry::converge::seat")
    }
}

/// A planned destination's `lstat`: also absent (`None`) when a path above
/// it is not a directory (`ENOTDIR`), where nothing can be: its first
/// landing's parent check then types it `GIT_DESTINATION_PARENT_MISSING`
/// (M2 finding 1), never a bare IO errno.
fn planned(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => Ok(None),
        _ => lstat(path),
    }
}

fn lstat(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::refuse::io(&error, "git_carry::converge::lstat")),
    }
}

/// Write `side`'s bytes (or link) at the seat's temporary name; the file
/// is fsynced at its final mode.
fn write_temp(seat: &Seat, side: &Side, objects: &mut BatchObjects) -> Result<Option<fs::File>> {
    const SITE: &str = "git_carry::converge::write_temp";
    match side.kind {
        Kind::Symlink => {
            let link = side
                .link
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            std::os::unix::fs::symlink(OsStr::from_bytes(link), &seat.temp_path).refuse_at(SITE)?;
            Ok(None)
        }
        Kind::Regular => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&seat.temp_path)
                .refuse_at(SITE)?;
            objects.copy_into(
                side.blob
                    .as_deref()
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                &mut file,
                None,
            )?;
            file.flush().refuse_at(SITE)?;
            file.set_permissions(fs::Permissions::from_mode(side.mode))
                .refuse_at(SITE)?;
            crate::counters::CountedSync::sync_file_counted(&file).refuse_at(SITE)?;
            Ok(Some(file))
        }
        Kind::Directory => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

/// Whether the seat at `path` holds `side`'s content (kind, bytes or link
/// target; not the mode). Reads destination bytes only, counted, compared
/// with the blob as the object store holds it. A regular seat whose side
/// carries hand-off prefixes also holds it as one of those blocks followed
/// by the blob (TIN-6569: the block is bulkload's, never work), or by the
/// blob without the carried blocks a recorded `--clear --clear-carried`
/// removed from its head (OI-1003-Q190).
fn holds(objects: &mut BatchObjects, path: &Path, side: &Side) -> Result<bool> {
    let Some(meta) = lstat(path)? else {
        return Ok(false);
    };
    match side.kind {
        Kind::Directory => Ok(meta.is_dir()),
        Kind::Symlink => Ok(meta.file_type().is_symlink()
            && fs::read_link(path)
                .refuse_at("git_carry::converge::holds")?
                .as_os_str()
                .as_bytes()
                == side.link.as_deref().unwrap_or_default()),
        Kind::Regular => {
            if !meta.is_file() {
                return Ok(false);
            }
            let blob = side
                .blob
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            let open = || {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .refuse_at("git_carry::converge::holds")
            };
            let candidates = if side.prefix.is_empty() {
                Vec::new()
            } else {
                handoff::matching(&mut open()?, &side.prefix)?
            };
            for prefix in candidates {
                let mut file = open()?;
                std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(prefix.len))
                    .refuse_at("git_carry::converge::holds")?;
                if holds_blob(objects, file, blob, prefix.strip, meta.len())? {
                    return Ok(true);
                }
            }
            holds_blob(objects, open()?, blob, 0, meta.len())
        }
    }
}

// Whether `file`, from its position on, is `blob`'s bytes after its first
// `skip` and nothing more.
fn holds_blob(
    objects: &mut BatchObjects,
    file: fs::File,
    blob: &str,
    skip: u64,
    len: u64,
) -> Result<bool> {
    let mut compare = Compare {
        file: std::io::BufReader::new(file),
        equal: true,
        scratch: Vec::new(),
        skip,
    };
    let length = objects.copy_into(blob, &mut compare, None);
    let mut rest = [0u8; 1];
    let ended = std::io::Read::read(&mut compare.file, &mut rest)
        .refuse_at("git_carry::converge::holds")?
        == 0;
    crate::counters::add_len(
        crate::counters::Counter::DestVerifyRead,
        usize::try_from(len).unwrap_or(usize::MAX),
    );
    length.map(|_| compare.equal && ended && compare.skip == 0)
}

/// A writer that compares what it is given, after its first `skip` bytes,
/// with a file's next bytes.
struct Compare {
    file: std::io::BufReader<fs::File>,
    equal: bool,
    scratch: Vec<u8>,
    skip: u64,
}

impl std::io::Write for Compare {
    fn write(&mut self, given: &[u8]) -> std::io::Result<usize> {
        let skipped = usize::try_from(self.skip)
            .unwrap_or(usize::MAX)
            .min(given.len());
        self.skip = self
            .skip
            .saturating_sub(u64::try_from(skipped).unwrap_or(u64::MAX));
        let bytes = given.get(skipped..).unwrap_or_default();
        if self.equal && !bytes.is_empty() {
            self.scratch.resize(bytes.len(), 0);
            let mut filled = 0;
            while filled < bytes.len() {
                let read = std::io::Read::read(
                    &mut self.file,
                    self.scratch.get_mut(filled..).unwrap_or_default(),
                )?;
                if read == 0 {
                    break;
                }
                filled += read;
            }
            self.equal = filled == bytes.len() && self.scratch.get(..filled) == Some(bytes);
        }
        Ok(given.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---- the proof ---------------------------------------------------------------

/// The print every landed seat has now: its ledger print, or, for a seat
/// whose print moved but whose content is the landed content, its new
/// print (adopted by content, design 4.2 and Q2).
struct Proof {
    prints: BTreeMap<Vec<u8>, SeatPrint>,
}

/// Prove the landed checkout at `worktree` unchanged since `ledger` (design
/// 4.2, amended). Read-only; `GIT_DESTINATION_MODIFIED` on any failure.
fn prove(
    worktree: &Path,
    common: &Path,
    ledger: &LandedWorkspace,
    standalone: bool,
    old: &BTreeMap<Vec<u8>, Side>,
) -> Result<Proof> {
    let modified = BulkloadRefusal::GitDestinationModified;
    if head_of(worktree)? != ledger.head || index_digest(worktree, None)? != ledger.index {
        return Err(modified);
    }
    // The common exclude is a linked item's main's (finding 12).
    if standalone && exclude_digest(&read_exclude(common)?) != ledger.exclude {
        return Err(modified);
    }
    let same_fs = landed::fsid(worktree)? == ledger.fsid;
    let racy_from = ledger
        .recorded_ns
        .saturating_sub(super::RACY_GRANULARITY_NS);
    let mut prints = BTreeMap::new();
    let mut objects = BatchObjects::new(worktree)?;
    for seat in &ledger.seats {
        let path = safe_destination(worktree, Path::new(OsStr::from_bytes(&seat.path)))
            .map_err(|_| modified.clone())?;
        let meta = lstat(&path)
            .map_err(|_| modified.clone())?
            .ok_or_else(|| modified.clone())?;
        let now = SeatPrint::of(&seat.path, &meta).ok_or_else(|| modified.clone())?;
        if now.kind != seat.kind || now.mode != seat.mode {
            return Err(modified);
        }
        if seat.kind == Kind::Directory {
            prints.insert(seat.path.clone(), now);
            continue;
        }
        if same_fs && now.same(seat) && seat.mtime_ns < racy_from {
            prints.insert(seat.path.clone(), seat.clone());
            continue;
        }
        let side = old.get(&seat.path).ok_or_else(|| modified.clone())?;
        // A hand-off record's print of the seat (its block on the landed
        // bytes), unchanged and not racy, is trusted as the ledger's is
        // (TIN-6569).
        if side
            .prefix
            .iter()
            .any(|prefix| prefix.trusted.as_ref().is_some_and(|print| now.same(print)))
        {
            prints.insert(seat.path.clone(), now);
            continue;
        }
        // The content fallback: the landed bytes, under a print that held
        // still while they were read, are adopted under that print.
        if !holds(&mut objects, &path, side)? {
            return Err(modified);
        }
        let settled = lstat(&path)?.and_then(|meta| SeatPrint::of(&seat.path, &meta));
        if settled.as_ref() != Some(&now) {
            return Err(modified);
        }
        prints.insert(seat.path.clone(), now);
    }
    objects.finish()?;
    Ok(Proof { prints })
}

/// Refuse a capture whose shape converge does not carry (A8): the shallow
/// frontier, nested custody, the omitted rebuildable roots, bare.
fn shape(old: &Meta, new: &Meta) -> Result<()> {
    for suffix in [
        "shallow-frontier-v1",
        "nested-repositories-v1",
        "nested-worktrees-v1",
        "rebuildable-omissions-v1",
        "bare-repository-v1",
    ] {
        if old.optional(suffix) != new.optional(suffix) {
            return Err(BulkloadRefusal::GitConvergeShapeChanged);
        }
    }
    Ok(())
}

// ---- the seat engine ---------------------------------------------------------

/// Runs seat operations, fresh (each seat checked against the print the
/// proof found) or settling (each seat classified by its content).
struct Engine<'a> {
    worktree: &'a Path,
    tag: &'a str,
    /// Fresh: the proof's prints. Settling: `None`.
    proof: Option<&'a BTreeMap<Vec<u8>, SeatPrint>>,
    objects: BatchObjects,
    /// Directories made writable or created: their mode before.
    touched: BTreeMap<PathBuf, u32>,
    written: BTreeMap<Vec<u8>, SeatPrint>,
    custody: Custody,
}

const fn interrupted() -> BulkloadRefusal {
    BulkloadRefusal::GitConvergeInterrupted
}

impl Engine<'_> {
    fn writable(&mut self, directory: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(directory).refuse_at("git_carry::converge::writable")?;
        if !meta.is_dir() {
            return Err(interrupted());
        }
        self.touched
            .entry(directory.to_path_buf())
            .or_insert_with(|| meta.mode() & 0o7777);
        if meta.mode() & 0o700 != 0o700 {
            fs::set_permissions(
                directory,
                fs::Permissions::from_mode((meta.mode() & 0o7777) | 0o700),
            )
            .refuse_at("git_carry::converge::writable")?;
        }
        Ok(())
    }

    fn record(&mut self, seat: &Seat, relative: &[u8]) -> Result<()> {
        let meta = lstat(&seat.path)?.ok_or_else(interrupted)?;
        self.written.insert(
            relative.to_vec(),
            SeatPrint::of(relative, &meta).ok_or_else(interrupted)?,
        );
        Ok(())
    }

    /// The print of a regular seat this converge just renamed into place,
    /// from the descriptor it wrote (review of #289, finding 6: a later
    /// `lstat` of the path took a write in between, or a file renamed over
    /// it, as bulkload's own output). `before` is that descriptor's
    /// metadata before the rename: the rename moves only its ctime, so a
    /// size or mtime that moved since is a write into it, and the print
    /// recorded is the one before (it matches nothing the seat holds now,
    /// so the next converge proves the seat modified). The path holding
    /// another inode is likewise never recorded as this converge's.
    fn record_written(
        &mut self,
        relative: &[u8],
        file: &fs::File,
        before: &fs::Metadata,
    ) -> Result<()> {
        let now = file
            .metadata()
            .refuse_at("git_carry::converge::record_written")?;
        let kept = |meta: &fs::Metadata| {
            let (a, b) = (
                crate::freshness::StatIdentity::from_metadata(meta),
                crate::freshness::StatIdentity::from_metadata(before),
            );
            a.ino == b.ino && a.size == b.size && a.mtime_ns == b.mtime_ns
        };
        let chosen = if kept(&now) { &now } else { before };
        self.written.insert(
            relative.to_vec(),
            SeatPrint::of(relative, chosen).ok_or_else(interrupted)?,
        );
        Ok(())
    }

    /// The print the seat at `seat` must have before it is replaced or
    /// removed: the proof's (fresh), or its print now when its content is
    /// the old side's (settling). `Ok(None)`: not the landed seat.
    fn expected(&mut self, seat: &Seat, op: &Op) -> Result<Option<SeatPrint>> {
        let Some(meta) = lstat(&seat.path)? else {
            return Ok(None);
        };
        let now = SeatPrint::of(&op.path, &meta);
        Ok(match self.proof {
            Some(prints) => prints
                .get(&op.path)
                .filter(|print| now.as_ref() == Some(*print))
                .cloned(),
            None => match &op.old {
                Some(old) if holds(&mut self.objects, &seat.path, old)? => now,
                _ => None,
            },
        })
    }

    /// Settling: a temporary of this converge left at `seat`'s temporary
    /// name. Ours (it holds the old or the new side) is removed; anything
    /// else is a file the crash displaced, and goes back to the seat when
    /// the seat holds this converge's file.
    fn sweep(&mut self, seat: &Seat, op: &Op) -> Result<()> {
        if self.proof.is_some() || lstat(&seat.temp_path)?.is_none() {
            return Ok(());
        }
        let mut ours = false;
        for side in [&op.new, &op.old].into_iter().flatten() {
            ours = ours || holds(&mut self.objects, &seat.temp_path, side)?;
        }
        if ours {
            fs::remove_file(&seat.temp_path).refuse_at("git_carry::converge::sweep")?;
            return landed::sync_dir(&seat.parent);
        }
        let directory = seat.directory()?;
        match &op.new {
            Some(new) if holds(&mut self.objects, &seat.path, new)? => {
                crate::io::exchange(&directory, &seat.temp, &seat.leaf)
                    .refuse_at("git_carry::converge::sweep")?;
                fs::remove_file(&seat.temp_path).refuse_at("git_carry::converge::sweep")?;
            }
            _ if lstat(&seat.path)?.is_none() => {
                crate::io::sys::rename_exclusive_at(&directory, &seat.temp, &directory, &seat.leaf)
                    .refuse_at("git_carry::converge::sweep")?;
            }
            _ => {}
        }
        landed::sync_dir(&seat.parent)?;
        self.custody.count("seat-restored-destination");
        Err(interrupted())
    }

    fn run(&mut self, op: &Op) -> Result<()> {
        let seat = match Seat::at(self.worktree, &op.path, self.tag, op.n) {
            Ok(seat) => seat,
            // Settling: a seat under an ancestor that is no longer a
            // directory (the converge turned it into a file) is removed.
            Err(_)
                if self.proof.is_none()
                    && matches!(op.action, Action::Remove | Action::RmDir | Action::DirMode) =>
            {
                return Ok(());
            }
            // Any other: a seat's parent the operator removed or replaced
            // after the intent. Every refusal after the intent is
            // GIT_CONVERGE_INTERRUPTED (review 3 finding 5), never a path
            // or Io code.
            Err(_) => return Err(interrupted()),
        };
        match op.action {
            Action::Remove => self.remove(&seat, op),
            Action::Replace => self.replace(&seat, op),
            Action::Add => self.add(&seat, op),
            Action::Chmod => self.chmod(&seat, op),
            Action::MkDir => {
                self.writable(&seat.parent)?;
                match fs::DirBuilder::new().create(&seat.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !lstat(&seat.path)?.is_some_and(|meta| meta.is_dir()) {
                            return Err(interrupted());
                        }
                    }
                    Err(error) => {
                        return Err(crate::refuse::io(&error, "git_carry::converge::mkdir"))
                    }
                }
                self.touched.entry(seat.path.clone()).or_insert(0o700);
                self.custody.count("seats-added");
                Ok(())
            }
            Action::RmDir => {
                self.writable(&seat.parent)?;
                match fs::remove_dir(&seat.path) {
                    Ok(()) => self.custody.count("seats-removed"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error)
                        if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) =>
                    {
                        // Entries the destination added: the directory stays.
                        self.custody.count("seat-kept-destination");
                    }
                    // Settling: the directory was already replaced by the
                    // converge's own add, which classifies it in its turn.
                    Err(error)
                        if self.proof.is_none() && error.raw_os_error() == Some(libc::ENOTDIR) => {}
                    Err(_) => return Err(interrupted()),
                }
                landed::sync_dir(&seat.parent)
            }
            Action::DirMode => {
                if let Some(meta) = lstat(&seat.path)? {
                    self.touched
                        .entry(seat.path.clone())
                        .or_insert_with(|| meta.mode() & 0o7777);
                }
                Ok(())
            }
        }
    }

    fn remove(&mut self, seat: &Seat, op: &Op) -> Result<()> {
        self.sweep(seat, op)?;
        let Some(meta) = lstat(&seat.path)? else {
            // Settling: done. Fresh: removed by someone since the proof.
            return if self.proof.is_none() {
                Ok(())
            } else {
                Err(interrupted())
            };
        };
        // Settling a file the source turned into a directory: the seat
        // already holds the new directory (its MkDir ran before the crash),
        // so the removal is done (the earlier report's finding 1; before,
        // `expected` asked a directory for the old file and refused on
        // every rerun). The MkDir and the additions under it check it.
        if self.proof.is_none() && op.to_dir && meta.is_dir() {
            return Ok(());
        }
        let expected = self.expected(seat, op)?.ok_or_else(interrupted)?;
        self.writable(&seat.parent)?;
        let directory = seat.directory()?;
        crate::io::sys::rename_exclusive_at(&directory, &seat.leaf, &directory, &seat.temp)
            .map_err(|_| interrupted())?;
        let moved = lstat(&seat.temp_path)?.and_then(|meta| SeatPrint::of(&op.path, &meta));
        if moved.is_some_and(|moved| {
            moved.ino == expected.ino
                && moved.size == expected.size
                && moved.mtime_ns == expected.mtime_ns
        }) {
            fs::remove_file(&seat.temp_path).refuse_at("git_carry::converge::remove")?;
            self.custody.count("seats-removed");
            return landed::sync_dir(&seat.parent);
        }
        let _ = crate::io::sys::rename_exclusive_at(&directory, &seat.temp, &directory, &seat.leaf);
        landed::sync_dir(&seat.parent)?;
        Err(interrupted())
    }

    fn add(&mut self, seat: &Seat, op: &Op) -> Result<()> {
        let new = op
            .new
            .as_ref()
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if self.proof.is_none() && lstat(&seat.temp_path)?.is_some() {
            if !holds(&mut self.objects, &seat.temp_path, new)? {
                return Err(interrupted());
            }
            fs::remove_file(&seat.temp_path).refuse_at("git_carry::converge::add")?;
        }
        if lstat(&seat.path)?.is_some() {
            if self.proof.is_none() && holds(&mut self.objects, &seat.path, new)? {
                return self.record(seat, &op.path);
            }
            return Err(interrupted());
        }
        self.writable(&seat.parent)?;
        let file = write_temp(seat, new, &mut self.objects)?;
        let before = file
            .as_ref()
            .map(fs::File::metadata)
            .transpose()
            .refuse_at("git_carry::converge::add")?;
        let directory = seat.directory()?;
        if let Err(error) =
            crate::io::sys::rename_exclusive_at(&directory, &seat.temp, &directory, &seat.leaf)
        {
            let _ = fs::remove_file(&seat.temp_path);
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                interrupted()
            } else {
                crate::refuse::io(&error, "git_carry::converge::add")
            });
        }
        landed::sync_dir(&seat.parent)?;
        self.custody.count("seats-added");
        match (&file, &before) {
            (Some(file), Some(before)) => self.record_written(&op.path, file, before),
            _ => self.record(seat, &op.path),
        }
    }

    fn replace(&mut self, seat: &Seat, op: &Op) -> Result<()> {
        let new = op
            .new
            .as_ref()
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        self.sweep(seat, op)?;
        if self.proof.is_none() && holds(&mut self.objects, &seat.path, new)? {
            return self.mode_and_record(seat, op, new);
        }
        // Fresh: the descriptor the seat is checked through (finding 10).
        let held = match op.old.as_ref().map(|old| old.kind) {
            Some(Kind::Regular) => OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&seat.path)
                .ok(),
            _ => None,
        };
        let expected = self.expected(seat, op)?.ok_or_else(interrupted)?;
        if let Some(file) = &held {
            let found = file.metadata().refuse_at("git_carry::converge::replace")?;
            if SeatPrint::of(&op.path, &found).as_ref() != Some(&expected) {
                return Err(interrupted());
            }
        }
        self.writable(&seat.parent)?;
        let file = write_temp(seat, new, &mut self.objects)?;
        let before = file
            .as_ref()
            .map(fs::File::metadata)
            .transpose()
            .refuse_at("git_carry::converge::replace")?;
        fault_point!(EstateConvergeSeatBeforeExchange);
        let directory = seat.directory()?;
        if let Err(error) = crate::io::exchange(&directory, &seat.temp, &seat.leaf) {
            let _ = fs::remove_file(&seat.temp_path);
            return Err(if crate::io::rename_unsupported(&error) {
                BulkloadRefusal::DestinationExchangeUnsupported
            } else {
                interrupted()
            });
        }
        fault_point!(EstateConvergeSeatAfterExchange);
        // The displaced file is the landed seat when it is the inode checked
        // above, with its size and mtime; the exchange moved its ctime.
        let displaced = lstat(&seat.temp_path)?.and_then(|meta| SeatPrint::of(&op.path, &meta));
        let unchanged = held.as_ref().is_none_or(|file| {
            file.metadata().is_ok_and(|found| {
                found.ino() == expected.ino
                    && found.size() == expected.size
                    && crate::freshness::StatIdentity::from_metadata(&found).mtime_ns
                        == expected.mtime_ns
            })
        });
        if unchanged
            && displaced.is_some_and(|found| {
                found.ino == expected.ino
                    && found.size == expected.size
                    && found.mtime_ns == expected.mtime_ns
            })
        {
            fs::remove_file(&seat.temp_path).refuse_at("git_carry::converge::replace")?;
            landed::sync_dir(&seat.parent)?;
            self.custody.count("seats-replaced");
            return match (&file, &before) {
                (Some(file), Some(before)) => self.record_written(&op.path, file, before),
                _ => self.record(seat, &op.path),
            };
        }
        // Not the landed seat: it goes back, this converge's file is removed.
        crate::io::exchange(&directory, &seat.temp, &seat.leaf)
            .refuse_at("git_carry::converge::replace")?;
        landed::sync_dir(&seat.parent)?;
        let _ = fs::remove_file(&seat.temp_path);
        self.custody.count("seat-restored-destination");
        Err(interrupted())
    }

    fn mode_and_record(&mut self, seat: &Seat, op: &Op, new: &Side) -> Result<()> {
        if new.kind == Kind::Regular {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&seat.path)
                .refuse_at("git_carry::converge::mode")?;
            if file
                .metadata()
                .refuse_at("git_carry::converge::mode")?
                .mode()
                & 0o777
                != new.mode
            {
                file.set_permissions(fs::Permissions::from_mode(new.mode))
                    .refuse_at("git_carry::converge::mode")?;
                crate::counters::CountedSync::sync_file_counted(&file)
                    .refuse_at("git_carry::converge::mode")?;
            }
        }
        self.record(seat, &op.path)
    }

    fn chmod(&mut self, seat: &Seat, op: &Op) -> Result<()> {
        let new = op
            .new
            .as_ref()
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if new.kind != Kind::Regular {
            return self.record(seat, &op.path);
        }
        if self.proof.is_none() {
            if !holds(&mut self.objects, &seat.path, new)? {
                return Err(interrupted());
            }
            self.custody.count("seats-mode");
            return self.mode_and_record(seat, op, new);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&seat.path)
            .map_err(|_| interrupted())?;
        let found = file.metadata().refuse_at("git_carry::converge::chmod")?;
        let expected = self.proof.and_then(|prints| prints.get(&op.path));
        if SeatPrint::of(&op.path, &found).as_ref() != expected {
            return Err(interrupted());
        }
        file.set_permissions(fs::Permissions::from_mode(new.mode))
            .refuse_at("git_carry::converge::chmod")?;
        crate::counters::CountedSync::sync_file_counted(&file)
            .refuse_at("git_carry::converge::chmod")?;
        self.custody.count("seats-mode");
        self.record(seat, &op.path)
    }

    /// The final mode pass: every directory touched gets its captured mode,
    /// or the mode it had, deepest first, and is sealed.
    fn modes(&mut self, new: &BTreeMap<Vec<u8>, Side>) -> Result<()> {
        let touched = std::mem::take(&mut self.touched);
        for (directory, before) in touched.into_iter().rev() {
            let relative = directory
                .strip_prefix(self.worktree)
                .ok()
                .map(|relative| relative.as_os_str().as_bytes().to_vec());
            let mode = relative
                .and_then(|relative| new.get(&relative))
                .filter(|side| side.kind == Kind::Directory)
                .map_or(before, |side| side.mode);
            if lstat(&directory)?.is_some_and(|meta| meta.is_dir()) {
                fs::set_permissions(&directory, fs::Permissions::from_mode(mode))
                    .refuse_at("git_carry::converge::modes")?;
                landed::sync_dir(&directory)?;
            }
        }
        Ok(())
    }
}

/// Refuse `DESTINATION_EXCHANGE_UNSUPPORTED` up front, before any write a
/// replacement needs (OI-1003-Q100), by exchanging two probe files, each
/// created new and never through a link.
fn probe_exchange(worktree: &Path, tag: &str) -> Result<()> {
    let a = format!(".bulkload-converge-{tag}-probe-a");
    let b = format!(".bulkload-converge-{tag}-probe-b");
    for name in [&a, &b] {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(worktree.join(name))
            .refuse_at("git_carry::converge::probe")?;
    }
    let directory = fs::File::open(worktree).refuse_at("git_carry::converge::probe")?;
    let exchanged =
        crate::io::exchange(&directory, &cname(OsStr::new(&a))?, &cname(OsStr::new(&b))?);
    for name in [&a, &b] {
        let _ = fs::remove_file(worktree.join(name));
    }
    match exchanged {
        Ok(()) => Ok(()),
        Err(error) if crate::io::rename_unsupported(&error) => {
            Err(BulkloadRefusal::DestinationExchangeUnsupported)
        }
        Err(error) => Err(crate::refuse::io(&error, "git_carry::converge::probe")),
    }
}

/// Whether `name` is a probe file of [`probe_exchange`]'s, any pass's.
fn is_probe(name: &[u8]) -> bool {
    name.starts_with(b".bulkload-converge-")
        && (name.ends_with(b"-probe-a") || name.ends_with(b"-probe-b"))
}

/// Remove the probe files a crashed pass left in the worktree root (review
/// 3 finding 9): under the destination lock no other pass is probing.
/// Returns how many.
fn sweep_probes(worktree: &Path) -> Result<u64> {
    const SITE: &str = "git_carry::converge::sweep_probes";
    let mut removed = 0;
    for entry in fs::read_dir(worktree).refuse_at(SITE)? {
        let entry = entry.refuse_at(SITE)?;
        if is_probe(entry.file_name().as_bytes()) && !entry.file_type().refuse_at(SITE)?.is_dir() {
            fs::remove_file(entry.path()).refuse_at(SITE)?;
            removed += 1;
        }
    }
    if removed > 0 {
        landed::sync_dir(worktree)?;
    }
    Ok(removed)
}

// ---- the index ---------------------------------------------------------------

/// Publish the index of `new` under Git's own `index.lock`, by
/// compare-and-swap on the entries (finding 3): the lock is taken, the
/// entries the converge replaces (`old`) are checked under it, and only
/// then is the new index renamed over. A two-way `read-tree -m` keeps the
/// stat cache of every unchanged entry; a fresh one is the fallback.
/// Returns the entries digest of the index it installed (or found already
/// installed), which the ledger records (review 3 finding 3: never a
/// re-read, which would take an operator's `git add` since for landed).
/// `before` names the tree the replaced index holds (a converge's old
/// capture's staged tree; an advance's live HEAD tree), for the two-way
/// `read-tree -m`. `held` is the marker of an `index.lock` the caller
/// already holds (an advance holds its own from the proof under the lock
/// on, review of #289 finding 1): the index is published through it, and a
/// refusal leaves it held for the caller to put back or keep.
fn publish_index(
    worktree: &Path,
    git_dir: &Path,
    (before, after): (&str, &Meta),
    old: [u8; 32],
    (tag, held): (&str, Option<&[u8]>),
) -> Result<[u8; 32]> {
    const SITE: &str = "git_carry::converge::publish_index";
    let lock = git_dir.join("index.lock");
    let index = git_dir.join("index");
    let fresh = git_dir.join(format!("bulkload-index-{tag}-fresh"));
    let merged = git_dir.join(format!("bulkload-index-{tag}-merged"));
    let marker = format!("bulkload-converge {tag}\n");
    // The scratch indexes and the locks Git takes on them: a `read-tree`
    // killed while holding one left it, and the settle (same tag) failed
    // on it for good (review 4). Private names, under the destination lock.
    let cleanup = || {
        for path in [&fresh, &merged] {
            let _ = fs::remove_file(path);
            let mut lock = path.clone().into_os_string();
            lock.push(".lock");
            let _ = fs::remove_file(lock);
        }
    };
    cleanup();
    let staged =
        |meta: &Meta| -> Result<String> { Ok(format!("{}^{{tree}}", meta.get("staged")?)) };
    super::read_tree(
        git(worktree).env("GIT_INDEX_FILE", &fresh),
        &[&staged(after)?],
    )?;
    super::restore_intent_to_add(worktree, Some(&fresh), &after.heads())?;
    let target = index_digest(worktree, Some(&fresh))?;
    // The caller's own lock, or one this converge left (a crash while
    // holding it, finding 5).
    let mut holding = false;
    if lstat(&lock)?.is_some() {
        let content = fs::read(&lock).ok();
        if held.is_some_and(|held| content.as_deref() == Some(held)) {
            holding = true;
        } else {
            let ours = content.is_some_and(|bytes| bytes == marker.as_bytes())
                || index_digest(worktree, Some(&lock)).is_ok_and(|found| found == target);
            if !ours {
                cleanup();
                return Err(interrupted());
            }
            fs::remove_file(&lock).refuse_at(SITE)?;
        }
    }
    let current = index_digest(worktree, None)?;
    if current == target {
        if let (true, Some(held)) = (holding, held) {
            native_refs::remove_owned_lock(&lock, &[held.to_vec()])?;
        }
        cleanup();
        return Ok(target);
    }
    if current != old {
        cleanup();
        return Err(interrupted());
    }
    if !holding {
        // The marker exists before the lock's name (review 4: a lock
        // created empty and left so by a power loss proved nothing, and
        // wedged every rerun and the operator's Git).
        if !native_refs::publish_lock(&lock, marker.as_bytes())? {
            cleanup();
            return Err(interrupted());
        }
        fault_point!(EstateConvergeIndexLocked);
        if index_digest(worktree, None)? != old {
            let _ = fs::remove_file(&lock);
            cleanup();
            return Err(interrupted());
        }
    }
    fs::copy(&index, &merged).refuse_at(SITE)?;
    let two_way = super::read_tree(
        git(worktree).env("GIT_INDEX_FILE", &merged),
        &[before, &staged(after)?],
    )
    .and_then(|()| super::restore_intent_to_add(worktree, Some(&merged), &after.heads()))
    .and_then(|()| index_digest(worktree, Some(&merged)));
    let chosen = if two_way.is_ok_and(|found| found == target) {
        &merged
    } else {
        &fresh
    };
    crate::counters::CountedSync::sync_file_counted(&fs::File::open(chosen).refuse_at(SITE)?)
        .refuse_at(SITE)?;
    fs::rename(chosen, &lock).refuse_at(SITE)?;
    fs::rename(&lock, &index).refuse_at(SITE)?;
    landed::sync_dir(git_dir)?;
    cleanup();
    Ok(target)
}

// The tree a capture's index holds, as `read-tree` names it.
fn staged_tree(meta: &Meta) -> Result<String> {
    Ok(format!("{}^{{tree}}", meta.get("staged")?))
}

/// The entries digest of the index a landing of `meta` installs in the
/// worktree at `worktree` (git directory `git_dir`): built in a scratch
/// index file from the capture, never read back from the worktree's own,
/// which an operator may already have changed (review 3 finding 3, the
/// linked first landing).
///
/// The scratch file has one name per worktree, removed with Git's lock on
/// it before and after (review 4: a per-process name left by a killed
/// `read-tree` was never removed); callers hold the destination lock.
pub(super) fn installed_index(worktree: &Path, git_dir: &Path, meta: &Meta) -> Result<[u8; 32]> {
    let scratch = git_dir.join("bulkload-index-landing");
    let scratch_lock = git_dir.join("bulkload-index-landing.lock");
    let cleanup = || {
        let _ = fs::remove_file(&scratch);
        let _ = fs::remove_file(&scratch_lock);
    };
    cleanup();
    let digest = super::read_tree(
        git(worktree).env("GIT_INDEX_FILE", &scratch),
        &[&format!("{}^{{tree}}", meta.get("staged")?)],
    )
    .and_then(|()| super::restore_intent_to_add(worktree, Some(&scratch), &meta.heads()))
    .and_then(|()| index_digest(worktree, Some(&scratch)));
    cleanup();
    digest
}

/// The entries digest of an index with no entries: an absent index, or the
/// one `git worktree add --no-checkout` leaves.
pub(super) fn empty_index_digest() -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-index-entries-v1\0");
    *hasher.finalize().as_bytes()
}

/// Every refusal after a workspace intent is written is
/// `GIT_CONVERGE_INTERRUPTED` (section 8, finding 6): a Git child that
/// fails there (a lock a killed child left, a scratch index Git cannot
/// lock) is a converge interrupted, never an untyped `GIT_CHILD_FAILED`
/// (review 4).
fn after_intent(refusal: BulkloadRefusal) -> BulkloadRefusal {
    match refusal {
        BulkloadRefusal::GitChildFailed(_) => interrupted(),
        other => other,
    }
}

// ---- the drivers ---------------------------------------------------------------

fn tag(landing: &Landing<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&landing.capture);
    hasher.update(&super::pass_start_ns().to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    hasher
        .finalize()
        .to_hex()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

/// The new workspace ledger after a converge or a settle. `installed` is
/// the index digest `publish_index` installed and the exclude digest the
/// converge wrote or proved (review 3 finding 3: neither is read back).
#[allow(clippy::too_many_arguments)]
fn next_ledger(
    worktree: &Path,
    before: &LandedWorkspace,
    to: ([u8; 32], i128, &str),
    head: (Head, Option<String>),
    installed: ([u8; 32], [u8; 32]),
    new: &BTreeMap<Vec<u8>, Side>,
    kept: &BTreeMap<Vec<u8>, SeatPrint>,
    written: &BTreeMap<Vec<u8>, SeatPrint>,
) -> Result<LandedWorkspace> {
    let mut seats = Vec::with_capacity(new.len());
    for (path, side) in new {
        let print = if let Some(print) = written.get(path) {
            print.clone()
        } else if let (Some(print), false) = (kept.get(path), side.kind == Kind::Directory) {
            print.clone()
        } else {
            let at = safe_destination(worktree, Path::new(OsStr::from_bytes(path)))?;
            let meta = lstat(&at)?.ok_or_else(interrupted)?;
            SeatPrint::of(path, &meta).ok_or_else(interrupted)?
        };
        seats.push(print);
    }
    Ok(LandedWorkspace {
        item: before.item.clone(),
        capture: to.0,
        observed_ns: to.1,
        instance: to.2.to_owned(),
        head: head.0,
        intended_branch: head.1,
        index: installed.0,
        exclude: installed.1,
        recorded_ns: super::pass_start_ns(),
        fsid: landed::fsid(worktree)?,
        seats,
    })
}

/// Where one checkout's records live.
struct Place<'a> {
    worktree: &'a Path,
    base: Option<&'a super::StagedBundle>,
    git_dir: PathBuf,
    common: PathBuf,
    source: &'a str,
    standalone: bool,
    /// A linked beside checkout (M2 round 2 item c; its converge, gap C12,
    /// A-115): a worktree of the planned (live) repository, so the native
    /// ref step never runs and HEAD stays detached at the captured head. A
    /// standalone beside checkout is its own repository and converges as
    /// any standalone landing does.
    beside: bool,
    /// `--advance-ancestor` (operator ruling 2026-10-19): the record of
    /// the advance that made this live checkout bulkload's, when one did
    /// ([`advance::Advanced`]). Its converges never touch what the advance
    /// left to the live checkout, never replace its `info/exclude`, and
    /// move its branch as the advance did, by compare-and-swap from the
    /// value bulkload left it at.
    advanced: Option<advance::Advanced>,
}

impl Place<'_> {
    /// The seat operations a converge of this checkout runs: for an
    /// advanced one, none at a path the advance left to the live checkout
    /// (an ignored entry, its subtree, or the directory itself).
    fn untouched(&self, ops: Vec<Op>) -> Vec<Op> {
        match &self.advanced {
            Some(advanced) => ops
                .into_iter()
                .filter(|op| !advanced.leaves(&op.path))
                .collect(),
            None => ops,
        }
    }

    /// The sides whose directories a converge seals at their captured
    /// mode: an advanced checkout's kept directories keep their own.
    fn mode_sides(&self, new: &BTreeMap<Vec<u8>, Side>) -> BTreeMap<Vec<u8>, Side> {
        new.iter()
            .filter(|(path, _)| self.advanced.as_ref().is_none_or(|a| !a.leaves(path)))
            .map(|(path, side)| (path.clone(), side.clone()))
            .collect()
    }

    /// The sides a converge's workspace ledger records: an advanced
    /// checkout's kept entries are the live checkout's, never bulkload's.
    fn ledger_sides(&self, new: &BTreeMap<Vec<u8>, Side>) -> BTreeMap<Vec<u8>, Side> {
        new.iter()
            .filter(|(path, _)| self.advanced.as_ref().is_none_or(|a| !a.keeps(path)))
            .map(|(path, side)| (path.clone(), side.clone()))
            .collect()
    }

    fn workspace_dir(&self) -> PathBuf {
        landed::ledger_dir(&self.git_dir, self.source)
    }

    fn ref_dir(&self) -> PathBuf {
        landed::ledger_dir(&self.common, self.source)
    }

    /// The ref ledger, when bulkload created this repository.
    fn ref_ledger(&self) -> Result<Option<RefLedger>> {
        Ok(
            landed::read::<RefLedger>(&self.ref_dir().join(landed::REFS_LANDED))?
                .filter(|ledger| ledger.created),
        )
    }

    /// The native ref step for `meta`'s instance, every checked-out branch
    /// left to its worktree's item, and so is `head`, the branch this
    /// checkout's HEAD moves to: only [`set_head`] moves it, with HEAD
    /// (findings 1 and 2; review 2 finding 1: a step that created or moved
    /// it first made `set_head`'s compare-and-swap fail on every rerun).
    /// So is every branch a pending workspace intent names
    /// ([`intent_branches`], the earlier report's finding 2). `only` plans
    /// that one ref, the branch HEAD left, after `set_head` (the earlier
    /// report's finding 5).
    fn refs(
        &self,
        meta: &Meta,
        t: i128,
        head: Option<&str>,
        only: Option<&str>,
        custody: &mut Custody,
    ) -> Result<()> {
        // A linked beside checkout's repository is the operator's live one:
        // its native refs are never stepped, whatever its marker says.
        if self.beside {
            return Ok(());
        }
        let Some(ledger) = self.ref_ledger()? else {
            return Ok(());
        };
        let mut skip: BTreeSet<String> = checked_out_by(self.worktree)?
            .into_iter()
            .map(|(_, branch)| branch)
            .collect();
        skip.extend(head.map(str::to_owned));
        skip.extend(intent_branches(self.worktree, self.source)?);
        let (_, more) = native_refs::step(
            self.worktree,
            &self.ref_dir(),
            &ledger,
            self.source,
            &meta.instance,
            t,
            &skip,
            only,
            true,
        )?;
        custody.merge(more);
        Ok(())
    }

    /// The step after `set_head` for the branch HEAD left (`old`, when HEAD
    /// moved off it): no longer checked out, it is brought to the capture
    /// now, not left behind while the source holds still (the earlier
    /// report's finding 5, review 2 finding 1's stated limit).
    fn left_branch(
        &self,
        meta: &Meta,
        t: i128,
        old: &Head,
        new: &Head,
        custody: &mut Custody,
    ) -> Result<()> {
        match &old.symbolic {
            Some(left) if new.symbolic.as_ref() != Some(left) => {
                match self.refs(meta, t, new.symbolic.as_deref(), Some(left), custody) {
                    // The branch checked out again, or a concurrent writer:
                    // the ref intent stays for the next apply's settle, and
                    // the item goes on to its ledger. Refusing here would
                    // leave the workspace intent pending with HEAD already
                    // moved, and its settle would move HEAD off a branch the
                    // operator switched back to.
                    Err(BulkloadRefusal::GitConvergeInterrupted) => {
                        custody.count("ref-left-branch-deferred");
                        Ok(())
                    }
                    other => other,
                }
            }
            _ => Ok(()),
        }
    }

    /// Write the HEAD branch's ref ledger row.
    fn branch_row(&self, row: Option<&(String, LandedRef)>) -> Result<()> {
        let Some((name, row)) = row else {
            return Ok(());
        };
        let Some(mut ledger) = self.ref_ledger()? else {
            return Ok(());
        };
        ledger.refs.insert(name.clone(), row.clone());
        landed::write(&self.ref_dir().join(landed::REFS_LANDED), &ledger)
    }

    /// The standalone item's `info/exclude`, replaced by `bytes` when it
    /// still holds `old` (settling: or already `bytes`).
    fn exclude(&self, change: Option<([u8; 32], [u8; 32])>, bytes: &[u8]) -> Result<()> {
        let Some((old, new)) = change else {
            return Ok(());
        };
        let path = self.common.join("info/exclude");
        let current = exclude_digest(&read_exclude(&self.common)?);
        if current == new {
            return Ok(());
        }
        if current != old {
            return Err(interrupted());
        }
        fs::create_dir_all(self.common.join("info")).refuse_at("git_carry::converge::exclude")?;
        landed::write_bytes_mode(&path, bytes, 0o644)
    }
}

/// Apply one workspace item (estate-apply rows A3 to A12, as amended):
/// a first landing when the workspace is absent, otherwise settle, prove
/// and converge.
///
/// # Errors
/// `GIT_DESTINATION_MODIFIED` (proof failed, or an earlier landing of the
/// item was removed), `GIT_DESTINATION_OCCUPIED` (not bulkload's, or
/// another item's), `GIT_CONVERGE_STALE_CAPTURE`, `GIT_CONVERGE_SHAPE_CHANGED`,
/// `GIT_CONVERGE_INTERRUPTED`, or whatever the restore or import refuses.
#[allow(clippy::too_many_lines)]
pub fn apply_workspace(
    staged: &super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    landed_before: bool,
    policy: &Policy<'_>,
) -> Result<Applied> {
    super::destination_git_ready()?;
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let standalone = workspace == target.repository;
    if planned(workspace)?.is_none() {
        // Finding 13: a checkout this item landed before and the operator
        // removed is not laid down again.
        if landed_before {
            return Err(BulkloadRefusal::GitDestinationModified);
        }
        return if standalone {
            super::restore_staged_landed(
                staged,
                workspace,
                target.source,
                (None, target.base),
                landing,
                policy.origins,
            )
        } else {
            super::restore_linked_staged_landed(
                staged,
                (target.repository, target.base),
                workspace,
                target.source,
                landing,
            )
        };
    }
    let git_dir =
        landed::worktree_git_dir(workspace)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    let advanced = advance::read_advanced(&landed::ledger_dir(&git_dir, target.source))?;
    let mut place = Place {
        worktree: workspace,
        // A standalone item's plan base, or a linked item's that the
        // apply deferred (dogfood F2): imported before the capture, after
        // every proof.
        base: target.base,
        common: landed::common_dir(&git_dir)?,
        git_dir,
        source: target.source,
        standalone,
        beside: landing.beside && !standalone,
        advanced,
    };
    let ledger_path = place.workspace_dir().join(landed::WORKSPACE_LANDED);
    // A12: no ledger, not bulkload's (checked before any write), unless a
    // landing intent names the worktree: a first linked landing a crash cut
    // short (#216's residual), finished forward under the lock.
    let destination = super::linked_destination(workspace)?;
    let landing_intent = landed::landing_intent_path(&place.ref_dir(), &destination);
    let mut recovering = !landed::exists(&ledger_path)?;
    // `--advance-ancestor` (operator ruling 2026-10-19): an advance a crash
    // cut short (before its ledger, or between its ledger and the intent's
    // removal) is settled forward first, under the lock, before anything
    // reads this checkout as one bulkload did or did not land. One that
    // wrote nothing but its intent is dropped, and the checkout is proved
    // afresh.
    let mut prior = Custody::default();
    let mut advanced_now = false;
    if advance::pending(&place)? {
        let lock = landed::lock(&place.common, place.source)?;
        prior = settle_repository(workspace, &place.common, place.source)?;
        prior.add("probe-removed", sweep_probes(workspace)?);
        if let Some(done) = advance::settle(&place, landing)? {
            prior.merge(done);
            advanced_now = true;
        }
        drop(lock);
        recovering = !landed::exists(&ledger_path)?;
        place.advanced = advance::read_advanced(&place.workspace_dir())?;
    }
    if recovering && (standalone || !landed::exists(&landing_intent)?) {
        // A11a (#226): with `--adopt`, an existing checkout bulkload never
        // landed becomes bulkload's when it proves equal to the capture;
        // with `--advance-ancestor`, one that differs is advanced to it
        // when it may be ([`advance`]).
        if policy.adopt {
            return adopt::workspace(staged, target, landing, &place, policy.advance);
        }
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let _lock = landed::lock(&place.common, place.source)?;
    let mut custody = settle_repository(workspace, &place.common, place.source)?;
    custody.merge(prior);
    // Review 3 finding 9: probe files a crashed pass left.
    custody.add("probe-removed", sweep_probes(workspace)?);
    if recovering {
        let intent: landed::LandingIntent =
            landed::read(&landing_intent)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
        if intent.item != landing.item || intent.destination != destination.as_os_str().as_bytes() {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        super::recover_linked_landing(&destination, &place.common, target.source, &intent)?;
        landed::remove(&landing_intent)?;
        custody.count("landing-recovered");
        // A beside landing (M2 round 2 item c) is finished forward; one of
        // this capture is done. One of an older capture converges below
        // (gap C12, A-115), detached, no native ref of the operator's
        // repository stepped (`Place::beside`).
        if landing.beside
            && landed::read::<LandedWorkspace>(&ledger_path)?
                .is_some_and(|ledger| ledger.capture == landing.capture)
        {
            return Ok(Applied::of(false, &custody));
        }
    }
    let mut ledger: LandedWorkspace =
        landed::read(&ledger_path)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    // Finding 15(b): a plan edit is not a destination edit. A11b (#226):
    // only with `--adopt`, a ledger naming another plan's item for the same
    // tree, spelled as it is, is re-keyed after its normal proof
    // ([`adopt::rekey`]; review of 2026-10-16, findings 1, 2 and 8).
    if ledger.item != landing.item {
        adopt::rekey(
            staged,
            target,
            &place,
            (&ledger_path, &mut ledger),
            landing,
            policy,
        )?;
        custody.count("rekeyed");
    }
    let converged = landed::exists(&place.workspace_dir().join(landed::WORKSPACE_INTENT))?;
    if converged {
        custody.merge(settle(&place, &ledger).map_err(after_intent)?);
        ledger = landed::read(&ledger_path)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    }
    let meta0 = Meta::read(workspace, place.source, &ledger.instance)?;
    let old = handoff::landed_sides(workspace, &place.git_dir, place.source, &meta0)?;
    // An advanced checkout's `info/exclude` is the live checkout's: never
    // bulkload's to prove.
    let own_exclude = standalone && place.advanced.is_none();
    // The advance of this capture, done now or before (a crash between
    // its ledger and the item's journal): the caller still retires the
    // beside landing it made redundant.
    let advanced = advanced_now
        || place
            .advanced
            .as_ref()
            .is_some_and(|advanced| advanced.capture == landing.capture);
    if ledger.capture == landing.capture {
        // A6 (finding 15a): the ledger is ahead of the journal; prove it.
        prove(workspace, &place.common, &ledger, own_exclude, &old)?;
        custody.count("journal-recovered");
        let mut applied = if advanced_now {
            Applied::adopted(&custody)
        } else {
            Applied::of(converged, &custody)
        };
        applied.advanced = advanced;
        return Ok(applied);
    }
    if landing.observed_ns <= ledger.observed_ns {
        return Err(BulkloadRefusal::GitConvergeStaleCapture);
    }
    let proof = prove(workspace, &place.common, &ledger, own_exclude, &old)?;
    custody.merge(converge(
        &place,
        staged,
        landing,
        &ledger,
        (&meta0, &old),
        &proof,
    )?);
    let mut applied = Applied::of(true, &custody);
    applied.advanced = advanced_now;
    Ok(applied)
}

/// Gap C12: why the checkout at `target`'s workspace, whose workspace
/// ledger `ledger` names another item, may not be taken by `landing`'s
/// item; `None` when it may.
///
/// Read-only, before any lock or write: the A11b re-key's own refusal
/// ([`adopt::rekey_refusal`]: its shape, an item of this plan, another
/// tree), and, for a linked checkout, a workspace intent pending there,
/// which that other item's converge planned and whose settle could attach
/// a branch of the live repository.
///
/// # Errors
/// Whatever reading the checkout or the capture refuses.
pub(super) fn rekey_check(
    staged: &super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    policy: &Policy<'_>,
    ledger: &LandedWorkspace,
) -> Result<Option<&'static str>> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let Some(git_dir) = landed::worktree_git_dir(workspace)? else {
        return Ok(Some("not-a-checkout"));
    };
    let standalone = workspace == target.repository;
    if !standalone
        && landed::exists(
            &landed::ledger_dir(&git_dir, target.source).join(landed::WORKSPACE_INTENT),
        )?
    {
        return Ok(Some("intent-pending"));
    }
    let common = landed::common_dir(&git_dir)?;
    adopt::rekey_refusal(
        staged,
        workspace,
        target.repository,
        (&git_dir, &common),
        standalone,
        target.base,
        target.source,
        ledger,
        landing,
        policy.items,
    )
}

#[allow(clippy::too_many_lines)]
fn converge(
    place: &Place<'_>,
    staged: &super::StagedBundle,
    landing: &Landing<'_>,
    ledger: &LandedWorkspace,
    before: (&Meta, &BTreeMap<Vec<u8>, Side>),
    proof: &Proof,
) -> Result<Custody> {
    let (meta0, old) = before;
    let worktree = place.worktree;
    if let Some(base) = place.base {
        super::import_verified(worktree, base.path(), place.source)?;
    }
    let (_, instance) = super::import_verified_instance(worktree, staged.path(), place.source)?;
    fault_point!(EstateConvergeAfterImport);
    let meta1 = Meta::read(worktree, place.source, &instance)?;
    shape(meta0, &meta1)?;
    let new = sides(worktree, &meta1)?;
    let ops = place.untouched(delta(old, &new));
    // TIN-6569: a file the hand-off created where the source now adds one
    // is bulkload's own when it is exactly its recorded block: lifted
    // before the clash check (design 6.3).
    let adds: Vec<&[u8]> = ops
        .iter()
        .filter(|op| op.action == Action::Add && !old.contains_key(&op.path))
        .map(|op| op.path.as_slice())
        .collect();
    let lifted = handoff::lift(worktree, &place.git_dir, place.source, &adds)?;
    // A seat a third party put where the source added one (after an
    // additive import; nothing else written).
    // A path the converge removes first is the landed seat's, checked by
    // its own removal; any other added path must be free.
    for op in ops.iter().filter(|op| {
        matches!(op.action, Action::Add | Action::MkDir) && !old.contains_key(&op.path)
    }) {
        let clash = match safe_destination(worktree, Path::new(OsStr::from_bytes(&op.path))) {
            Ok(at) => lstat(&at)?.is_some_and(|meta| op.action == Action::Add || !meta.is_dir()),
            // A parent the converge creates first.
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => false,
            // A parent that is not a directory: the converge's own when it
            // is a landed seat (its removal checks it), a third party's
            // otherwise.
            Err(_) => !ancestors(&op.path).any(|parent| old.contains_key(parent)),
        };
        if clash {
            return Err(BulkloadRefusal::GitDestinationModified);
        }
    }
    // Review 2 findings 2 and 4: a landed directory the source turned into
    // a file or a symlink is freed only when it holds nothing but landed
    // seats. A destination entry inside it (an editor's swap file, build
    // output) would keep it after the removals and block the add for good,
    // so it refuses here, before the intent, with nothing written.
    for op in ops.iter().filter(|op| {
        op.action == Action::Add
            && old
                .get(&op.path)
                .is_some_and(|side| side.kind == Kind::Directory)
    }) {
        let at = safe_destination(worktree, Path::new(OsStr::from_bytes(&op.path)))
            .map_err(|_| BulkloadRefusal::GitDestinationModified)?;
        if holds_unlanded(&at, &op.path, old)? {
            return Err(BulkloadRefusal::GitDestinationModified);
        }
    }
    let tag = tag(landing);
    if ops.iter().any(|op| op.action == Action::Replace) {
        probe_exchange(worktree, &tag)?;
    }
    let others: BTreeSet<String> = checked_out_by(worktree)?
        .into_iter()
        .filter(|(path, _)| !same_path(path, worktree))
        .map(|(_, branch)| branch)
        .collect();
    let (native, symrefs) = native_refs::native_refs(worktree)?;
    let symbolic = meta1.symbolic(worktree)?;
    let mut plan = head_plan(
        symbolic.as_deref().filter(|_| !place.beside),
        meta1.get("head")?,
        (&native, &symrefs),
        place.ref_ledger()?.as_ref(),
        &others,
        landing.observed_ns,
    );
    // A linked beside checkout converges as it landed: detached at the
    // captured head, its branch recorded, no branch of the operator's
    // repository created or moved (gap C12, A-115).
    if place.beside {
        plan.intended.clone_from(&symbolic);
        plan.line = Some("head-detached-beside");
    }
    // `--advance-ancestor`: an advanced checkout's branch is moved as the
    // advance moved it, by compare-and-swap from the value bulkload left
    // it at (its own unchanged output, LIVE-30), never detached for being
    // a foreign repository's.
    if let Some(moved) = advance::branch_plan(
        place,
        ledger,
        symbolic.as_deref(),
        (&native, &others),
        meta1.get("head")?,
    ) {
        plan = moved;
    }
    let exclude_bytes = meta1.exclude(worktree)?;
    // An advanced checkout's `info/exclude` is the live checkout's, never
    // replaced.
    let exclude = (place.standalone
        && place.advanced.is_none()
        && exclude_digest(&exclude_bytes) != ledger.exclude)
        .then(|| (ledger.exclude, exclude_digest(&exclude_bytes)));
    let mut intent = WorkspaceIntent {
        from: ledger.capture,
        to: landing.capture,
        to_instance: instance.clone(),
        to_observed_ns: landing.observed_ns,
        tag: tag.clone(),
        index_old: ledger.index,
        head: (ledger.head.clone(), plan.head.clone()),
        branch: plan.branch.clone(),
        intended_branch: plan.intended.clone(),
        exclude,
        head_done: false,
    };
    let intent_path = place.workspace_dir().join(landed::WORKSPACE_INTENT);
    landed::write(&intent_path, &intent)?;
    fault_point!(EstateConvergeAfterIntent);
    let after = || -> Result<Custody> {
        let mut custody = Custody::default();
        // Q6: configuration is applied once, at a first landing; a changed one
        // is reported, never converged.
        if meta0.optional("configuration-v1") != meta1.optional("configuration-v1") {
            custody.count("configuration-not-converged");
        }
        place.refs(
            &meta1,
            landing.observed_ns,
            symbolic.as_deref(),
            None,
            &mut custody,
        )?;
        fault_point!(EstateConvergeAfterRefs);
        let mut engine = Engine {
            worktree,
            tag: &tag,
            proof: Some(&proof.prints),
            objects: BatchObjects::new(worktree)?,
            touched: BTreeMap::new(),
            written: BTreeMap::new(),
            custody: Custody::default(),
        };
        for op in &ops {
            engine.run(op)?;
            fault_point!(EstateConvergeAfterSeat);
        }
        engine.modes(&place.mode_sides(&new))?;
        engine.objects.finish()?;
        place.exclude(intent.exclude, &exclude_bytes)?;
        super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
        let index = publish_index(
            worktree,
            &place.git_dir,
            (&staged_tree(meta0)?, &meta1),
            ledger.index,
            (&tag, None),
        )?;
        fault_point!(EstateConvergeAfterIndex);
        set_head(worktree, &ledger.head, &plan.head, plan.branch.as_ref())
            .map_err(|_| interrupted())?;
        head_done(&intent_path, &mut intent)?;
        place.branch_row(plan.row.as_ref())?;
        fault_point!(EstateConvergeAfterHead);
        place.left_branch(
            &meta1,
            landing.observed_ns,
            &ledger.head,
            &plan.head,
            &mut custody,
        )?;
        if let Some(line) = plan.line {
            custody.count(line);
        }
        let next = next_ledger(
            worktree,
            ledger,
            (landing.capture, landing.observed_ns, &instance),
            (plan.head, plan.intended),
            (index, intent.exclude.map_or(ledger.exclude, |(_, new)| new)),
            &place.ledger_sides(&new),
            &proof.prints,
            &engine.written,
        )?;
        landed::write(&place.workspace_dir().join(landed::WORKSPACE_LANDED), &next)?;
        landed::remove(&intent_path)?;
        fault_point!(EstateConvergeAfterLanded);
        custody.merge(engine.custody);
        custody.add("handoff-lifted", lifted);
        Ok(custody)
    };
    after().map_err(after_intent)
}

// Whether the directory `at` (the seat `relative`) holds, at any depth, an
// entry that is not a landed seat of `old`.
fn holds_unlanded(at: &Path, relative: &[u8], old: &BTreeMap<Vec<u8>, Side>) -> Result<bool> {
    const SITE: &str = "git_carry::converge::holds_unlanded";
    if !lstat(at)?.is_some_and(|meta| meta.is_dir()) {
        return Ok(false);
    }
    for entry in fs::read_dir(at).refuse_at(SITE)? {
        let entry = entry.refuse_at(SITE)?;
        let mut path = relative.to_vec();
        path.push(b'/');
        path.extend_from_slice(entry.file_name().as_bytes());
        if !old.contains_key(&path) {
            return Ok(true);
        }
        if entry.file_type().refuse_at(SITE)?.is_dir() && holds_unlanded(&entry.path(), &path, old)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Settle a pending workspace intent forward (A3): every component that is
/// old is brought to new, one that is new is done, and anything else is a
/// third party's: `GIT_CONVERGE_INTERRUPTED`, the intent kept. HEAD still
/// at its old value whose branch line can no longer apply (the branch
/// moved, deleted or checked out since, or its create blocked by a ref put
/// back) is re-planned against the destination now, the intent rewritten
/// first (the earlier report's finding 2, review 3's finding 19): replaying
/// the line would refuse on every rerun.
#[allow(clippy::too_many_lines)]
fn settle(place: &Place<'_>, ledger: &LandedWorkspace) -> Result<Custody> {
    let worktree = place.worktree;
    let intent_path = place.workspace_dir().join(landed::WORKSPACE_INTENT);
    let mut intent: WorkspaceIntent = landed::read(&intent_path)?.ok_or_else(interrupted)?;
    let meta0 = Meta::read(worktree, place.source, &ledger.instance)?;
    let meta1 = Meta::read(worktree, place.source, &intent.to_instance)?;
    let (old, new) = (
        handoff::landed_sides(worktree, &place.git_dir, place.source, &meta0)?,
        sides(worktree, &meta1)?,
    );
    let mut custody = Custody::default();
    custody.add(
        "git-lock-removed",
        remove_intent_head_locks(&place.git_dir, &place.common, &intent)?,
    );
    place.refs(
        &meta1,
        intent.to_observed_ns,
        meta1.symbolic(worktree)?.as_deref(),
        None,
        &mut custody,
    )?;
    let tag = intent.tag.clone();
    let mut engine = Engine {
        worktree,
        tag: &tag,
        proof: None,
        objects: BatchObjects::new(worktree)?,
        touched: BTreeMap::new(),
        written: BTreeMap::new(),
        custody: Custody::default(),
    };
    for op in place.untouched(delta(&old, &new)) {
        engine.run(&op)?;
    }
    engine.modes(&place.mode_sides(&new))?;
    engine.objects.finish()?;
    place.exclude(intent.exclude, &meta1.exclude(worktree)?)?;
    super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
    let index = publish_index(
        worktree,
        &place.git_dir,
        (&staged_tree(&meta0)?, &meta1),
        intent.index_old,
        (&intent.tag, None),
    )?;
    let head_old = intent.head.0.clone();
    let current = head_of(worktree)?;
    let mut row = intent.branch.as_ref().map(|(name, _, value)| {
        (
            name.clone(),
            LandedRef {
                oid: Some(value.clone()),
                observed_ns: intent.to_observed_ns,
            },
        )
    });
    if current != intent.head.1 {
        // Once HEAD was recorded at its new value, any other HEAD is the
        // operator's: refused, never moved again.
        if intent.head_done || current != head_old {
            return Err(interrupted());
        }
        let others: BTreeSet<String> = checked_out_by(worktree)?
            .into_iter()
            .filter(|(path, _)| !same_path(path, worktree))
            .map(|(_, branch)| branch)
            .collect();
        // Review 4 (the blocker): the branch HEAD goes to, checked out by
        // another worktree since the intent (an operator's `git switch` or
        // `git worktree add`), whether or not a branch line remains.
        let taken = intent
            .head
            .1
            .symbolic
            .as_ref()
            .is_some_and(|b| others.contains(b) && current.symbolic.as_ref() != Some(b));
        if taken || branch_line_fails(worktree, intent.branch.as_ref(), &others)? {
            let (native, symrefs) = native_refs::native_refs(worktree)?;
            let symbolic = meta1.symbolic(worktree)?;
            let mut plan = head_plan(
                symbolic.as_deref().filter(|_| !place.beside),
                meta1.get("head")?,
                (&native, &symrefs),
                place.ref_ledger()?.as_ref(),
                &others,
                intent.to_observed_ns,
            );
            if place.beside {
                plan.intended = symbolic;
                plan.line = Some("head-detached-beside");
            }
            if plan.head != intent.head.1 || plan.branch != intent.branch {
                // A branch line the crash left done (its own transaction
                // committed) is bulkload's write: its row stays recorded.
                let done = intent
                    .branch
                    .as_ref()
                    .filter(|(name, _, after)| native.get(name) == Some(after))
                    .map(|(name, _, value)| {
                        (
                            name.clone(),
                            LandedRef {
                                oid: Some(value.clone()),
                                observed_ns: intent.to_observed_ns,
                            },
                        )
                    });
                intent.head.1 = plan.head;
                intent.branch = plan.branch;
                intent.intended_branch = plan.intended;
                landed::write(&intent_path, &intent)?;
                row = plan.row.or(done);
                custody.count("head-replanned");
                if let Some(line) = plan.line {
                    custody.count(line);
                }
            }
        }
        set_head(worktree, &head_old, &intent.head.1, intent.branch.as_ref())
            .map_err(|_| interrupted())?;
    }
    head_done(&intent_path, &mut intent)?;
    place.branch_row(row.as_ref())?;
    place.left_branch(
        &meta1,
        intent.to_observed_ns,
        &head_old,
        &intent.head.1,
        &mut custody,
    )?;
    let kept: BTreeMap<Vec<u8>, SeatPrint> = ledger
        .seats
        .iter()
        .map(|seat| (seat.path.clone(), seat.clone()))
        .collect();
    let next = next_ledger(
        worktree,
        ledger,
        (intent.to, intent.to_observed_ns, &intent.to_instance),
        (intent.head.1.clone(), intent.intended_branch.clone()),
        (index, intent.exclude.map_or(ledger.exclude, |(_, new)| new)),
        &place.ledger_sides(&new),
        &kept,
        &engine.written,
    )?;
    landed::write(&place.workspace_dir().join(landed::WORKSPACE_LANDED), &next)?;
    landed::remove(&intent_path)?;
    custody.merge(engine.custody);
    custody.count("workspace-intent-settled");
    Ok(custody)
}

/// Record in the pending intent that HEAD is at its new value, right after
/// `set_head`: a settle then never moves
/// HEAD again, so an operator's switch back to the old HEAD while this pass
/// still runs (the branch row, the left branch's step, the ledger) is kept
/// and refused typed, not undone (found by EstateConverge.tla,
/// `MC_ec_head_concurrent_crash`).
fn head_done(intent_path: &Path, intent: &mut WorkspaceIntent) -> Result<()> {
    if intent.head_done {
        return Ok(());
    }
    intent.head_done = true;
    landed::write(intent_path, intent)
}

/// Whether the HEAD branch line of a pending intent (`name`, old, new) can
/// no longer apply: another worktree has the branch checked out (`others`;
/// checked first, review 4: a branch already at its new value is no
/// license to attach a second worktree to it), the branch is neither its
/// old nor its new value, or its create sits beside a ref that exists (a
/// symbolic one too).
fn branch_line_fails(
    worktree: &Path,
    branch: Option<&(String, Option<String>, String)>,
    others: &BTreeSet<String>,
) -> Result<bool> {
    let Some((name, before, after)) = branch else {
        return Ok(false);
    };
    if others.contains(name) {
        return Ok(true);
    }
    let (native, symrefs) = native_refs::native_refs(worktree)?;
    let now = native.get(name);
    if now == Some(after) {
        return Ok(false);
    }
    Ok(now != before.as_ref()
        || (before.is_none()
            && (symrefs.contains(name)
                || native_refs::name_conflict(
                    name,
                    native
                        .keys()
                        .chain(&symrefs)
                        .map(String::as_str)
                        .filter(|other| other != name),
                ))))
}

/// Apply one refs-only item: the import, and into a repository bulkload
/// created, the native ref step (finding 9). A foreign repository keeps
/// provenance only, as before.
///
/// # Errors
/// Whatever the import refuses, or `GIT_CONVERGE_INTERRUPTED`.
pub fn apply_refs(
    staged: &super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    policy: &Policy<'_>,
) -> Result<Applied> {
    super::destination_git_ready()?;
    let repo = target.repository;
    // A11c (#226 item 4): a bare capture into a path that does not exist
    // lands a bare repository bulkload makes itself, native refs and all.
    if staged.is_bare() && planned(repo)?.is_none() {
        return super::land_bare_staged(
            staged,
            repo,
            target.source,
            target.base,
            landing,
            policy.origins,
        );
    }
    // S4 (#183): a destination that is missing, or a directory Git finds no
    // repository in, refuses `GIT_REPOSITORY_NOT_AT_PATH` before the ledger
    // lookup asks Git for its common directory.
    super::destination_repository(repo)?;
    let common = super::common_repository(repo)?;
    let ref_dir = landed::ledger_dir(&common, target.source);
    let created = landed::read::<RefLedger>(&ref_dir.join(landed::REFS_LANDED))?
        .is_some_and(|ledger| ledger.created);
    if !created {
        let (_, instance) = super::import_verified_instance(repo, staged.path(), target.source)?;
        // A11d (#226): with `--adopt`, a repository bulkload did not create
        // takes native refs when it is the capture's (or empty).
        if policy.adopt {
            return adopt::repository(repo, &common, target.source, &instance, landing);
        }
        return Ok(Applied::default());
    }
    let _lock = landed::lock(&common, target.source)?;
    let mut custody = settle_repository(repo, &common, target.source)?;
    let ledger = landed::read::<RefLedger>(&ref_dir.join(landed::REFS_LANDED))?.unwrap_or_default();
    let (_, instance) = super::import_verified_instance(repo, staged.path(), target.source)?;
    let mut skip = native_refs::checked_out(repo)?;
    skip.extend(intent_branches(repo, target.source)?);
    let (_, more) = native_refs::step(
        repo,
        &ref_dir,
        &ledger,
        target.source,
        &instance,
        landing.observed_ns,
        &skip,
        None,
        true,
    )?;
    custody.merge(more);
    Ok(Applied::of(false, &custody))
}

/// The intents pending at an item's destination (A3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pending {
    /// A ref intent in the repository's common directory, which every item
    /// of the repository shares.
    pub refs: bool,
    /// This item's own workspace intent.
    pub workspace: bool,
}

// An item's git directory and its repository's common directory, when
// they exist.
fn locate(workspace: Option<&Path>, repository: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let git_dir = match workspace {
        Some(workspace) => landed::worktree_git_dir(workspace)?,
        None => match landed::worktree_git_dir(repository)? {
            Some(git_dir) => Some(git_dir),
            None if lstat(&repository.join("HEAD"))?.is_some() => Some(repository.to_path_buf()),
            None => None,
        },
    };
    let Some(git_dir) = git_dir.filter(|git_dir| git_dir.is_dir()) else {
        return Ok(None);
    };
    let common = landed::common_dir(&git_dir)?;
    Ok(Some((git_dir, common)))
}

/// Which converge intents a crash or an interrupted converge left at an
/// item's destination (A3): metadata only, so a no-op rerun stays a no-op
/// (S3).
///
/// # Errors
/// Refuses unreadable Git administration.
pub fn pending(workspace: Option<&Path>, repository: &Path, source: &str) -> Result<Pending> {
    let Some((git_dir, common)) = locate(workspace, repository)? else {
        return Ok(Pending::default());
    };
    let workspace_dir = landed::ledger_dir(&git_dir, source);
    Ok(Pending {
        refs: landed::exists(&landed::ledger_dir(&common, source).join(landed::REFS_INTENT))?,
        // An advance a crash cut short (`--advance-ancestor`) is pending
        // too: a rerun with or without the flag settles it.
        workspace: landed::exists(&workspace_dir.join(landed::WORKSPACE_INTENT))?
            || landed::exists(&workspace_dir.join(advance::INTENT))?,
    })
}

/// Settle a repository's pending ref intent for a finished item.
///
/// For an item whose journal for its current capture exists (review 2
/// finding 7): the shared intent is brought forward without proving or
/// restaging that item, so another item's interruption never re-proves a
/// finished one. Returns the custody words.
///
/// # Errors
/// `GIT_CONVERGE_INTERRUPTED` when the settle loses a compare-and-swap,
/// or whatever reading the administration refuses.
pub fn settle_refs(
    workspace: Option<&Path>,
    repository: &Path,
    source: &str,
) -> Result<Option<String>> {
    let Some((_, common)) = locate(workspace, repository)? else {
        return Ok(None);
    };
    super::destination_git_ready()?;
    let _lock = landed::lock(&common, source)?;
    let repo = workspace.unwrap_or(repository);
    Ok(settle_repository(repo, &common, source)?.words())
}

// ---- first landings (#215) ---------------------------------------------------

/// A first standalone landing's native refs, in its private stage `work`
/// (a repository bulkload just created, so the ref ledger is created with
/// the repository marker), and its HEAD: on the source's branch, or
/// detached with the branch recorded (finding 11). Replaces the old
/// create-only `set_ref` of the HEAD branch and every `carry/*` branch.
pub(super) fn land_standalone_refs(
    work: &Path,
    source: &str,
    instance: &str,
    landing: &Landing<'_>,
) -> Result<(HeadPlan, Meta, Custody)> {
    let t = landing.observed_ns;
    let directory = landed::ensure_dir(&work.join(".git"), source)?;
    let meta = Meta::read(work, source, instance)?;
    let base = RefLedger {
        created: true,
        exclude: Some(landed::LandedExclude {
            digest: exclude_digest(&meta.exclude(work)?),
            observed_ns: t,
        }),
        ..RefLedger::default()
    };
    let (mut ledger, mut custody) = native_refs::step(
        work,
        &directory,
        &base,
        source,
        instance,
        t,
        &BTreeSet::new(),
        None,
        false,
    )?;
    let (native, symrefs) = native_refs::native_refs(work)?;
    let plan = head_plan(
        meta.symbolic(work)?.as_deref(),
        meta.get("head")?,
        (&native, &symrefs),
        Some(&ledger),
        &BTreeSet::new(),
        t,
    );
    if let Some((name, before, after)) = &plan.branch {
        native_refs::transact_private(
            work,
            &[landed::RefOp {
                name: name.clone(),
                old: before.clone(),
                new: Some(after.clone()),
                before: None,
                after: None,
            }],
        )?;
    }
    match &plan.head.symbolic {
        Some(branch) => super::set_symbolic_head(work, branch)?,
        None => native_refs::update_refs(
            work,
            None,
            format!("update HEAD\0{}\0\0", plan.head.oid).as_bytes(),
            &["HEAD"],
        )?,
    }
    if let Some((name, row)) = &plan.row {
        ledger.refs.insert(name.clone(), row.clone());
        landed::write(&directory.join(landed::REFS_LANDED), &ledger)?;
    }
    if let Some(line) = plan.line {
        custody.count(line);
    }
    Ok((plan, meta, custody))
}

/// A first landing's own path (estate-apply A5, and `estate-diff`'s
/// report of it): the checks it makes before anything is read from the
/// capture or written anywhere. A bare capture is
/// `GIT_BARE_CAPTURE_WORKSPACE`, a path already there
/// `GIT_DESTINATION_OCCUPIED`, absent parents an estate apply may not make
/// (a symlink or a non-directory where they would start, M2 finding 1)
/// `GIT_DESTINATION_PARENT_MISSING`. Returns the path, with its parent
/// canonical or the absent parents the landing makes before its first
/// write ([`super::parents::FirstTarget::place`]).
///
/// # Errors
/// As above.
pub(super) fn first_landing_target(
    staged: &super::StagedBundle,
    destination: &Path,
) -> Result<super::parents::FirstTarget> {
    super::refuse_bare_capture(staged)?;
    if destination.symlink_metadata().is_ok() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    super::parents::FirstTarget::of(destination)
}

/// What a first linked landing decided before its first write to the
/// destination repository ([`check_linked`]).
pub(super) struct LinkedChecked {
    /// The new worktree's path: its parent canonical, or its absent
    /// parents, made by the landing after these checks (M2 finding 1).
    pub destination: super::parents::FirstTarget,
    /// The plan's repository, canonical.
    pub repository: PathBuf,
    /// The capture's ref lines (`shallow::headers`'s form).
    pub heads: String,
    /// The capture's provenance instance (the name the import gives it).
    pub instance: String,
    /// The repository's common `info/exclude`.
    pub exclude_path: PathBuf,
    /// The capture's exclude bytes.
    pub captured_exclude: Vec<u8>,
    /// The lines `--exclude-union` appends (empty when the exclude is kept
    /// as it is).
    pub exclude_union: Vec<Vec<u8>>,
    /// The worktree tree's `ls-tree -r -z` listing.
    pub entries: Vec<u8>,
}

/// The checks of a first linked landing that need no write, run before
/// the first write to the destination repository (dogfood of 2026-10-16,
/// findings F1 and F2; OI-1003-Q171, Q172, Q173): its path
/// ([`first_landing_target`]); then, the capture read from a scratch
/// repository beside the staged bundle that borrows the destination's
/// objects read-only (`adopt::Scratch`, as the adopt proof reads it), its
/// metadata, the common exclude policy ([`linked_exclude`]:
/// `GIT_IGNORE_POLICY_CONFLICT`) and its intent-to-add custody. A refusal
/// here leaves the destination repository's refs, objects and files as
/// they were. `estate-diff` reports a first linked landing through this
/// same function, so the report and the apply cannot disagree. `union`
/// (`--exclude-union`) decides the exclude by [`linked_exclude`]'s union
/// rule; nothing is written here.
///
/// # Errors
/// As above; whatever reading the capture refuses.
pub(super) fn check_linked(
    staged: &super::StagedBundle,
    (repository, base): (&Path, Option<&super::StagedBundle>),
    destination: &Path,
    source: &str,
    (observed_ns, union): (i128, bool),
) -> Result<LinkedChecked> {
    const SITE: &str = "git_carry::converge::check_linked";
    let destination = first_landing_target(staged, destination)?;
    let repository = fs::canonicalize(repository).refuse_at(SITE)?;
    let scratch =
        adopt::Scratch::new(staged, Some(&adopt::objects_of(&repository)?), base, source)?;
    // The metadata the landing reads after its import: a malformed capture
    // refuses here, before it.
    scratch.meta(source)?;
    let heads = super::shallow::headers(&scratch.repo, staged.path())?;
    let captured_exclude = output(git(&scratch.repo).args([
        "show",
        &format!("{}:value", super::capture_revision(&heads, "exclude")?),
    ]))?;
    let exclude_path = PathBuf::from(text(git(&repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?);
    let common = super::common_repository(&repository)?;
    let (_, exclude_union) = linked_exclude(
        &common,
        &exclude_path,
        &captured_exclude,
        source,
        (observed_ns, union),
    )?;
    // #131: a capture whose intent-to-add custody cannot be restored
    // refuses with nothing laid down.
    let entries = output(git(&scratch.repo).args([
        "ls-tree",
        "-r",
        "-z",
        &super::capture_revision(&heads, "worktree")?,
    ]))?;
    super::intent_to_add_restorable(
        &super::carried_intent_to_add(&scratch.repo, &heads)?,
        &entries,
    )?;
    Ok(LinkedChecked {
        destination,
        repository,
        heads,
        instance: scratch.instance,
        exclude_path,
        captured_exclude,
        exclude_union,
        entries,
    })
}

/// The common `info/exclude` (at `path`) a first linked landing keeps: the
/// capture's (`captured`), or a newer one the main item set (finding 12),
/// with no lines to append. Anything else is `GIT_IGNORE_POLICY_CONFLICT`:
/// it is never overwritten. With `union` (`--exclude-union`, M2 round 2
/// item 2) the existing bytes are kept with the captured lines they lack,
/// to be appended, where [`super::exclude_union::plan`] allows it (no
/// negation on either side, not a ledgered exclude, a regular file or
/// absent); otherwise the refusal stands.
///
/// # Errors
/// As above; IO reading it.
pub(super) fn linked_exclude(
    common: &Path,
    path: &Path,
    captured: &[u8],
    source: &str,
    (t, union): (i128, bool),
) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let existing = match fs::read(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(crate::refuse::io(
                &error,
                "git_carry::converge::linked_exclude",
            ))
        }
    };
    if existing == captured || exclude_newer(common, source, &existing, t)? {
        Ok((existing, Vec::new()))
    } else if union {
        let append = super::exclude_union::plan(common, path, &existing, captured)?;
        Ok((existing, append))
    } else {
        Err(BulkloadRefusal::GitIgnorePolicyConflict)
    }
}

/// What a first linked landing decided before `worktree add`.
pub(super) struct LinkedPlan {
    pub head: HeadPlan,
    pub meta: Meta,
    pub custody: Custody,
    pub common: PathBuf,
    /// The capture's provenance instance, imported.
    pub instance: String,
    /// The common `info/exclude` the landing keeps, read under the lock.
    pub exclude: Vec<u8>,
    /// The HEAD branch `worktree add -b` creates (a foreign repository).
    pub create: Option<String>,
    /// Held until the landing's ledger is written.
    pub lock: landed::DestinationLock,
}

/// A first linked landing's native refs and HEAD, before `worktree add`,
/// after [`check_linked`]: under the destination lock, settle any pending
/// ref intent, check the exclude policy again (a settled intent may have
/// changed it), and only then import the plan `base` a linked item's apply
/// deferred and the capture (`bundle`; dogfood F2:
/// no refusal decidable without a write follows the import); then the
/// native ref step into a repository bulkload created, then where the new
/// worktree's HEAD goes, its branch created or moved first by
/// compare-and-swap. A branch that cannot be attached is never a refusal:
/// the worktree lands detached at the captured head (finding 11). A
/// foreign repository keeps provenance only.
#[allow(clippy::too_many_lines)]
pub(super) fn plan_linked(
    checked: &LinkedChecked,
    (bundle, base): (&Path, Option<&super::StagedBundle>),
    source: &str,
    landing: &Landing<'_>,
) -> Result<LinkedPlan> {
    let t = landing.observed_ns;
    let repository = checked.repository.as_path();
    let common = super::common_repository(repository)?;
    let lock = landed::lock(&common, source)?;
    let directory = landed::ledger_dir(&common, source);
    let mut custody = settle_repository(repository, &common, source)?;
    // `--exclude-union`: under the repository-wide exclude lock, an append
    // a crash cut short is settled first.
    let exclude_path = checked.exclude_path.as_path();
    let exclude_lock = union_settle(landing, &common, exclude_path)?;
    let (exclude, append) = linked_exclude(
        &common,
        &checked.exclude_path,
        &checked.captured_exclude,
        source,
        (t, landing.exclude_union.is_some()),
    )?;
    if landing.beside && !append.is_empty() {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    if let Some(base) = base {
        super::import_verified(repository, base.path(), source)?;
    }
    let (_, instance) = super::import_verified_instance(repository, bundle, source)?;
    if instance != checked.instance {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let instance = instance.as_str();
    let meta = Meta::read(repository, source, instance)?;
    // A beside landing (M2 round 2 item c) never steps the repository's
    // native refs: it is the operator's, whatever its marker says.
    let mut ledger = landed::read::<RefLedger>(&directory.join(landed::REFS_LANDED))?
        .filter(|ledger| ledger.created && !landing.beside);
    let checked: BTreeSet<String> = checked_out_by(repository)?
        .into_iter()
        .map(|(_, branch)| branch)
        .collect();
    if let Some(current) = &ledger {
        let mut skip = checked.clone();
        skip.extend(intent_branches(repository, source)?);
        let (next, more) = native_refs::step(
            repository, &directory, current, source, instance, t, &skip, None, true,
        )?;
        custody.merge(more);
        ledger = Some(next);
    }
    let (native, symrefs) = native_refs::native_refs(repository)?;
    let symbolic = meta.symbolic(repository)?;
    let mut head = head_plan(
        symbolic.as_deref().filter(|_| !landing.beside),
        meta.get("head")?,
        (&native, &symrefs),
        ledger.as_ref(),
        &checked,
        t,
    );
    // A beside landing is detached at the captured head, its branch
    // recorded: no branch of the operator's repository is created or moved.
    if landing.beside {
        head.intended = symbolic;
        head.line = Some("head-detached-beside");
    }
    let mut create = None;
    match (&head.branch, ledger) {
        (Some((name, before, after)), Some(mut next)) => {
            let before_row = next.refs.get(name).cloned();
            if let Some((_, row)) = &head.row {
                next.refs.insert(name.clone(), row.clone());
            }
            native_refs::commit(
                repository,
                &directory,
                vec![landed::RefOp {
                    name: name.clone(),
                    old: before.clone(),
                    new: Some(after.clone()),
                    before: before_row,
                    after: head.row.as_ref().map(|(_, row)| row.clone()),
                }],
                None,
                next,
                Custody::default(),
                true,
            )?;
        }
        // A foreign repository: `worktree add -b` creates the branch, and
        // refuses if anyone created it meanwhile.
        (Some((name, None, _)), None) => create = Some(name.clone()),
        (None, Some(mut next)) => {
            if let Some((name, row)) = &head.row {
                next.refs.insert(name.clone(), row.clone());
                landed::write(&directory.join(landed::REFS_LANDED), &next)?;
            }
        }
        _ => {}
    }
    if let Some(line) = head.line {
        custody.count(line);
    }
    // `--exclude-union`: the append is the plan's last write, after every
    // refusal it can decide, under the lock.
    let exclude = union_append(landing, exclude_path, exclude, &append, &mut custody)?;
    drop(exclude_lock);
    Ok(LinkedPlan {
        head,
        meta,
        custody,
        common,
        instance: instance.to_owned(),
        exclude,
        create,
        lock,
    })
}

/// `--exclude-union`: the repository-wide exclude lock, taken and held by
/// the caller, and the item's intents settled under it; `None` without
/// the flag.
fn union_settle(landing: &Landing<'_>, common: &Path, path: &Path) -> Result<Option<fs::File>> {
    let Some(record) = landing.exclude_union else {
        return Ok(None);
    };
    let held = super::exclude_union::lock(common)?;
    super::exclude_union::settle(record, path)?;
    Ok(Some(held))
}

/// `--exclude-union`'s append of `append` to the exclude at `path` (which
/// holds `exclude`), recorded; the exclude the landing keeps. Counts
/// `exclude-appended=<lines>`: every appended line of the item's record no
/// finished landing reported yet (this append's, and one a later refusal
/// or a crash kept from its receipt).
fn union_append(
    landing: &Landing<'_>,
    path: &Path,
    exclude: Vec<u8>,
    append: &[Vec<u8>],
    custody: &mut Custody,
) -> Result<Vec<u8>> {
    let Some(record) = landing.exclude_union else {
        return Ok(exclude);
    };
    let kept = if append.is_empty() {
        exclude
    } else {
        super::exclude_union::apply((record, landing.item), path, &exclude, append)?
    };
    let lines = super::exclude_union::unreported(record, path)?;
    if lines > 0 {
        custody.add("exclude-appended", lines);
    }
    Ok(kept)
}

/// Whether a first linked landing may keep a common `info/exclude` that is
/// not the capture's: the main item set it from a newer capture than this
/// one (finding 12).
pub(super) fn exclude_newer(common: &Path, source: &str, existing: &[u8], t: i128) -> Result<bool> {
    Ok(
        landed::read::<RefLedger>(&landed::ledger_dir(common, source).join(landed::REFS_LANDED))?
            .and_then(|ledger| ledger.exclude)
            .is_some_and(|row| row.digest == exclude_digest(existing) && row.observed_ns > t),
    )
}

/// Record a first landing's workspace ledger at `worktree`; `custody`
/// gains `not-convergeable` when the ledger is over its cap. `index` is
/// the digest of the index the landing installed (a linked landing's,
/// [`installed_index`]); `None` reads a private stage's.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_landing(
    worktree: &Path,
    common: &Path,
    source: &str,
    landing: &Landing<'_>,
    meta: &Meta,
    head: (&Head, Option<&String>),
    index: Option<[u8; 32]>,
    mut custody: Custody,
) -> Result<Applied> {
    let git_dir =
        landed::worktree_git_dir(worktree)?.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let target = Target {
        repository: worktree,
        workspace: Some(worktree),
        source,
        base: None,
    };
    if !record_landing(
        worktree,
        &git_dir,
        common,
        &target,
        landing,
        meta,
        (head.0.clone(), head.1.cloned()),
        index,
    )? {
        custody.count("not-convergeable");
    }
    Ok(Applied::of(false, &custody))
}

mod adopt;
pub mod advance;
pub mod diff;
pub mod handoff;

#[cfg(test)]
mod tests;
