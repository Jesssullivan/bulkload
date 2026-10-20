//! `estate-apply --adopt --beside SUFFIX` (M2 round 2, item c, 2026-10-17).
//!
//! The in-place run's i02 batch ended 10 workspace items refused
//! `GIT_DESTINATION_DIVERGED`: the live checkout exists and differs from the
//! capture, so `--adopt` cannot prove it. Each was carried by hand as a
//! refs-only item into the live repository and a second, standalone item at
//! `<path>.neo-20261016`. `--beside` does it natively, and leaves the live
//! checkout's working tree, index, HEAD and native refs as they were:
//!
//! - **When.** Only under `--adopt`, and only for a workspace item whose
//!   planned checkout existed and whose adopt proof refused
//!   `GIT_DESTINATION_DIVERGED` (nothing written). No other refusal leads
//!   here: a first landing that refused after its writes leaves its partial
//!   checkout for review, never a second copy beside it.
//! - **Where.** The workspace path with `SUFFIX` appended verbatim (the
//!   same meaning as `estate-carry-files --beside SUFFIX`): `.neo-20261016`
//!   gives `<path>.neo-20261016`. Its ref namespace is `SUFFIX` without
//!   its leading `.`, which must be one plain ref component (letters,
//!   digits, `.`, `_`, `-`; not starting with `.` or `-`, no `..`, not
//!   ending in `.` or `.lock`), else `FIELD_DOMAIN_VIOLATION`. Labels that
//!   use the same SUFFIX share the namespace: their refs are kept, never
//!   moved, and a differing one keeps the item refused (below).
//! - **What lands there.** A first linked landing of the capture into the
//!   planned (live) repository, detached at the captured head with its
//!   branch recorded (`head-detached-beside`): no branch of the live
//!   repository is created or moved, and its native refs are never
//!   stepped, whatever its marker says. The capture's objects and
//!   provenance refs are imported, the worktree's administration is added
//!   under `worktrees/`, and the landing's own ledgers are written, as any
//!   linked landing writes them. The exclude policy is a linked landing's,
//!   except that a beside landing never appends to the shared
//!   `info/exclude` (that would change what the live checkout ignores):
//!   under `--exclude-union` it lands only when nothing is missing, and
//!   otherwise refuses `GIT_IGNORE_POLICY_CONFLICT`. A landing a crash cut
//!   short is finished forward, then converged when it held an older
//!   capture (below).
//! - **Every pass lands it (gap C12; operator ruling A-115, 2026-10-18:
//!   "Landings placed beside refuse every later pass once neo changes" →
//!   "Yes, converge them").** [`land`] reads what the beside path holds
//!   before anything is written, and takes it one way only:
//!   - absent: the first linked landing above. One this item's STATE
//!     record says it landed there, and that is gone now, was folded or
//!     retired by the operator's agent: it is never laid down again
//!     (`GIT_DESTINATION_MODIFIED`, as a removed live checkout is);
//!   - this item's own landing (its workspace ledger names the item): of
//!     this capture, present; of an older one, converged by the converge
//!     that converges a live landing ([`super::converge`]). It is proved
//!     unchanged against its ledger first, read-only, so a beside checkout
//!     edited since (a seat, the index, HEAD, a commit) refuses
//!     `GIT_DESTINATION_MODIFIED` with nothing written, and the hand-off
//!     blocks bulkload recorded there are its own. A linked beside checkout
//!     converges as it landed: detached at the captured head
//!     (`head-detached-beside`), the live repository's native refs never
//!     stepped. A standalone one (its own repository) converges as any
//!     standalone landing does, in that repository only;
//!   - a landing of another item (the in-place lane's hand plans landed
//!     `<path>.neo-20261016` as standalone items of their own): re-keyed
//!     by A11b after its normal proof (`GIT_DESTINATION_MODIFIED` when that
//!     fails), when its item is not one of this plan's and the capture is
//!     the landing's own or descends from its HEAD, then converged as
//!     above;
//!   - a standalone checkout no ledger names: adopted by proof in its own
//!     repository (A11a), and never converted to a linked worktree: the
//!     adoption writes only that checkout's `.git` (its ledgers, its
//!     carried refs stepped: absent ones created, equal adopted, differing
//!     kept), and the live repository gains provenance and the namespace
//!     refs only. The proof compares HEAD, the index entries, its
//!     `info/exclude`, its stash (each entry's commit, newest first) and
//!     every carried seat's kind, mode and bytes. The one difference
//!     declared is a bulkload hand-off block at byte 0 of `AGENTS.md` or
//!     `CLAUDE.md` above the captured bytes, sealed as estate-handoff
//!     renders it (its `id=` is the blake3 of the block itself, so an edit
//!     inside it or a block a person wrote is a difference): its own write
//!     never blocks the adoption (counted `beside-handoff-block`). Untracked files the capture does not carry,
//!     and the checkout's extra branches and remotes, are not compared;
//!   - anything else, or one of the above whose shape or proof finds it
//!     another's (`GIT_DESTINATION_OCCUPIED` or `GIT_DESTINATION_DIVERGED`
//!     inside): `GIT_BESIDE_FOREIGN`, naming the beside path. A `.git`
//!     gitfile is resolved read-only first (review of #282, F1): one naming
//!     an administration directory that is gone (a copy of a pruned or
//!     moved worktree) or whose common directory is not the planned
//!     repository's is foreign too, never a bare `IO`. Nothing there
//!     is written, moved or deleted, and closure holds the item pending
//!     review; the item's refs still go in (`--refs-in`).
//! - **Refs in.** Before anything is landed, a ref named `refs/<ns>` or
//!   `refs/<ns>-stash` refuses `GIT_DESTINATION_OCCUPIED`. After the
//!   landing, every captured ref `refs/<name>` is wanted as
//!   `refs/<ns>/<name>`, and stash entry `n` (newest first) as
//!   `refs/<ns>-stash/<n>`, a namespace of its own. A wanted name that a
//!   ref of the repository holds the same value at is present. One held at
//!   the value this item created it at (its `.beside` or `.refs-in` record
//!   names that name and value: bulkload's own output, unchanged since) is
//!   moved to the new value by compare-and-swap ([`plan_own`],
//!   `beside-ref-moved`; the old value stays in the earlier capture's
//!   provenance refs). One this item created that is absent now was
//!   removed by the operator and is never created again
//!   (`beside-ref-retired`). Stash entries are decided by commit, never by
//!   index (review of #282, F2, [`stash_wanted`]): a stash commit this
//!   item put there that no `refs/<ns>-stash/` ref holds now is left out
//!   and the rest renumbered, so a removed commit never comes back under
//!   another index. One held at any other value is kept, never
//!   moved or deleted; one a ref of the repository would collide with as a
//!   directory or a file refuses `GIT_DESTINATION_OCCUPIED` before any ref
//!   is written. The creates and moves are recorded in STATE as pending,
//!   then written in one compare-and-swap transaction through the
//!   converge's ref writer (synced), then recorded as created.
//! - **Recorded, counted.** STATE holds `{item}-{blake3(SOURCE)}.beside`
//!   ([`BesideRecord`]): the beside path, the suffix, the capture (from the
//!   beside checkout's own ledger), the instance, the refs pending,
//!   created (at their current values) and kept. A rerun after a crash
//!   moves each pending ref it finds at its value to created. The receipt
//!   counts how the beside path was taken (`beside-landed`,
//!   `beside-present`, `beside-converged`, `beside-rekeyed` or
//!   `beside-adopted`), `beside-linked` or `beside-standalone`,
//!   `beside-ref-created`, `beside-ref-present`, `beside-ref-moved`,
//!   `beside-ref-retired` and `beside-ref-kept`. With every wanted ref in,
//!   the journal and outcome are `workspace-beside`, which `closure-report`
//!   (v5) classifies `applied-beside`; a kept ref refuses the item
//!   `GIT_DESTINATION_OCCUPIED` instead, after the record, so it stays
//!   pending review until the namespace is resolved.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::converge::{self, Applied, Landing, Policy, Target};
use super::{git, landed, native_refs, oid, text};
use crate::{BulkloadRefusal, Result};

/// What STATE records of one item's beside landing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BesideRecord {
    /// `estate::id` of the item.
    pub item: String,
    /// The beside checkout's absolute path.
    pub path: Vec<u8>,
    /// `--beside`'s suffix, as given.
    pub suffix: String,
    /// The capture the beside checkout holds (its own ledger's).
    pub capture: [u8; 32],
    /// The capture's provenance instance in the planned repository.
    pub instance: String,
    /// `(name, oid)` each ref about to be created (write-ahead).
    pub pending: Vec<(String, String)>,
    /// `(name, oid)` each ref this item's beside landing created.
    pub created: Vec<(String, String)>,
    /// `(name, existing, carried)` each ref kept with another value.
    pub kept: Vec<(String, String, String)>,
}

impl landed::Record for BesideRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-beside2";
}

/// The record at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<BesideRecord>> {
    landed::read::<BesideRecord>(path)
}

/// Write `record` at `path` (atomically, durably).
///
/// # Errors
/// IO.
pub fn write_record(path: &Path, record: &BesideRecord) -> Result<()> {
    landed::write(path, record)
}

/// What STATE records of one item's refs-in (M2 item 7).
///
/// `estate-apply --refs-in` (and `estate-move`): the captured branches and
/// tags the live repository holds at another value, or lacks, created under
/// `refs/<ns>/`. Recorded write-ahead like a [`BesideRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefsInRecord {
    /// `estate::id` of the item.
    pub item: String,
    /// The ref namespace (the `--beside` suffix without its leading `.`).
    pub ns: String,
    /// The capture whose refs went in.
    pub capture: [u8; 32],
    /// The capture's provenance instance in the repository.
    pub instance: String,
    /// `(name, oid)` each ref about to be created (write-ahead).
    pub pending: Vec<(String, String)>,
    /// `(name, oid)` each ref this item's refs-in created.
    pub created: Vec<(String, String)>,
    /// `(name, existing, carried)` each ref kept with another value.
    pub kept: Vec<(String, String, String)>,
}

impl landed::Record for RefsInRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-refsin1";
}

/// The refs-in record at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_refs_in(path: &Path) -> Result<Option<RefsInRecord>> {
    landed::read::<RefsInRecord>(path)
}

/// Write the refs-in `record` at `path` (atomically, durably).
///
/// # Errors
/// IO.
pub fn write_refs_in(path: &Path, record: &RefsInRecord) -> Result<()> {
    landed::write(path, record)
}

/// The refs a refs-in wants in `repo` (M2 item 7).
///
/// The in-place lane's `refs-in.py` made native: each captured branch `refs/heads/<name>` or
/// tag `refs/tags/<name>` of `instance` that `repo` lacks or holds at
/// another value, as `refs/<ns>/heads/<name>` (`refs/<ns>/tags/<name>`).
/// A captured ref `repo` holds at its value wants nothing. Read-only.
///
/// # Errors
/// Whatever reading the provenance or the refs refuses.
pub fn refs_in_wanted(
    repo: &Path,
    source: &str,
    instance: &str,
    ns: &str,
) -> Result<BTreeMap<String, String>> {
    let held = names(repo)?;
    let mut wanted = BTreeMap::new();
    for (name, value) in native_refs::carried(repo, source, instance)? {
        if !(name.starts_with("refs/heads/") || name.starts_with("refs/tags/")) {
            continue;
        }
        if held.get(&name) == Some(&value) {
            continue;
        }
        let rest = name
            .strip_prefix("refs/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        wanted.insert(format!("refs/{ns}/{rest}"), value);
    }
    Ok(wanted)
}

/// The refs a clean source's refs-in wants in `repo` (operator ruling
/// 2026-10-16 21:24Z: no tree beside holds them, so the namespace does).
///
/// Every captured ref of `instance` but the stash (its entries go in by
/// [`stash_wanted`]) that `repo` lacks or holds at another value, as
/// `refs/<ns>/<rest>`, as a beside landing wants them. Read-only.
///
/// # Errors
/// Whatever reading the provenance or the refs refuses.
pub fn refs_in_wanted_every(
    repo: &Path,
    source: &str,
    instance: &str,
    ns: &str,
) -> Result<BTreeMap<String, String>> {
    let held = names(repo)?;
    let mut wanted = BTreeMap::new();
    for (name, value) in native_refs::carried(repo, source, instance)? {
        if name == "refs/stash" || held.get(&name) == Some(&value) {
            continue;
        }
        let rest = name
            .strip_prefix("refs/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        wanted.insert(format!("refs/{ns}/{rest}"), value);
    }
    Ok(wanted)
}

/// Whether `name` is a stash entry ref, `refs/<ns>-stash/<n>`: named by
/// its position in the source's stash, so it is decided by its commit
/// ([`stash_wanted`]), never retired by its name ([`plan_own`]).
fn stash_entry(name: &str) -> bool {
    name.strip_prefix("refs/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(ns, index)| {
            ns.ends_with("-stash") && !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The carried stash entries of `instance` in `repo` as this item wants
/// them under `refs/<ns>-stash/`, decided by commit (gap C12; review of
/// #282, F2).
///
/// A stash commit this item put there before (`own`: its `.beside` and
/// `.refs-in` records' `created`) that no ref there holds now was removed
/// by the operator: it is left out, so a removed commit never comes back
/// under another index. With `positional` (a beside landing, which mirrors
/// the source's stash) the rest are numbered newest first, entry `n` as
/// `refs/<ns>-stash/<n>`, and [`plan_own`] moves this item's own entries
/// to them. Without it (create-only refs-in) a commit a ref there already
/// holds is not wanted again, and each new one takes the lowest free
/// index. Read-only.
///
/// # Errors
/// Whatever reading the provenance or the refs refuses.
pub fn stash_wanted(
    repo: &Path,
    (source, instance, ns): (&str, &str, &str),
    own: &[(String, String)],
    positional: bool,
) -> Result<BTreeMap<String, String>> {
    let prefix = format!("refs/{ns}-stash/");
    let existing = names(repo)?;
    let held: BTreeSet<&str> = existing
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(_, value)| value.as_str())
        .collect();
    let removed: BTreeSet<&str> = own
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(_, value)| value.as_str())
        .filter(|value| !held.contains(value))
        .collect();
    let (stash, _) = native_refs::carried_stash(repo, source, instance)?;
    let mut wanted = BTreeMap::new();
    let mut free = 0_usize;
    for entry in stash
        .into_iter()
        .filter(|entry| !removed.contains(entry.oid.as_str()))
    {
        if positional {
            wanted.insert(format!("{prefix}{}", wanted.len()), entry.oid);
            continue;
        }
        if held.contains(entry.oid.as_str()) || wanted.values().any(|value| *value == entry.oid) {
            continue;
        }
        while existing.contains_key(&format!("{prefix}{free}")) {
            free += 1;
        }
        wanted.insert(format!("{prefix}{free}"), entry.oid);
        free += 1;
    }
    Ok(wanted)
}

/// The ref namespace of `suffix`: the suffix without its leading `.`, one
/// plain ref component (see the module docs).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for any other suffix.
pub fn namespace(suffix: &str) -> Result<String> {
    let name = suffix.strip_prefix('.').unwrap_or(suffix);
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !name.starts_with(['.', '-'])
        && !name.ends_with('.')
        && !name.as_bytes().ends_with(b".lock")
        && !name.contains("..");
    if plain {
        Ok(name.to_owned())
    } else {
        Err(BulkloadRefusal::FieldDomainViolation)
    }
}

/// The beside path of `workspace` under `suffix`: the path with `suffix`
/// appended verbatim.
#[must_use]
pub fn path(workspace: &Path, suffix: &str) -> PathBuf {
    let mut name = workspace.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Every ref name of `repo`.
fn names(repo: &Path) -> Result<BTreeMap<String, String>> {
    let listed = text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    let mut names = BTreeMap::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        names.insert(name.to_owned(), value.to_owned());
    }
    Ok(names)
}

/// Whether `repo` holds a ref named `refs/<ns>` or `refs/<ns>-stash`: the
/// namespace a beside landing writes under would collide with it.
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn namespace_taken(repo: &Path, ns: &str) -> Result<bool> {
    let names = names(repo)?;
    Ok(
        names.contains_key(&format!("refs/{ns}"))
            || names.contains_key(&format!("refs/{ns}-stash")),
    )
}

/// Import `staged` into `repo` and name the instance it wrote.
///
/// Provenance only, as a refs-only item imports: for a beside checkout
/// adopted in its own repository, whose refs go in under `refs/<ns>/` of
/// the planned one.
///
/// # Errors
/// Whatever the import refuses.
pub fn import_instance(repo: &Path, staged: &super::StagedBundle, source: &str) -> Result<String> {
    super::import_verified_instance(repo, staged.path(), source).map(|(_, instance)| instance)
}

/// The refs a beside landing wants in `repo`.
///
/// Each captured ref of `instance` under `refs/<ns>/`, each stash entry as
/// `refs/<ns>-stash/<n>` ([`stash_wanted`], positional, less the commits
/// this item put there and the operator removed since: `own`).
///
/// # Errors
/// Whatever reading the provenance refuses.
pub fn wanted(
    repo: &Path,
    (source, instance, ns): (&str, &str, &str),
    own: &[(String, String)],
) -> Result<BTreeMap<String, String>> {
    let mut wanted = BTreeMap::new();
    for (name, value) in native_refs::carried(repo, source, instance)? {
        let rest = name
            .strip_prefix("refs/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        wanted.insert(format!("refs/{ns}/{rest}"), value);
    }
    wanted.extend(stash_wanted(repo, (source, instance, ns), own, true)?);
    Ok(wanted)
}

/// What [`plan`] (or [`plan_own`]) decided for the wanted refs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefsPlan {
    /// `(name, oid)` to create.
    pub create: Vec<(String, String)>,
    /// `(name, oid)` already held at their value.
    pub present: Vec<(String, String)>,
    /// `(name, existing, carried)` held at another value: kept.
    pub kept: Vec<(String, String, String)>,
    /// `(name, existing, carried)` held at the value this item created it
    /// at: moved to the carried value by compare-and-swap ([`plan_own`]).
    pub moved: Vec<(String, String, String)>,
    /// `(name, carried)` this item created once and that is absent now:
    /// never created again ([`plan_own`]).
    pub retired: Vec<(String, String)>,
}

impl RefsPlan {
    /// The refs this plan writes, `(name, new value)`: the creates, then
    /// the moves.
    #[must_use]
    pub fn writes(&self) -> Vec<(String, String)> {
        self.create
            .iter()
            .cloned()
            .chain(
                self.moved
                    .iter()
                    .map(|(name, _, value)| (name.clone(), value.clone())),
            )
            .collect()
    }
}

/// Decide each wanted ref against `repo`'s refs now, create-only (see the
/// module docs). Read-only.
///
/// # Errors
/// `GIT_DESTINATION_OCCUPIED` when a ref to create collides with one of the
/// repository's as a directory or a file; `GIT_INVENTORY_MALFORMED` for a
/// malformed listing.
pub fn plan(repo: &Path, wanted: &BTreeMap<String, String>) -> Result<RefsPlan> {
    plan_own(repo, wanted, &[])
}

/// [`plan`] for a beside landing's later passes (gap C12, A-115).
///
/// `own` holds every `(name, value)` this item created under the namespace (its
/// `.beside` and `.refs-in` records' `created`). A wanted name held at its
/// own value is bulkload's own output, unchanged since it wrote it, and
/// moves to the carried value (`moved`); a name this item created that is
/// absent now was removed by the operator and is never created again
/// (`retired`), except a stash entry, whose removed commit
/// [`stash_wanted`] already left out (its index may hold another commit
/// now). Every other name is decided as [`plan`] decides it: a ref held at
/// another value is kept, never moved. Read-only.
///
/// # Errors
/// As [`plan`].
pub fn plan_own(
    repo: &Path,
    wanted: &BTreeMap<String, String>,
    own: &[(String, String)],
) -> Result<RefsPlan> {
    let existing = names(repo)?;
    let mut planned = RefsPlan::default();
    for (name, value) in wanted {
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let ours = |held: &str| own.iter().any(|(mine, at)| mine == name && at == held);
        match existing.get(name) {
            Some(held) if held == value => planned.present.push((name.clone(), value.clone())),
            Some(held) if ours(held) => {
                planned
                    .moved
                    .push((name.clone(), held.clone(), value.clone()));
            }
            Some(held) => planned
                .kept
                .push((name.clone(), held.clone(), value.clone())),
            None if !stash_entry(name) && own.iter().any(|(mine, _)| mine == name) => {
                planned.retired.push((name.clone(), value.clone()));
            }
            None => {
                if native_refs::name_conflict(name, existing.keys().map(String::as_str)) {
                    return Err(BulkloadRefusal::GitDestinationOccupied);
                }
                planned.create.push((name.clone(), value.clone()));
            }
        }
    }
    Ok(planned)
}

/// Create `refs` in `repo`, create-only.
///
/// One transaction of compare-and-swap `create` lines through the
/// converge's ref writer, which syncs the refs and their directories before
/// returning. A ref another writer created meanwhile refuses the whole
/// transaction, with nothing moved.
///
/// # Errors
/// Whatever the transaction refuses.
pub fn create(repo: &Path, refs: &[(String, String)]) -> Result<()> {
    write(
        repo,
        &RefsPlan {
            create: refs.to_vec(),
            ..RefsPlan::default()
        },
    )
}

/// The refs this item created under its namespace, one value per name:
/// `newest`'s (its `.beside` record's `created`, which a move updates)
/// over `older`'s (its `.refs-in` record's), for [`plan_own`].
#[must_use]
pub fn own_refs(newest: &[(String, String)], older: &[(String, String)]) -> Vec<(String, String)> {
    let mut own: BTreeMap<&str, &str> = older
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    own.extend(
        newest
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    own.into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Write `planned`'s creates and moves in `repo`.
///
/// One transaction of compare-and-swap lines (`create` for a new name, `update` from the value
/// this item left for a move) through the converge's ref writer, which
/// syncs the refs and their directories before returning. A ref another
/// writer created or moved meanwhile refuses the whole transaction, with
/// nothing moved.
///
/// # Errors
/// Whatever the transaction refuses.
pub fn write(repo: &Path, planned: &RefsPlan) -> Result<()> {
    if planned.create.is_empty() && planned.moved.is_empty() {
        return Ok(());
    }
    let mut transaction = Vec::new();
    for (name, value) in &planned.create {
        transaction.extend_from_slice(b"create ");
        transaction.extend_from_slice(name.as_bytes());
        transaction.push(0);
        transaction.extend_from_slice(value.as_bytes());
        transaction.push(0);
    }
    for (name, old, value) in &planned.moved {
        transaction.extend_from_slice(b"update ");
        transaction.extend_from_slice(name.as_bytes());
        transaction.push(0);
        transaction.extend_from_slice(value.as_bytes());
        transaction.push(0);
        transaction.extend_from_slice(old.as_bytes());
        transaction.push(0);
    }
    let names: Vec<&str> = planned
        .create
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(planned.moved.iter().map(|(name, _, _)| name.as_str()))
        .collect();
    native_refs::update_refs(repo, None, &transaction, &names)
}

/// Record `(name, value)` in `created`: the name's entry takes the value
/// (a moved ref), or the pair is appended.
pub fn upsert(created: &mut Vec<(String, String)>, (name, value): (String, String)) {
    match created.iter_mut().find(|(held, _)| *held == name) {
        Some(entry) => entry.1 = value,
        None => created.push((name, value)),
    }
}

/// Settle `record`'s pending refs against `repo`.
///
/// Each pending ref `repo` holds at its value moves to created, at that
/// value (a crash cut the record off after the refs; a moved ref's entry
/// takes its new value); the rest of the pending list is dropped (never
/// written, or since changed: decided again).
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn settle(repo: &Path, record: &mut BesideRecord) -> Result<()> {
    settle_lists(repo, &mut record.pending, &mut record.created)
}

/// [`settle`] for a refs-in record.
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn settle_refs_in(repo: &Path, record: &mut RefsInRecord) -> Result<()> {
    settle_lists(repo, &mut record.pending, &mut record.created)
}

// Each pending ref `repo` holds at its value moves to `created` (upserted);
// the rest of `pending` is dropped.
fn settle_lists(
    repo: &Path,
    pending: &mut Vec<(String, String)>,
    created: &mut Vec<(String, String)>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let existing = names(repo)?;
    for (name, value) in std::mem::take(pending) {
        if existing.get(&name) == Some(&value) {
            upsert(created, (name, value));
        }
    }
    Ok(())
}

/// How [`land`] took the beside path (the receipt's `beside-<how>=1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A first linked landing (or one a crash cut short, finished).
    Landed,
    /// This item's own landing of this capture: nothing to land.
    Present,
    /// This item's own landing of an older capture, converged.
    Converged,
    /// Another item's landing, re-keyed (A11b) and converged.
    Rekeyed,
    /// A standalone checkout no ledger names, adopted by proof (A11a).
    Adopted,
}

impl How {
    /// The receipt word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Present => "present",
            Self::Converged => "converged",
            Self::Rekeyed => "rekeyed",
            Self::Adopted => "adopted",
        }
    }
}

/// What [`land`] did at the beside path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    /// The landing's, converge's or adoption's own receipt words.
    pub applied: Applied,
    pub how: How,
    /// The beside checkout is its own repository (not a linked worktree
    /// of the planned one).
    pub standalone: bool,
}

/// The workspace ledger of the checkout at `beside` for `source`, if any:
/// read through its `.git` (a directory, or a `gitdir:` file).
///
/// # Errors
/// Whatever reading the ledger refuses.
pub fn ledger_of(beside: &Path, source: &str) -> Result<Option<landed::LandedWorkspace>> {
    let Some(git_dir) = landed::worktree_git_dir(beside)? else {
        return Ok(None);
    };
    landed::read(&landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_LANDED))
}

// Review of #282, F1: whether `beside`, which holds no `.git` directory, is
// a linked worktree of `repository`: a `.git` gitfile naming an
// administration directory that exists, whose common directory is the
// repository's. Read-only; anything else (no `.git`, a gitfile naming a
// gitdir that is gone, another repository's worktree, an unreadable one)
// is foreign.
fn linked_into(beside: &Path, repository: &Path) -> bool {
    let Ok(Some(git_dir)) = landed::worktree_git_dir(beside) else {
        return false;
    };
    if !std::fs::symlink_metadata(&git_dir).is_ok_and(|meta| meta.is_dir()) {
        return false;
    }
    match (
        landed::common_dir(&git_dir),
        super::common_repository(repository),
    ) {
        (Ok(common), Ok(planned)) => converge::same_dir(&common, &planned),
        _ => false,
    }
}

/// Land the capture at `beside` for the planned item `target` (gap C12).
///
/// See the module docs (A-115): a first linked landing when the path is
/// absent, this item's own landing kept or converged, another item's
/// landing re-keyed and converged, a standalone checkout no ledger names
/// adopted by proof, and anything else refused `GIT_BESIDE_FOREIGN`
/// naming the path, with nothing at it written. `landing` is the item's
/// with `beside` set; `retired` says this item's STATE record names an
/// earlier landing of its own at `beside`, so an absent path is not laid
/// down again (`GIT_DESTINATION_MODIFIED`).
///
/// # Errors
/// `GIT_BESIDE_FOREIGN`; `GIT_DESTINATION_MODIFIED` (a landing changed
/// since, or retired); `GIT_CONVERGE_STALE_CAPTURE`,
/// `GIT_CONVERGE_SHAPE_CHANGED`, `GIT_CONVERGE_INTERRUPTED`, and whatever
/// the landing, the converge or the adoption refuses.
pub fn land(
    staged: &super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    policy: &Policy<'_>,
    (beside, retired): (&Path, bool),
) -> Result<Landed> {
    const SITE: &str = "git_carry::beside::land";
    let foreign = || BulkloadRefusal::GitBesideForeign(beside.as_os_str().as_bytes().to_vec());
    let linked = Target {
        workspace: Some(beside),
        ..*target
    };
    let own_repository = Target {
        repository: beside,
        workspace: Some(beside),
        ..*target
    };
    // A beside checkout is never advanced (`--advance-ancestor` acts on the
    // live checkout only).
    let keep = Policy {
        adopt: false,
        advance: false,
        ..*policy
    };
    let adopt = Policy {
        adopt: true,
        advance: false,
        ..*policy
    };
    let meta = match std::fs::symlink_metadata(beside) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let applied = converge::apply_workspace(staged, &linked, landing, retired, &keep)?;
            return Ok(Landed {
                applied,
                how: How::Landed,
                standalone: false,
            });
        }
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    // A file, a symlink (never followed) or anything but a directory.
    if !meta.is_dir() {
        return Err(foreign());
    }
    let standalone = std::fs::symlink_metadata(beside.join(".git")).is_ok_and(|meta| meta.is_dir());
    // Review of #282, F1: a `.git` gitfile is read before anything else: one
    // naming an administration directory that is gone (a copy of a pruned
    // or moved worktree) or another repository's is foreign, never a bare IO.
    if !standalone && !linked_into(beside, target.repository) {
        return Err(foreign());
    }
    let at = if standalone { &own_repository } else { &linked };
    let ledger = ledger_of(beside, target.source)?;
    let (how, policy) = match &ledger {
        Some(ledger) if ledger.item == landing.item && ledger.capture == landing.capture => {
            return Ok(Landed {
                applied: Applied::default(),
                how: How::Present,
                standalone,
            });
        }
        Some(ledger) if ledger.item == landing.item => (How::Converged, &keep),
        // Another item's landing: whether A11b re-keys it is decided
        // read-only, before the converge takes its lock or writes anything.
        Some(ledger) => {
            if converge::rekey_check(staged, at, landing, &adopt, ledger)?.is_some() {
                return Err(foreign());
            }
            (How::Rekeyed, &adopt)
        }
        None if standalone => (How::Adopted, &adopt),
        // A linked worktree no ledger names is a first landing a crash cut
        // short only when its landing intent names it (finished forward,
        // then converged); anything else refuses inside.
        None => (How::Landed, &keep),
    };
    let applied =
        converge::apply_workspace(staged, at, landing, false, policy).map_err(|refusal| {
            match refusal {
                BulkloadRefusal::GitDestinationOccupied
                | BulkloadRefusal::GitDestinationDiverged => foreign(),
                other => other,
            }
        })?;
    // A first landing a crash cut short, finished forward, then converged.
    let how = if how == How::Landed && applied.converged {
        How::Converged
    } else {
        how
    };
    Ok(Landed {
        applied,
        how,
        standalone,
    })
}

// `--advance-ancestor` (operator ruling 2026-10-19): a beside landing an
// advance made redundant, retired once proved equal to the live tree.
pub mod retire;
// The clean-source rule (operator ruling 2026-10-16 21:24Z): a beside tree
// whose content the live repository holds, retired by proof.
pub mod subset;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_namespace_is_one_plain_ref_component() {
        assert_eq!(namespace(".neo-20261016").unwrap(), "neo-20261016");
        assert_eq!(namespace("neo-20261016").unwrap(), "neo-20261016");
        assert_eq!(namespace("neo_1.x").unwrap(), "neo_1.x");
        for bad in [
            "", ".", "..neo", ".-neo", "neo/x", "neo..x", "neo.", "neo.lock", "neo x", "neo@{1}",
            "neo:x", "neo~1", "neo^", "neo*", "neo\\x",
        ] {
            assert_eq!(
                namespace(bad),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_path_appends_the_suffix_verbatim() {
        assert_eq!(
            path(Path::new("/srv/git/xoxd.ai"), ".neo-20261016"),
            PathBuf::from("/srv/git/xoxd.ai.neo-20261016")
        );
        assert_eq!(
            path(Path::new("/srv/git/xoxd.ai"), "-beside"),
            PathBuf::from("/srv/git/xoxd.ai-beside")
        );
    }

    #[test]
    fn a_pending_ref_found_at_its_value_settles_created() {
        let base = std::env::temp_dir().join(format!("bulkload-beside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.name=T", "-c", "user.email=t@localhost", "-C"])
                .arg(&base)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        run(&["init", "--quiet", "--template=", "-b", "main"]);
        run(&["commit", "--quiet", "--allow-empty", "-m", "c0"]);
        let head = run(&["rev-parse", "HEAD"]).trim().to_owned();
        let wanted: BTreeMap<String, String> = [
            ("refs/ns/heads/main".to_owned(), head.clone()),
            ("refs/ns/heads/other".to_owned(), head.clone()),
            ("refs/ns-stash/0".to_owned(), head.clone()),
        ]
        .into();
        // A crash after the refs: the record still says pending.
        let first = plan(&base, &wanted).unwrap();
        assert_eq!(first.create.len(), 3);
        create(&base, &first.create).unwrap();
        let mut record = BesideRecord {
            pending: first.create.clone(),
            ..BesideRecord::default()
        };
        // One pending ref was never written (a different value).
        record
            .pending
            .push(("refs/ns/heads/never".to_owned(), head.clone()));
        settle(&base, &mut record).unwrap();
        assert!(record.pending.is_empty());
        assert_eq!(record.created, first.create);
        let again = plan(&base, &wanted).unwrap();
        assert!(again.create.is_empty());
        assert_eq!(again.present.len(), 3);
        // A ref at another value is kept; a directory/file collision refuses.
        run(&["update-ref", "refs/ns/heads/main", "HEAD"]);
        run(&["commit", "--quiet", "--allow-empty", "-m", "c1"]);
        let moved: BTreeMap<String, String> = [(
            "refs/ns/heads/main".to_owned(),
            run(&["rev-parse", "HEAD"]).trim().to_owned(),
        )]
        .into();
        assert_eq!(plan(&base, &moved).unwrap().kept.len(), 1);
        let collides: BTreeMap<String, String> =
            [("refs/ns/heads/main/deeper".to_owned(), head.clone())].into();
        assert_eq!(
            plan(&base, &collides),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert!(!namespace_taken(&base, "ns").unwrap());
        run(&["update-ref", "refs/taken", &head]);
        assert!(namespace_taken(&base, "taken").unwrap());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
