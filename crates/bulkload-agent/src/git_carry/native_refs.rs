//! Native ref restore (#215) and ref convergence: the per-ref decision
//! table (design 7.2, as amended by the 2026-10-08 review), its plan over a
//! repository's inventory, the compare-and-swap transaction, and the stash
//! table (7.3).
//!
//! [`decide`] is pure and total over its finite domain; P84 pins it row by
//! row and checks, over every permutation of item order inside one apply,
//! that a ref bulkload landed before ends at the newest capture's value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
/// checked out (`checked_out`, full ref names).
pub fn plan(
    native: &BTreeMap<String, String>,
    ledger: &RefLedger,
    carried: &BTreeMap<String, String>,
    t: i128,
    checked_out: &BTreeSet<String>,
) -> Plan {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    names.extend(native.keys().map(String::as_str));
    names.extend(ledger.refs.keys().map(String::as_str));
    names.extend(carried.keys().map(String::as_str));
    let mut decisions: Vec<(&str, Decision)> = names
        .into_iter()
        .filter(|name| restorable(name))
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
pub fn transact(repo: &Path, ops: &[RefOp]) -> Result<()> {
    let (mut deletes, mut writes) = (Vec::new(), Vec::new());
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
        into.extend_from_slice(line.as_bytes());
    }
    for transaction in [deletes, writes] {
        if !transaction.is_empty() {
            update_refs(repo, None, &transaction)?;
        }
    }
    Ok(())
}

/// One `update-ref --stdin -z` transaction of `lines` in `repo` (the one
/// destination ref writer of the converge; every line it is given is a
/// compare-and-swap), with `message` on a reflog it creates for a stash
/// entry.
pub fn update_refs(repo: &Path, message: Option<&str>, lines: &[u8]) -> Result<()> {
    let mut command = git(repo);
    durable(&mut command);
    command.arg("update-ref");
    if let Some(message) = message {
        command.args(["--create-reflog", "-m", message]);
    }
    input(command.args(["--no-deref", "--stdin", "-z"]), lines).map(drop)
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

/// Carry `op` out from the reflog the destination holds now (finding 4):
/// re-read just before, under the destination lock. A reflog that is a
/// suffix of `new` (an append, or a rewrite in flight) is continued; one
/// equal to `old` is rewritten, its tip deleted by compare-and-swap first;
/// anything else is the destination's and is kept. Every store is a
/// compare-and-swap on the stash tip. Returns whether the reflog is now
/// `new`.
pub fn stash_apply(repo: &Path, op: &StashOp) -> Result<bool> {
    let mut dst = stash_reflog(repo)?;
    if dst == op.new {
        return Ok(true);
    }
    if !op.new.ends_with(&dst) {
        if dst != op.old {
            return Ok(false);
        }
        if let Some(tip) = dst.first() {
            if update_refs(repo, None, format!("delete refs/stash\0{tip}\0").as_bytes()).is_err() {
                return Ok(false);
            }
        }
        dst = stash_reflog(repo)?;
        if !dst.is_empty() {
            return Ok(false);
        }
    }
    let mut tip = dst.first().cloned();
    for index in (0..op.new.len() - dst.len()).rev() {
        let (Some(value), Some(message)) = (op.new.get(index), op.messages.get(index)) else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        let message = if message.is_empty() {
            "bulkload stash"
        } else {
            message.as_str()
        };
        let line = tip.as_deref().map_or_else(
            || format!("create refs/stash\0{value}\0"),
            |tip| format!("update refs/stash\0{value}\0{tip}\0"),
        );
        if update_refs(repo, Some(message), line.as_bytes()).is_err() {
            return Ok(false);
        }
        tip = Some(value.clone());
    }
    Ok(stash_reflog(repo)? == op.new)
}

/// The native ref step for one item into `repo` (any worktree of the
/// repository, or the stage), with `ledger` as loaded. Writes the intent
/// (when `durable`), the transaction, the stash, the ledger, then removes
/// the intent. Refs in `checked_out` are left to their worktree's item.
///
/// # Errors
/// A failed transaction (a concurrent writer) leaves the intent and refuses
/// `GIT_CONVERGE_INTERRUPTED`; the next apply settles it.
pub fn step(
    repo: &Path,
    ledger_dir: &Path,
    ledger: &RefLedger,
    source: &str,
    instance: &str,
    t: i128,
    checked_out: &BTreeSet<String>,
) -> Result<(RefLedger, Custody)> {
    let (native, symbolic) = native_refs(repo)?;
    let mut skip = checked_out.clone();
    skip.extend(symbolic);
    let carried_refs = carried(repo, source, instance)?;
    let mut planned = plan(&native, ledger, &carried_refs, t, &skip);
    let (carried_stash, inferred) = carried_stash(repo, source, instance)?;
    if inferred {
        planned.custody.count("stash-order-inferred");
    }
    let (stash, word) = stash_decision(
        &stash_reflog(repo)?,
        ledger.stash.as_ref(),
        &carried_stash,
        t,
    );
    if let Some(word) = word {
        planned.custody.count(word);
    }
    if let Some(op) = &stash {
        planned.ledger.stash = Some(LandedStash {
            order: op.new.clone(),
            observed_ns: t,
        });
    }
    let (ledger, custody) = commit(
        repo,
        ledger_dir,
        planned.ops,
        stash,
        planned.ledger,
        planned.custody,
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
) -> Result<(RefLedger, Custody)> {
    let intent = RefIntent { ops, stash, ledger };
    let intent_path = ledger_dir.join(super::landed::REFS_INTENT);
    let writes = !intent.ops.is_empty() || intent.stash.is_some();
    if writes {
        super::landed::write(&intent_path, &intent)?;
        fault_point!(EstateConvergeAfterRefIntent);
        if transact(repo, &intent.ops).is_err() {
            return Err(BulkloadRefusal::GitConvergeInterrupted);
        }
    }
    let (ledger, custody) = finish(repo, &intent, custody)?;
    super::landed::write(&ledger_dir.join(super::landed::REFS_LANDED), &ledger)?;
    if writes {
        super::landed::remove(&intent_path)?;
    }
    Ok((ledger, custody))
}

/// Settle a pending ref intent forward (a rerun's first step): each op
/// whose native value is `new` is done, `old` is redone by CAS, anything
/// else is a third party's and keeps its row as before.
pub fn settle(repo: &Path, ledger_dir: &Path) -> Result<Custody> {
    let intent_path = ledger_dir.join(super::landed::REFS_INTENT);
    let Some(intent) = super::landed::read::<RefIntent>(&intent_path)? else {
        return Ok(Custody::default());
    };
    let (ledger, mut custody) = finish(repo, &intent, Custody::default())?;
    super::landed::write(&ledger_dir.join(super::landed::REFS_LANDED), &ledger)?;
    super::landed::remove(&intent_path)?;
    custody.count("ref-intent-settled");
    Ok(custody)
}

// Bring every op of `intent` to `new` where the destination still holds
// `old`, and the stash forward; the ledger that results.
fn finish(repo: &Path, intent: &RefIntent, mut custody: Custody) -> Result<(RefLedger, Custody)> {
    let (native, _) = native_refs(repo)?;
    let mut ledger = intent.ledger.clone();
    let mut again = Vec::new();
    // The names that exist once every op still to run has run.
    let mut after: BTreeSet<&str> = native.keys().map(String::as_str).collect();
    for op in &intent.ops {
        if op.new.is_none() {
            after.remove(op.name.as_str());
        }
    }
    for op in &intent.ops {
        let now = native.get(&op.name);
        if now == op.new.as_ref() {
            continue;
        }
        // A create that can never apply (a name another ref now sits above
        // or below) is dropped, never retried forever (review 2 finding 3).
        let blocked = op.old.is_none()
            && now.is_none()
            && name_conflict(
                &op.name,
                after.iter().copied().filter(|name| *name != op.name),
            );
        if now == op.old.as_ref() && !blocked {
            again.push(op.clone());
            continue;
        }
        custody.count(if blocked {
            "ref-name-conflict"
        } else {
            "ref-modified-at-destination"
        });
        match &op.before {
            Some(row) => ledger.refs.insert(op.name.clone(), row.clone()),
            None => ledger.refs.remove(&op.name),
        };
    }
    if !again.is_empty() && transact(repo, &again).is_err() {
        return Err(BulkloadRefusal::GitConvergeInterrupted);
    }
    custody.add(
        "ref-settled",
        u64::try_from(again.len()).unwrap_or(u64::MAX),
    );
    if let Some(op) = &intent.stash {
        if !stash_apply(repo, op)? {
            custody.count("stash-modified-at-destination");
            ledger.stash = None;
        }
    }
    Ok((ledger, custody))
}

#[cfg(test)]
mod tests;
