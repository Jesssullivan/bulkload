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
//! 3. The landed tree's `.git` entries are audited (#261). A `.git`
//!    directory is a repository's own administration, carried as itself. A
//!    `.git` pointer file or symlink is resolved where it landed: one that
//!    names nothing on this host is inert and recorded with its target; one
//!    that resolves inside the carried tree reattaches to the carried copy
//!    and is recorded too; one that resolves to an existing directory
//!    outside it would let Git act on another repository through the
//!    carried copy. That refuses `GIT_CARRY_POINTER_ATTACHED` with its
//!    path, recorded as the item's outcome. The landed bytes are left as
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

/// The outcome name a carried item's record holds.
pub const CARRIED_OUTCOME: &str = "carried-as-files";

/// The site a carry's own refusal is recorded at.
const SITE: &str = "estate::carry_files";

/// The current carry record format.
const RECORD_VERSION: u8 = 1;

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

/// A `.git` entry below a carried tree that is not a directory, and what
/// it resolved to where it landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedPointer {
    /// Path of the `.git` entry relative to the destination, raw bytes.
    pub rel_path: Vec<u8>,
    /// The pointer's target as written (a file's `gitdir:` value or a
    /// symlink's target), raw bytes.
    pub target: Vec<u8>,
    /// Whether it resolves inside the carried tree (the carried copy's own
    /// administration); false when it names nothing on this host.
    pub internal: bool,
}

/// The durable carry record (`{item}-{blake3(SOURCE)}.carried`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedRecord {
    pub version: u8,
    pub item: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Landed beside the planned workspace rather than at it.
    pub beside: bool,
    /// The carry made the destination directory.
    pub created: bool,
    /// The transfer's destination state.
    pub transfer_state: PathBuf,
    /// The transfer session's row identity.
    pub transfer_item: String,
    /// blake3 of the transfer record's persisted bytes at the session's end.
    pub transfer_digest: [u8; 32],
    /// Every `.git` pointer or symlink below the destination.
    pub pointers: Vec<CarriedPointer>,
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
    pub pointers: u64,
}

/// Read and prove an item's carry record. Reads only.
///
/// The record is the last state directory's. It must decode and name this
/// item, and the transfer record it names must exist, decode, have the
/// bytes the carry recorded, be a completed session into the recorded
/// destination, and hold no refused path. `None` when no state directory
/// holds a carry record.
#[must_use]
pub fn prove(
    states: &[PathBuf],
    identity: &str,
    source_label: &str,
) -> Option<std::result::Result<CarriedProof, &'static str>> {
    let path = states
        .iter()
        .rev()
        .map(|state| carried_path(state, identity, source_label))
        .find(|path| path.is_file())?;
    Some(prove_record(&path, identity))
}

fn prove_record(path: &Path, identity: &str) -> std::result::Result<CarriedProof, &'static str> {
    let record: CarriedRecord = read(path).map_err(|_| "carried-record-unreadable")?;
    if record.version != RECORD_VERSION || record.item != identity {
        return Err("carried-record-mismatch");
    }
    let bytes = fs::read(transfer_outcome::record_path(
        &record.transfer_state,
        &record.transfer_item,
    ))
    .map_err(|_| "carried-transfer-missing")?;
    if *blake3::hash(&bytes).as_bytes() != record.transfer_digest {
        return Err("carried-transfer-changed");
    }
    let transfer = transfer_outcome::decode(&bytes).map_err(|_| "carried-transfer-unreadable")?;
    if transfer.session != Session::Completed
        || transfer.session_item() != record.transfer_item
        || transfer.destination != record.destination
    {
        return Err("carried-transfer-mismatch");
    }
    if transfer.entries.iter().any(|entry| {
        matches!(
            entry.outcome,
            transfer_outcome::EntryOutcome::Refused { .. }
        )
    }) {
        return Err("carried-transfer-refused-paths");
    }
    Ok(CarriedProof {
        via: record.via,
        source: record.source,
        destination: record.destination,
        beside: record.beside,
        transfer_state: record.transfer_state,
        pointers: record.pointers.len() as u64,
    })
}

/// Carry every eligible item, one line per item to `line`.
///
/// # Errors
/// Refuses an insecure state directory, a held estate lock, a malformed
/// plan or an invalid suffix. Per-item refusals are reported and recorded
/// and the verb ends `CONTRACT_SELF_INCONSISTENT` when any item was not
/// carried.
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
    let _lock = if options.dry_run {
        None
    } else {
        private_directory(state)?;
        private_directory(carry_state)?;
        Some(exclusive(&state.join("estate.lock"))?)
    };
    let mut failed = false;
    let mut seen = std::collections::BTreeSet::new();
    let mut refs_only = Vec::new();
    // The workspace items each source's eligible workspace items were
    // planned to, for a refs-only item of the same source.
    let mut by_source = std::collections::BTreeMap::<PathBuf, Vec<String>>::new();
    for item in &contents.items {
        let identity = id(item)?;
        if !seen.insert(identity.clone())
            || (!options.items.is_empty() && !options.items.contains(&identity))
        {
            continue;
        }
        let prefix = format!("item={identity} source={:?}", item.source);
        match eligibility(item, &identity, corpus, state)? {
            Eligibility::Eligible | Eligibility::Carried => {}
            Eligibility::Skip(why) => {
                line(&format!("{prefix} skipped={why}"))?;
                continue;
            }
        }
        let Some(workspace) = &item.workspace else {
            // Decided after every workspace item: it rides on one.
            refs_only.push((item, identity, prefix));
            continue;
        };
        by_source
            .entry(item.source.clone())
            .or_default()
            .push(identity.clone());
        let destination = options.beside.as_ref().map_or_else(
            || workspace.clone(),
            |suffix| {
                let mut name = workspace.as_os_str().to_owned();
                name.push(suffix);
                PathBuf::from(name)
            },
        );
        let bound = options.bind.get(&identity);
        let job = CarryJob {
            item: identity.clone(),
            source: item.source.clone(),
            destination,
            destination_state: bound
                .cloned()
                .unwrap_or_else(|| carry_state.join(&identity).join("dest")),
        };
        if options.dry_run {
            line(&format!(
                "{prefix} {}",
                dry_run(&job, transport, bound.is_some())
            ))?;
            continue;
        }
        let run = (item, identity.as_str(), state, source_label);
        match carry_one(&job, run, transport, options, bound.is_some()) {
            Ok((true, summary)) => line(&format!("{prefix} {summary}"))?,
            Ok((false, summary)) => {
                failed = true;
                line(&format!("{prefix} {summary}"))?;
            }
            Err(refusal) => {
                failed = true;
                line(&format!(
                    "{prefix} destination={:?} carried=refused reason={refusal}",
                    job.destination
                ))?;
            }
        }
    }
    for (item, identity, prefix) in refs_only {
        let siblings = by_source.get(&item.source).cloned().unwrap_or_default();
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
        match carry_via(item, &identity, state, source_label, &siblings) {
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

// A refs-only item whose capture refused: its custody is the source's
// `.git`, which a carried workspace item of the same source holds. Its
// carry record names that item (`via`) and binds the same transfer record;
// without one, `CAPTURE_ABSENT` stands (`CLOSURE_UNACCOUNTED` is printed).
fn carry_via(
    item: &Item,
    identity: &str,
    state: &Path,
    source_label: &str,
    siblings: &[String],
) -> Result<String> {
    let states = [state.to_path_buf()];
    for sibling in siblings {
        if !matches!(prove(&states, sibling, source_label), Some(Ok(_))) {
            continue;
        }
        let mut record: CarriedRecord = read(&carried_path(state, sibling, source_label))?;
        identity.clone_into(&mut record.item);
        record.source.clone_from(&item.source);
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
    options: &CarryOptions,
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
    if !bound {
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
        )?;
    }
    let session_item =
        TransferRecord::running(transport.verb(), &source_key, &destination_key).session_item();
    let record_bytes = match fs::read(transfer_outcome::record_path(
        &job.destination_state,
        &session_item,
    )) {
        Ok(bytes) => bytes,
        Err(_) if bound => return Err(BulkloadRefusal::ClosureUnaccounted),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let transfer =
        transfer_outcome::decode(&record_bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let refused = refused_entries(&transfer);
    if transfer.session != Session::Completed {
        return Err(BulkloadRefusal::ClosureUnaccounted);
    }
    if let Some(first) = refused.first() {
        // The transfer's own rows name every refused path; the item is not
        // carried and its outcome stays as it was.
        return Ok((
            false,
            format!(
                "destination={:?} carried=incomplete refused_paths={} first_refused=\"{}\" first_code={}",
                job.destination,
                refused.len(),
                first.rel_path.escape_ascii(),
                match &first.outcome {
                    transfer_outcome::EntryOutcome::Refused { refusal, .. } => refusal.code(),
                    transfer_outcome::EntryOutcome::SidecarCovered => "",
                }
            ),
        ));
    }
    // 3. The pointer audit, read-only.
    let pointers = match audit_pointers(&job.destination) {
        Ok(pointers) => pointers,
        Err(refusal @ BulkloadRefusal::GitCarryPointerAttached(_)) => {
            record_outcome(state, identity, item, Err(&refusal))?;
            return Err(refusal);
        }
        Err(other) => return Err(other),
    };
    // 4. The carry record, then 5. the outcome.
    let record = CarriedRecord {
        version: RECORD_VERSION,
        item: identity.to_owned(),
        source: item.source.clone(),
        destination: destination_key,
        beside: options.beside.is_some(),
        created,
        transfer_state: job.destination_state.clone(),
        transfer_item: session_item,
        transfer_digest: *blake3::hash(&record_bytes).as_bytes(),
        pointers,
        via: None,
    };
    write(&carried_path(state, identity, source_label), &record)?;
    fault_point!(EstateCarryRecorded);
    record_outcome(state, identity, item, Ok(&record))?;
    Ok((
        true,
        format!(
            "destination={:?} outcome={CARRIED_OUTCOME} bound={bound} beside={} created={} completed={} reused={} pointers={}",
            record.destination,
            record.beside,
            record.created,
            transfer.completed,
            transfer.reused,
            record.pointers.len(),
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

/// Every `.git` entry below `root` that is not a directory, resolved.
///
/// Each is resolved where it landed. A `.git` directory is not descended
/// (a repository's own administration). Reads only; never follows a
/// symlink while walking.
///
/// # Errors
/// `GIT_CARRY_POINTER_ATTACHED` with the entry's relative path when one
/// resolves to an existing directory outside `root`.
pub fn audit_pointers(root: &Path) -> Result<Vec<CarriedPointer>> {
    let root = fs::canonicalize(root).refuse_at(SITE)?;
    let mut pending = vec![root.clone()];
    let mut found = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).refuse_at(SITE)? {
            let entry = entry.refuse_at(SITE)?;
            let kind = entry.file_type().refuse_at(SITE)?;
            let path = entry.path();
            if entry.file_name() != ".git" {
                if kind.is_dir() {
                    pending.push(path);
                }
                continue;
            }
            if kind.is_dir() {
                continue;
            }
            let rel_path = path
                .strip_prefix(&root)
                .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
                .as_os_str()
                .as_bytes()
                .to_vec();
            let target = if kind.is_symlink() {
                fs::read_link(&path)
                    .refuse_at(SITE)?
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else if kind.is_file() {
                let bytes = fs::read(&path).refuse_at(SITE)?;
                pointer_value(&bytes).unwrap_or_default().to_vec()
            } else {
                Vec::new()
            };
            let resolved = (!target.is_empty())
                .then(|| directory.join(std::ffi::OsStr::from_bytes(&target)))
                .and_then(|candidate| fs::canonicalize(candidate).ok())
                .filter(|resolved| resolved.is_dir());
            let internal = match resolved {
                None => false,
                Some(resolved) if resolved.starts_with(&root) => true,
                Some(_) => return Err(BulkloadRefusal::GitCarryPointerAttached(rel_path)),
            };
            found.push(CarriedPointer {
                rel_path,
                target,
                internal,
            });
        }
    }
    found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
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
