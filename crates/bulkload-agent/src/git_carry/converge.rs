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
use super::{durable, git, oid, output, safe_destination, text};
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
}

/// What an apply did, for its receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// A converge (or a settle) ran, not a first landing.
    pub converged: bool,
    /// Custody counts, as `key=value` words.
    pub custody: Option<String>,
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
    fn of(converged: bool, custody: &Custody) -> Self {
        let mut shown = custody.clone();
        if !converged {
            shown.0.retain(|key, _| !ROUTINE.contains(key));
        }
        Self {
            converged,
            custody: shown.words(),
        }
    }
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

/// A worktree's HEAD.
pub(super) fn head_of(worktree: &Path) -> Result<Head> {
    let oid = text(git(worktree).args(["rev-parse", "--verify", "HEAD"]))?;
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

/// Branches checked out by each worktree of `repo`: (worktree path, branch).
pub(super) fn checked_out_by(repo: &Path) -> Result<Vec<(PathBuf, String)>> {
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found = Vec::new();
    let mut current: Option<PathBuf> = None;
    for field in listed.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            current = Some(PathBuf::from(OsStr::from_bytes(path)));
        } else if let Some(name) = field.strip_prefix(b"branch ") {
            let name =
                std::str::from_utf8(name).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
            if let Some(path) = &current {
                found.push((path.clone(), name.to_owned()));
            }
        }
    }
    Ok(found)
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
/// directory `git_dir`, common `common`) from `meta`. `None` over the
/// ledger cap: the checkout is landed without one and reported
/// `not-convergeable` (finding 20.6).
pub(super) fn record_landing(
    worktree: &Path,
    git_dir: &Path,
    common: &Path,
    target: &Target<'_>,
    landing: &Landing<'_>,
    meta: &Meta,
    head: (Head, Option<String>),
) -> Result<bool> {
    let ledger = LandedWorkspace {
        item: landing.item.to_owned(),
        capture: landing.capture,
        observed_ns: landing.observed_ns,
        instance: meta.instance.clone(),
        head: head.0,
        intended_branch: head.1,
        index: index_digest(worktree, None)?,
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
/// where a branch is used only when it already names `h1`).
pub(super) fn head_plan(
    symbolic: Option<&str>,
    h1: &str,
    native: &BTreeMap<String, String>,
    ledger: Option<&RefLedger>,
    others: &BTreeSet<String>,
    t: i128,
) -> HeadPlan {
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
    if current.is_none() && native_refs::name_conflict(branch, native.keys().map(String::as_str)) {
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

/// Move HEAD from `old` to `plan` in one compare-and-swap transaction, the
/// branch move with it.
fn set_head(
    worktree: &Path,
    old: &Head,
    new: &Head,
    branch: Option<&(String, Option<String>, String)>,
) -> Result<()> {
    let mut lines = Vec::new();
    // A branch already at its new value is done (a settle after another
    // item's ref step moved it): HEAD alone moves.
    let branch = match branch {
        Some((name, _, after)) => {
            let (native, _) = native_refs::native_refs(worktree)?;
            branch.filter(|_| native.get(name) != Some(after))
        }
        None => None,
    };
    if let Some((name, before, after)) = branch {
        lines.extend_from_slice(
            before
                .as_ref()
                .map_or_else(
                    || format!("create {name}\0{after}\0"),
                    |before| format!("update {name}\0{after}\0{before}\0"),
                )
                .as_bytes(),
        );
    }
    match (&old.symbolic, &new.symbolic) {
        (Some(a), Some(b)) if a == b => {}
        (_, Some(b)) => {
            let check = old
                .symbolic
                .as_ref()
                .map_or_else(|| format!("oid\0{}\0", old.oid), |a| format!("ref\0{a}\0"));
            lines.extend_from_slice(format!("symref-update HEAD\0{b}\0{check}").as_bytes());
        }
        (_, None) => {
            if old.symbolic.is_some() || old.oid != new.oid {
                lines.extend_from_slice(
                    format!("update HEAD\0{}\0{}\0", new.oid, old.oid).as_bytes(),
                );
            }
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    native_refs::update_refs(worktree, None, &lines)
}

// ---- the seat delta ----------------------------------------------------------

/// One side of a seat: what the capture holds at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Side {
    kind: Kind,
    mode: u32,
    blob: Option<String>,
    link: Option<Vec<u8>>,
}

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
}

// One planned seat operation before it is numbered.
type Planned = (Action, Vec<u8>, Option<Side>, Option<Side>);

/// The seat operations from `old` to `new`, in execution order: removals
/// (deepest first), directory removals (deepest first), directory creations
/// (shallowest first), replacements, additions and mode changes, then the
/// directory modes (deepest first).
fn delta(old: &BTreeMap<Vec<u8>, Side>, new: &BTreeMap<Vec<u8>, Side>) -> Vec<Op> {
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
/// with the blob as the object store holds it.
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
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .refuse_at("git_carry::converge::holds")?;
            let mut compare = Compare {
                file: std::io::BufReader::new(file),
                equal: true,
                scratch: Vec::new(),
            };
            let length = objects.copy_into(blob, &mut compare, None);
            let mut rest = [0u8; 1];
            let ended = std::io::Read::read(&mut compare.file, &mut rest)
                .refuse_at("git_carry::converge::holds")?
                == 0;
            crate::counters::add_len(
                crate::counters::Counter::DestVerifyRead,
                usize::try_from(meta.len()).unwrap_or(usize::MAX),
            );
            length.map(|_| compare.equal && ended)
        }
    }
}

/// A writer that compares what it is given with a file's next bytes.
struct Compare {
    file: std::io::BufReader<fs::File>,
    equal: bool,
    scratch: Vec<u8>,
}

impl std::io::Write for Compare {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.equal {
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
        Ok(bytes.len())
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
        // The content fallback: the landed bytes, under a print that held
        // still while they were read, are adopted under that print.
        let side = old.get(&seat.path).ok_or_else(|| modified.clone())?;
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
            Err(refusal) => return Err(refusal),
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
        if lstat(&seat.path)?.is_none() {
            // Settling: done. Fresh: removed by someone since the proof.
            return if self.proof.is_none() {
                Ok(())
            } else {
                Err(interrupted())
            };
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
        write_temp(seat, new, &mut self.objects)?;
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
        self.record(seat, &op.path)
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
        write_temp(seat, new, &mut self.objects)?;
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
            return self.record(seat, &op.path);
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
/// replacement needs (OI-1003-Q100), by exchanging two probe files.
fn probe_exchange(worktree: &Path, tag: &str) -> Result<()> {
    let a = format!(".bulkload-converge-{tag}-probe-a");
    let b = format!(".bulkload-converge-{tag}-probe-b");
    for name in [&a, &b] {
        fs::write(worktree.join(name), b"").refuse_at("git_carry::converge::probe")?;
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

// ---- the index ---------------------------------------------------------------

/// Publish the index of `new` under Git's own `index.lock`, by
/// compare-and-swap on the entries (finding 3): the lock is taken, the
/// entries the converge replaces (`old`) are checked under it, and only
/// then is the new index renamed over. A two-way `read-tree -m` keeps the
/// stat cache of every unchanged entry; a fresh one is the fallback.
fn publish_index(
    worktree: &Path,
    git_dir: &Path,
    metas: (&Meta, &Meta),
    old: [u8; 32],
    tag: &str,
) -> Result<()> {
    const SITE: &str = "git_carry::converge::publish_index";
    let (before, after) = metas;
    let lock = git_dir.join("index.lock");
    let index = git_dir.join("index");
    let fresh = git_dir.join(format!("bulkload-index-{tag}-fresh"));
    let merged = git_dir.join(format!("bulkload-index-{tag}-merged"));
    let marker = format!("bulkload-converge {tag}\n");
    let cleanup = || {
        let _ = fs::remove_file(&fresh);
        let _ = fs::remove_file(&merged);
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
    // A lock this converge left (a crash while holding it, finding 5).
    if lstat(&lock)?.is_some() {
        let ours = fs::read(&lock).is_ok_and(|bytes| bytes == marker.as_bytes())
            || index_digest(worktree, Some(&lock)).is_ok_and(|found| found == target);
        if !ours {
            cleanup();
            return Err(interrupted());
        }
        fs::remove_file(&lock).refuse_at(SITE)?;
    }
    let current = index_digest(worktree, None)?;
    if current == target {
        cleanup();
        return Ok(());
    }
    if current != old {
        cleanup();
        return Err(interrupted());
    }
    let Ok(mut held) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock)
    else {
        cleanup();
        return Err(interrupted());
    };
    held.write_all(marker.as_bytes()).refuse_at(SITE)?;
    drop(held);
    fault_point!(EstateConvergeIndexLocked);
    if index_digest(worktree, None)? != old {
        let _ = fs::remove_file(&lock);
        cleanup();
        return Err(interrupted());
    }
    fs::copy(&index, &merged).refuse_at(SITE)?;
    let two_way = super::read_tree(
        git(worktree).env("GIT_INDEX_FILE", &merged),
        &[&staged(before)?, &staged(after)?],
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
    Ok(())
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

/// The new workspace ledger after a converge or a settle.
#[allow(clippy::too_many_arguments)]
fn next_ledger(
    worktree: &Path,
    common: &Path,
    before: &LandedWorkspace,
    to: ([u8; 32], i128, &str),
    head: (Head, Option<String>),
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
        index: index_digest(worktree, None)?,
        exclude: exclude_digest(&read_exclude(common)?),
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
}

impl Place<'_> {
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
    fn refs(&self, meta: &Meta, t: i128, head: Option<&str>, custody: &mut Custody) -> Result<()> {
        let Some(ledger) = self.ref_ledger()? else {
            return Ok(());
        };
        let mut skip: BTreeSet<String> = checked_out_by(self.worktree)?
            .into_iter()
            .map(|(_, branch)| branch)
            .collect();
        skip.extend(head.map(str::to_owned));
        let (_, more) = native_refs::step(
            self.worktree,
            &self.ref_dir(),
            &ledger,
            self.source,
            &meta.instance,
            t,
            &skip,
        )?;
        custody.merge(more);
        Ok(())
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
pub fn apply_workspace(
    staged: &super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    landed_before: bool,
) -> Result<Applied> {
    super::destination_git_ready()?;
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let standalone = workspace == target.repository;
    if lstat(workspace)?.is_none() {
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
            )
        } else {
            super::restore_linked_staged_landed(
                staged,
                target.repository,
                workspace,
                target.source,
                landing,
            )
        };
    }
    let git_dir =
        landed::worktree_git_dir(workspace)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    let place = Place {
        worktree: workspace,
        base: target.base.filter(|_| standalone),
        common: landed::common_dir(&git_dir)?,
        git_dir,
        source: target.source,
        standalone,
    };
    let ledger_path = place.workspace_dir().join(landed::WORKSPACE_LANDED);
    // A12: no ledger, not bulkload's (checked before any write).
    if !landed::exists(&ledger_path)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let _lock = landed::lock(&place.common, place.source)?;
    let mut custody = native_refs::settle(workspace, &place.ref_dir())?;
    let mut ledger: LandedWorkspace =
        landed::read(&ledger_path)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    // Finding 15(b): a plan edit is not a destination edit.
    if ledger.item != landing.item {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let converged = landed::exists(&place.workspace_dir().join(landed::WORKSPACE_INTENT))?;
    if converged {
        custody.merge(settle(&place, &ledger)?);
        ledger = landed::read(&ledger_path)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    }
    let meta0 = Meta::read(workspace, place.source, &ledger.instance)?;
    let old = sides(workspace, &meta0)?;
    if ledger.capture == landing.capture {
        // A6 (finding 15a): the ledger is ahead of the journal; prove it.
        prove(workspace, &place.common, &ledger, standalone, &old)?;
        custody.count("journal-recovered");
        return Ok(Applied::of(converged, &custody));
    }
    if landing.observed_ns <= ledger.observed_ns {
        return Err(BulkloadRefusal::GitConvergeStaleCapture);
    }
    let proof = prove(workspace, &place.common, &ledger, standalone, &old)?;
    custody.merge(converge(
        &place,
        staged,
        landing,
        &ledger,
        (&meta0, &old),
        &proof,
    )?);
    Ok(Applied::of(true, &custody))
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
    let ops = delta(old, &new);
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
    let (native, _) = native_refs::native_refs(worktree)?;
    let symbolic = meta1.symbolic(worktree)?;
    let plan = head_plan(
        symbolic.as_deref(),
        meta1.get("head")?,
        &native,
        place.ref_ledger()?.as_ref(),
        &others,
        landing.observed_ns,
    );
    let exclude_bytes = meta1.exclude(worktree)?;
    let exclude = (place.standalone && exclude_digest(&exclude_bytes) != ledger.exclude)
        .then(|| (ledger.exclude, exclude_digest(&exclude_bytes)));
    let intent = WorkspaceIntent {
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
    };
    let intent_path = place.workspace_dir().join(landed::WORKSPACE_INTENT);
    landed::write(&intent_path, &intent)?;
    fault_point!(EstateConvergeAfterIntent);
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
    engine.modes(&new)?;
    engine.objects.finish()?;
    place.exclude(intent.exclude, &exclude_bytes)?;
    super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
    publish_index(
        worktree,
        &place.git_dir,
        (meta0, &meta1),
        ledger.index,
        &tag,
    )?;
    fault_point!(EstateConvergeAfterIndex);
    set_head(worktree, &ledger.head, &plan.head, plan.branch.as_ref())
        .map_err(|_| interrupted())?;
    place.branch_row(plan.row.as_ref())?;
    fault_point!(EstateConvergeAfterHead);
    if let Some(line) = plan.line {
        custody.count(line);
    }
    let next = next_ledger(
        worktree,
        &place.common,
        ledger,
        (landing.capture, landing.observed_ns, &instance),
        (plan.head, plan.intended),
        &new,
        &proof.prints,
        &engine.written,
    )?;
    landed::write(&place.workspace_dir().join(landed::WORKSPACE_LANDED), &next)?;
    landed::remove(&intent_path)?;
    fault_point!(EstateConvergeAfterLanded);
    custody.merge(engine.custody);
    Ok(custody)
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
/// third party's: `GIT_CONVERGE_INTERRUPTED`, the intent kept.
fn settle(place: &Place<'_>, ledger: &LandedWorkspace) -> Result<Custody> {
    let worktree = place.worktree;
    let intent_path = place.workspace_dir().join(landed::WORKSPACE_INTENT);
    let intent: WorkspaceIntent = landed::read(&intent_path)?.ok_or_else(interrupted)?;
    let meta0 = Meta::read(worktree, place.source, &ledger.instance)?;
    let meta1 = Meta::read(worktree, place.source, &intent.to_instance)?;
    let (old, new) = (sides(worktree, &meta0)?, sides(worktree, &meta1)?);
    let mut custody = Custody::default();
    place.refs(
        &meta1,
        intent.to_observed_ns,
        meta1.symbolic(worktree)?.as_deref(),
        &mut custody,
    )?;
    let mut engine = Engine {
        worktree,
        tag: &intent.tag,
        proof: None,
        objects: BatchObjects::new(worktree)?,
        touched: BTreeMap::new(),
        written: BTreeMap::new(),
        custody: Custody::default(),
    };
    for op in delta(&old, &new) {
        engine.run(&op)?;
    }
    engine.modes(&new)?;
    engine.objects.finish()?;
    place.exclude(intent.exclude, &meta1.exclude(worktree)?)?;
    super::restore_gitlink_directories(worktree, meta1.get("staged")?, &meta1.heads())?;
    publish_index(
        worktree,
        &place.git_dir,
        (&meta0, &meta1),
        intent.index_old,
        &intent.tag,
    )?;
    let (head_old, head_new) = &intent.head;
    let current = head_of(worktree)?;
    if current != *head_new {
        if current != *head_old {
            return Err(interrupted());
        }
        set_head(worktree, head_old, head_new, intent.branch.as_ref())
            .map_err(|_| interrupted())?;
    }
    if let Some((name, _, value)) = &intent.branch {
        place.branch_row(Some(&(
            name.clone(),
            LandedRef {
                oid: Some(value.clone()),
                observed_ns: intent.to_observed_ns,
            },
        )))?;
    }
    let kept: BTreeMap<Vec<u8>, SeatPrint> = ledger
        .seats
        .iter()
        .map(|seat| (seat.path.clone(), seat.clone()))
        .collect();
    let next = next_ledger(
        worktree,
        &place.common,
        ledger,
        (intent.to, intent.to_observed_ns, &intent.to_instance),
        (head_new.clone(), intent.intended_branch.clone()),
        &new,
        &kept,
        &engine.written,
    )?;
    landed::write(&place.workspace_dir().join(landed::WORKSPACE_LANDED), &next)?;
    landed::remove(&intent_path)?;
    custody.merge(engine.custody);
    custody.count("workspace-intent-settled");
    Ok(custody)
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
) -> Result<Applied> {
    super::destination_git_ready()?;
    let repo = target.repository;
    let common = super::common_repository(repo)?;
    let ref_dir = landed::ledger_dir(&common, target.source);
    let created = landed::read::<RefLedger>(&ref_dir.join(landed::REFS_LANDED))?
        .is_some_and(|ledger| ledger.created);
    if !created {
        super::import_verified(repo, staged.path(), target.source)?;
        return Ok(Applied::default());
    }
    let _lock = landed::lock(&common, target.source)?;
    let mut custody = native_refs::settle(repo, &ref_dir)?;
    let ledger = landed::read::<RefLedger>(&ref_dir.join(landed::REFS_LANDED))?.unwrap_or_default();
    let (_, instance) = super::import_verified_instance(repo, staged.path(), target.source)?;
    let skip = native_refs::checked_out(repo)?;
    let (_, more) = native_refs::step(
        repo,
        &ref_dir,
        &ledger,
        target.source,
        &instance,
        landing.observed_ns,
        &skip,
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
    Ok(Pending {
        refs: landed::exists(&landed::ledger_dir(&common, source).join(landed::REFS_INTENT))?,
        workspace: landed::exists(
            &landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_INTENT),
        )?,
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
    Ok(native_refs::settle(repo, &landed::ledger_dir(&common, source))?.words())
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
    )?;
    let (native, _) = native_refs::native_refs(work)?;
    let plan = head_plan(
        meta.symbolic(work)?.as_deref(),
        meta.get("head")?,
        &native,
        Some(&ledger),
        &BTreeSet::new(),
        t,
    );
    if let Some((name, before, after)) = &plan.branch {
        native_refs::transact(
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
        Some(branch) => {
            let mut command = git(work);
            durable(&mut command);
            output(command.args(["symbolic-ref", "HEAD", branch])).map(drop)?;
        }
        None => native_refs::update_refs(
            work,
            None,
            format!("update HEAD\0{}\0\0", plan.head.oid).as_bytes(),
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

/// What a first linked landing decided before `worktree add`.
pub(super) struct LinkedPlan {
    pub head: HeadPlan,
    pub meta: Meta,
    pub custody: Custody,
    pub common: PathBuf,
    /// The HEAD branch `worktree add -b` creates (a foreign repository).
    pub create: Option<String>,
    /// Held until the landing's ledger is written.
    pub lock: landed::DestinationLock,
}

/// A first linked landing's native refs and HEAD, before `worktree add`:
/// settle any pending ref intent, the native ref step into a repository
/// bulkload created, then where the new worktree's HEAD goes, its branch
/// created or moved first by compare-and-swap. A branch that cannot be
/// attached is never a refusal: the worktree lands detached at the
/// captured head (finding 11). A foreign repository keeps provenance only.
pub(super) fn plan_linked(
    repository: &Path,
    source: &str,
    instance: &str,
    landing: &Landing<'_>,
) -> Result<LinkedPlan> {
    let t = landing.observed_ns;
    let common = super::common_repository(repository)?;
    let lock = landed::lock(&common, source)?;
    let directory = landed::ledger_dir(&common, source);
    let mut custody = native_refs::settle(repository, &directory)?;
    let meta = Meta::read(repository, source, instance)?;
    let mut ledger = landed::read::<RefLedger>(&directory.join(landed::REFS_LANDED))?
        .filter(|ledger| ledger.created);
    let checked: BTreeSet<String> = checked_out_by(repository)?
        .into_iter()
        .map(|(_, branch)| branch)
        .collect();
    if let Some(current) = &ledger {
        let (next, more) = native_refs::step(
            repository, &directory, current, source, instance, t, &checked,
        )?;
        custody.merge(more);
        ledger = Some(next);
    }
    let (native, _) = native_refs::native_refs(repository)?;
    let head = head_plan(
        meta.symbolic(repository)?.as_deref(),
        meta.get("head")?,
        &native,
        ledger.as_ref(),
        &checked,
        t,
    );
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
    Ok(LinkedPlan {
        head,
        meta,
        custody,
        common,
        create,
        lock,
    })
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
/// gains `not-convergeable` when the ledger is over its cap.
pub(super) fn finish_landing(
    worktree: &Path,
    common: &Path,
    source: &str,
    landing: &Landing<'_>,
    meta: &Meta,
    head: &HeadPlan,
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
        (head.head.clone(), head.intended.clone()),
    )? {
        custody.count("not-convergeable");
    }
    Ok(Applied::of(false, &custody))
}

#[cfg(test)]
mod tests;
