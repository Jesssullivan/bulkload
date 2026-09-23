//! Explicit reviewed Git batches. Capture reuse still performs a metadata census.
//!
//! Ref custody and usable restored workspaces are separate outcomes. Completed
//! restores are never replayed over subsequent operator edits.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{git_carry, BulkloadRefusal, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub source: PathBuf,
    pub repository: PathBuf,
    pub workspace: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct Plan {
    items: Vec<Item>,
}

#[derive(Serialize, Deserialize)]
struct Capture {
    key: [u8; 32],
    bundle: String,
    digest: [u8; 32],
    identity: crate::freshness::StatIdentity,
}

// Separate sidecars preserve the existing Capture postcard wire layout.
#[derive(Clone, Serialize, Deserialize)]
struct Base {
    bundle: String,
    digest: [u8; 32],
    identity: crate::freshness::StatIdentity,
}

#[derive(Debug)]
pub struct Receipt {
    pub item: String,
    pub source: PathBuf,
    pub outcome: &'static str,
    pub reason: Option<String>,
    /// One line per ref or seat that drifted under the capture this receipt
    /// names. Empty is the ordinary case, and always empty on apply: apply
    /// refuses a drifted capture (`CAPTURE_DRIFTED`). The durable statement is
    /// the `{bundle}.drift` sidecar.
    pub drift: Vec<String>,
    /// Bytes streamed from source file descriptors by this operation. A reuse
    /// hit and an apply read no source bytes; an incremental pass reads
    /// exactly the drifted and racy seats (R25).
    pub bytes_read: u64,
    /// Why a pass that had a retained capture reused none of its blobs, as in
    /// `reuse_unavailable=shallow`. `None` when reuse ran or was not offered.
    pub reuse_unavailable: Option<&'static str>,
    /// One line per foreign nested repository or gitlink the capture names
    /// as custody (R-N73), from [`git_carry::NestedRepository::receipt_line`],
    /// naming the carrying item for a nest planned as its own (R-N114).
    /// Empty for a repository without nests. The durable statement is the
    /// `{bundle}.nested` sidecar.
    pub nested: Vec<String>,
}

/// One item's completed operation and what it carried.
struct Completion {
    outcome: &'static str,
    drift: Vec<String>,
    bytes_read: u64,
    reuse_unavailable: Option<&'static str>,
    /// Receipt lines, one per nest, already naming any carrying item.
    nested: Vec<String>,
}

impl Completion {
    const fn clean(outcome: &'static str) -> Self {
        Self {
            outcome,
            drift: Vec::new(),
            bytes_read: 0,
            reuse_unavailable: None,
            nested: Vec::new(),
        }
    }

    fn naming(self, nested: Vec<String>) -> Self {
        Self { nested, ..self }
    }
}

// What the next pass needs from a capture, beside its record. Same sidecar
// shape as `Base`: the Capture postcard gains no field.
#[derive(Serialize, Deserialize)]
struct Parts {
    // The authority digest of the key parts the capture was taken under. A
    // later pass with the same authority extends a drifted capture: only the
    // ref inventory and the worktree census differ.
    authority: [u8; 32],
    // When the capture's pass began. A seat stamped within one timestamp tick
    // of it is racy and never reused by identity (R-N72).
    started_ns: i128,
}

// A drifted capture deliberately omits the drifted seats' bytes, so its sidecar
// is the record of what it does not hold. Absent means the pass raced nothing.
fn retained_drift(corpus: &Path, bundle: &str) -> Result<git_carry::CaptureDrift> {
    let path = corpus.join(format!("{bundle}.drift"));
    if path.try_exists()? {
        read(&path)
    } else {
        Ok(git_carry::CaptureDrift::default())
    }
}

// Absent for captures retained from before this sidecar existed. With no
// recorded pass start none of their seats can be proved free of racy
// timestamps, so such a capture is never reused whole or blob by blob: the
// next pass re-reads every seat, says so, and records its own start (R-N76).
fn retained_parts(corpus: &Path, bundle: &str) -> Result<Option<Parts>> {
    let path = corpus.join(format!("{bundle}.parts"));
    if path.try_exists()? {
        Ok(Some(read::<Parts>(&path)?))
    } else {
        Ok(None)
    }
}

// The nests a retained capture recorded. Absent means it recorded none: the
// sidecar is written whenever the capture's custody is non-empty.
fn retained_nested(corpus: &Path, bundle: &str) -> Result<Vec<git_carry::NestedRepository>> {
    let path = corpus.join(format!("{bundle}.nested"));
    if path.try_exists()? {
        read(&path)
    } else {
        Ok(Vec::new())
    }
}

/// Plan items by canonical source: which item carries a checkout (R-N114).
type Owners = std::collections::BTreeMap<PathBuf, String>;

fn owners(plan: &Plan) -> Result<Owners> {
    let mut owners = Owners::new();
    for item in &plan.items {
        owners.entry(item.source.clone()).or_insert(id(item)?);
    }
    Ok(owners)
}

// R-N114: the sources of other plan items nested strictly inside this one.
// Those nests carry their own seats; this item's capture carries none of them.
fn planned_nests(item: &Item, owners: &Owners) -> Vec<PathBuf> {
    owners
        .keys()
        .filter(|source| source.starts_with(&item.source) && **source != item.source)
        .cloned()
        .collect()
}

// One receipt line per nest; a nest planned as its own item is named with the
// item that carries it (R-N114).
fn nest_lines(item: &Item, owners: &Owners, nested: &[git_carry::NestedRepository]) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt;
    nested
        .iter()
        .map(|nest| {
            let line = nest.receipt_line();
            let carrier = nest
                .own_item
                .then(|| {
                    owners.get(
                        &item
                            .source
                            .join(std::ffi::OsStr::from_bytes(&nest.rel_path)),
                    )
                })
                .flatten();
            match carrier {
                Some(carrier) => format!("{line} carried-by={carrier}"),
                None => line,
            }
        })
        .collect()
}

/// Read the exact reviewed items without performing capture or apply.
///
/// # Errors
/// Refuses malformed, oversized or symlinked plans.
pub fn inspect(plan: &Path) -> Result<Vec<Item>> {
    Ok(read::<Plan>(plan)?.items)
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)?;
    // All callers hold a plan or phase lock. Retain interrupted publications
    // rather than overwrite their bytes or make them a permanent resume blocker.
    let mut generation = 0u64;
    let (temporary, mut file) = loop {
        let temporary = path.with_extension(format!("pending-{generation}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                generation = generation
                    .checked_add(1)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_all()?;
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            // SAFETY: geteuid has no preconditions or side effects.
            if metadata.is_dir()
                && metadata.mode().trailing_zeros() >= 6
                && metadata.uid() == unsafe { libc::geteuid() }
            {
                Ok(())
            } else {
                Err(BulkloadRefusal::PathEscapesRoot)
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn filename(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

fn id(item: &Item) -> Result<String> {
    let bytes = postcard::to_allocvec(item).map_err(|_| BulkloadRefusal::FrameCodec)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Append one operator-selected source and destination. No inventory guesses.
///
/// # Errors
/// Refuses invalid paths, conflicting workspace targets, locked or malformed plans.
pub fn add(plan: &Path, source: &Path, repository: &Path, workspace: Option<&Path>) -> Result<()> {
    add_batch(
        plan,
        &[Item {
            source: source.to_owned(),
            repository: repository.to_owned(),
            workspace: workspace.map(Path::to_owned),
        }],
    )
}

/// Append a reviewed batch with one plan read/write and linear duplicate checks.
///
/// # Errors
/// Refuses malformed paths or conflicting targets without publishing a partial plan.
pub fn add_batch(plan: &Path, items: &[Item]) -> Result<()> {
    if !plan.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let _lock = exclusive(&plan.with_extension("lock"))?;
    let mut contents: Plan = if plan.try_exists()? {
        read(plan)?
    } else {
        Plan::default()
    };
    let mut identities = std::collections::HashSet::new();
    let mut targets: Vec<(PathBuf, PathBuf)> = Vec::new();
    for previous in &contents.items {
        identities.insert(id(previous)?);
        if let Some(target) = &previous.workspace {
            targets.push((target.clone(), previous.source.clone()));
        }
    }
    for incoming in items {
        let mut item = incoming.clone();
        item.source = fs::canonicalize(&item.source)?;
        if !item.repository.is_absolute()
            || item.workspace.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        if !identities.insert(id(&item)?) {
            continue;
        }
        if let Some(target) = &item.workspace {
            if targets
                .iter()
                .any(|(other, source)| overlapping(target, &item.source, other, source))
            {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            targets.push((target.clone(), item.source.clone()));
        }
        contents.items.push(item);
    }
    write(plan, &contents)
}

// R-N114: two workspace targets collide when they are equal, or when one lies
// inside the other anywhere but at exactly the place the inner item's source
// lies inside the outer item's source (a nested repository planned as its own
// item, restored where it was). Component-wise, never string prefixes.
fn overlapping(target: &Path, source: &Path, other: &Path, other_source: &Path) -> bool {
    if target == other {
        return true;
    }
    let nested = |inner: &Path, inner_source: &Path, outer: &Path, outer_source: &Path| {
        inner
            .strip_prefix(outer)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| outer_source.join(relative) != inner_source)
    };
    nested(target, source, other, other_source)
        .or_else(|| nested(other, other_source, target, source))
        .unwrap_or(false)
}

struct Exclusive(fs::File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit the open file description until
        // exec closes it. Explicit unlock ends our operation's ownership even
        // while such a descriptor exists; closing only this fd is insufficient.
        // SAFETY: this guard owns a live descriptor for the acquired flock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn exclusive(path: &Path) -> Result<Exclusive> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // SAFETY: the owned file descriptor remains open for the lock lifetime.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(Exclusive(file))
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(buffer.get(..count).ok_or(BulkloadRefusal::FrameCodec)?);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn base_path(corpus: &Path, base: &Base) -> Result<PathBuf> {
    if !filename(&base.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(corpus.join(&base.bundle))
}

fn retained_base(corpus: &Path, base: &Base) -> Result<bool> {
    let path = base_path(corpus, base)?;
    Ok(path.try_exists()?
        && base.identity
            == crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(path)?))
}

fn prepare_base(item: &Item, group: &str, state: &Path, corpus: &Path) -> Result<Base> {
    let record = corpus.join(format!("shared-{group}.base"));
    if record.try_exists()? {
        let base: Base = read(&record)?;
        if retained_base(corpus, &base)? {
            return Ok(base);
        }
        // Do not replace a missing or changed prerequisite while older deltas
        // still depend on it. Keep the missing custody visible.
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let mut generation = 0u64;
    let attempt = loop {
        let attempt = state.join(format!("shared-{group}-{generation}"));
        if !attempt.try_exists()? {
            break attempt;
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    };
    let bundle = git_carry::shared::export_base(&item.source, &attempt)?;
    let digest = hash_file(&bundle)?;
    let name = format!(
        "shared-{}.bundle",
        blake3::Hash::from_bytes(digest).to_hex()
    );
    let published = corpus.join(&name);
    if published.try_exists()? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(&bundle, &published)?;
    }
    // Git's successful pack write is not a durability guarantee. Flush the
    // payload before write() publishes and directory-syncs its dependency.
    fs::File::open(&published)?.sync_all()?;
    let base = Base {
        bundle: name,
        digest,
        identity: crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(published)?),
    };
    write(&record, &base)?;
    Ok(base)
}

/// What a retained capture record offers the next pass.
enum Retained {
    /// Same key, no drift, no racy seat: the retained bundle is the capture.
    Hit,
    /// A retained bundle of this checkout whose blobs this pass may reuse:
    /// every seat at an unchanged, non-racy `StatIdentity` costs zero source
    /// bytes (R25). `started_ns` is its recorded pass start, absent for a
    /// capture from before that was recorded. `extends` says the retained
    /// capture drifted and the difference is confined to the ref inventory and
    /// the worktree census, so this pass completes it.
    Extend {
        bundle: PathBuf,
        started_ns: Option<i128>,
        extends: bool,
    },
    /// Nothing retained.
    None,
}

fn retained_capture(
    record: &Path,
    corpus: &Path,
    parts: &git_carry::KeyParts,
    key: [u8; 32],
    authority: [u8; 32],
) -> Result<Retained> {
    if !record.try_exists()? {
        return Ok(Retained::None);
    }
    let previous: Capture = read(record)?;
    if !filename(&previous.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let bundle = corpus.join(&previous.bundle);
    if !bundle.try_exists()?
        || previous.identity
            != crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(&bundle)?)
    {
        return Ok(Retained::None);
    }
    let drift = retained_drift(corpus, &previous.bundle)?;
    let recorded = retained_parts(corpus, &previous.bundle)?;
    // A drifted bundle does not hold the drifted seats' bytes, and its key is
    // a pre-pass key the source may return to. It is never a reuse hit; it is
    // the input the next pass extends (R-N28, R-N72). An equal key vouches for
    // no seat that is racy against the retained pass start, and a record with
    // no recorded start vouches for none: either takes the per-seat path,
    // which re-reads exactly the racy seats (R-N76).
    let settled = recorded
        .as_ref()
        .is_some_and(|recorded| !parts.racy_since(recorded.started_ns, git_carry::pass_start_ns()));
    if previous.key == key && drift.is_empty() && settled {
        if git_carry::shared::requires_base(&bundle)? {
            let bound: Base = read(&corpus.join(format!("{}.base", previous.bundle)))?;
            if !retained_base(corpus, &bound)? {
                return Err(BulkloadRefusal::ReceiptBindingInvalid);
            }
        }
        return Ok(Retained::Hit);
    }
    let extends = !drift.is_empty()
        && recorded
            .as_ref()
            .is_some_and(|recorded| recorded.authority == authority);
    Ok(Retained::Extend {
        bundle,
        started_ns: recorded.map(|recorded| recorded.started_ns),
        extends,
    })
}

// The key a drifted capture records: derived from the pre-pass key under its
// own domain, so no census ever hashes to it and the record can never be a
// reuse hit, whatever happens to its drift sidecar (R-N72).
fn poisoned(key: [u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"bulkload-capture-drifted-v1\0");
    hash.update(&key);
    *hash.finalize().as_bytes()
}

// Without a recorded pass start no retained seat can be proved non-racy.
const fn reuse_offer(
    retained: Option<&Path>,
    started_ns: Option<i128>,
) -> (
    Option<git_carry::RetainedCapture<'_>>,
    Option<git_carry::ReuseUnavailable>,
) {
    match (retained, started_ns) {
        (Some(bundle), Some(started_ns)) => (
            Some(git_carry::RetainedCapture { bundle, started_ns }),
            None,
        ),
        (Some(_), None) => (None, Some(git_carry::ReuseUnavailable::PassStartUnrecorded)),
        (None, _) => (None, None),
    }
}

// Failed private attempts are retained, never silently overwritten.
fn attempt_directory(state: &Path, identity: &str, key: [u8; 32]) -> Result<PathBuf> {
    let mut generation = 0u64;
    loop {
        let candidate = state.join(format!(
            "{identity}-{}-{generation}",
            blake3::Hash::from_bytes(key).to_hex()
        ));
        if !candidate.try_exists()? {
            return Ok(candidate);
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    }
}

fn capture_item(
    item: &Item,
    state: &Path,
    corpus: &Path,
    base: Option<&Base>,
    policy: git_carry::CapturePolicy,
    owners: &Owners,
) -> Result<Completion> {
    let identity = id(item)?;
    let record = corpus.join(format!("{identity}.capture"));
    let planned = planned_nests(item, owners);
    // The opaque key cannot say what moved. Keep its typed parts so the
    // post-capture re-read can separate tolerable drift from Git authority.
    // The parts carry the nested custody, so a reuse hit names exactly the
    // nests the retained capture recorded (R-N73, B4).
    let parts = git_carry::capture_key_parts_with_planned(&item.source, policy, &planned)?;
    let nested = nest_lines(item, owners, parts.nested_repositories());
    let key = parts.digest()?;
    let authority = parts.authority()?;
    let (retained, started_ns, extends) =
        match retained_capture(&record, corpus, &parts, key, authority)? {
            Retained::Hit => {
                return Ok(Completion::clean("capture-reused-after-census").naming(nested));
            }
            Retained::Extend {
                bundle,
                started_ns,
                extends,
            } => (Some(bundle), started_ns, extends),
            Retained::None => (None, None, false),
        };
    let (reuse, unrecorded) = reuse_offer(retained.as_deref(), started_ns);
    // A future-stamped seat blocked the whole-capture reuse above, and will on
    // every pass until the clock passes it: say so (round-3 N5).
    let future = (retained.is_some() && parts.stamped_after(git_carry::pass_start_ns()))
        .then_some(git_carry::ReuseUnavailable::FutureStamp);
    let attempt = attempt_directory(state, &identity, key)?;
    let prerequisite = base.map(|base| base_path(corpus, base)).transpose()?;
    let export = git_carry::export_repository_with_drift(
        &item.source,
        &attempt,
        &git_carry::ExportOptions {
            prerequisite: prerequisite.as_deref(),
            policy,
            reuse,
            planned: &planned,
        },
    )?;
    // The export's own census must name the nests the key did: a nest that
    // moved between the key and the export's snapshot is authority (B4).
    if export.nested_repositories != parts.nested_repositories() {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    // Not an opaque key comparison. Anything outside the ref inventory and the
    // worktree census moving is Git authority changing under the capture and
    // still refuses (R-N30). Within them, the export's own window is not the
    // capture's: a ref can vanish before the export's snapshot and return
    // after its last ref read. The pass is clean only when the pre-pass parts,
    // the export's inventories and the post-pass parts all agree (R-N72).
    //
    // Two drift classes. Export drift moved under the export itself; the
    // bundle carries the in-band capture-drift-v1 marker and every restore
    // and import verb refuses it. Key drift moved only outside the export's
    // window; the bundle is a coherent snapshot of the export's own view and
    // applies. Either way the recorded key is poisoned, so a lost or
    // interleaved sidecar can never make a drifted capture a reuse hit.
    let key_drift = parts.drift_across(
        &export,
        &git_carry::capture_key_parts_with_planned(&item.source, policy, &planned)?,
    )?;
    let mut drift = export.drift;
    drift.merge(key_drift)?;
    let recorded_key = if drift.is_empty() { key } else { poisoned(key) };
    let (name, digest, metadata) = publish_bundle(corpus, &identity, &export.bundle)?;
    if let Some(base) = base {
        // Publish dependency custody before the unchanged completion codec.
        write(&corpus.join(format!("{name}.base")), base)?;
    }
    // Separate sidecars, exactly as the shared-base dependency is: the Capture
    // postcard is positional and gains no field, so every retained record and
    // every live restore journal still decodes. This sidecar can be a superset
    // of the bundle's own capture-drift-v1 ref: it also names what moved
    // between the pre-pass key and the export's snapshot.
    let drift_sidecar = publish_sidecars(
        corpus,
        &name,
        &drift,
        &export.nested_repositories,
        &Parts {
            authority,
            started_ns: export.started_ns,
        },
    )?;
    // A clean pass records its key, as it always did. A drifted pass records
    // a poisoned key no census can hash to, so the record itself, not the
    // sidecar, keeps it from ever being a reuse hit; the next pass re-reads
    // exactly the drifted and racy seats and reuses every other blob.
    write(
        &record,
        &Capture {
            key: recorded_key,
            bundle: name,
            digest,
            identity: crate::freshness::StatIdentity::from_metadata(&metadata),
        },
    )?;
    #[cfg(test)]
    git_carry::mid_pass::fire(
        &fs::canonicalize(&item.source)?,
        git_carry::mid_pass::Stage::RecordWritten,
    );
    if drift.is_empty() && drift_sidecar.try_exists()? {
        // A clean pass can reproduce a drifted pass's bundle byte for byte
        // when the drift lay only before the export's snapshot. Retire the
        // stale record only after the clean completion is durable: a crash in
        // between leaves the capture drifted, which costs one more pass and
        // never a stale reuse.
        fs::remove_file(&drift_sidecar)?;
        fs::File::open(corpus)?.sync_all()?;
    }
    let outcome = if !drift.is_empty() {
        "captured-with-drift"
    } else if extends {
        "capture-extended-from-drift"
    } else {
        "captured"
    };
    Ok(Completion {
        outcome,
        drift: drift.lines(),
        bytes_read: export.bytes_read,
        reuse_unavailable: unrecorded
            .or(future)
            .or(export.reuse_unavailable)
            .map(git_carry::ReuseUnavailable::code),
        nested,
    })
}

// Publish an exported bundle into the corpus under its content name, durable
// before any record names it. Returns the name, its digest and its metadata.
fn publish_bundle(
    corpus: &Path,
    identity: &str,
    bundle: &Path,
) -> Result<(String, [u8; 32], fs::Metadata)> {
    let digest = hash_file(bundle)?;
    let name = format!(
        "{identity}-{}.bundle",
        blake3::Hash::from_bytes(digest).to_hex()
    );
    let published = corpus.join(&name);
    if published.try_exists()? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(bundle, &published)?;
    }
    // Completion may survive a crash only after its bundle bytes are durable.
    fs::File::open(&published)?.sync_all()?;
    let metadata = fs::symlink_metadata(&published)?;
    Ok((name, digest, metadata))
}

// The capture's sidecars beside its bundle, before its record: the drift rows
// (only when it drifted), the nest custody (only when there is any, R-N73)
// and the key parts. Returns the drift sidecar's path, present or not.
fn publish_sidecars(
    corpus: &Path,
    name: &str,
    drift: &git_carry::CaptureDrift,
    nested: &[git_carry::NestedRepository],
    parts: &Parts,
) -> Result<PathBuf> {
    let drift_sidecar = corpus.join(format!("{name}.drift"));
    if !drift.is_empty() {
        write(&drift_sidecar, drift)?;
    }
    if !nested.is_empty() {
        write(&corpus.join(format!("{name}.nested")), &nested)?;
    }
    write(&corpus.join(format!("{name}.parts")), parts)?;
    Ok(drift_sidecar)
}

fn execute(
    plan: &Plan,
    jobs: usize,
    operation: &(impl Fn(&Item) -> Result<Completion> + Sync),
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    if !(1..=2).contains(&jobs) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    let refused = std::sync::atomic::AtomicBool::new(false);
    pool.install(|| {
        plan.items.par_iter().try_for_each(|item| {
            let (done, reason) = match operation(item) {
                Ok(done) => {
                    // The count rides in the layout-safe reason; the rows ride
                    // in the sidecar and the in-process receipt.
                    // Nest custody is counted the same way (R-N73).
                    let counts: Vec<String> = [
                        (!done.drift.is_empty()).then(|| format!("drift={}", done.drift.len())),
                        (!done.nested.is_empty()).then(|| format!("nested={}", done.nested.len())),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    let reason = (!counts.is_empty()).then(|| counts.join(" "));
                    (done, reason)
                }
                Err(error) => {
                    refused.store(true, std::sync::atomic::Ordering::Relaxed);
                    (Completion::clean("refused"), Some(error.to_string()))
                }
            };
            receipt(&Receipt {
                item: id(item)?,
                source: item.source.clone(),
                outcome: done.outcome,
                reason,
                drift: done.drift,
                bytes_read: done.bytes_read,
                reuse_unavailable: done.reuse_unavailable,
                nested: done.nested,
            })
        })
    })?;
    if refused.load(std::sync::atomic::Ordering::Relaxed) {
        Err(BulkloadRefusal::ContractSelfInconsistent)
    } else {
        Ok(())
    }
}

fn emit(state: &Path, row: &Receipt, receipt: &impl Fn(&Receipt) -> Result<()>) -> Result<()> {
    write(
        &state.join(format!("{}.outcome", row.item)),
        &(&row.source, row.outcome, &row.reason),
    )?;
    receipt(row)
}

#[derive(Default)]
struct CaptureGroups {
    items: std::collections::BTreeMap<String, String>,
    bases: std::collections::BTreeMap<String, Mutex<Option<Base>>>,
}

fn capture_groups(plan: &Plan) -> Result<CaptureGroups> {
    let mut candidates = std::collections::BTreeMap::<String, Vec<String>>::new();
    for item in &plan.items {
        // Invalid sources still run through the ordinary per-item refusal path
        // so one bad item cannot suppress receipts for unrelated valid work.
        let Ok(common) = git_carry::common_repository(&item.source) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&common) else {
            continue;
        };
        let bytes = postcard::to_allocvec(&(common, metadata.dev(), metadata.ino()))
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        candidates
            .entry(blake3::hash(&bytes).to_hex().to_string())
            .or_default()
            .push(id(item)?);
    }
    let mut groups = CaptureGroups::default();
    for (group, items) in candidates {
        if items.len() > 1 {
            for item in items {
                groups.items.insert(item, group.clone());
            }
            groups.bases.insert(group, Mutex::new(None));
        }
    }
    Ok(groups)
}

fn group_base(
    item: &Item,
    groups: &CaptureGroups,
    state: &Path,
    corpus: &Path,
) -> Result<Option<Base>> {
    let Some(group) = groups.items.get(&id(item)?) else {
        return Ok(None);
    };
    let mut base = groups
        .bases
        .get(group)
        .ok_or(BulkloadRefusal::GitAuthorityChanged)?
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
    if base.is_none() {
        *base = Some(prepare_base(item, group, state, corpus)?);
    }
    // Drop the base-creation lock before the independent workspace capture.
    Ok(base.clone())
}

/// Capture explicit items with at most two Git pack workers at a time.
///
/// # Errors
/// Refuses insecure state directories, changing sources or failed captures; completed
/// items remain durable and each failed item is reported independently.
pub fn capture(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    capture_with_policy(
        plan,
        state,
        corpus,
        jobs,
        git_carry::CapturePolicy::default(),
        receipt,
    )
}

/// [`capture`] under an explicit capture policy.
///
/// A policy that carries the rebuildable set reproduces the pre-omission
/// capture keys, so retained full-fidelity captures are still reused.
///
/// # Errors
/// Refuses everything [`capture`] refuses.
pub fn capture_with_policy(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    policy: git_carry::CapturePolicy,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    private_directory(state)?;
    private_directory(corpus)?;
    let contents: Plan = read(plan)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let groups = capture_groups(&contents)?;
    let owners = owners(&contents)?;
    execute(
        &contents,
        jobs,
        &|item| {
            let base = group_base(item, &groups, state, corpus)?;
            capture_item(item, state, corpus, base.as_ref(), policy, &owners)
        },
        &|row| emit(state, row, receipt),
    )
}

type ImportedBases = Mutex<std::collections::BTreeSet<(PathBuf, [u8; 32])>>;

// `bundle` is the staged copy of the capture this apply restores.
fn import_base(
    item: &Item,
    captured: &Capture,
    bundle: &Path,
    corpus: &Path,
    source: &str,
    imported: &ImportedBases,
) -> Result<()> {
    if !git_carry::shared::requires_base(bundle)? {
        return Ok(());
    }
    let base: Base = read(&corpus.join(format!("{}.base", captured.bundle)))?;
    let path = base_path(corpus, &base)?;
    // Existing shared repositories are the supported optimization. Creating a
    // standalone destination needs a separate private preseed implementation.
    if !item.repository.try_exists()?
        || item
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace == &item.repository)
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let key = (git_carry::common_repository(&item.repository)?, base.digest);
    if imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .contains(&key)
    {
        return Ok(());
    }
    let staged = git_carry::stage_bundle(&path)?;
    if hash_file(staged.path())? != base.digest || git_carry::shared::requires_base(staged.path())?
    {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    git_carry::import_staged(&item.repository, &staged, source)?;
    imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .insert(key);
    Ok(())
}

fn apply_item(
    item: &Item,
    corpus: &Path,
    state: &Path,
    source: &str,
    imported: &ImportedBases,
    owners: &Owners,
) -> Result<Completion> {
    let identity = id(item)?;
    let captured: Capture = read(&corpus.join(format!("{identity}.capture")))?;
    if !filename(&captured.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    // The nests a capture did not carry ride into every receipt that names
    // its bundle, so an apply never presents them as restored.
    let nested = nest_lines(item, owners, &retained_nested(corpus, &captured.bundle)?);
    // A capture that drifted under its export does not hold the drifted
    // seats' bytes. Its bundle says so in-band, and apply refuses it on that
    // marker, fail-closed, before any base import, journal or destination is
    // touched, whether or not the corpus sidecar survived; the next capture
    // pass extends it clean. Key-only drift leaves a coherent snapshot, which
    // applies. R-N29 (apply proceeds on an occupied destination, recording
    // uncaptured seats) is deferred to W6 git carry v2 (bulkload#48).
    //
    // One private stage is the only copy this apply reads: the marker check,
    // the digest check and the restore all see the same bytes (round-3 N4),
    // and the restore verbs below do not check again.
    let staged = git_carry::stage_bundle(&corpus.join(&captured.bundle))?;
    let journal = state.join(format!(
        "{identity}-{}-{}.done",
        blake3::hash(source.as_bytes()).to_hex(),
        blake3::Hash::from_bytes(captured.digest).to_hex()
    ));
    if journal.try_exists()? {
        let done: String = read(&journal)?;
        let outcome = match done.as_str() {
            "workspace-restored" => "previous-workspace-restoration-not-revalidated",
            "refs-imported" => "previous-ref-custody-not-workspace-parity",
            _ => return Err(BulkloadRefusal::ReceiptBindingInvalid),
        };
        return Ok(Completion::clean(outcome).naming(nested));
    }
    if hash_file(staged.path())? != captured.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    import_base(item, &captured, staged.path(), corpus, source, imported)?;
    let outcome = if let Some(workspace) = &item.workspace {
        if item.repository == *workspace {
            git_carry::restore_staged(&staged, workspace, source, None)?;
        } else {
            git_carry::restore_linked_staged(&staged, &item.repository, workspace, source)?;
        }
        "workspace-restored"
    } else {
        git_carry::import_staged(&item.repository, &staged, source)?;
        "refs-imported"
    };
    write(&journal, &outcome.to_owned())?;
    Ok(Completion::clean(outcome).naming(nested))
}

/// Apply explicit restores only; common Git administration is serialized.
///
/// # Errors
/// Refuses occupied restore targets, invalid bundles or conflicting Git authority.
/// Previously completed work is retained without replaying over operator edits.
pub fn apply(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source: &str,
    jobs: usize,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    private_directory(state)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let contents: Plan = read(plan)?;
    let mut locks = std::collections::BTreeMap::new();
    let mut groups = std::collections::BTreeMap::new();
    let imported = ImportedBases::default();
    for item in &contents.items {
        // A standalone restore's repository is its own workspace: if it is
        // already there, that is a collision the restore refuses by type
        // (R-N114), not Git administration to serialise on.
        let standalone = item.workspace.as_ref() == Some(&item.repository);
        let common = if !standalone && item.repository.try_exists()? {
            git_carry::common_repository(&item.repository)?
        } else {
            item.repository.clone()
        };
        groups.insert(id(item)?, common.clone());
        locks.entry(common).or_insert_with(|| Mutex::new(()));
    }
    let owners = owners(&contents)?;
    let operation = |item: &Item| {
        let common = groups
            .get(&id(item)?)
            .ok_or(BulkloadRefusal::GitAuthorityChanged)?;
        let lock = locks
            .get(common)
            .ok_or(BulkloadRefusal::GitAuthorityChanged)?;
        let _guard = lock
            .lock()
            .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
        apply_item(item, corpus, state, source, &imported, &owners)
    };
    // R-N114: an item whose workspace lies inside another item's workspace
    // restores after it, level by level, so the outer checkout lays down the
    // parent directories and the nested item creates its own directory. Items
    // within a level still run in parallel. Every level runs; any refusal is
    // reported at the end, exactly as within one level.
    let mut levels = std::collections::BTreeMap::<usize, Plan>::new();
    for item in &contents.items {
        let depth = item.workspace.as_ref().map_or(0, |workspace| {
            contents
                .items
                .iter()
                .filter_map(|other| other.workspace.as_ref())
                .filter(|other| workspace.starts_with(other) && workspace != *other)
                .count()
        });
        levels.entry(depth).or_default().items.push(item.clone());
    }
    let mut outcome = Ok(());
    for level in levels.values() {
        if let Err(error) = execute(level, jobs, &operation, &|row| emit(state, row, receipt)) {
            outcome = Err(error);
        }
    }
    outcome
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "user.name=Bulkload test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null"
            ])
            .args(args)
            .output()
            .expect("git command")
            .status
            .success());
    }

    #[test]
    fn shared_capture_keeps_completion_codec_and_requires_retained_base() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-shared-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        let second = root.join("second");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                second.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(source.join("file"), b"first dirty").unwrap();
        fs::write(second.join("file"), b"second dirty").unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let first_target = root.join("first-target");
        let second_target = root.join("second-target");
        let plan = root.join("plan");
        add(&plan, &source, &repository, Some(&first_target)).unwrap();
        add(&plan, &second, &repository, Some(&second_target)).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        // Whole-capture reuse needs seats older than one timestamp tick (R-N76).
        settle();
        capture(&plan, &state, &corpus, 2, &|_| Ok(())).unwrap();
        let outcomes = Mutex::new(Vec::new());
        capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec!["capture-reused-after-census"; 2]
        );
        let items = inspect(&plan).unwrap();
        let mut dependencies = Vec::new();
        for item in &items {
            // Decode through the unchanged completion struct used by existing
            // independent standalone captures and their live restore journals.
            let record: Capture =
                read(&corpus.join(format!("{}.capture", id(item).unwrap()))).unwrap();
            let base: Base = read(&corpus.join(format!("{}.base", record.bundle))).unwrap();
            dependencies.push(base.bundle);
        }
        assert_eq!(dependencies.first(), dependencies.last());
        let base = corpus.join(dependencies.first().unwrap());
        let held = root.join("held-base");
        fs::rename(&base, &held).unwrap();
        assert!(capture(&plan, &state, &corpus, 2, &|_| Ok(())).is_err());
        let applied = root.join("applied");
        assert!(apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).is_err());
        assert!(!first_target.exists() && !second_target.exists());
        fs::rename(&held, &base).unwrap();
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(fs::read(first_target.join("file")).unwrap(), b"first dirty");
        assert_eq!(
            fs::read(second_target.join("file")).unwrap(),
            b"second dirty"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_operation_unlocks_even_with_an_inherited_description() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-lock-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("estate.lock");
        let owner = exclusive(&path).unwrap();
        let inherited = owner.0.try_clone().unwrap();
        assert!(exclusive(&path).is_err());
        drop(owner);
        let next = exclusive(&path).expect("completed owner explicitly unlocked");
        drop(inherited);
        assert!(exclusive(&path).is_err());
        drop(next);
        assert!(exclusive(&path).is_ok());
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn explicit_batches_reuse_capture_preserve_edits_and_refuse_public_corpus() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-test-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("owned root");
        let source = root.join("source");
        fs::create_dir(&source).expect("source");
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").expect("base");
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        fs::write(source.join("file"), b"dirty").expect("dirty");
        let target = root.join("destination");
        let plan = root.join("plan");
        let interrupted = plan.with_extension("pending-0");
        fs::write(&interrupted, b"retained interrupted plan").expect("simulate interrupted write");
        add(&plan, &source, &target, Some(&target)).expect("plan");
        assert_eq!(
            fs::read(&interrupted).expect("retained"),
            b"retained interrupted plan"
        );
        add(&plan, &source, &target, Some(&target)).expect("idempotent plan");
        assert_eq!(read::<Plan>(&plan).expect("read plan").items.len(), 1);
        let state = root.join("state");
        let corpus = root.join("corpus");
        // Whole-capture reuse needs seats older than one timestamp tick (R-N76).
        settle();
        capture(&plan, &state, &corpus, 2, &|_| Ok(())).expect("capture");
        let outcomes = Mutex::new(Vec::new());
        capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().expect("lock").push(row.outcome);
            Ok(())
        })
        .expect("reuse");
        assert_eq!(
            *outcomes.lock().expect("lock"),
            vec!["capture-reused-after-census"]
        );
        let applied = root.join("applied");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).expect("restore");
        assert_eq!(
            fs::read(target.join("file")).expect("dirty restored"),
            b"dirty"
        );
        fs::write(target.join("file"), b"operator edited").expect("active write");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).expect("do not replay");
        assert_eq!(
            fs::read(target.join("file")).expect("preserved"),
            b"operator edited"
        );
        let invalid = root.join("not-a-repository");
        fs::create_dir(&invalid).expect("invalid source");
        add(&plan, &invalid, &target, None).expect("explicit failing item");
        outcomes.lock().expect("lock").clear();
        assert!(capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().expect("lock").push(row.outcome);
            Ok(())
        })
        .is_err());
        let mut partial = outcomes.lock().expect("lock").clone();
        partial.sort_unstable();
        assert_eq!(partial, vec!["capture-reused-after-census", "refused"]);
        fs::set_permissions(&corpus, fs::Permissions::from_mode(0o755)).expect("public corpus");
        assert!(capture(&plan, &state, &corpus, 2, &|_| Ok(())).is_err());
        assert!(!filename("/"));
        assert!(!filename("../escape"));
        fs::remove_dir_all(&root).expect("remove owned fixture");
    }

    // ---- drift tolerance (R25, bulkload #34; R-N28/R-N30/R-N72; R-N29 deferred to bulkload#48) ----

    fn drifting_plan(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("tcfs-estate-drift-{name}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("tracked"), b"tracked bytes at census").unwrap();
        git(&source, &["add", "tracked"]);
        git(&source, &["commit", "-m", "base"]);
        fs::write(source.join("big"), vec![b'b'; 65_536]).unwrap();
        fs::write(source.join("small"), b"small untracked").unwrap();
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let corpus = root.join("corpus");
        (root, source, target, plan, corpus)
    }

    type Row = (&'static str, Option<String>, Vec<String>, u64);

    fn receipts(plan: &Path, state: &Path, corpus: &Path) -> Result<Vec<Row>> {
        let rows = Mutex::new(Vec::new());
        capture(plan, state, corpus, 2, &|row| {
            rows.lock().unwrap().push((
                row.outcome,
                row.reason.clone(),
                row.drift.clone(),
                row.bytes_read,
            ));
            Ok(())
        })?;
        Ok(rows.into_inner().unwrap())
    }

    // Arm the export-level hook so one estate capture races a rewrite of a
    // tracked seat, a new seat, and a branch created in the shared ref store.
    fn arm_drift(source: &Path) {
        let inside = fs::canonicalize(source).unwrap();
        git_carry::mid_pass::arm(source, move || {
            fs::write(inside.join("tracked"), b"rewritten mid-pass").unwrap();
            fs::write(inside.join("appeared"), b"new seat").unwrap();
            git(&inside, &["update-ref", "refs/heads/lane-a", "HEAD"]);
        });
    }

    #[test]
    fn a_drifted_capture_reports_captured_with_drift_and_exits_zero() {
        let (root, source, _, plan, corpus) = drifting_plan("exit-zero");
        let state = root.join("state");
        arm_drift(&source);
        let rows = receipts(&plan, &state, &corpus).expect("Ok(()): drift is not a refusal");
        assert_eq!(rows.len(), 1);
        let (outcome, reason, drift, bytes_read) = rows.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift");
        assert_eq!(reason.as_deref(), Some("drift=3"));
        assert_eq!(
            *drift,
            vec![
                "RefAdded \"refs/heads/lane-a\"".to_owned(),
                "SeatAdded \"appeared\"".to_owned(),
                "SeatChanged \"tracked\"".to_owned(),
            ]
        );
        // The skipped seat cost nothing; everything else was read once.
        assert_eq!(*bytes_read, 65_536 + b"small untracked".len() as u64);
        // The durable outcome still decodes as the existing tuple, unchanged.
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let durable: (PathBuf, String, Option<String>) =
            read(&state.join(format!("{item}.outcome"))).unwrap();
        assert_eq!(durable.0, fs::canonicalize(&source).unwrap());
        assert_eq!(durable.1, "captured-with-drift");
        assert_eq!(durable.2.as_deref(), Some("drift=3"));
        // The Capture record is the unchanged codec; the rows ride beside it.
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let sidecar: git_carry::CaptureDrift =
            read(&corpus.join(format!("{}.drift", record.bundle))).unwrap();
        assert_eq!(sidecar.len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 3: a drifted capture does not hold the drifted
    // seats' bytes, so apply refuses it, fail-closed, before touching the
    // destination. R-N29 (apply proceeds on an occupied destination, recording
    // uncaptured seats) is deferred to W6 git carry v2 (bulkload#48).
    #[test]
    fn a_drifted_capture_refuses_to_apply_until_a_later_pass_extends_it() {
        let (root, source, target, plan, corpus) = drifting_plan("applies");
        let state = root.join("state");
        arm_drift(&source);
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "captured-with-drift");
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        assert!(corpus.join(format!("{}.drift", record.bundle)).is_file());
        let applied = root.join("applied");
        let refused = Mutex::new(Vec::new());
        let result = apply(&plan, &corpus, &applied, "neo", 2, &|row| {
            refused
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone()));
            Ok(())
        });
        assert!(result.is_err(), "a drifted capture must never apply");
        assert_eq!(
            *refused.lock().unwrap(),
            vec![("refused", Some("CAPTURE_DRIFTED".to_owned()))]
        );
        assert!(!target.exists(), "the destination is never touched");
        // The next pass extends the capture clean, and only that capture applies.
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "capture-extended-from-drift");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(
            fs::read(target.join("tracked")).unwrap(),
            b"rewritten mid-pass"
        );
        assert_eq!(fs::read(target.join("appeared")).unwrap(), b"new seat");
        fs::remove_dir_all(root).unwrap();
    }

    fn rev_parse(path: &Path, name: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--verify", name])
            .output()
            .expect("git rev-parse");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    // R-N72 (TIN-4540) finding 1: a ref deleted between the pre-pass key and
    // the export's own snapshot is invisible to the export. Recording the
    // pre-pass key as a clean reuse key would let the branch's re-creation at
    // the same commit Hit a bundle that holds neither the ref nor its commit.
    #[test]
    fn a_ref_deleted_before_the_export_snapshot_never_reuses_a_stale_key() {
        let (root, source, _, plan, corpus) = drifting_plan("stale-key");
        let state = root.join("state");
        git(&source, &["checkout", "-q", "-b", "side"]);
        fs::write(source.join("side-only"), b"unique").unwrap();
        git(&source, &["add", "side-only"]);
        git(&source, &["commit", "-q", "-m", "side"]);
        let side = rev_parse(&source, "side");
        git(&source, &["checkout", "-q", "-"]);
        let inside = fs::canonicalize(&source).unwrap();
        git_carry::mid_pass::arm_at(&source, git_carry::mid_pass::Stage::Snapshot, move || {
            git(&inside, &["update-ref", "-d", "refs/heads/side"]);
        });
        let first = receipts(&plan, &state, &corpus).unwrap();
        // The branch returns at the same commit: the source is again exactly
        // the state the pre-pass key described.
        git(&source, &["update-ref", "refs/heads/side", &side]);
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_ne!(
            second.first().unwrap().0,
            "capture-reused-after-census",
            "the retained bundle lacks refs/heads/side and its commit"
        );
        let (outcome, reason, drift, _) = first.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift");
        assert_eq!(reason.as_deref(), Some("drift=1"));
        assert_eq!(*drift, vec!["RefRemoved \"refs/heads/side\"".to_owned()]);
        assert_eq!(second.first().unwrap().0, "capture-extended-from-drift");
        // The capture the second pass recorded holds the ref and its commit.
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let heads = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["bundle", "list-heads"])
            .arg(corpus.join(&record.bundle))
            .output()
            .unwrap();
        let heads = String::from_utf8(heads.stdout).unwrap();
        assert!(heads
            .lines()
            .any(|line| line.starts_with(&side) && line.ends_with("refs/heads/side")));
        let third = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(third.first().unwrap().0, "capture-reused-after-census");
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 4: `capture-extended-from-drift` names only a
    // pass that follows a drifted capture. A clean capture followed by an
    // ordinary change between passes is an ordinary capture.
    #[test]
    fn a_change_after_a_clean_capture_is_not_labelled_extended_from_drift() {
        let (root, source, _, plan, corpus) = drifting_plan("clean-change");
        let state = root.join("state");
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured");
        fs::write(source.join("later"), b"a seat added between passes").unwrap();
        let second = receipts(&plan, &state, &corpus).unwrap();
        let (outcome, reason, drift, _) = second.first().unwrap();
        assert_eq!(*outcome, "captured");
        assert_eq!(*reason, None);
        assert!(drift.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    // The guard for the journals under /srv/fast-local/jess/state/git-carry/neo/
    // and every retained bundle in the corpus: a record whose key was computed
    // by the pre-change hashing, with no sidecars, still decodes and its key is
    // bit-identical. With no recorded pass start it vouches for no seat, so it
    // is re-captured once, never reused whole (R-N76); after that it Hits.
    #[test]
    fn retained_captures_from_before_this_change_keep_their_key_and_recapture_once() {
        let (root, source, _, plan, corpus) = drifting_plan("retained");
        let state = root.join("state");
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "captured");
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record_path = corpus.join(format!("{item}.capture"));
        let record: Capture = read(&record_path).unwrap();
        let legacy = git_carry::legacy::reusable_capture_key_with_policy(
            &source,
            git_carry::CapturePolicy::default(),
        )
        .unwrap();
        assert_eq!(record.key, legacy, "KeyParts::digest must be bit-identical");
        // Rewrite the record exactly as the pre-change agent would have written
        // it, and remove the sidecars it never wrote.
        fs::remove_file(corpus.join(format!("{}.parts", record.bundle))).unwrap();
        fs::remove_file(&record_path).unwrap();
        write(
            &record_path,
            &Capture {
                key: legacy,
                bundle: record.bundle.clone(),
                digest: record.digest,
                identity: record.identity,
            },
        )
        .unwrap();
        settle();
        assert_eq!(
            reuse_signals(&plan, &state, &corpus),
            vec![("captured", Some("pass-start-unrecorded"))]
        );
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "capture-reused-after-census");
        assert_eq!(rows.first().unwrap().3, 0);
        fs::remove_dir_all(root).unwrap();
    }

    fn reuse_signals(
        plan: &Path,
        state: &Path,
        corpus: &Path,
    ) -> Vec<(&'static str, Option<&'static str>)> {
        let rows = Mutex::new(Vec::new());
        capture(plan, state, corpus, 2, &|row| {
            rows.lock()
                .unwrap()
                .push((row.outcome, row.reuse_unavailable));
            Ok(())
        })
        .unwrap();
        rows.into_inner().unwrap()
    }

    // Seats written by a fixture are racy for any pass that starts within one
    // timestamp tick of them. A test proving identity reuse waits that out.
    fn settle() {
        std::thread::sleep(std::time::Duration::from_nanos(
            u64::try_from(git_carry::RACY_GRANULARITY_NS).unwrap() + 100_000_000,
        ));
    }

    fn drift_sidecar(plan: &Path, corpus: &Path) -> PathBuf {
        let item = id(inspect(plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        corpus.join(format!("{}.drift", record.bundle))
    }

    fn recorded_heads(plan: &Path, corpus: &Path, source: &Path) -> String {
        let item = id(inspect(plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let heads = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["bundle", "list-heads"])
            .arg(corpus.join(&record.bundle))
            .output()
            .unwrap();
        String::from_utf8(heads.stdout).unwrap()
    }

    fn update_ref_at(repo: &Path, stage: git_carry::mid_pass::Stage, args: &[&str]) {
        let inside = fs::canonicalize(repo).unwrap();
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        git_carry::mid_pass::arm_at(repo, stage, move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git(&inside, &args);
        });
    }

    // R-N72 re-review finding 1: a ref deleted before the export's snapshot
    // and re-created after its last ref read leaves the pre- and post-pass
    // keys equal, yet the bundle lacks the ref. Only comparing the keyed
    // inventories with the export's own catches the round trip.
    #[test]
    fn a_ref_deleted_and_recreated_across_both_unguarded_windows_is_drift() {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan("aba");
        let state = root.join("state");
        git(&source, &["branch", "side"]);
        let tip = rev_parse(&source, "side");
        settle();
        update_ref_at(
            &source,
            Stage::Snapshot,
            &["update-ref", "-d", "refs/heads/side"],
        );
        update_ref_at(
            &source,
            Stage::AfterPass,
            &["update-ref", "refs/heads/side", &tip],
        );
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rev_parse(&source, "side"), tip, "the source is back at A");
        let (outcome, _, drift, _) = first.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift");
        assert!(drift.contains(&"RefRemoved \"refs/heads/side\"".to_owned()));
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_ne!(second.first().unwrap().0, "capture-reused-after-census");
        assert!(recorded_heads(&plan, &corpus, &source)
            .lines()
            .any(|line| line.starts_with(&tip) && line.ends_with("refs/heads/side")));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 re-review finding 2: the sidecar is not the guard. A capture that
    // drifted under its export carries the in-band capture-drift-v1 marker,
    // and apply refuses on the marker even when the sidecar is gone.
    #[test]
    fn deleting_the_drift_sidecar_never_lets_an_export_drifted_capture_apply() {
        let (root, source, target, plan, corpus) = drifting_plan("sidecar-apply");
        let state = root.join("state");
        arm_drift(&source);
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured-with-drift");
        fs::remove_file(drift_sidecar(&plan, &corpus)).unwrap();
        let refused = Mutex::new(Vec::new());
        let result = apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|row| {
            refused
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone()));
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            *refused.lock().unwrap(),
            vec![("refused", Some("CAPTURE_DRIFTED".to_owned()))]
        );
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 re-review finding 2: a drifted capture records a poisoned key, so
    // losing its sidecar can never turn it into a whole-capture reuse. Both
    // drift classes: in-band export drift, and key-only drift the export
    // never saw.
    #[test]
    fn deleting_the_drift_sidecar_never_makes_a_drifted_capture_a_hit() {
        use git_carry::mid_pass::Stage;
        for class in ["export", "key"] {
            let (root, source, _, plan, corpus) = drifting_plan(&format!("sidecar-hit-{class}"));
            let state = root.join("state");
            let head = rev_parse(&source, "HEAD");
            if class == "export" {
                update_ref_at(
                    &source,
                    Stage::BytePass,
                    &["update-ref", "refs/heads/lane", &head],
                );
            } else {
                git(&source, &["branch", "gone"]);
                update_ref_at(
                    &source,
                    Stage::Snapshot,
                    &["update-ref", "-d", "refs/heads/gone"],
                );
            }
            settle();
            let first = receipts(&plan, &state, &corpus).unwrap();
            assert_eq!(first.first().unwrap().0, "captured-with-drift", "{class}");
            // The source returns exactly to the state the pre-pass key names.
            if class == "export" {
                git(&source, &["update-ref", "-d", "refs/heads/lane"]);
            } else {
                git(&source, &["update-ref", "refs/heads/gone", &head]);
            }
            fs::remove_file(drift_sidecar(&plan, &corpus)).unwrap();
            settle();
            let second = receipts(&plan, &state, &corpus).unwrap();
            assert_ne!(
                second.first().unwrap().0,
                "capture-reused-after-census",
                "{class}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    // R-N72 re-review finding 2: key-only drift (the export saw nothing move)
    // leaves the bundle a coherent snapshot of the export's own view. It is
    // never reused whole, but it applies.
    #[test]
    fn a_key_only_drifted_capture_is_a_coherent_snapshot_and_applies() {
        use git_carry::mid_pass::Stage;
        let (root, source, target, plan, corpus) = drifting_plan("key-only-applies");
        let state = root.join("state");
        git(&source, &["branch", "gone"]);
        update_ref_at(
            &source,
            Stage::Snapshot,
            &["update-ref", "-d", "refs/heads/gone"],
        );
        let first = receipts(&plan, &state, &corpus).unwrap();
        let (outcome, _, drift, _) = first.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift");
        assert_eq!(*drift, vec!["RefRemoved \"refs/heads/gone\"".to_owned()]);
        apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(fs::read(target.join("small")).unwrap(), b"small untracked");
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 re-review finding 2: two state directories sharing one corpus are
    // not serialized by the per-state-dir lock. Interleave them so a drifted
    // record from one lands between another's clean record and its stale
    // drift-record retirement; the corpus must still never reuse a bundle
    // that lacks a keyed ref.
    #[test]
    fn two_state_dirs_sharing_a_corpus_fail_closed_on_interleaved_records() {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan("two-states");
        let (state_a, state_b) = (root.join("state-a"), root.join("state-b"));
        let head = rev_parse(&source, "HEAD");
        settle();
        let (inside, plan_a, corpus_a, state_a_hook) = (
            fs::canonicalize(&source).unwrap(),
            plan.clone(),
            corpus.clone(),
            state_a.clone(),
        );
        let tip = head.clone();
        // B captures the checkout without `gone`, cleanly. After B's record is
        // written, A captures from the state its key names (with `gone`), but
        // `gone` vanishes before A's export snapshot: A's bundle is B's bundle
        // byte for byte, and A's drift is key-only.
        git_carry::mid_pass::arm_at(&source, Stage::RecordWritten, move || {
            git(&inside, &["update-ref", "refs/heads/gone", &tip]);
            update_ref_at(
                &inside,
                Stage::Snapshot,
                &["update-ref", "-d", "refs/heads/gone"],
            );
            settle();
            let rows = receipts(&plan_a, &state_a_hook, &corpus_a).unwrap();
            assert_eq!(rows.first().unwrap().0, "captured-with-drift");
        });
        let b = receipts(&plan, &state_b, &corpus).unwrap();
        assert_eq!(b.first().unwrap().0, "captured");
        // The source returns to the state A's pre-pass key names.
        git(&source, &["update-ref", "refs/heads/gone", &head]);
        settle();
        let next = receipts(&plan, &state_a, &corpus).unwrap();
        assert_ne!(next.first().unwrap().0, "capture-reused-after-census");
        assert!(recorded_heads(&plan, &corpus, &source)
            .lines()
            .any(|line| line.ends_with("refs/heads/gone")));
        fs::remove_dir_all(root).unwrap();
    }

    // ---- round-3 adversarial tests (review of 72fc71e), ported ----

    fn r3_git_out(path: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "user.name=Bulkload test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    // Unpack the recorded bundle's refs/carry-export/* into a scratch bare repo.
    fn r3_recorded(plan: &Path, corpus: &Path, root: &Path, tag: &str) -> PathBuf {
        let item = id(inspect(plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let scratch = root.join(format!("scratch-{tag}"));
        r3_git_out(root, &["init", "-q", "--bare", scratch.to_str().unwrap()]);
        r3_git_out(
            &scratch,
            &[
                "fetch",
                "-q",
                corpus.join(&record.bundle).to_str().unwrap(),
                "+refs/carry-export/*:refs/b/*",
            ],
        );
        scratch
    }

    // Every receipt of one capture, refused rows included.
    fn attempt(plan: &Path, state: &Path, corpus: &Path) -> Vec<(&'static str, Option<String>)> {
        let rows = Mutex::new(Vec::new());
        let _ = capture(plan, state, corpus, 2, &|row| {
            rows.lock().unwrap().push((row.outcome, row.reason.clone()));
            Ok(())
        });
        rows.into_inner().unwrap()
    }

    type Hook = Box<dyn FnOnce() + Send>;

    // R-N72 round-3 N1: Git authority A->B->A across the two windows no export
    // comparison brackets (pre-pass key -> export snapshot, export end ->
    // post-pass key). The pre- and post-pass keys are equal. The capture must
    // refuse, and must never leave a whole-capture Hit on a bundle carrying B.
    fn r3_authority_round_trip(class: &str) {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan(&format!("r3-auth-{class}"));
        let state = root.join("state");
        let a = rev_parse(&source, "HEAD");
        let inside = fs::canonicalize(&source).unwrap();
        let (to_b, to_a): (Hook, Hook) = match class {
            "detached-head" => {
                git(&source, &["checkout", "-q", "--detach"]);
                #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax.
                let tree = rev_parse(&source, "HEAD^{tree}");
                let b = r3_git_out(&source, &["commit-tree", &tree, "-p", &a, "-m", "b"])
                    .trim()
                    .to_owned();
                let (i1, i2, a2) = (inside.clone(), inside, a.clone());
                (
                    Box::new(move || git(&i1, &["update-ref", "--no-deref", "HEAD", &b])),
                    Box::new(move || git(&i2, &["update-ref", "--no-deref", "HEAD", &a2])),
                )
            }
            "symbolic-head" => {
                let main = r3_git_out(&source, &["symbolic-ref", "HEAD"])
                    .trim()
                    .to_owned();
                git(&source, &["branch", "twin"]);
                let (i1, i2) = (inside.clone(), inside);
                (
                    Box::new(move || git(&i1, &["symbolic-ref", "HEAD", "refs/heads/twin"])),
                    Box::new(move || git(&i2, &["symbolic-ref", "HEAD", &main])),
                )
            }
            "exclude" => {
                let path = inside.join(".git/info/exclude");
                fs::create_dir_all(inside.join(".git/info")).unwrap();
                fs::write(&path, b"# A\n").unwrap();
                let p2 = path.clone();
                (
                    Box::new(move || fs::write(&path, b"# B: never on disk at a key\n").unwrap()),
                    Box::new(move || fs::write(&p2, b"# A\n").unwrap()),
                )
            }
            "config" => {
                let path = inside.join(".git/config");
                let original = fs::read(&path).unwrap();
                let mut changed = original.clone();
                changed.extend_from_slice(b"[user]\n\tname = B never at a key\n");
                let p2 = path.clone();
                (
                    Box::new(move || fs::write(&path, changed).unwrap()),
                    Box::new(move || fs::write(&p2, original).unwrap()),
                )
            }
            _ => {
                let path = inside.join(".git/index");
                let original = fs::read(&path).unwrap();
                let i1 = inside;
                let p2 = path;
                (
                    Box::new(move || git(&i1, &["update-index", "--chmod=+x", "tracked"])),
                    Box::new(move || fs::write(&p2, original).unwrap()),
                )
            }
        };
        settle();
        git_carry::mid_pass::arm_at(&source, Stage::Snapshot, to_b);
        git_carry::mid_pass::arm_at(&source, Stage::AfterPass, to_a);
        let first = attempt(&plan, &state, &corpus);
        settle();
        let second = attempt(&plan, &state, &corpus);
        let hit = second.first().unwrap().0 == "capture-reused-after-census";
        if hit {
            let scratch = r3_recorded(&plan, &corpus, &root, class);
            let carried = match class {
                "detached-head" => r3_git_out(&scratch, &["rev-parse", "refs/b/head"]).trim() == a,
                "symbolic-head" => {
                    !r3_git_out(&scratch, &["show", "refs/b/head-symbolic:value"]).contains("twin")
                }
                "exclude" => r3_git_out(&scratch, &["show", "refs/b/exclude:value"]) == "# A\n",
                "config" => !r3_git_out(&scratch, &["show", "refs/b/configuration-v1:value"])
                    .contains("never at a key"),
                #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax.
                _ => r3_git_out(&scratch, &["ls-tree", "refs/b/staged^{tree}"])
                    .contains("100644 blob"),
            };
            assert!(carried, "{class}: a whole-capture Hit on a stale bundle");
        }
        assert_eq!(
            first,
            vec![("refused", Some("GIT_AUTHORITY_CHANGED".to_owned()))],
            "{class}: authority moved inside the capture window"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r3_detached_head_round_trip_across_unguarded_windows_refuses() {
        r3_authority_round_trip("detached-head");
    }

    #[test]
    fn r3_symbolic_head_round_trip_across_unguarded_windows_refuses() {
        r3_authority_round_trip("symbolic-head");
    }

    #[test]
    fn r3_exclude_round_trip_across_unguarded_windows_refuses() {
        r3_authority_round_trip("exclude");
    }

    #[test]
    fn r3_config_round_trip_across_unguarded_windows_refuses() {
        r3_authority_round_trip("config");
    }

    #[test]
    fn r3_index_round_trip_across_unguarded_windows_refuses() {
        r3_authority_round_trip("index");
    }

    // N1, stash: refs/stash keeps its value throughout; only the reflog below
    // it is rewritten and restored, so stash S1 is missing from the bundle.
    #[test]
    fn r3_stash_reflog_round_trip_across_unguarded_windows_refuses() {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan("r3-stash");
        let state = root.join("state");
        fs::write(source.join("tracked"), b"stash one").unwrap();
        git(&source, &["stash", "push", "-q"]);
        let s1 = rev_parse(&source, "refs/stash");
        fs::write(source.join("tracked"), b"stash two").unwrap();
        git(&source, &["stash", "push", "-q"]);
        let s2 = rev_parse(&source, "refs/stash");
        let inside = fs::canonicalize(&source).unwrap();
        let (i1, i2) = (inside.clone(), inside);
        let (s1c, s2c) = (s1.clone(), s2.clone());
        settle();
        git_carry::mid_pass::arm_at(&source, Stage::Snapshot, move || {
            git(&i1, &["stash", "drop", "-q", "stash@{1}"]);
        });
        git_carry::mid_pass::arm_at(&source, Stage::AfterPass, move || {
            git(&i2, &["stash", "clear"]);
            git(&i2, &["stash", "store", "-m", "one", &s1c]);
            git(&i2, &["stash", "store", "-m", "two", &s2c]);
        });
        let first = attempt(&plan, &state, &corpus);
        assert_eq!(
            r3_git_out(&source, &["reflog", "show", "--format=%H", "refs/stash"]),
            format!("{s2}\n{s1}\n"),
            "the source is back at A"
        );
        settle();
        let second = attempt(&plan, &state, &corpus);
        let heads = recorded_heads(&plan, &corpus, &source);
        assert!(
            second.first().unwrap().0 != "capture-reused-after-census"
                || heads.contains(&format!("stashes/{s1}")),
            "a whole-capture Hit on a bundle that lost stash S1"
        );
        assert_eq!(
            first,
            vec![("refused", Some("GIT_AUTHORITY_CHANGED".to_owned()))]
        );
        assert!(heads.contains(&format!("stashes/{s1}")));
        fs::remove_dir_all(root).unwrap();
    }

    // Packed-refs rewrite under the pass: not drift, and the next pass Hits.
    #[test]
    fn r3_packed_refs_rewrite_is_not_drift() {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan("r3-packrefs");
        let state = root.join("state");
        git(&source, &["branch", "side"]);
        settle();
        let inside = fs::canonicalize(&source).unwrap();
        git_carry::mid_pass::arm_at(&source, Stage::BytePass, move || {
            git(&inside, &["pack-refs", "--all", "--prune"]);
        });
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured");
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "capture-reused-after-census");
        fs::remove_dir_all(root).unwrap();
    }

    // Reflog-only change on an ordinary branch between passes: not custody.
    #[test]
    fn r3_branch_reflog_only_change_keeps_the_hit() {
        let (root, source, _, plan, corpus) = drifting_plan("r3-reflog");
        let state = root.join("state");
        settle();
        receipts(&plan, &state, &corpus).unwrap();
        git(&source, &["reflog", "expire", "--expire=now", "--all"]);
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "capture-reused-after-census");
        fs::remove_dir_all(root).unwrap();
    }

    // Sidecar deleted AND record key forged to the source's current key. The
    // record is not authenticated (a documented known limit), so the forged
    // record is a whole-capture Hit; the bundle still refuses in-band on
    // apply. Ref-only export drift, so no racy seat blocks the Hit (the
    // reviewer's variant forged the pre-drift key, which the drifted source
    // no longer matches).
    #[test]
    fn r3_sidecar_deleted_and_key_forged_still_refuses_in_band() {
        use git_carry::mid_pass::Stage;
        let (root, source, target, plan, corpus) = drifting_plan("r3-forge");
        let state = root.join("state");
        let head = rev_parse(&source, "HEAD");
        update_ref_at(
            &source,
            Stage::BytePass,
            &["update-ref", "refs/heads/lane", &head],
        );
        settle();
        assert_eq!(
            receipts(&plan, &state, &corpus).unwrap().first().unwrap().0,
            "captured-with-drift"
        );
        fs::remove_file(drift_sidecar(&plan, &corpus)).unwrap();
        let key = git_carry::reusable_capture_key_with_policy(
            &source,
            git_carry::CapturePolicy::default(),
        )
        .unwrap();
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record_path = corpus.join(format!("{item}.capture"));
        let record: Capture = read(&record_path).unwrap();
        write(&record_path, &Capture { key, ..record }).unwrap();
        settle();
        assert_eq!(
            receipts(&plan, &state, &corpus).unwrap().first().unwrap().0,
            "capture-reused-after-census",
            "the forged record Hits: records are not authenticated"
        );
        let refused = Mutex::new(Vec::new());
        let result = apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|row| {
            refused.lock().unwrap().push(row.reason.clone());
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            *refused.lock().unwrap(),
            vec![Some("CAPTURE_DRIFTED".to_owned())]
        );
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 round-3 N5: a future-dated seat blocks the whole-capture Hit on
    // every pass, and the receipt says so instead of re-reading silently.
    #[test]
    fn a_future_stamped_seat_reports_reuse_unavailable_future_stamp() {
        let (root, source, _, plan, corpus) = drifting_plan("future-stamp");
        let state = root.join("state");
        let future = std::time::SystemTime::now() + std::time::Duration::from_hours(24);
        fs::File::options()
            .write(true)
            .open(source.join("small"))
            .unwrap()
            .set_modified(future)
            .unwrap();
        settle();
        receipts(&plan, &state, &corpus).unwrap();
        settle();
        assert_eq!(
            reuse_signals(&plan, &state, &corpus),
            vec![("captured", Some("future-stamp"))]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // R-N76 (TIN-4540): a whole-capture reuse is a stat-identity reuse of
    // every seat at once, so it gets Git's racy check too. A same-size
    // rewrite inside the capture's timestamp tick leaves the key unchanged on
    // a coarse-timestamp filesystem; the record is forged to that unchanged
    // key here, because a nanosecond filesystem always moves ctime.
    #[test]
    fn a_same_size_rewrite_in_the_capture_tick_never_reuses_the_whole_capture() {
        let (root, source, target, plan, corpus) = drifting_plan("racy-hit");
        let state = root.join("state");
        assert_eq!(
            receipts(&plan, &state, &corpus).unwrap().first().unwrap().0,
            "captured"
        );
        fs::write(source.join("small"), b"SMALL UNTRACKED").unwrap();
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record_path = corpus.join(format!("{item}.capture"));
        let record: Capture = read(&record_path).unwrap();
        write(
            &record_path,
            &Capture {
                key: git_carry::reusable_capture_key_with_policy(
                    &source,
                    git_carry::CapturePolicy::default(),
                )
                .unwrap(),
                ..record
            },
        )
        .unwrap();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_ne!(
            second.first().unwrap().0,
            "capture-reused-after-census",
            "a racy capture is never reused whole"
        );
        assert!(
            second.first().unwrap().3 > 0,
            "the racy seats were read again"
        );
        apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(fs::read(target.join("small")).unwrap(), b"SMALL UNTRACKED");
        fs::remove_dir_all(root).unwrap();
    }

    // R-N76 (TIN-4540): the racy check never costs a clean repository its
    // reuse. Seats older than one timestamp tick before the pass still Hit.
    #[test]
    fn a_capture_whose_seats_predate_the_racy_window_is_still_reused_whole() {
        let (root, _, _, plan, corpus) = drifting_plan("settled-hit");
        let state = root.join("state");
        settle();
        assert_eq!(
            receipts(&plan, &state, &corpus).unwrap().first().unwrap().0,
            "captured"
        );
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "capture-reused-after-census");
        assert_eq!(second.first().unwrap().3, 0);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 4: a shallow checkout's retained capture offers
    // no blobs to reuse, and the receipt says so instead of re-reading silently.
    #[test]
    fn a_shallow_item_reports_reuse_unavailable_on_its_receipt() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-shallow-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let full = root.join("full");
        fs::create_dir(&full).unwrap();
        git(&full, &["init", "--template="]);
        for bytes in ["first", "second"] {
            fs::write(full.join("tracked"), bytes).unwrap();
            git(&full, &["add", "tracked"]);
            git(&full, &["commit", "-q", "-m", bytes]);
        }
        let source = root.join("source");
        git(
            &root,
            &[
                "clone",
                "-q",
                "--depth=1",
                "--no-local",
                &format!("file://{}", full.display()),
                source.to_str().unwrap(),
            ],
        );
        git(
            &source,
            &["config", "remote.origin.url", "https://example.test/x.git"],
        );
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let (state, corpus) = (root.join("state"), root.join("corpus"));
        assert_eq!(
            reuse_signals(&plan, &state, &corpus),
            vec![("captured", None)]
        );
        fs::write(source.join("later"), b"a seat added between passes").unwrap();
        assert_eq!(
            reuse_signals(&plan, &state, &corpus),
            vec![("captured", Some("shallow"))]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // A clean pass can reproduce a drifted pass's bundle byte for byte when the
    // drift lay only before the export's snapshot. The stale drift record must
    // not outlive it, or the capture could never be reused or applied again.
    #[test]
    fn a_clean_pass_after_pre_snapshot_drift_retires_the_drift_record() {
        let (root, source, target, plan, corpus) = drifting_plan("retire");
        let state = root.join("state");
        git(&source, &["branch", "gone"]);
        let inside = fs::canonicalize(&source).unwrap();
        git_carry::mid_pass::arm_at(&source, git_carry::mid_pass::Stage::Snapshot, move || {
            git(&inside, &["update-ref", "-d", "refs/heads/gone"]);
        });
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured-with-drift");
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "capture-extended-from-drift");
        let third = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(third.first().unwrap().0, "capture-reused-after-census");
        apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(fs::read(target.join("small")).unwrap(), b"small untracked");
        fs::remove_dir_all(root).unwrap();
    }

    // The R25 proof: pass 2 reads exactly the drifted seats, not the corpus.
    #[test]
    fn a_second_pass_after_drift_rereads_only_the_drifted_seats() {
        let (root, source, target, plan, corpus) = drifting_plan("rereads");
        let state = root.join("state");
        // The fixture's seats must predate pass 1 by more than one timestamp
        // tick, or they are racy and pass 2 rightly reads them again.
        settle();
        arm_drift(&source);
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured-with-drift");
        // Pass 2 re-reads the seats pass 1 raced; they must settle before it
        // for pass 3 to reuse the whole capture (R-N76).
        settle();
        let second = receipts(&plan, &state, &corpus).unwrap();
        let (outcome, reason, drift, bytes_read) = second.first().unwrap();
        assert_eq!(*outcome, "capture-extended-from-drift");
        assert_eq!(*reason, None);
        assert!(drift.is_empty());
        assert_eq!(
            *bytes_read,
            (b"rewritten mid-pass".len() + b"new seat".len()) as u64,
            "pass 2 must read the drifted seats and nothing else"
        );
        let third = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(third.first().unwrap().0, "capture-reused-after-census");
        assert_eq!(third.first().unwrap().3, 0);
        // The extended bundle is complete: reused blobs and re-read seats alike.
        apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(
            fs::read(target.join("tracked")).unwrap(),
            b"rewritten mid-pass"
        );
        assert_eq!(fs::read(target.join("appeared")).unwrap(), b"new seat");
        assert_eq!(fs::read(target.join("big")).unwrap(), vec![b'b'; 65_536]);
        assert_eq!(fs::read(target.join("small")).unwrap(), b"small untracked");
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B3: every estate receipt names every nest. A clean nest two
    // commits ahead of its remote-tracking ref is custody; the capture, the
    // reuse hit and the apply each name it with unpushed=2, the count rides
    // in the layout-safe reason, and the rows ride in `{bundle}.nested`.
    #[test]
    fn every_estate_receipt_names_every_nest_with_its_unpushed_count() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-nested-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        let nest = source.join("vendor/inner");
        fs::create_dir_all(&nest).unwrap();
        git(&nest, &["init", "--template="]);
        fs::write(nest.join("lib.c"), b"v1").unwrap();
        git(&nest, &["add", "lib.c"]);
        git(&nest, &["commit", "-m", "v1"]);
        git(
            &nest,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/inner.git",
            ],
        );
        git(&nest, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for step in ["v2", "v3"] {
            fs::write(nest.join("lib.c"), step).unwrap();
            git(&nest, &["commit", "-am", step]);
        }
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        let rows = Mutex::new(Vec::new());
        let record = |row: &Receipt| {
            rows.lock()
                .unwrap()
                .push((row.outcome, row.reason.clone(), row.nested.clone()));
            Ok(())
        };
        // Wait out the racy window before the reuse pass (R-N76), as #52's
        // own reuse tests do: seats stamped within 2 s of a pass never make a
        // whole-capture hit.
        settle();
        capture(&plan, &state, &corpus, 1, &record).unwrap();
        settle();
        capture(&plan, &state, &corpus, 1, &record).unwrap();
        apply(&plan, &corpus, &root.join("applied"), "neo", 1, &record).unwrap();
        let rows = rows.into_inner().unwrap();
        let outcomes: Vec<_> = rows.iter().map(|row| row.0).collect();
        assert_eq!(
            outcomes,
            vec![
                "captured",
                "capture-reused-after-census",
                "workspace-restored"
            ]
        );
        for (_, reason, nested) in &rows {
            assert_eq!(reason.as_deref(), Some("nested=1"));
            assert_eq!(nested.len(), 1);
            let line = nested.first().unwrap();
            assert!(line.starts_with("nested-repository path=\"vendor/inner\" "));
            assert!(line.contains(" unpushed=2 remotes=yes"));
        }
        // The durable sidecar beside the bundle; the Capture codec is unchanged.
        let items = inspect(&plan).unwrap();
        let item = items.first().unwrap();
        let captured: Capture =
            read(&corpus.join(format!("{}.capture", id(item).unwrap()))).unwrap();
        let sidecar: Vec<git_carry::NestedRepository> =
            read(&corpus.join(format!("{}.nested", captured.bundle))).unwrap();
        assert_eq!(
            sidecar.iter().map(|nest| nest.unpushed).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(!target.join("vendor/inner").exists());
        assert_eq!(fs::read(target.join("file")).unwrap(), b"base");

        // A dirty nest is refused by name of refusal, never silently dropped.
        fs::write(nest.join("lib.c"), b"unsaved edit").unwrap();
        let refused = Mutex::new(Vec::new());
        assert!(capture(&plan, &state, &corpus, 1, &|row| {
            refused
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone()));
            Ok(())
        })
        .is_err());
        assert_eq!(
            refused.into_inner().unwrap(),
            vec![(
                "refused",
                Some(BulkloadRefusal::GitInventoryMalformed.to_string())
            )]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // B4: a nest's HEAD moving between the capture's pre-pass key and the
    // export, or after the export's last read, refuses the item with
    // GIT_AUTHORITY_CHANGED; it is never recorded as drift.
    #[test]
    fn a_nest_head_moving_around_the_export_refuses_the_capture() {
        for stage in [
            git_carry::mid_pass::Stage::Snapshot,
            git_carry::mid_pass::Stage::AfterPass,
        ] {
            let root = std::env::temp_dir().join(format!(
                "tcfs-estate-nest-moves-{stage:?}-{}",
                std::process::id()
            ));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let source = root.join("source");
            fs::create_dir(&source).unwrap();
            git(&source, &["init", "--template="]);
            fs::write(source.join("file"), b"base").unwrap();
            git(&source, &["add", "file"]);
            git(&source, &["commit", "-m", "base"]);
            let nest = source.join("vendor/inner");
            fs::create_dir_all(&nest).unwrap();
            git(&nest, &["init", "--template="]);
            fs::write(nest.join("lib.c"), b"v1").unwrap();
            git(&nest, &["add", "lib.c"]);
            git(&nest, &["commit", "-m", "v1"]);
            let plan = root.join("plan");
            add(&plan, &source, &root.join("destination"), None).unwrap();
            let inner = fs::canonicalize(&nest).unwrap();
            git_carry::mid_pass::arm_at(&source, stage, move || {
                fs::write(inner.join("lib.c"), b"v2").unwrap();
                git(&inner, &["commit", "-am", "v2"]);
            });
            let rows = Mutex::new(Vec::new());
            let result = capture(
                &plan,
                &root.join("state"),
                &root.join("corpus"),
                1,
                &|row| {
                    rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                    Ok(())
                },
            );
            fs::remove_dir_all(&root).unwrap();
            assert!(result.is_err(), "{stage:?}");
            assert_eq!(
                rows.into_inner().unwrap(),
                vec![(
                    "refused",
                    Some(BulkloadRefusal::GitAuthorityChanged.to_string())
                )],
                "{stage:?}"
            );
        }
    }

    // A capture of a repository with no nest writes no `.nested` sidecar and
    // its receipt reason stays None, exactly as before.
    #[test]
    fn a_nest_free_capture_has_no_nested_sidecar_or_reason() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-no-nest-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        let plan = root.join("plan");
        add(&plan, &source, &root.join("destination"), None).unwrap();
        let corpus = root.join("corpus");
        let rows = Mutex::new(Vec::new());
        capture(&plan, &root.join("state"), &corpus, 1, &|row| {
            rows.lock()
                .unwrap()
                .push((row.reason.clone(), row.nested.clone()));
            Ok(())
        })
        .unwrap();
        assert_eq!(rows.into_inner().unwrap(), vec![(None, Vec::new())]);
        assert!(!fs::read_dir(&corpus)
            .unwrap()
            .any(|entry| entry.unwrap().path().extension() == Some("nested".as_ref())));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N114: a populated submodule planned as its own item. The outer names
    // it as carried by that item and creates no directory for it; the item
    // restores it exactly once; the restored outer is clean.
    #[test]
    fn a_populated_submodule_planned_as_its_own_item_restores_once() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-own-sub-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let upstream = root.join("upstream");
        fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "--template=", "-b", "main"]);
        fs::write(upstream.join("lib.c"), b"v1").unwrap();
        git(&upstream, &["add", "lib.c"]);
        git(&upstream, &["commit", "-m", "v1"]);
        let outer = root.join("outer");
        fs::create_dir(&outer).unwrap();
        git(&outer, &["init", "--template=", "-b", "main"]);
        fs::write(outer.join("file"), b"outer").unwrap();
        git(&outer, &["add", "file"]);
        git(&outer, &["commit", "-m", "outer"]);
        git(
            &outer,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                upstream.to_str().unwrap(),
                "sub",
            ],
        );
        git(&outer, &["commit", "-m", "sub"]);
        let sub = outer.join("sub");
        git(
            &sub,
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/sub.git",
            ],
        );
        let target = root.join("target");
        let plan = root.join("plan");
        add(&plan, &outer, &target, Some(&target)).unwrap();
        add(&plan, &sub, &target.join("sub"), Some(&target.join("sub"))).unwrap();
        let items = inspect(&plan).unwrap();
        let sub_id = id(items
            .iter()
            .find(|item| item.source.ends_with("sub"))
            .unwrap())
        .unwrap();
        let rows = Mutex::new(Vec::new());
        let record = |row: &Receipt| {
            rows.lock()
                .unwrap()
                .push((row.outcome, row.reason.clone(), row.nested.clone()));
            Ok(())
        };
        capture(&plan, &root.join("state"), &root.join("corpus"), 1, &record).unwrap();
        apply(
            &plan,
            &root.join("corpus"),
            &root.join("applied"),
            "neo",
            1,
            &record,
        )
        .unwrap();
        let rows = rows.into_inner().unwrap();
        assert!(rows.iter().all(|row| row.0 != "refused"), "{rows:?}");
        assert!(rows.iter().any(|row| row
            .2
            .iter()
            .any(|line| line.ends_with(&format!(" seats=own-item carried-by={sub_id}")))));
        assert!(target.join("sub/.git").exists());
        assert_eq!(fs::read(target.join("sub/lib.c")).unwrap(), b"v1");
        let status = Command::new("git")
            .arg("-C")
            .arg(&target)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(
            status.stdout.is_empty(),
            "{:?}",
            String::from_utf8_lossy(&status.stdout)
        );
        fs::remove_dir_all(root).unwrap();
    }

    // R-N114: overlapping workspace targets are refused unless the inner item
    // is the outer item's nested source restored at the same relative place;
    // a restore onto an existing destination is a typed collision, never an
    // errno.
    #[test]
    fn overlapping_targets_and_occupied_destinations_refuse_by_type() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-overlap-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let outer = root.join("outer");
        fs::create_dir(&outer).unwrap();
        git(&outer, &["init", "--template="]);
        fs::write(outer.join("file"), b"outer").unwrap();
        git(&outer, &["add", "file"]);
        git(&outer, &["commit", "-m", "outer"]);
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        git(&other, &["init", "--template="]);
        fs::write(other.join("file"), b"other").unwrap();
        git(&other, &["add", "file"]);
        git(&other, &["commit", "-m", "other"]);
        let target = root.join("target");
        let plan = root.join("plan");
        add(&plan, &outer, &target, Some(&target)).unwrap();
        // Another repository restored inside the outer's target: a collision.
        let inside = target.join("vendor/other");
        assert_eq!(
            add(&plan, &other, &inside, Some(&inside)),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        // The outer restored inside another item's target: a collision too.
        let around = root.join("around");
        let plan2 = root.join("plan2");
        add(&plan2, &other, &around.join("x"), Some(&around.join("x"))).unwrap();
        assert_eq!(
            add(&plan2, &outer, &around, Some(&around)),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        // A restore onto an existing directory is typed.
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        fs::create_dir(&target).unwrap();
        let refused = Mutex::new(Vec::new());
        assert!(
            apply(&plan, &corpus, &root.join("applied"), "neo", 1, &|row| {
                refused.lock().unwrap().push(row.reason.clone());
                Ok(())
            })
            .is_err()
        );
        assert_eq!(
            refused.into_inner().unwrap(),
            vec![Some(BulkloadRefusal::GitDestinationOccupied.to_string())]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
