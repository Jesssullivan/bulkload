//! Native ref restore (#215) and ref convergence: the per-ref decision
//! table (design 7.2, as amended by the 2026-10-08 review), its plan over a
//! repository's inventory, the compare-and-swap transaction, and the stash
//! table (7.3).
//!
//! [`decide`] is pure and total over its finite domain; P84 pins it row by
//! row and checks, over every permutation of item order inside one apply,
//! that a ref bulkload landed before ends at the newest capture's value.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use super::landed::{LandedRef, LandedStash, RefIntent, RefLedger, RefOp, StashOp};
use super::{durable, git, input, oid, text};
use crate::{BulkloadRefusal, Result};

/// What kind of ref a name is, for the rows that treat kinds differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// `refs/heads/*`.
    Local,
    /// `refs/tags/*`.
    Tag,
    /// `refs/remotes/*`: follows the source, deleted when the source pruned.
    RemoteTracking,
    /// Every other restorable namespace (`refs/notes`, `refs/replace`, ...).
    Other,
}

impl Class {
    pub fn of(name: &str) -> Self {
        if name.starts_with("refs/heads/") {
            Self::Local
        } else if name.starts_with("refs/tags/") {
            Self::Tag
        } else if name.starts_with("refs/remotes/") {
            Self::RemoteTracking
        } else {
            Self::Other
        }
    }
}

/// One ref's inputs to [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input<'a> {
    /// The destination's current value.
    pub native: Option<&'a str>,
    /// The ledger row (`oid: None` is a tombstone).
    pub row: Option<&'a LandedRef>,
    /// The value the capture carries.
    pub carried: Option<&'a str>,
    /// The capture's `started_ns`.
    pub t: i128,
    pub class: Class,
    /// The ref is some destination worktree's HEAD (finding 1). Only that
    /// worktree's own item moves it, together with its HEAD (step 9).
    pub checked_out: bool,
}

/// The write a decision makes, as a compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    None,
    Create(String),
    Update { old: String, new: String },
    Delete(String),
}

/// What happens to the ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Keep,
    Set(LandedRef),
}

/// The custody line a decision reports (counted per apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Line {
    None,
    Created,
    Adopted,
    KeptDestination,
    Moved,
    Pruned,
    Stale,
    DeletedAtSource,
    ModifiedAtDestination,
    DeletedAtDestination,
    CheckedOutElsewhere,
    /// A create whose name is a path prefix of a ref that stays, or under
    /// one (`refs/heads/foo` kept, `refs/heads/foo/bar` carried): Git can
    /// never create it, so it stays at provenance (review 2 finding 3).
    NameConflict,
}

impl Line {
    pub const fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::None => return None,
            Self::Created => "ref-created",
            Self::Adopted => "ref-adopted",
            Self::KeptDestination => "ref-kept-destination",
            Self::Moved => "ref-moved",
            Self::Pruned => "ref-pruned",
            Self::Stale => "ref-stale",
            Self::DeletedAtSource => "ref-deleted-at-source",
            Self::ModifiedAtDestination => "ref-modified-at-destination",
            Self::DeletedAtDestination => "ref-deleted-at-destination",
            Self::CheckedOutElsewhere => "ref-checked-out-elsewhere",
            Self::NameConflict => "ref-name-conflict",
        })
    }
}

/// One ref's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub write: Write,
    pub row: Row,
    pub line: Line,
}

const fn keep(line: Line) -> Decision {
    Decision {
        write: Write::None,
        row: Row::Keep,
        line,
    }
}

fn set(write: Write, oid: Option<&str>, t: i128, line: Line) -> Decision {
    Decision {
        write,
        row: Row::Set(LandedRef {
            oid: oid.map(str::to_owned),
            observed_ns: t,
        }),
        line,
    }
}

/// The decision table (design 7.2, amended):
///
/// | Row | Native | Ledger | Carried | Action |
/// |---|---|---|---|---|
/// | G | any | any | any | ref checked out by a worktree: skip, `ref-checked-out-elsewhere` |
/// | S | any | tombstone `tl >= t` | any | skip, `ref-stale` |
/// | R1 | absent | none | `v` | create `v` |
/// | R2 | `x` | none | `x` | adopt |
/// | R3 | `x` | none | `v != x` | keep, `ref-kept-destination` |
/// | R10 | `x` | none | absent | untouched |
/// | R4 | `x` | `x` | `x` | verify; row `t := max` |
/// | R5 | `x` | `x`, `t > tl` | `v != x` | CAS `x -> v`, `ref-moved` |
/// | R6 | `x` | `x`, `t <= tl` | `!= x` | skip, `ref-stale` |
/// | R7 | `x` | `x`, `t > tl` | absent | remote-tracking: CAS delete, tombstone, `ref-pruned`; else keep, `ref-deleted-at-source` |
/// | R8 | `y` | `x != y` | any | keep, `ref-modified-at-destination` |
/// | R9 | absent | `x` | any | keep absent, `ref-deleted-at-destination` |
///
/// A tombstone older than the capture reads as no row, except that a
/// capture that also lacks the ref moves the tombstone to its instant.
pub fn decide(input: Input<'_>) -> Decision {
    if input.checked_out {
        return keep(Line::CheckedOutElsewhere);
    }
    let t = input.t;
    let live = input
        .row
        .and_then(|row| row.oid.as_deref().map(|value| (value, row.observed_ns)));
    let tomb = input
        .row
        .filter(|row| row.oid.is_none())
        .map(|row| row.observed_ns);
    if tomb.is_some_and(|tl| tl >= t) && input.carried.is_some() {
        return keep(Line::Stale);
    }
    // A newer capture that also lacks the ref moves its tombstone forward,
    // so a capture between the two cannot resurrect it (P84).
    if input.native.is_none() && input.carried.is_none() && tomb.is_some_and(|tl| tl < t) {
        return set(Write::None, None, t, Line::None);
    }
    match (input.native, live, input.carried) {
        (None | Some(_), None, None) => keep(Line::None),
        (None, None, Some(v)) => set(Write::Create(v.to_owned()), Some(v), t, Line::Created),
        (None, Some(_), _) => keep(Line::DeletedAtDestination),
        (Some(x), None, Some(v)) if v == x => set(Write::None, Some(x), t, Line::Adopted),
        (Some(_), None, Some(_)) => keep(Line::KeptDestination),
        (Some(y), Some((x, _)), _) if y != x => keep(Line::ModifiedAtDestination),
        (Some(x), Some((_, tl)), Some(v)) if v == x => {
            if t > tl {
                set(Write::None, Some(x), t, Line::None)
            } else {
                keep(Line::None)
            }
        }
        (Some(_), Some((_, tl)), _) if t <= tl => keep(Line::Stale),
        (Some(x), Some(_), Some(v)) => set(
            Write::Update {
                old: x.to_owned(),
                new: v.to_owned(),
            },
            Some(v),
            t,
            Line::Moved,
        ),
        (Some(x), Some(_), None) => {
            if input.class == Class::RemoteTracking {
                set(Write::Delete(x.to_owned()), None, t, Line::Pruned)
            } else {
                keep(Line::DeletedAtSource)
            }
        }
    }
}

/// Whether a carried or native ref is restored natively. Never: bulkload's
/// own namespaces, the stash (its own table), per-worktree refs, and the
/// remote `HEAD` symrefs (finding 16), which `for-each-ref` carries resolved.
pub fn restorable(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.starts_with("refs/carry/")
        && !name.starts_with("refs/carry-")
        && name != "refs/stash"
        && !name.starts_with("refs/bisect/")
        && !name.starts_with("refs/worktree/")
        && !name.starts_with("refs/rewritten/")
        && !(name.starts_with("refs/remotes/") && name.ends_with("/HEAD"))
        && !name.contains('\0')
}

/// Per-apply custody counts, printed as `key=value` words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Custody(pub BTreeMap<&'static str, u64>);

impl Custody {
    pub fn count(&mut self, key: &'static str) {
        *self.0.entry(key).or_default() += 1;
    }

    pub fn add(&mut self, key: &'static str, by: u64) {
        if by > 0 {
            *self.0.entry(key).or_default() += by;
        }
    }

    pub fn merge(&mut self, other: Self) {
        for (key, by) in other.0 {
            self.add(key, by);
        }
    }

    /// `None` when nothing was counted.
    pub fn words(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| {
            self.0
                .iter()
                .map(|(key, by)| format!("{key}={by}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
    }
}

/// The plan for one repository's native refs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub ops: Vec<RefOp>,
    pub ledger: RefLedger,
    pub custody: Custody,
}

/// Plan every restorable ref: the destination's `native` values, `ledger`,
/// the capture's `carried` refs at `t`, and the branches some worktree has
/// checked out (`checked_out`, full ref names), with no symbolic ref (the
/// tests' entry; [`step`] plans with the destination's symbolic refs).
#[cfg(test)]
pub fn plan(
    native: &BTreeMap<String, String>,
    ledger: &RefLedger,
    carried: &BTreeMap<String, String>,
    t: i128,
    checked_out: &BTreeSet<String>,
) -> Plan {
    plan_names(
        native,
        ledger,
        carried,
        t,
        checked_out,
        (&BTreeSet::new(), None),
    )
}

// [`plan`] of every restorable ref, or of `only` (the branch a HEAD left,
// planned again once it is no longer checked out). `symbolic` are the
// destination's symbolic refs: never restored over, and names that exist
// after the plan (review 4: a create beside an operator's symbolic ref
// failed its transaction on every rerun).
fn plan_names(
    native: &BTreeMap<String, String>,
    ledger: &RefLedger,
    carried: &BTreeMap<String, String>,
    t: i128,
    checked_out: &BTreeSet<String>,
    (symbolic, only): (&BTreeSet<String>, Option<&str>),
) -> Plan {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    names.extend(native.keys().map(String::as_str));
    names.extend(ledger.refs.keys().map(String::as_str));
    names.extend(carried.keys().map(String::as_str));
    let mut decisions: Vec<(&str, Decision)> = names
        .into_iter()
        .filter(|name| restorable(name) && only.is_none_or(|only| only == *name))
        .map(|name| {
            let decision = decide(Input {
                native: native.get(name).map(String::as_str),
                row: ledger.refs.get(name),
                carried: carried.get(name).map(String::as_str),
                t,
                class: Class::of(name),
                checked_out: checked_out.contains(name),
            });
            (name, decision)
        })
        .collect();
    // Review 2 finding 3: the names that exist once the plan has run; a
    // create that would sit above or below one of them is kept back.
    let mut after: BTreeSet<&str> = native.keys().map(String::as_str).collect();
    after.extend(symbolic.iter().map(String::as_str));
    for (name, decision) in &decisions {
        match decision.write {
            Write::Delete(_) => {
                after.remove(name);
            }
            Write::Create(_) => {
                after.insert(name);
            }
            Write::None | Write::Update { .. } => {}
        }
    }
    for (name, decision) in &mut decisions {
        if matches!(decision.write, Write::Create(_)) && name_conflict(name, after.iter().copied())
        {
            *decision = keep(Line::NameConflict);
        }
    }
    let mut next = ledger.clone();
    let mut ops = Vec::new();
    let mut custody = Custody::default();
    for (name, decision) in decisions {
        let row = ledger.refs.get(name);
        if let Some(word) = decision.line.name() {
            custody.count(word);
        }
        let after = match &decision.row {
            Row::Keep => row.cloned(),
            Row::Set(value) => {
                next.refs.insert(name.to_owned(), value.clone());
                Some(value.clone())
            }
        };
        let (old, new) = match decision.write {
            Write::None => continue,
            Write::Create(new) => (None, Some(new)),
            Write::Update { old, new } => (Some(old), Some(new)),
            Write::Delete(old) => (Some(old), None),
        };
        ops.push(RefOp {
            name: name.to_owned(),
            old,
            new,
            before: row.cloned(),
            after,
        });
    }
    next.prune_tombstones();
    Plan {
        ops,
        ledger: next,
        custody,
    }
}

/// Whether `name` is a path prefix of one of `names`, or one of them is a
/// path prefix of `name` (`refs/heads/foo` and `refs/heads/foo/bar`): Git
/// keeps both in one directory tree and refuses to create the second.
pub fn name_conflict<'a>(name: &str, names: impl IntoIterator<Item = &'a str>) -> bool {
    let below = format!("{name}/");
    names.into_iter().any(|other| {
        other.starts_with(&below)
            || name
                .strip_prefix(other)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The destination's native refs outside bulkload's namespaces, and the
/// names of the symbolic ones (never restored over, finding 16).
pub fn native_refs(repo: &Path) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let listed = text(git(repo).args([
        "for-each-ref",
        "--exclude=refs/carry/",
        "--exclude=refs/carry-reuse/",
        "--exclude=refs/carry-export/",
        "--format=%(objectname) %(refname) %(symref)",
    ]))?;
    let mut values = BTreeMap::new();
    let mut symbolic = BTreeSet::new();
    for line in listed.lines() {
        let mut fields = line.splitn(3, ' ');
        let (Some(value), Some(name)) = (fields.next(), fields.next()) else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        if !fields.next().unwrap_or_default().is_empty() {
            symbolic.insert(name.to_owned());
            continue;
        }
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        values.insert(name.to_owned(), value.to_owned());
    }
    Ok((values, symbolic))
}

/// The branches each worktree of `repo` has checked out, as full names.
pub fn checked_out(repo: &Path) -> Result<BTreeSet<String>> {
    Ok(super::converge::checked_out_by(repo)?
        .into_iter()
        .map(|(_, branch)| branch)
        .collect())
}

/// The carried native refs of a provenance instance in `repo`.
pub fn carried(repo: &Path, source: &str, instance: &str) -> Result<BTreeMap<String, String>> {
    let prefix = format!("refs/carry/v1/{source}/{instance}/");
    let listed = text(git(repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        &format!("{prefix}refs/"),
    ]))?;
    let mut refs = BTreeMap::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let name = name
            .strip_prefix(prefix.as_str())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        refs.insert(name.to_owned(), value.to_owned());
    }
    Ok(refs)
}

/// Run `ops` as `update-ref --stdin -z` transactions of compare-and-swap
/// lines. A concurrent writer fails the transaction it meets: nothing in
/// it moves, and the caller's refusal is typed.
///
/// The deletes run first, in their own transaction: Git refuses a delete
/// of `refs/remotes/o/foo` and a create of `refs/remotes/o/foo/bar` in one
/// transaction (review 2 finding 3). A crash between the two leaves the
/// intent, whose settle sees the deletes done.
///
/// Every worktree's HEAD is held for the transactions (review 3 finding
/// 10): Git's own `HEAD.lock` in each worktree's git directory, taken
/// before the branches checked out (or held by a rebase or bisect) are read
/// under them, so no HEAD can be written onto a branch between that check
/// and the commit. An op on such a branch refuses the whole transaction.
/// (`update-ref` refuses another worktree's HEAD in a transaction,
/// `worktrees/<id>/HEAD`, so a `symref-verify` line can name only the
/// worktree it runs in; the lock files are Git's own protocol.)
///
/// Stated limit (review 4): `HEAD.lock` serialises only the final HEAD
/// write of a `git switch`. Git resolves the branch's commit, checks out
/// its files and commits the index (under `index.lock`) before it takes
/// `HEAD.lock`, and writes HEAD without comparing the branch's value. A
/// transaction that moves the branch after the switch read it and before
/// the switch writes HEAD leaves that worktree on the branch over the
/// files and index of the branch's old commit (`git status` shows the
/// move as staged changes). Neither lock closes the window: the index is
/// committed before HEAD is locked. EstateConverge.tla models a switch as
/// one atomic step and does not cover it.
///
/// # Errors
/// `GIT_CONVERGE_INTERRUPTED` when a HEAD is locked by another writer or an
/// op names a checked-out branch; whatever `update-ref` refuses.
pub fn transact(repo: &Path, ops: &[RefOp]) -> Result<()> {
    transact_in(repo, ops, true)
}

/// [`transact`] in a repository nobody else can see (a first landing's
/// private stage): no HEAD is held, and the stage's unborn HEAD branch may
/// be among the ops.
pub fn transact_private(repo: &Path, ops: &[RefOp]) -> Result<()> {
    transact_in(repo, ops, false)
}

// [`transact`] or, unguarded, [`transact_private`].
fn run(repo: &Path, ops: &[RefOp], guarded: bool) -> Result<()> {
    if guarded {
        transact(repo, ops)
    } else {
        transact_private(repo, ops)
    }
}

fn transact_in(repo: &Path, ops: &[RefOp], guarded: bool) -> Result<()> {
    let (mut deletes, mut writes) = ((Vec::new(), Vec::new()), (Vec::new(), Vec::new()));
    for op in ops {
        if op.name.contains('\0') {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let (into, line) = match (&op.old, &op.new) {
            (None, Some(new)) => (&mut writes, format!("create {}\0{new}\0", op.name)),
            (Some(old), Some(new)) => (&mut writes, format!("update {}\0{new}\0{old}\0", op.name)),
            (Some(old), None) => (&mut deletes, format!("delete {}\0{old}\0", op.name)),
            (None, None) => continue,
        };
        into.0.extend_from_slice(line.as_bytes());
        into.1.push(op.name.as_str());
    }
    if deletes.0.is_empty() && writes.0.is_empty() {
        return Ok(());
    }
    let _held = if guarded {
        let held = HeadLocks::take(repo)?;
        let checked = checked_out(repo)?;
        if ops.iter().any(|op| checked.contains(&op.name)) {
            return Err(BulkloadRefusal::GitConvergeInterrupted);
        }
        Some(held)
    } else {
        None
    };
    for (transaction, names) in [deletes, writes] {
        if !transaction.is_empty() {
            update_refs(repo, None, &transaction, &names)?;
        }
    }
    Ok(())
}

/// One `update-ref --stdin -z` transaction of `lines` in `repo` (the one
/// destination ref writer of the converge; every line it is given is a
/// compare-and-swap), with `message` on a reflog it creates for a stash
/// entry. `names` are the refs the lines write (`HEAD` for the worktree's
/// own): once Git has renamed them into place, the directories that hold
/// them and their reflogs are synced (review 3 finding 7: `core.fsync=
/// reference` syncs each file, never the directory its rename changed), so
/// each write is durable before any ledger names it.
pub fn update_refs(repo: &Path, message: Option<&str>, lines: &[u8], names: &[&str]) -> Result<()> {
    #[cfg(test)]
    if let Some(result) = UPDATE_REFS.with(|hook| {
        hook.borrow()
            .as_ref()
            .and_then(|hook| hook(repo, lines, names))
    }) {
        return result;
    }
    let mut command = git(repo);
    durable(&mut command);
    command.arg("update-ref");
    if let Some(message) = message {
        command.args(["--create-reflog", "-m", message]);
    }
    input(command.args(["--no-deref", "--stdin", "-z"]), lines)?;
    sync_ref_files(repo, names)?;
    sync_ref_dirs(repo, names)
}

/// The files a write of `names` appended to that a decision reads back and
/// Git never syncs: the stash's reflog (review 4: `update-ref` appends a
/// reflog entry with `O_APPEND` and never fsyncs it, whatever
/// `core.fsync` says, so after a power loss `refs/stash` could name an
/// entry its reflog had lost, and `git stash list` and the next decision
/// read the stash as the operator's). Other reflogs are never read back.
pub(super) fn ref_files(common: &Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .filter(|name| **name == "refs/stash")
        .map(|name| common.join("logs").join(name))
        .collect()
}

// Sync every file [`ref_files`] names that exists, before the directory
// syncs and any record or ledger that names the write.
fn sync_ref_files(repo: &Path, names: &[&str]) -> Result<()> {
    use crate::counters::CountedSync as _;
    const SITE: &str = "git_carry::native_refs::sync_ref_files";
    if !names.contains(&"refs/stash") {
        return Ok(());
    }
    let (_, common) = git_dirs(repo)?;
    for path in ref_files(&common, names) {
        match std::fs::File::open(&path) {
            Ok(file) => file.sync_file_counted().refuse_at(SITE)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    }
    Ok(())
}

/// The git directory and the common directory of `repo`, absolute.
pub(super) fn git_dirs(repo: &Path) -> Result<(PathBuf, PathBuf)> {
    let listed = text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ]))?;
    let mut lines = listed.lines();
    match (lines.next(), lines.next()) {
        (Some(git_dir), Some(common)) => Ok((PathBuf::from(git_dir), PathBuf::from(common))),
        _ => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

/// The directories a write of `names` changed: each ref's directory and its
/// reflog's (`HEAD` and its log in the worktree's git directory), and the
/// common directory (`packed-refs`).
pub(super) fn ref_dirs(git_dir: &Path, common: &Path, names: &[&str]) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::from([common.to_path_buf()]);
    for name in names {
        let (base, relative) = if *name == "HEAD" {
            (git_dir, Path::new(*name))
        } else {
            (common, Path::new(*name))
        };
        for root in [base.to_path_buf(), base.join("logs")] {
            if let Some(parent) = root.join(relative).parent() {
                dirs.insert(parent.to_path_buf());
            }
        }
    }
    dirs
}

// Sync every directory [`ref_dirs`] names, or, where Git removed one (an
// emptied ref directory), its nearest existing ancestor under the git
// directories.
fn sync_ref_dirs(repo: &Path, names: &[&str]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let (git_dir, common) = git_dirs(repo)?;
    let mut synced = BTreeSet::new();
    for mut dir in ref_dirs(&git_dir, &common, names) {
        while !dir.is_dir() && dir != common && dir != git_dir {
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => break,
            }
        }
        if dir.is_dir() && synced.insert(dir.clone()) {
            super::landed::sync_dir(&dir)?;
        }
    }
    Ok(())
}

/// What a HEAD lock this process takes holds, so a settle under the
/// destination lock knows one a crash left as its own.
pub const HEAD_LOCK_MARKER: &[u8] = b"bulkload-converge head lock\n";

/// The name prefix of the private temporary a lock's content is written to
/// before the lock is published ([`publish_lock`]); in a git directory, so
/// no checkout shows it.
pub const LOCK_TEMP_PREFIX: &str = "bulkload-lock-";

/// A test hook called with a lock's path the moment its name exists.
#[cfg(test)]
pub(super) type LockHook = Box<dyn Fn(&Path)>;

/// A test hook over [`update_refs`]: `Some` result replaces the Git child.
#[cfg(test)]
pub(super) type UpdateRefsHook = Box<dyn Fn(&Path, &[u8], &[&str]) -> Option<Result<()>>>;

#[cfg(test)]
thread_local! {
    /// Test hook: called with a lock's path the moment its name exists.
    pub(super) static LOCK_VISIBLE: std::cell::RefCell<Option<LockHook>> =
        std::cell::RefCell::new(None);
    /// Test hook over this thread's [`update_refs`] calls.
    pub(super) static UPDATE_REFS: std::cell::RefCell<Option<UpdateRefsHook>> =
        std::cell::RefCell::new(None);
}

/// Publish the lock file `lock` (Git's own `<name>.lock` protocol) holding
/// `content`, so that its name never exists without its content (review 4:
/// a lock created and then written, never synced, is left empty by a power
/// loss, and an empty lock is not provably bulkload's, so it wedged every
/// rerun and blocked the operator's Git). The content goes to a private
/// temporary beside it, is synced, and is renamed into place without
/// replacing (`renameat2(RENAME_NOREPLACE)`, or `linkat` where the file
/// system has none). Returns `false` when the lock is held already (its
/// name exists): nothing is left behind.
///
/// # Errors
/// Whatever writing, syncing or renaming refuses.
pub fn publish_lock(lock: &Path, content: &[u8]) -> Result<bool> {
    use crate::counters::CountedSync as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    const SITE: &str = "git_carry::native_refs::publish_lock";
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (Some(directory), Some(leaf)) = (lock.parent(), lock.file_name()) else {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    };
    let temp = format!(
        "{LOCK_TEMP_PREFIX}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = directory.join(&temp);
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .refuse_at(SITE)?;
        file.write_all(content).refuse_at(SITE)?;
        file.sync_file_counted().refuse_at(SITE)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    let published = std::fs::File::open(directory)
        .refuse_at(SITE)
        .and_then(|dir| {
            let from = crate::io::c_name(temp.as_bytes()).refuse_at(SITE)?;
            let to = crate::io::c_name(leaf.as_bytes()).refuse_at(SITE)?;
            Ok(crate::io::publish_noreplace(&dir, &from, &to))
        });
    match published {
        Ok(Ok(_)) => {
            #[cfg(test)]
            LOCK_VISIBLE.with(|hook| {
                if let Some(hook) = hook.borrow().as_ref() {
                    hook(lock);
                }
            });
            Ok(true)
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&path);
            Ok(false)
        }
        Ok(Err(error)) => {
            let _ = std::fs::remove_file(&path);
            Err(crate::refuse::io(&error, SITE))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
}

/// Remove the temporaries [`publish_lock`] left in `git_dir` (a crash
/// between writing one and publishing it), under the destination lock: the
/// names are bulkload's own. Returns how many.
///
/// # Errors
/// Whatever reading the directory or removing a file refuses.
pub fn sweep_lock_temps(git_dir: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    const SITE: &str = "git_carry::native_refs::sweep_lock_temps";
    let mut removed = 0;
    let entries = match std::fs::read_dir(git_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        if entry
            .file_name()
            .as_bytes()
            .starts_with(LOCK_TEMP_PREFIX.as_bytes())
            && entry.file_type().refuse_at(SITE)?.is_file()
        {
            std::fs::remove_file(entry.path()).refuse_at(SITE)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// HEADs held by Git's own `HEAD.lock` while bulkload writes refs (review 3
/// finding 10): every worktree's for a ref transaction, the other
/// worktrees' while `set_head` attaches a HEAD to a branch (review 4).
/// Dropping it removes the locks; a crash leaves them, marked
/// [`HEAD_LOCK_MARKER`], for the next settle to remove
/// ([`remove_marked_head_locks`]).
#[derive(Debug)]
pub struct HeadLocks(Vec<PathBuf>);

impl HeadLocks {
    /// Take `HEAD.lock` in the git directory of every worktree of `repo`.
    ///
    /// # Errors
    /// `GIT_CONVERGE_INTERRUPTED` when one is held already (a Git command
    /// writing that HEAD, or a lock a crashed writer left).
    pub fn take(repo: &Path) -> Result<Self> {
        Self::take_except(repo, None)
    }

    /// [`take`](Self::take), but not `except`'s (a git directory whose HEAD
    /// the caller's own transaction locks).
    ///
    /// # Errors
    /// As [`take`](Self::take).
    pub fn take_except(repo: &Path, except: Option<&Path>) -> Result<Self> {
        let dirs = super::converge::worktree_git_dirs(repo)?
            .into_iter()
            .filter(|git_dir| {
                !except.is_some_and(|except| super::converge::same_dir(except, git_dir))
            });
        Self::take_dirs(dirs)
    }

    /// `HEAD.lock` in each of `dirs` (git directories) that has a HEAD.
    ///
    /// # Errors
    /// As [`take`](Self::take).
    pub fn take_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let mut held = Self(Vec::new());
        for git_dir in dirs {
            if !git_dir.join("HEAD").is_file() {
                continue;
            }
            let lock = git_dir.join("HEAD.lock");
            if !publish_lock(&lock, HEAD_LOCK_MARKER)? {
                return Err(BulkloadRefusal::GitConvergeInterrupted);
            }
            held.0.push(lock);
        }
        Ok(held)
    }
}

impl Drop for HeadLocks {
    fn drop(&mut self) {
        for lock in &self.0 {
            let _ = std::fs::remove_file(lock);
        }
    }
}

/// The content of the lock file at `path`, `None` when there is none. A
/// lock file is a few bytes; more than a page is no ref lock of Git's.
fn lock_content(path: &Path) -> Result<Option<(Vec<u8>, std::fs::Metadata)>> {
    const SITE: &str = "git_carry::native_refs::lock_content";
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        // A ref directory that is a ref now (`o/x` beside `o/x/y`): no lock.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let meta = file.metadata().refuse_at(SITE)?;
    if !meta.is_file() {
        return Ok(Some((Vec::new(), meta)));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, 4097), &mut bytes).refuse_at(SITE)?;
    Ok(Some((bytes, meta)))
}

/// Remove the lock file at `path` when it is bulkload's own: its content is
/// one of `owned` (Git writes the new value into a ref's lock before it
/// renames it into place; bulkload's own locks hold their marker). An empty
/// lock is never provably bulkload's (review 4: Git leaves a delete's lock
/// and `packed-refs.lock` empty for every writer, live or killed; a live
/// `update-ref --stdin` session holds them for as long as it likes), so it
/// stays, and the transaction that meets it refuses typed. Returns whether
/// it was removed.
///
/// # Errors
/// Whatever reading or removing it refuses.
pub fn remove_owned_lock(path: &Path, owned: &[Vec<u8>]) -> Result<bool> {
    let Some((content, meta)) = lock_content(path)? else {
        return Ok(false);
    };
    if !meta.is_file() || content.is_empty() || !owned.contains(&content) {
        return Ok(false);
    }
    std::fs::remove_file(path).refuse_at("git_carry::native_refs::remove_owned_lock")?;
    if let Some(parent) = path.parent() {
        super::landed::sync_dir(parent)?;
    }
    Ok(true)
}

/// Every worktree's `HEAD.lock` that [`HeadLocks`] took and a crash left
/// (marked [`HEAD_LOCK_MARKER`]), under the destination lock. Returns how
/// many.
///
/// # Errors
/// Whatever reading the administration or removing a lock refuses.
pub fn remove_marked_head_locks(repo: &Path) -> Result<u64> {
    let mut removed = 0;
    for git_dir in super::converge::worktree_git_dirs(repo)? {
        removed += u64::from(remove_owned_lock(
            &git_dir.join("HEAD.lock"),
            &[HEAD_LOCK_MARKER.to_vec()],
        )?);
    }
    Ok(removed)
}

/// The lock files a crashed ref step left, under the destination lock
/// (review 3 finding 8, the earlier report's finding 3): every worktree's
/// `HEAD.lock` this process marks as its own, and the lock of each ref and
/// of `refs/stash` that `intent` writes, when its content proves it
/// bulkload's (the value the op writes). An empty lock (a delete's, a
/// rewrite's stash delete, `packed-refs.lock`) is never removed (review 4);
/// any other lock stays too, and the transaction that meets it refuses
/// typed until its owner, or the operator as Git instructs, removes it.
/// A `packed-refs.new` with no `packed-refs.lock` beside it is removed when
/// `intent` deletes (review 4: Git writes it only while it holds
/// `packed-refs.lock` and creates it exclusively, so a killed delete's
/// leftover failed every later delete and `pack-refs`, ours and the
/// operator's). Returns how many were removed.
///
/// # Errors
/// Whatever reading the administration or removing a lock refuses.
pub fn remove_stale_locks(repo: &Path, intent: &RefIntent) -> Result<u64> {
    let line = |value: &str| format!("{value}\n").into_bytes();
    let (_, common) = git_dirs(repo)?;
    let mut removed = remove_marked_head_locks(repo)?;
    for op in &intent.ops {
        let owned: Vec<Vec<u8>> = op.new.as_deref().map(line).into_iter().collect();
        removed += u64::from(remove_owned_lock(
            &common.join(format!("{}.lock", op.name)),
            &owned,
        )?);
    }
    let deletes = intent.ops.iter().any(|op| op.new.is_none());
    if let Some(stash) = &intent.stash {
        let owned: Vec<Vec<u8>> = stash.new.iter().map(|value| line(value)).collect();
        removed += u64::from(remove_owned_lock(&common.join("refs/stash.lock"), &owned)?);
    }
    if deletes || intent.stash.is_some() {
        removed += u64::from(remove_orphan_packed_new(&common)?);
    }
    Ok(removed)
}

/// Remove `packed-refs.new` in `common` when no `packed-refs.lock` is
/// there: no writer owns it (Git writes it only under that lock), and Git
/// creates it exclusively, so while it stays no later writer can create
/// its own. Checked lock first: a writer that takes the lock after the
/// check cannot create the file until it is gone. Returns whether it was
/// removed.
///
/// # Errors
/// Whatever reading or removing it refuses.
pub fn remove_orphan_packed_new(common: &Path) -> Result<bool> {
    const SITE: &str = "git_carry::native_refs::remove_orphan_packed_new";
    if std::fs::symlink_metadata(common.join("packed-refs.lock")).is_ok() {
        return Ok(false);
    }
    let new = common.join("packed-refs.new");
    match std::fs::symlink_metadata(&new) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    match std::fs::remove_file(&new) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    super::landed::sync_dir(common)?;
    Ok(true)
}

/// The destination's stash reflog, newest first.
pub fn stash_reflog(repo: &Path) -> Result<Vec<String>> {
    let present = git(repo)
        .args(["rev-parse", "--verify", "--quiet", "refs/stash"])
        .output()
        .map_err(|error| crate::refuse::io(&error, "git_carry::native_refs::stash_reflog"))?;
    if !present.status.success() {
        return Ok(Vec::new());
    }
    Ok(
        text(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]))?
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

/// The carried stash of a provenance instance, newest first, with each
/// commit's subject as its message (for `git stash push` that is the reflog
/// message byte for byte). The order is the capture's `stash-order-v1`; a
/// capture without it (taken before this lane) is ordered by committer time,
/// newest first, the carried `refs/stash` tip first, and says so
/// (`stash-order-inferred`).
pub fn carried_stash(
    repo: &Path,
    source: &str,
    instance: &str,
) -> Result<(Vec<(String, String)>, bool)> {
    let prefix = format!("refs/carry/v1/{source}/{instance}/");
    let listed = text(git(repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        &format!("{prefix}stashes/"),
        &format!("{prefix}refs/stash"),
        &format!("{prefix}{}", super::STASH_ORDER_METADATA),
    ]))?;
    let mut tip = None;
    let mut order = None;
    let mut commits = BTreeSet::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if name.ends_with(super::STASH_ORDER_METADATA) {
            order = Some(value.to_owned());
            continue;
        }
        if name.ends_with("/refs/stash") {
            tip = Some(value.to_owned());
        }
        commits.insert(value.to_owned());
    }
    if commits.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut command = git(repo);
    command.args(["show", "-s", "--no-walk=unsorted", "--format=%H %ct %s"]);
    command.args(commits.iter());
    let described = text(&mut command)?;
    let mut subjects = BTreeMap::new();
    let mut rows = Vec::new();
    for line in described.lines() {
        let mut fields = line.splitn(3, ' ');
        let (Some(value), Some(time)) = (fields.next(), fields.next()) else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        let time: i64 = time
            .parse()
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let subject = fields.next().unwrap_or_default().to_owned();
        let first = tip.as_deref() == Some(value);
        subjects.insert(value.to_owned(), subject.clone());
        rows.push((!first, std::cmp::Reverse(time), value.to_owned(), subject));
    }
    if let Some(order) = order {
        let bytes = text(git(repo).args(["show", &format!("{order}:value")]))?;
        let mut carried = Vec::new();
        for value in bytes.lines() {
            let subject = subjects
                .get(value)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            carried.push((value.to_owned(), subject.clone()));
        }
        return Ok((carried, false));
    }
    rows.sort();
    rows.dedup_by(|a, b| a.2 == b.2);
    Ok((
        rows.into_iter()
            .map(|(_, _, value, subject)| (value, subject))
            .collect(),
        true,
    ))
}

/// The stash table (design 7.3, amended by finding 4): `dst` is the
/// destination's reflog, `ledger` what bulkload built last, `carried` the
/// capture's stash at `t`. `Ok(None)` writes nothing.
pub fn stash_decision(
    dst: &[String],
    ledger: Option<&LandedStash>,
    carried: &[(String, String)],
    t: i128,
) -> (Option<StashOp>, Option<&'static str>) {
    let new: Vec<String> = carried.iter().map(|(value, _)| value.clone()).collect();
    let messages: Vec<String> = carried.iter().map(|(_, subject)| subject.clone()).collect();
    let op = |old: &[String]| StashOp {
        old: old.to_vec(),
        new: new.clone(),
        messages: messages.clone(),
        observed_ns: t,
    };
    match ledger {
        None if new.is_empty() => (None, None),
        None if dst.is_empty() => (Some(op(dst)), Some("stash-restored")),
        None => (None, Some("stash-kept-destination")),
        Some(landed) if dst != landed.order.as_slice() => {
            (None, Some("stash-modified-at-destination"))
        }
        Some(landed) if landed.order == new => (None, None),
        Some(landed) if t <= landed.observed_ns => (None, Some("stash-stale")),
        Some(landed) if new.ends_with(&landed.order) => (Some(op(dst)), Some("stash-appended")),
        Some(_) => (Some(op(dst)), Some("stash-rewritten")),
    }
}

/// The reflogs a stash op passes through, from its old order to its new
/// one: an append stores each new entry on top of the old order; a rewrite
/// first deletes the old tip (a compare-and-swap that drops the whole
/// reflog), then stores every entry, oldest first.
pub fn stash_path(op: &StashOp) -> Vec<Vec<String>> {
    let mut path = vec![op.old.clone()];
    let base = if op.new.ends_with(&op.old) {
        op.old.len()
    } else {
        path.push(Vec::new());
        0
    };
    for from in (0..op.new.len().saturating_sub(base)).rev() {
        path.push(op.new.get(from..).unwrap_or_default().to_vec());
    }
    path
}

/// Carry `op` out from the reflog the destination holds now (finding 4),
/// re-read just before, under the destination lock. `at` is the reflog the
/// intent records as reached (`None`: the old order). The stash continues
/// only from there, or from the reflog one store past it (a crash between
/// a store and its record); anything else is the destination's and is kept
/// (the earlier report's finding 4: an operator's `git stash drop` or
/// `clear` after a crash is never undone). Each store is a compare-and-swap
/// on the stash tip, and `record` makes the reflog it reached durable
/// before the next. Returns whether the reflog is now `new`.
///
/// # Errors
/// `GIT_CONVERGE_INTERRUPTED` when a store meets `refs/stash.lock` (another
/// writer's, or a killed store's the settle could not prove its own);
/// whatever `record` refuses.
pub fn stash_apply(
    repo: &Path,
    op: &StashOp,
    at: Option<&[String]>,
    record: &mut dyn FnMut(&[String]) -> Result<()>,
) -> Result<bool> {
    let path = stash_path(op);
    let dst = stash_reflog(repo)?;
    if dst == op.new {
        return Ok(true);
    }
    let reached = at.map_or(Some(0), |at| {
        path.iter().position(|state| state.as_slice() == at)
    });
    let start = match reached {
        Some(index) if path.get(index) == Some(&dst) => index,
        Some(index) if path.get(index + 1) == Some(&dst) => index + 1,
        _ => return Ok(false),
    };
    for pair in path.get(start..).unwrap_or_default().windows(2) {
        let [from, to] = pair else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        let (message, line) = match to.first() {
            None => {
                let tip = from.first().ok_or(BulkloadRefusal::GitInventoryMalformed)?;
                (None, format!("delete refs/stash\0{tip}\0"))
            }
            Some(value) => {
                let message = op
                    .messages
                    .get(op.new.len() - to.len())
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
                let message = if message.is_empty() {
                    "bulkload stash"
                } else {
                    message.as_str()
                };
                let line = from.first().map_or_else(
                    || format!("create refs/stash\0{value}\0"),
                    |tip| format!("update refs/stash\0{value}\0{tip}\0"),
                );
                (Some(message), line)
            }
        };
        if update_refs(repo, message, line.as_bytes(), &["refs/stash"]).is_err() {
            // A lock is typed (review 3 finding 8): never read as the
            // destination's edit, which would clear the stash row.
            let (_, common) = git_dirs(repo)?;
            if common.join("refs/stash.lock").exists() {
                return Err(BulkloadRefusal::GitConvergeInterrupted);
            }
            return Ok(false);
        }
        record(to)?;
    }
    Ok(stash_reflog(repo)? == op.new)
}

/// The native ref step for one item into `repo` (any worktree of the
/// repository, or the stage), with `ledger` as loaded. Writes the intent
/// (when `durable`), the transaction, the stash, the ledger, then removes
/// the intent. Refs in `checked_out` are left to their worktree's item.
/// `only` plans that one ref and no stash: the branch a HEAD left, once it
/// is no longer checked out (the earlier report's finding 5). `guarded`
/// holds every worktree's HEAD for the transaction ([`transact`]); a first
/// landing's private stage is not.
///
/// # Errors
/// A failed transaction (a concurrent writer) leaves the intent and refuses
/// `GIT_CONVERGE_INTERRUPTED`; the next apply settles it.
#[allow(clippy::too_many_arguments)]
pub fn step(
    repo: &Path,
    ledger_dir: &Path,
    ledger: &RefLedger,
    source: &str,
    instance: &str,
    t: i128,
    checked_out: &BTreeSet<String>,
    only: Option<&str>,
    guarded: bool,
) -> Result<(RefLedger, Custody)> {
    let (native, symbolic) = native_refs(repo)?;
    let mut skip = checked_out.clone();
    skip.extend(symbolic.iter().cloned());
    let carried_refs = carried(repo, source, instance)?;
    let mut planned = plan_names(&native, ledger, &carried_refs, t, &skip, (&symbolic, only));
    let stash = if only.is_none() {
        let (carried_stash, inferred) = carried_stash(repo, source, instance)?;
        if inferred {
            planned.custody.count("stash-order-inferred");
        }
        let (op, word) = stash_decision(
            &stash_reflog(repo)?,
            ledger.stash.as_ref(),
            &carried_stash,
            t,
        );
        if let Some(word) = word {
            planned.custody.count(word);
        }
        if let Some(op) = &op {
            planned.ledger.stash = Some(LandedStash {
                order: op.new.clone(),
                observed_ns: t,
            });
        }
        op
    } else {
        None
    };
    let (ledger, custody) = commit(
        repo,
        ledger_dir,
        planned.ops,
        stash,
        planned.ledger,
        planned.custody,
        guarded,
    )?;
    Ok((ledger, custody))
}

/// Make `ops` and `stash` durable as an intent, run them, and write
/// `ledger` (adjusted for any op a third party overtook); then remove the
/// intent. Nothing to write writes only the ledger.
///
/// # Errors
/// A failed transaction leaves the intent: `GIT_CONVERGE_INTERRUPTED`.
pub fn commit(
    repo: &Path,
    ledger_dir: &Path,
    ops: Vec<RefOp>,
    stash: Option<StashOp>,
    ledger: RefLedger,
    custody: Custody,
    guarded: bool,
) -> Result<(RefLedger, Custody)> {
    let intent = RefIntent {
        ops,
        stash,
        ledger,
        stash_at: None,
    };
    let intent_path = ledger_dir.join(super::landed::REFS_INTENT);
    let writes = !intent.ops.is_empty() || intent.stash.is_some();
    if writes {
        super::landed::write(&intent_path, &intent)?;
        fault_point!(EstateConvergeAfterRefIntent);
        if run(repo, &intent.ops, guarded).is_err() {
            return Err(BulkloadRefusal::GitConvergeInterrupted);
        }
    }
    let (ledger, custody) = finish(
        repo,
        &intent,
        writes.then_some(intent_path.as_path()),
        custody,
        guarded,
    )?;
    super::landed::write(&ledger_dir.join(super::landed::REFS_LANDED), &ledger)?;
    if writes {
        super::landed::remove(&intent_path)?;
    }
    Ok((ledger, custody))
}

/// Settle a pending ref intent forward (a rerun's first step): the lock
/// files a crashed step left are removed when they are provably bulkload's
/// ([`remove_stale_locks`]); then each op whose native value is `new` is
/// done, `old` is redone by CAS unless the ref is now checked out, anything
/// else is a third party's and keeps its row as before.
///
/// # Errors
/// `GIT_CONVERGE_INTERRUPTED` when a redo loses its compare-and-swap.
pub fn settle(repo: &Path, ledger_dir: &Path) -> Result<Custody> {
    let intent_path = ledger_dir.join(super::landed::REFS_INTENT);
    let Some(intent) = super::landed::read::<RefIntent>(&intent_path)? else {
        return Ok(Custody::default());
    };
    let removed = remove_stale_locks(repo, &intent)?;
    let (ledger, mut custody) =
        finish(repo, &intent, Some(&intent_path), Custody::default(), true)?;
    custody.add("git-lock-removed", removed);
    super::landed::write(&ledger_dir.join(super::landed::REFS_LANDED), &ledger)?;
    super::landed::remove(&intent_path)?;
    custody.count("ref-intent-settled");
    Ok(custody)
}

// Bring every op of `intent` to `new` where the destination still holds
// `old` and no worktree has the ref checked out (review 3 finding 10, the
// blocker: an op on a branch checked out since the crash is never redone;
// its row is restored, `ref-checked-out-elsewhere`), and the stash forward;
// the ledger that results. The names that exist after the step are the
// native ones less the deletes that will run, plus the creates that will
// (review 3 finding 1): a create beside a ref whose delete will not run is
// dropped (`ref-name-conflict`), never retried forever.
fn finish(
    repo: &Path,
    intent: &RefIntent,
    intent_path: Option<&Path>,
    mut custody: Custody,
    guarded: bool,
) -> Result<(RefLedger, Custody)> {
    let (native, symbolic) = native_refs(repo)?;
    let checked = if guarded {
        checked_out(repo)?
    } else {
        BTreeSet::new()
    };
    let mut ledger = intent.ledger.clone();
    let runs = |op: &RefOp| {
        let now = native.get(&op.name);
        now != op.new.as_ref() && now == op.old.as_ref() && !checked.contains(&op.name)
    };
    let mut after: BTreeSet<&str> = native.keys().map(String::as_str).collect();
    after.extend(symbolic.iter().map(String::as_str));
    for op in intent.ops.iter().filter(|op| runs(op)) {
        match (&op.old, &op.new) {
            (_, None) => {
                after.remove(op.name.as_str());
            }
            (None, Some(_)) => {
                after.insert(op.name.as_str());
            }
            (Some(_), Some(_)) => {}
        }
    }
    let mut again = Vec::new();
    for op in &intent.ops {
        let now = native.get(&op.name);
        if now == op.new.as_ref() {
            continue;
        }
        let blocked = op.old.is_none()
            && now.is_none()
            && name_conflict(
                &op.name,
                after.iter().copied().filter(|name| *name != op.name),
            );
        if runs(op) && !blocked {
            again.push(op.clone());
            continue;
        }
        custody.count(if blocked {
            "ref-name-conflict"
        } else if checked.contains(&op.name) {
            "ref-checked-out-elsewhere"
        } else {
            "ref-modified-at-destination"
        });
        match &op.before {
            Some(row) => ledger.refs.insert(op.name.clone(), row.clone()),
            None => ledger.refs.remove(&op.name),
        };
    }
    if !again.is_empty() && run(repo, &again, guarded).is_err() {
        return Err(BulkloadRefusal::GitConvergeInterrupted);
    }
    custody.add(
        "ref-settled",
        u64::try_from(again.len()).unwrap_or(u64::MAX),
    );
    if let Some(op) = &intent.stash {
        let mut reached = intent.stash_at.clone().unwrap_or_else(|| op.old.clone());
        let mut record = |at: &[String]| -> Result<()> {
            reached = at.to_vec();
            let Some(path) = intent_path else {
                return Ok(());
            };
            let mut next = intent.clone();
            next.stash_at = Some(at.to_vec());
            super::landed::write(path, &next)
        };
        if !stash_apply(repo, op, intent.stash_at.as_deref(), &mut record)? {
            // The destination's stash is kept. The row names the reflog
            // bulkload last landed, so every later decision reads the
            // operator's change as theirs (`stash-modified-at-destination`,
            // kept); with no row an emptied stash read as never restored
            // and was restored again.
            custody.count("stash-modified-at-destination");
            ledger.stash = Some(LandedStash {
                order: reached,
                observed_ns: op.observed_ns,
            });
        }
    }
    Ok((ledger, custody))
}

#[cfg(test)]
mod tests;
