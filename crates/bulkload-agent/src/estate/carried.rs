//! `estate-carry-files` (M2r2 b): carry capture-refused items as files.
//!
//! A planned item whose capture refused is carried byte-exact as files by
//! bulkload's own transfer, and the estate ledger records it, so
//! `closure-report` can prove it `carried`.
//!
//! An item is eligible when it plans a workspace, the corpus holds no
//! capture record for it, and its current outcome record (the last state
//! directory's, as closure reads it) is the apply's `CAPTURE_ABSENT`
//! refusal, or an earlier `carried-as-files` (a rerun re-proves it).
//! Anything else is reported `skipped` with a reason and left alone.
//!
//! Per eligible item, in this order:
//!
//! 1. The destination is the planned workspace, or with `--beside SUFFIX`
//!    the workspace's path with SUFFIX appended. An absent destination is
//!    made (one level, 0755; its parent must exist,
//!    `GIT_DESTINATION_PARENT_MISSING`).
//! 2. One transfer session (`copy`, or `pull` over SSH) into it, recorded in
//!    `CARRY_STATE/<item>/dest` like any `copy` or `pull`
//!    ([`crate::transfer_outcome::record_session`]). The transfer's rules
//!    hold: bytes are carried exactly, an equal destination file is
//!    adopted, a differing one is refused and never overwritten, and its
//!    own durability order applies. A session that refused, or completed
//!    with any refused path, leaves the item's outcome as it was: it is not
//!    carried, and its transfer rows name why.
//! 3. Whenever anything landed (a completed, incomplete or refused session),
//!    every directory below the destination holding a `.git` entry is
//!    resolved as Git resolves it (#261): `rev-parse` for its git dir,
//!    common dir and top level (hardened: no optional locks, no hooks, no
//!    fsmonitor, no discovery above it), its object alternates, and every
//!    symlink inside its administration. All must stay inside the carried
//!    tree, or Git cannot use the directory at all. Anything that resolves
//!    outside refuses `GIT_CARRY_POINTER_ATTACHED` with the `.git` entry's
//!    path, recorded as the item's outcome: Git would act on another
//!    repository through the carried copy. The landed bytes are left as
//!    they are for the operator; nothing is rewritten.
//! 4. The carry record `{item}-{blake3(SOURCE)}.carried` is written
//!    durably in `PRIVATE_STATE`: the item, source, destination, whether it is
//!    beside, the transfer state, the session's identity and the blake3 of
//!    its record bytes, and the audited pointers.
//! 5. Then the outcome record `{item}.outcome` becomes `carried-as-files`.
//!
//! A crash before step 5 leaves the old outcome: closure still reads the
//! item `refused-pending-review` (`CAPTURE_ABSENT`) and a rerun finishes it.
//! The rerun's transfer adopts what the crash left. `closure-report` reads
//! the outcome, the carry record and the transfer record it names, and
//! counts the item `carried` only when all three agree and the session is
//! complete with no refused path.
//!
//! `carried` means carried at the session's time. What is checked again on
//! every proof (closure, and `estate-apply` keeping the outcome): the
//! transfer record's bytes, its session, destination and source, the
//! destination against the plan, and each recorded `.git` directory's
//! resolution (a target that appeared since attaches it, and the item is
//! unaccounted). What is not: the destination's bytes, and `.git` entries
//! made under it after the carry.
//!
//! Every `--item` and `--bind` must name a planned, eligible item, and a
//! bind a workspace item, or the run refuses `FIELD_DOMAIN_VIOLATION`
//! before any write. `--bind-only` refuses a selected workspace item with no
//! bind. An item already carried whose carry still proves is kept, with no
//! session, unless `--recarry` or a bind for it asks again; one whose
//! carry no longer proves needs either. A rerun that ends refused or
//! incomplete for an item that was carried writes `CAPTURE_ABSENT` back,
//! for it and every refs-only item riding on it.
//!
//! `--dry-run` reads only: it prints each item's eligibility, destination,
//! what is there now and what the run would do, and writes nothing.

// Receipt paths print escaped and quoted, as every estate receipt's do.
#![allow(clippy::unnecessary_debug_formatting)]

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{exclusive, id, private_directory, read, write, Item, Plan};
use crate::outcome::{Outcome, OutcomeRecord};
use crate::refuse::RefuseAt as _;
use crate::transfer_outcome::{self, Session, TransferRecord};
use crate::{BulkloadRefusal, Result};
use std::collections::{BTreeMap, BTreeSet};

/// The outcome name a carried item's record holds.
pub const CARRIED_OUTCOME: &str = "carried-as-files";

/// The site a carry's own refusal is recorded at.
const SITE: &str = "estate::carry_files";

/// The current carry record format.
const RECORD_VERSION: u8 = 2;

/// What one `estate-carry-files` run is asked to do.
#[derive(Debug, Clone, Default)]
pub struct CarryOptions {
    /// Land beside the workspace: its path with this suffix appended.
    pub beside: Option<std::ffi::OsString>,
    /// Read only: report, write nothing.
    pub dry_run: bool,
    /// Only these item identities (all eligible items when empty).
    pub items: Vec<String>,
    /// Bind an item to a session already recorded in this destination
    /// state (`ITEM=DEST_STATE`) instead of running one: an earlier `copy`
    /// or `pull` of the item's source into its destination by this same
    /// transport, completed with no refused path. Nothing is transferred
    /// and nothing is written under the destination.
    pub bind: std::collections::BTreeMap<String, PathBuf>,
    /// Refuse a selected workspace item with no bind: no session runs.
    pub bind_only: bool,
    /// Carry again an item whose carry still proves.
    pub recarry: bool,
}

/// One item's carry, as the transport runs it.
#[derive(Debug, Clone)]
pub struct CarryJob {
    pub item: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// The transfer's destination state (local).
    pub destination_state: PathBuf,
}

/// How an item's bytes are carried: a local `copy` or an SSH `pull`.
pub trait Transport {
    /// `copy` or `pull`, as the transfer record names it.
    fn verb(&self) -> &'static str;
    /// The source as the transfer record names it.
    fn source_key(&self, source: &Path) -> String;
    /// The roots-apart decision, before any record write.
    ///
    /// # Errors
    /// `SNAPSHOT_ROOTS_OVERLAP`, or a root that does not resolve.
    fn roots_apart(&self, job: &CarryJob) -> Result<()>;
    /// Run the session.
    ///
    /// # Errors
    /// The session's refusal.
    fn run(&self, job: &CarryJob) -> Result<crate::transfer::TransferStats>;
}

/// A local `copy`, its source state under `CARRY_STATE/<item>/source`.
#[derive(Debug, Clone)]
pub struct LocalCopy;

impl LocalCopy {
    fn source_state(job: &CarryJob) -> PathBuf {
        job.destination_state.with_file_name("source")
    }
}

impl Transport for LocalCopy {
    fn verb(&self) -> &'static str {
        "copy"
    }

    fn source_key(&self, source: &Path) -> String {
        transfer_outcome::copy_source(source)
    }

    fn roots_apart(&self, job: &CarryJob) -> Result<()> {
        crate::transfer::copy_roots_apart(
            &job.source,
            &job.destination,
            &Self::source_state(job),
            &job.destination_state,
        )
    }

    fn run(&self, job: &CarryJob) -> Result<crate::transfer::TransferStats> {
        crate::transfer::copy(
            &job.source,
            &job.destination,
            &Self::source_state(job),
            &job.destination_state,
        )
    }
}

/// What a `.git` entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

/// A directory below the carried tree that holds a `.git` entry, and how
/// Git resolved it when the carry was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedGit {
    /// The directory holding `.git`, relative to the destination ("" for
    /// the destination itself), raw bytes.
    pub rel_dir: Vec<u8>,
    pub kind: GitEntryKind,
    /// A pointer file's `gitdir:` value or a symlink's target, raw bytes.
    pub target: Vec<u8>,
    /// Git resolves it inside the carried tree; false when Git cannot use
    /// the directory at all (an administration absent on this host).
    pub inside: bool,
}

/// The durable carry record (`{item}-{blake3(SOURCE)}.carried`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedRecord {
    pub version: u8,
    pub item: String,
    pub source: PathBuf,
    /// `--beside`'s suffix, raw bytes, when the item landed beside its
    /// planned workspace.
    pub suffix: Option<Vec<u8>>,
    pub destination: PathBuf,
    /// The carry made the destination directory.
    pub created: bool,
    /// The transfer's destination state.
    pub transfer_state: PathBuf,
    /// The transfer session's row identity.
    pub transfer_item: String,
    /// The transfer record's source key (`SOURCE` or `HOST:SOURCE`).
    pub transfer_source: String,
    /// blake3 of the transfer record's persisted bytes at the session's end.
    pub transfer_digest: [u8; 32],
    /// Every directory below the destination holding a `.git` entry.
    pub gits: Vec<CarriedGit>,
    /// For a refs-only item: the workspace item with the same source whose
    /// carry holds its custody (the whole source, `.git` included).
    pub via: Option<String>,
}

/// Where an item's carry record lives, per item and SOURCE label.
#[must_use]
pub fn carried_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.carried",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// What closure needs from a proved carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedProof {
    /// The workspace item whose carry this refs-only item's rides on.
    pub via: Option<String>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub beside: bool,
    pub transfer_state: PathBuf,
    /// Directories holding a `.git` entry, re-resolved by this proof.
    pub gits: u64,
}

/// The largest transfer record a proof reads.
const TRANSFER_RECORD_LIMIT: u64 = 64 * 1024 * 1024;

/// Read and prove `item`'s carry record. Reads only; runs `git rev-parse`
/// in each recorded `.git` directory.
///
/// The record is the last state directory's. It must decode, name this item
/// and its source, and its destination must be the planned workspace (with
/// the recorded suffix), or none for a refs-only item riding on another
/// (`via`). The transfer record it names must exist, have the bytes the
/// carry recorded, and be a completed session from this item's source into
/// that destination with no refused path. Every recorded `.git` directory
/// must still resolve inside the destination or not at all. `None` when no
/// state directory holds a carry record.
#[must_use]
pub fn prove(
    states: &[PathBuf],
    identity: &str,
    source_label: &str,
    item: &Item,
) -> Option<std::result::Result<CarriedProof, &'static str>> {
    let path = states
        .iter()
        .rev()
        .map(|state| carried_path(state, identity, source_label))
        .find(|path| path.is_file())?;
    Some(prove_record(&path, identity, item))
}

fn planned_destination(workspace: &Path, suffix: Option<&[u8]>) -> PathBuf {
    let mut name = workspace.as_os_str().to_owned();
    if let Some(suffix) = suffix {
        name.push(std::ffi::OsStr::from_bytes(suffix));
    }
    transfer_outcome::stable(Path::new(&name))
}

fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn source_matches(transfer_source: &str, source: &Path) -> bool {
    let source = source.to_string_lossy();
    transfer_source == source
        || transfer_source
            .strip_suffix(source.as_ref())
            .is_some_and(|host| host.ends_with(':') && host.len() > 1)
}

fn prove_record(
    path: &Path,
    identity: &str,
    item: &Item,
) -> std::result::Result<CarriedProof, &'static str> {
    let record: CarriedRecord = read(path).map_err(|_| "carried-record-unreadable")?;
    if record.version != RECORD_VERSION || record.item != identity {
        return Err("carried-record-mismatch");
    }
    if record.source != item.source {
        return Err("carried-source-mismatch");
    }
    match (&item.workspace, &record.via) {
        (Some(workspace), None) => {
            if planned_destination(workspace, record.suffix.as_deref()) != record.destination {
                return Err("carried-destination-mismatch");
            }
        }
        (None, Some(_)) => {}
        _ => return Err("carried-shape-mismatch"),
    }
    let bytes = read_bounded(
        &transfer_outcome::record_path(&record.transfer_state, &record.transfer_item),
        TRANSFER_RECORD_LIMIT,
    )
    .ok_or("carried-transfer-missing")?;
    if *blake3::hash(&bytes).as_bytes() != record.transfer_digest {
        return Err("carried-transfer-changed");
    }
    let transfer = transfer_outcome::decode(&bytes).map_err(|_| "carried-transfer-unreadable")?;
    if transfer.session != Session::Completed
        || transfer.session_item() != record.transfer_item
        || transfer.destination != record.destination
        || transfer.source != record.transfer_source
        || !source_matches(&transfer.source, &item.source)
    {
        return Err("carried-transfer-mismatch");
    }
    if !refused_entries(&transfer).is_empty() {
        return Err("carried-transfer-refused-paths");
    }
    for git in &record.gits {
        let dir = record
            .destination
            .join(std::ffi::OsStr::from_bytes(&git.rel_dir));
        // A destination gone since resolves to nothing: nothing attaches.
        if matches!(
            resolve_git(&record.destination, &dir),
            Ok(Resolved::Outside)
        ) {
            return Err("carried-pointer-attached");
        }
    }
    Ok(CarriedProof {
        via: record.via,
        source: record.source,
        destination: record.destination,
        beside: record.suffix.is_some(),
        transfer_state: record.transfer_state,
        gits: record.gits.len() as u64,
    })
}

/// Why a selected `--item` or `--bind` cannot run.
fn selection_errors(
    contents: &Plan,
    eligible: &BTreeMap<String, Eligibility>,
    options: &CarryOptions,
) -> Result<Vec<String>> {
    let mut planned = BTreeMap::new();
    for item in &contents.items {
        planned.insert(id(item)?, item);
    }
    let mut errors = Vec::new();
    let why = |identity: &String| -> Option<&'static str> {
        match (planned.get(identity), eligible.get(identity)) {
            (None, _) | (_, None) => Some("unplanned"),
            (_, Some(Eligibility::Skip(why))) => Some(why),
            _ => None,
        }
    };
    for identity in &options.items {
        if let Some(why) = why(identity) {
            errors.push(format!("item={identity} refused=not-eligible reason={why}"));
        }
    }
    for identity in options.bind.keys() {
        let reason = why(identity)
            .or_else(|| {
                planned
                    .get(identity)
                    .filter(|item| item.workspace.is_none())
                    .map(|_| "bind-refs-only")
            })
            .or_else(|| {
                (!options.items.is_empty() && !options.items.contains(identity))
                    .then_some("bind-not-selected")
            });
        if let Some(why) = reason {
            errors.push(format!("item={identity} refused=bind reason={why}"));
        }
    }
    if options.bind_only {
        for (identity, item) in &planned {
            let selected = options.items.is_empty() || options.items.contains(identity);
            if selected
                && item.workspace.is_some()
                && !matches!(eligible.get(identity), Some(Eligibility::Skip(_)) | None)
                && !options.bind.contains_key(identity)
            {
                errors.push(format!(
                    "item={identity} refused=bind-only reason=bind-missing"
                ));
            }
        }
    }
    Ok(errors)
}

/// Carry every eligible item, one line per item to `line`.
///
/// # Errors
/// Refuses an insecure state directory, a held estate lock, a malformed
/// plan or an invalid suffix, and `FIELD_DOMAIN_VIOLATION` (before any
/// write) for an `--item` or `--bind` that names no planned eligible item
/// or, under `--bind-only`, a selected workspace item with no bind.
/// Per-item refusals are reported and recorded and the verb ends
/// `CONTRACT_SELF_INCONSISTENT` when any item was not carried.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn carry_files(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source_label: &str,
    carry_state: &Path,
    options: &CarryOptions,
    transport: &dyn Transport,
    line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if let Some(suffix) = &options.beside {
        let bytes = suffix.as_bytes();
        if bytes.is_empty() || bytes.contains(&b'/') || bytes.contains(&0) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
    }
    let contents: Plan = read(plan)?;
    // Decided read-only, before any lock or write.
    let mut items = Vec::new();
    let mut eligible = BTreeMap::new();
    for item in &contents.items {
        let identity = id(item)?;
        if eligible.contains_key(&identity) {
            continue;
        }
        eligible.insert(
            identity.clone(),
            eligibility(item, &identity, corpus, state)?,
        );
        items.push((item, identity));
    }
    let errors = selection_errors(&contents, &eligible, options)?;
    if !errors.is_empty() {
        for error in &errors {
            line(error)?;
        }
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let _lock = if options.dry_run {
        None
    } else {
        private_directory(state)?;
        private_directory(carry_state)?;
        Some(exclusive(&state.join("estate.lock"))?)
    };
    let selected = |identity: &String| options.items.is_empty() || options.items.contains(identity);
    let mut failed = false;
    let mut recarried = BTreeSet::new();
    for (item, identity) in &items {
        let Some(workspace) = &item.workspace else {
            continue;
        };
        if !selected(identity) {
            continue;
        }
        let prefix = format!("item={identity} source={:?}", item.source);
        let carried_now = match eligible
            .get(identity)
            .copied()
            .unwrap_or(Eligibility::Skip("unplanned"))
        {
            Eligibility::Skip(why) => {
                line(&format!("{prefix} skipped={why}"))?;
                continue;
            }
            Eligibility::Eligible => false,
            Eligibility::Carried => true,
        };
        let bound = options.bind.get(identity);
        if carried_now && bound.is_none() && !options.recarry {
            // Re-proved, never transferred again unless asked.
            match prove(&[state.to_path_buf()], identity, source_label, item) {
                Some(Ok(proof)) => line(&format!(
                    "{prefix} destination={:?} outcome={CARRIED_OUTCOME} kept=proved",
                    proof.destination
                ))?,
                Some(Err(why)) => {
                    failed = true;
                    line(&format!(
                        "{prefix} carried=unproved reason={why} (rerun with --recarry or --bind)"
                    ))?;
                }
                None => {
                    failed = true;
                    line(&format!(
                        "{prefix} carried=unproved reason=carried-record-missing (rerun with --recarry or --bind)"
                    ))?;
                }
            }
            continue;
        }
        let suffix = options
            .beside
            .as_ref()
            .map(|suffix| suffix.as_bytes().to_vec());
        let mut name = workspace.as_os_str().to_owned();
        if let Some(suffix) = &options.beside {
            name.push(suffix);
        }
        let job = CarryJob {
            item: identity.clone(),
            source: item.source.clone(),
            destination: PathBuf::from(name),
            destination_state: bound
                .cloned()
                .unwrap_or_else(|| carry_state.join(identity).join("dest")),
        };
        if options.dry_run {
            line(&format!(
                "{prefix} {}",
                dry_run(&job, transport, bound.is_some())
            ))?;
            continue;
        }
        let run = (*item, identity.as_str(), state, source_label);
        let outcome = carry_one(&job, run, transport, suffix, bound.is_some());
        if !matches!(outcome, Ok((true, _))) {
            failed = true;
            if carried_now {
                revert(&contents, item, identity, state, source_label)?;
            }
        }
        match outcome {
            Ok((true, summary)) => {
                recarried.insert(identity.clone());
                line(&format!("{prefix} {summary}"))?;
            }
            Ok((false, summary)) => line(&format!("{prefix} {summary}"))?,
            Err(refusal) => line(&format!(
                "{prefix} destination={:?} carried=refused reason={refusal}",
                job.destination
            ))?,
        }
    }
    // Refs-only items: selected ones, and any riding on a workspace item
    // this run carried again (its carry record names the new session).
    for (item, identity) in &items {
        if item.workspace.is_some() {
            continue;
        }
        let state_now = &eligible
            .get(identity)
            .copied()
            .unwrap_or(Eligibility::Skip("unplanned"));
        let riding = matches!(state_now, Eligibility::Carried)
            && read::<CarriedRecord>(&carried_path(state, identity, source_label))
                .ok()
                .and_then(|record| record.via)
                .is_some_and(|via| recarried.contains(&via));
        if !(riding || selected(identity)) {
            continue;
        }
        let prefix = format!("item={identity} source={:?}", item.source);
        if let Eligibility::Skip(why) = state_now {
            line(&format!("{prefix} skipped={why}"))?;
            continue;
        }
        let siblings: Vec<String> = items
            .iter()
            .filter(|(other, _)| other.workspace.is_some() && other.source == item.source)
            .map(|(_, other)| other.clone())
            .collect();
        if options.dry_run {
            line(&format!(
                "{prefix} refs-only would=via {}",
                if siblings.is_empty() {
                    "none".to_owned()
                } else {
                    siblings.join(",")
                }
            ))?;
            continue;
        }
        if matches!(state_now, Eligibility::Carried) && !riding && !options.recarry {
            match prove(&[state.to_path_buf()], identity, source_label, item) {
                Some(Ok(_)) if via_proves(&contents, identity, state, source_label) => {
                    line(&format!(
                        "{prefix} refs-only outcome={CARRIED_OUTCOME} kept=proved"
                    ))?;
                    continue;
                }
                _ => {}
            }
        }
        match carry_via(&contents, item, identity, state, source_label, &siblings) {
            Ok(summary) => line(&format!("{prefix} {summary}"))?,
            Err(refusal) => {
                failed = true;
                line(&format!(
                    "{prefix} refs-only carried=refused reason={refusal}"
                ))?;
            }
        }
    }
    if failed {
        Err(BulkloadRefusal::ContractSelfInconsistent)
    } else {
        Ok(())
    }
}

// Whether the workspace item `sibling` is a planned workspace item of
// `source` whose current outcome is carried-as-files with a carry of its
// own that proves.
fn sibling_proves(
    contents: &Plan,
    sibling: &str,
    source: &Path,
    state: &Path,
    source_label: &str,
) -> bool {
    let Some(item) = contents
        .items
        .iter()
        .find(|item| id(item).is_ok_and(|other| other == sibling))
    else {
        return false;
    };
    item.workspace.is_some()
        && item.source == source
        && matches!(
            crate::outcome::read_record(&state.join(format!("{sibling}.outcome"))),
            Ok(OutcomeRecord {
                outcome: Outcome::CarriedAsFiles,
                ..
            })
        )
        && matches!(
            prove(&[state.to_path_buf()], sibling, source_label, item),
            Some(Ok(CarriedProof { via: None, .. }))
        )
}

fn via_proves(contents: &Plan, identity: &str, state: &Path, source_label: &str) -> bool {
    let Ok(record) = read::<CarriedRecord>(&carried_path(state, identity, source_label)) else {
        return false;
    };
    record
        .via
        .is_some_and(|via| sibling_proves(contents, &via, &record.source, state, source_label))
}

// A refs-only item whose capture refused: its custody is the source's
// `.git`, which a carried workspace item of the same source holds. Its
// carry record names that item (`via`) and binds the same transfer record;
// without one, `CAPTURE_ABSENT` stands (`CLOSURE_UNACCOUNTED` is printed).
fn carry_via(
    contents: &Plan,
    item: &Item,
    identity: &str,
    state: &Path,
    source_label: &str,
    siblings: &[String],
) -> Result<String> {
    for sibling in siblings {
        if !sibling_proves(contents, sibling, &item.source, state, source_label) {
            continue;
        }
        let mut record: CarriedRecord = read(&carried_path(state, sibling, source_label))?;
        identity.clone_into(&mut record.item);
        record.via = Some(sibling.clone());
        write(&carried_path(state, identity, source_label), &record)?;
        record_outcome(state, identity, item, Ok(&record))?;
        return Ok(format!(
            "refs-only destination={:?} outcome={CARRIED_OUTCOME} via={sibling}",
            record.destination
        ));
    }
    Err(BulkloadRefusal::ClosureUnaccounted)
}

// A rerun that did not carry an item that was carried: `CAPTURE_ABSENT`
// again, for it and for every refs-only item riding on it.
fn revert(
    contents: &Plan,
    item: &Item,
    identity: &str,
    state: &Path,
    source_label: &str,
) -> Result<()> {
    let absent = BulkloadRefusal::CaptureAbsent;
    record_outcome(state, identity, item, Err(&absent))?;
    for other in &contents.items {
        if other.workspace.is_some() || other.source != item.source {
            continue;
        }
        let other_id = id(other)?;
        let rides = read::<CarriedRecord>(&carried_path(state, &other_id, source_label))
            .ok()
            .and_then(|record| record.via)
            .is_some_and(|via| via == identity);
        if rides {
            record_outcome(state, &other_id, other, Err(&absent))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eligibility {
    Eligible,
    Carried,
    Skip(&'static str),
}

// Eligible: no capture record, and the current outcome is the apply's
// CAPTURE_ABSENT, or an earlier carry (re-proved by the rerun).
fn eligibility(item: &Item, identity: &str, corpus: &Path, state: &Path) -> Result<Eligibility> {
    if corpus
        .join(format!("{identity}.capture"))
        .try_exists()
        .refuse_at(SITE)?
    {
        return Ok(Eligibility::Skip("captured"));
    }
    let path = state.join(format!("{identity}.outcome"));
    if !path.try_exists().refuse_at(SITE)? {
        return Ok(Eligibility::Skip("no-outcome-record"));
    }
    let Ok(record) = crate::outcome::read_record(&path) else {
        return Ok(Eligibility::Skip("outcome-unreadable"));
    };
    if record.source != item.source {
        return Ok(Eligibility::Skip("record-source-mismatch"));
    }
    Ok(match &record.outcome {
        Outcome::Refused(refusal) if refusal.code() == "CAPTURE_ABSENT" => Eligibility::Eligible,
        Outcome::CarriedAsFiles => Eligibility::Carried,
        Outcome::Refused(_) => Eligibility::Skip("refused-other"),
        _ => Eligibility::Skip("not-capture-absent"),
    })
}

fn dry_run(job: &CarryJob, transport: &dyn Transport, bound: bool) -> String {
    let destination = format!("{:?}", job.destination);
    let now = match fs::symlink_metadata(&job.destination) {
        Err(_) => "absent",
        Ok(meta) if meta.is_dir() => {
            fs::read_dir(&job.destination).map_or("unreadable", |mut entries| {
                if entries.next().is_none() {
                    "empty"
                } else {
                    "nonempty"
                }
            })
        }
        Ok(_) => "not-a-directory",
    };
    let parent = job.destination.parent().is_some_and(Path::is_dir);
    let prior = transfer_outcome::records(&job.destination_state).len();
    let source_key = transport.source_key(&job.source);
    if bound {
        // What a bind would find, read only: the session record and its end.
        let item = TransferRecord::running(
            transport.verb(),
            &source_key,
            &transfer_outcome::stable(&job.destination),
        )
        .session_item();
        let found = match transfer_outcome::read(&job.destination_state, &item) {
            None => "no-record".to_owned(),
            Some(Err(_)) => "unreadable".to_owned(),
            Some(Ok(record)) => format!(
                "{} refused_paths={} completed={} reused={} started={}",
                match record.session {
                    Session::Completed => "completed",
                    Session::Running => "running",
                    Session::Refused(_) => "refused",
                },
                refused_entries(&record).len(),
                record.completed,
                record.reused,
                record.started,
            ),
        };
        return format!(
            "destination={destination} now={now} would=bind {} {source_key:?} -> {destination} session={found}",
            transport.verb()
        );
    }
    format!(
        "destination={destination} now={now} parent={} prior_transfer_records={prior} would={} {source_key:?} -> {destination}",
        if parent { "present" } else { "missing" },
        transport.verb(),
    )
}

fn refused_entries(record: &TransferRecord) -> Vec<&transfer_outcome::Entry> {
    record
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.outcome,
                transfer_outcome::EntryOutcome::Refused { .. }
            )
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // One item's ordered steps, read top to bottom.
fn carry_one(
    job: &CarryJob,
    (item, identity, state, source_label): (&Item, &str, &Path, &str),
    transport: &dyn Transport,
    suffix: Option<Vec<u8>>,
    bound: bool,
) -> Result<(bool, String)> {
    // 1. The destination: made one level, under an existing parent. A bind
    // transfers nothing, so its destination must already be there.
    let created = match fs::symlink_metadata(&job.destination) {
        Ok(meta) if meta.is_dir() => false,
        Err(_) if bound => return Err(BulkloadRefusal::ClosureUnaccounted),
        Ok(_) => return Err(BulkloadRefusal::DestinationOccupied),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = job
                .destination
                .parent()
                .ok_or(BulkloadRefusal::PathNotAbsolute)?;
            if !parent.is_dir() {
                return Err(BulkloadRefusal::GitDestinationParentMissing);
            }
            fs::DirBuilder::new()
                .mode(0o755)
                .create(&job.destination)
                .refuse_at(SITE)?;
            fs::File::open(parent)
                .refuse_at(SITE)?
                .sync_all()
                .refuse_at(SITE)?;
            true
        }
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    // 2. The transfer session, recorded in its destination state; or, for
    // a bind, the session already recorded there.
    let source_key = transport.source_key(&job.source);
    let destination_key = transfer_outcome::stable(&job.destination);
    let session = if bound {
        Ok(())
    } else {
        if let Some(parent) = job.destination_state.parent() {
            private_directory(parent)?;
        }
        transfer_outcome::record_session(
            &job.destination_state,
            transport.verb(),
            &source_key,
            &destination_key,
            "transfer::carry_files",
            || transport.roots_apart(job),
            || transport.run(job),
        )
        .map(drop)
    };
    // 3. Whatever landed, the `.git` resolution, read-only. An attachment
    // is the item's outcome, whatever the session did.
    let gits = match audit_gits(&job.destination) {
        Ok(gits) => gits,
        Err(refusal @ BulkloadRefusal::GitCarryPointerAttached(_)) => {
            record_outcome(state, identity, item, Err(&refusal))?;
            return Err(refusal);
        }
        Err(other) => return Err(other),
    };
    session?;
    let session_item =
        TransferRecord::running(transport.verb(), &source_key, &destination_key).session_item();
    let Some(record_bytes) = read_bounded(
        &transfer_outcome::record_path(&job.destination_state, &session_item),
        TRANSFER_RECORD_LIMIT,
    ) else {
        return Err(BulkloadRefusal::ClosureUnaccounted);
    };
    let transfer =
        transfer_outcome::decode(&record_bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
    if transfer.session != Session::Completed {
        return Err(BulkloadRefusal::ClosureUnaccounted);
    }
    let refused = refused_entries(&transfer);
    if let Some(first) = refused.first() {
        // The transfer's own rows name every refused path; the item is not
        // carried.
        return Ok((
            false,
            format!(
                "destination={:?} carried=incomplete refused_paths={} first_refused=\"{}\" first_code={} gits={} attached=none",
                job.destination,
                refused.len(),
                first.rel_path.escape_ascii(),
                match &first.outcome {
                    transfer_outcome::EntryOutcome::Refused { refusal, .. } => refusal.code(),
                    transfer_outcome::EntryOutcome::SidecarCovered => "",
                },
                gits.len(),
            ),
        ));
    }
    // 4. The carry record, then 5. the outcome.
    let record = CarriedRecord {
        version: RECORD_VERSION,
        item: identity.to_owned(),
        source: item.source.clone(),
        suffix,
        destination: destination_key,
        created,
        transfer_state: job.destination_state.clone(),
        transfer_item: session_item,
        transfer_source: source_key,
        transfer_digest: *blake3::hash(&record_bytes).as_bytes(),
        gits,
        via: None,
    };
    write(&carried_path(state, identity, source_label), &record)?;
    fault_point!(EstateCarryRecorded);
    record_outcome(state, identity, item, Ok(&record))?;
    Ok((
        true,
        format!(
            "destination={:?} outcome={CARRIED_OUTCOME} bound={bound} beside={} created={} completed={} reused={} gits={}",
            record.destination,
            record.suffix.is_some(),
            record.created,
            transfer.completed,
            transfer.reused,
            record.gits.len(),
        ),
    ))
}

fn record_outcome(
    state: &Path,
    identity: &str,
    item: &Item,
    outcome: std::result::Result<&CarriedRecord, &BulkloadRefusal>,
) -> Result<()> {
    let record = match outcome {
        Ok(carried) => OutcomeRecord::of_receipt(
            item.source.clone(),
            CARRIED_OUTCOME,
            None,
            Some(format!(
                "destination={} transfer={}",
                carried.destination.display(),
                carried.transfer_item
            )),
            SITE,
        )?,
        Err(refusal) => OutcomeRecord::of_receipt(
            item.source.clone(),
            crate::outcome::REFUSED,
            Some(refusal),
            Some(refusal.to_string()),
            SITE,
        )?,
    };
    write(
        &state.join(format!("{identity}.outcome")),
        &record.persisted(),
    )
}

/// How Git resolves a directory holding a `.git` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolved {
    /// Git cannot use the directory (an administration absent here).
    Absent,
    /// Everything Git would use is inside the carried tree.
    Inside,
    /// Something Git would use is outside it.
    Outside,
}

// `git rev-parse ARGS` in `dir`, hardened (no optional locks, no hooks, no
// fsmonitor, no discovery above `dir`'s parent): its lines, or None when
// Git refuses the directory.
fn rev_parse(dir: &Path, args: &[&str]) -> Option<Vec<PathBuf>> {
    let out = crate::git_carry::git(dir)
        .arg("rev-parse")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| {
        out.stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| PathBuf::from(std::ffi::OsStr::from_bytes(line)))
            .collect()
    })
}

fn inside(root: &Path, path: &Path) -> bool {
    fs::canonicalize(path).map_or(true, |path| path.starts_with(root))
}

// Every symlink below `admin` (never followed while walking) must resolve
// inside `root`, or nowhere.
fn admin_symlinks_inside(root: &Path, admin: &Path) -> bool {
    let mut pending = vec![admin.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                if !inside(root, &entry.path()) {
                    return false;
                }
            } else if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    true
}

// Each object alternate of `common` must resolve inside `root`, or nowhere.
fn alternates_inside(root: &Path, common: &Path) -> bool {
    let objects = common.join("objects");
    let Ok(text) = fs::read(objects.join("info/alternates")) else {
        return true;
    };
    text.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .all(|line| {
            let line = line.strip_prefix(b"\"").unwrap_or(line);
            let line = line.strip_suffix(b"\"").unwrap_or(line);
            inside(root, &objects.join(std::ffi::OsStr::from_bytes(line)))
        })
}

/// Resolve `dir`, which holds a `.git` entry, as Git does, against the
/// carried tree at `root`: its git dir, common dir and top level, its
/// object alternates and every symlink in its administration.
fn resolve_git(root: &Path, dir: &Path) -> Result<Resolved> {
    let root = fs::canonicalize(root).refuse_at(SITE)?;
    let Some(dirs) = rev_parse(
        dir,
        &[
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    ) else {
        return Ok(Resolved::Absent);
    };
    let [git_dir, common] = dirs.as_slice() else {
        return Ok(Resolved::Outside);
    };
    if !inside(&root, git_dir) || !inside(&root, common) {
        return Ok(Resolved::Outside);
    }
    // A bare administration has no top level; a work tree must be ours.
    if let Some(top) = rev_parse(dir, &["--show-toplevel"]) {
        if top.iter().any(|top| !inside(&root, top)) {
            return Ok(Resolved::Outside);
        }
    }
    if !alternates_inside(&root, common)
        || !admin_symlinks_inside(&root, git_dir)
        || !admin_symlinks_inside(&root, common)
    {
        return Ok(Resolved::Outside);
    }
    Ok(Resolved::Inside)
}

/// Every directory below `root` holding a `.git` entry, resolved as Git
/// resolves it.
///
/// A `.git` directory is not descended; symlinks are never followed while
/// walking. Reads only (Git's `rev-parse` with no optional locks).
///
/// # Errors
/// `GIT_CARRY_POINTER_ATTACHED` with the `.git` entry's relative path when
/// Git would use anything outside `root` there.
pub fn audit_gits(root: &Path) -> Result<Vec<CarriedGit>> {
    let root = fs::canonicalize(root).refuse_at(SITE)?;
    let mut pending = vec![root.clone()];
    let mut found = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut holds = None;
        for entry in fs::read_dir(&directory).refuse_at(SITE)? {
            let entry = entry.refuse_at(SITE)?;
            let kind = entry.file_type().refuse_at(SITE)?;
            if entry.file_name() == ".git" {
                holds = Some((kind, entry.path()));
            } else if kind.is_dir() {
                pending.push(entry.path());
            }
        }
        let Some((kind, path)) = holds else {
            continue;
        };
        let rel_dir = directory
            .strip_prefix(&root)
            .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
            .as_os_str()
            .as_bytes()
            .to_vec();
        let (kind, target) = if kind.is_symlink() {
            (
                GitEntryKind::Symlink,
                fs::read_link(&path)
                    .refuse_at(SITE)?
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            )
        } else if kind.is_file() {
            let bytes = read_bounded(&path, 64 * 1024).unwrap_or_default();
            (
                GitEntryKind::File,
                pointer_value(&bytes).unwrap_or_default().to_vec(),
            )
        } else if kind.is_dir() {
            (GitEntryKind::Directory, Vec::new())
        } else {
            (GitEntryKind::Other, Vec::new())
        };
        let inside = match resolve_git(&root, &directory)? {
            Resolved::Absent => false,
            Resolved::Inside => true,
            Resolved::Outside => {
                return Err(BulkloadRefusal::GitCarryPointerAttached(
                    path.strip_prefix(&root)
                        .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                ))
            }
        };
        found.push(CarriedGit {
            rel_dir,
            kind,
            target,
            inside,
        });
    }
    found.sort_by(|a, b| a.rel_dir.cmp(&b.rel_dir));
    Ok(found)
}

// A gitdir pointer's value as Git reads it: after `gitdir: `, trailing CR
// and LF stripped.
fn pointer_value(bytes: &[u8]) -> Option<&[u8]> {
    let mut value = bytes.strip_prefix(b"gitdir: ")?;
    while let [rest @ .., b'\n' | b'\r'] = value {
        value = rest;
    }
    (!value.is_empty() && !value.contains(&0)).then_some(value)
}
