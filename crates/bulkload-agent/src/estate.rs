//! Explicit reviewed Git batches. Capture reuse still performs a metadata census.
//!
//! Ref custody and usable restored workspaces are separate outcomes. Completed
//! restores are never replayed over subsequent operator edits.

use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
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

// WP2 (OI-1003-Q15): the `{bundle}.prior` sidecar of a capture whose bundle
// declares its predecessor's source-held tips as prerequisites. It names that
// predecessor exactly as `Base` names a shared base, plus the predecessor's
// own chain depth: 0 for a self-contained bundle, else one more than the
// depth its own `.prior` records. A sidecar, so the Capture postcard gains no
// field and every retained record still decodes.
#[derive(Clone, Serialize, Deserialize)]
struct Prior {
    bundle: String,
    digest: [u8; 32],
    identity: crate::freshness::StatIdentity,
    depth: u32,
}

fn prior_sidecar(corpus: &Path, bundle: &str) -> PathBuf {
    corpus.join(format!("{bundle}.prior"))
}

/// How [`chain_links`] binds each link it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkBinding {
    /// Capture side (reuse, chaining, sidecar publication) on the capture
    /// host: every link must also sit at the `StatIdentity` its `.prior`
    /// recorded, so a link replaced in place is never extended or reused.
    Custody,
    /// Apply side (restore, space preflight): a link is bound by its corpus
    /// name and recorded digest only. A corpus pulled to another host gives
    /// every file a new device, inode and ctime, so identity cannot bind
    /// there; [`git_carry::chain::flatten`] digest-checks every staged link,
    /// as `import_base` digest-checks a shared base (R-N72).
    Digest,
}

/// The bundles a chained capture's bundle depends on, oldest first, each with
/// its recorded digest. Empty for a bundle without a `.prior` sidecar.
///
/// Every link must be retained (and, under [`LinkBinding::Custody`], at its
/// recorded identity), and depths must fall by exactly one per link to a
/// depth-0 root within [`git_carry::chain::CHAIN_DEPTH_LIMIT`]; the walk is
/// therefore bounded. Digests and prerequisites are checked when the chain is
/// flattened.
///
/// # Errors
/// `SEALED_OBJECT_MISSING` for a link the corpus no longer holds,
/// `RECEIPT_BINDING_INVALID` for inconsistent depths or (under `Custody`) a
/// replaced link, and `PATH_ESCAPES_ROOT` for a link name that is not a
/// corpus file name.
fn chain_links(
    corpus: &Path,
    bundle: &str,
    binding: LinkBinding,
) -> Result<Vec<(PathBuf, [u8; 32])>> {
    let mut links = Vec::new();
    let mut current = bundle.to_owned();
    let mut expected: Option<u32> = None;
    loop {
        let sidecar = prior_sidecar(corpus, &current);
        if !sidecar.try_exists().refuse_at("estate::chain_links")? {
            // `expected` is the depth `current`'s own link must record: a
            // chained bundle that lost its link breaks the chain. Only an
            // unchained head ends the walk here; a root ends it below.
            return match expected {
                None => Ok(links),
                Some(_) => Err(BulkloadRefusal::ReceiptBindingInvalid),
            };
        }
        let prior: Prior = read(&sidecar)?;
        if prior.depth >= git_carry::chain::CHAIN_DEPTH_LIMIT
            || expected.is_some_and(|depth| depth != prior.depth)
        {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        let path = link_path(corpus, &prior)?;
        if !path.try_exists().refuse_at("estate::chain_links")? {
            return Err(BulkloadRefusal::SealedObjectMissing);
        }
        if binding == LinkBinding::Custody
            && prior.identity
                != crate::freshness::StatIdentity::from_metadata(
                    &fs::symlink_metadata(&path).refuse_at("estate::chain_links")?,
                )
        {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        links.push((path, prior.digest));
        if prior.depth == 0 {
            if prior_sidecar(corpus, &prior.bundle)
                .try_exists()
                .refuse_at("estate::chain_links")?
            {
                return Err(BulkloadRefusal::ReceiptBindingInvalid);
            }
            links.reverse();
            return Ok(links);
        }
        expected = Some(prior.depth - 1);
        current = prior.bundle;
    }
}

/// The link a new capture may chain onto `previous`, if any: `None` when
/// `previous` depends on a shared plan base, its own chain is not intact, or
/// it already sits at the depth limit (the next capture re-bases).
fn chainable(corpus: &Path, previous: &Capture, bundle: &Path) -> Result<Option<Prior>> {
    let depth = if prior_sidecar(corpus, &previous.bundle)
        .try_exists()
        .refuse_at("estate::chainable")?
    {
        match chain_links(corpus, &previous.bundle, LinkBinding::Custody) {
            Ok(links) => u32::try_from(links.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
            // A broken chain is never extended; the next bundle re-bases.
            Err(_) => return Ok(None),
        }
    } else if git_carry::shared::requires_base(bundle)? {
        return Ok(None);
    } else {
        0
    };
    Ok(
        (depth < git_carry::chain::CHAIN_DEPTH_LIMIT).then(|| Prior {
            bundle: previous.bundle.clone(),
            digest: previous.digest,
            identity: previous.identity,
            depth,
        }),
    )
}

#[derive(Debug)]
pub struct Receipt {
    pub item: String,
    pub source: PathBuf,
    pub outcome: &'static str,
    pub reason: Option<String>,
    /// The refusal of a `refused` receipt (WP3 PR 3): its outcome record is
    /// typed from this value, not from `reason`.
    pub refusal: Option<BulkloadRefusal>,
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

    // WP1 PR 4 (S5): the source's object store was rewritten under the pass
    // and a Git child failed on it. Drift custody, never a refusal: no record
    // is written, so any retained capture stays exactly what it was, and the
    // next pass captures the rewritten store. The receipt names no bytes: the
    // pass's reads are in the process counters, not in a capture.
    fn deferred(drift: &git_carry::CaptureDrift, nested: Vec<String>) -> Self {
        Self {
            drift: drift.lines(),
            nested,
            ..Self::clean("deferred-with-drift")
        }
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
    if path.try_exists().refuse_at("estate::retained_drift")? {
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
    if path.try_exists().refuse_at("estate::retained_parts")? {
        Ok(Some(read::<Parts>(&path)?))
    } else {
        Ok(None)
    }
}

// The nests a retained capture recorded. Absent means it recorded none: the
// sidecar is written whenever the capture's custody is non-empty.
fn retained_nested(corpus: &Path, bundle: &str) -> Result<Vec<git_carry::NestedRepository>> {
    let path = corpus.join(format!("{bundle}.nested"));
    if path.try_exists().refuse_at("estate::retained_nested")? {
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

// R-N114, round 4 N5: a planned carrier whose own capture refused in this
// pass carries none of its seats. Refuse by name rather than record the nest
// as carried by it, reuse hit or not (nested items capture first).
fn carriers_captured(
    item: &Item,
    planned: &[PathBuf],
    refused: &Mutex<std::collections::BTreeSet<PathBuf>>,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let refused = refused
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
    match planned.iter().find(|source| refused.contains(*source)) {
        Some(carrier) => Err(BulkloadRefusal::GitNestCarrierRefused(
            carrier
                .strip_prefix(&item.source)
                .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
        )),
        None => Ok(()),
    }
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
        .open(path)
        .refuse_at("estate::read")?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .refuse_at("estate::read")?;
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
            Err(error) => return Err(crate::refuse::io(&error, "estate::write")),
        }
    };
    file.write_all(&bytes).refuse_at("estate::write")?;
    file.sync_file_counted().refuse_at("estate::write")?;
    fs::rename(&temporary, path).refuse_at("estate::write")?;
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at("estate::write")?
        .sync_dir_counted()
        .refuse_at("estate::write")?;
    Ok(())
}

/// Create `path` (0700) or check the private directory already there, then
/// seal its entry in the parent (#161): the records written inside it are
/// sealed in it, and must not outlive the directory itself. Sealed whoever
/// created it, since a creator may have died before its seal.
fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).refuse_at("estate::private_directory")?;
            // SAFETY: geteuid has no preconditions or side effects.
            if !(metadata.is_dir()
                && metadata.mode().trailing_zeros() >= 6
                && metadata.uid() == unsafe { libc::geteuid() })
            {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        }
        Err(error) => return Err(crate::refuse::io(&error, "estate::private_directory")),
    }
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::File::open(parent)
        .refuse_at("estate::private_directory")?
        .sync_dir_counted()
        .refuse_at("estate::private_directory")?;
    Ok(())
}

fn filename(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

pub(crate) fn id(item: &Item) -> Result<String> {
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
    let mut contents: Plan = if plan.try_exists().refuse_at("estate::add_batch")? {
        read(plan)?
    } else {
        Plan::default()
    };
    let mut identities = std::collections::HashSet::new();
    let mut targets: Vec<(TargetKey, PathBuf)> = Vec::new();
    // Round 4 N4, R6-1: targets are held as target_key forms (canonical,
    // unfolded, with their volume's case answer); `overlapping` decides how
    // each comparison folds.
    for previous in &contents.items {
        identities.insert(id(previous)?);
        if let Some(target) = &previous.workspace {
            targets.push((target_key(target)?, previous.source.clone()));
        }
    }
    for incoming in items {
        let mut item = incoming.clone();
        item.source = fs::canonicalize(&item.source).refuse_at("estate::add_batch")?;
        if !item.repository.is_absolute()
            || item.workspace.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        if !identities.insert(id(&item)?) {
            continue;
        }
        if let Some(target) = &item.workspace {
            let key = target_key(target)?;
            if targets
                .iter()
                .any(|(other, source)| overlapping(&key, &item.source, other, source))
            {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            targets.push((key, item.source.clone()));
        }
        contents.items.push(item);
    }
    write(plan, &contents)
}

// Whether a volume compares names case-insensitively, as far as it could be
// told (R6-1). `Unknown` is kept apart from `Insensitive` because the two
// kinds of comparison below must fail in opposite directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Sensitive,
    Insensitive,
    Unknown,
}

/// A workspace target as `target_key` resolved it, unfolded, with its volume's
/// case answer.
type TargetKey = (PathBuf, Case);

// Two target keys in the form they compare in. Targets are compared folded
// unless both answers are positively Sensitive: an unknown answer fails
// toward detecting an overlap (R5-7).
fn comparable((target, case): &TargetKey, (other, other_case): &TargetKey) -> (PathBuf, PathBuf) {
    if *case == Case::Sensitive && *other_case == Case::Sensitive {
        (target.clone(), other.clone())
    } else {
        (fold(target), fold(other))
    }
}

// Whether `inner` lies strictly inside `outer`, compared as `comparable`.
fn nests_within(inner: &TargetKey, outer: &TargetKey) -> bool {
    let (inner, outer) = comparable(inner, outer);
    inner.starts_with(&outer) && inner != outer
}

// R-N114: two workspace targets collide when they are equal, or when one lies
// inside the other anywhere but at exactly the place the inner item's source
// lies inside the outer item's source (a nested repository planned as its own
// item, restored where it was). Component-wise, never string prefixes.
//
// The two checks fail in opposite directions (R6-1, R-N83, R-N123). Target
// overlap is compared folded unless both volumes answered Sensitive, so an
// unanswerable probe can only add refusals. The source relation only ever
// allows an overlap, so it is compared case-folded only when both volumes
// answered Insensitive; otherwise exactly, taking the relative path from the
// unfolded targets wherever they nest, so an unanswerable probe never relaxes
// a planned-nest refusal and a same-case plan still passes.
fn overlapping(target: &TargetKey, source: &Path, other: &TargetKey, other_source: &Path) -> bool {
    let (folded_target, folded_other) = comparable(target, other);
    if folded_target == folded_other {
        return true;
    }
    let relation_folded = target.1 == Case::Insensitive && other.1 == Case::Insensitive;
    let nested = |inner: &TargetKey,
                  folded_inner: &Path,
                  inner_source: &Path,
                  outer: &TargetKey,
                  folded_outer: &Path,
                  outer_source: &Path| {
        folded_inner
            .strip_prefix(folded_outer)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|_| {
                // R7-1: the relative path is always the inner key's own
                // unfolded components past the outer's depth (folding changes
                // only ASCII letters, never the component count), so the
                // relation is compared exactly even when the targets nest only
                // case-folded; it is folded solely under (Insensitive,
                // Insensitive) below.
                let relative: PathBuf = inner
                    .0
                    .components()
                    .skip(outer.0.components().count())
                    .collect();
                let joined = outer_source.join(relative);
                if relation_folded {
                    fold(&joined) != fold(inner_source)
                } else {
                    joined != inner_source
                }
            })
    };
    nested(
        target,
        &folded_target,
        source,
        other,
        &folded_other,
        other_source,
    )
    .or_else(|| {
        nested(
            other,
            &folded_other,
            other_source,
            target,
            &folded_target,
            source,
        )
    })
    .unwrap_or(false)
}

// Round 4 N4 (R-N114): a workspace target in a form two targets compare in.
// The nearest existing ancestor is canonicalized (symlinks and `..` resolved
// by the filesystem); the rest is appended lexically (`.` dropped, `..`
// popped, a trailing slash ignored). The key is returned unfolded with its
// volume's case answer; comparisons decide how to fold (R6-1). A Unicode case
// or normalisation variant that slips through meets the restore's typed
// GIT_DESTINATION_OCCUPIED.
fn target_key(target: &Path) -> Result<TargetKey> {
    use std::path::Component;
    let components: Vec<Component<'_>> = target.components().collect();
    for existing in (1..=components.len()).rev() {
        let prefix: PathBuf = components.iter().take(existing).collect();
        let Ok(mut key) = fs::canonicalize(&prefix) else {
            continue;
        };
        let case = case_answer(&key);
        for part in components.iter().skip(existing) {
            match part {
                Component::Normal(name) => key.push(name),
                Component::ParentDir => {
                    key.pop();
                }
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            }
        }
        return Ok((key, case));
    }
    Err(BulkloadRefusal::PathNotAbsolute)
}

fn fold(path: &Path) -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    PathBuf::from(std::ffi::OsString::from_vec(
        path.as_os_str().as_bytes().to_ascii_lowercase(),
    ))
}

// Whether names on the volume holding `path` compare case-insensitively,
// decided at run time per volume, never assumed per platform. macOS answers
// through pathconf(_PC_CASE_SENSITIVE); when it cannot answer, the read-only
// probe below decides.
#[cfg(target_os = "macos")]
fn case_answer(path: &Path) -> Case {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return probe_case(path);
    };
    // SAFETY: `name` is a valid NUL-terminated C string that outlives the
    // call; pathconf only reads it and has no other preconditions.
    match unsafe { libc::pathconf(name.as_ptr(), libc::_PC_CASE_SENSITIVE) } {
        1 => Case::Sensitive,
        0 => Case::Insensitive,
        _ => probe_case(path),
    }
}

// Elsewhere there is no volume-wide query (ext4 casefold is per directory,
// vfat folds everywhere), so probe read-only: find the deepest component of
// `path` whose name has an ASCII letter, and ask whether its case-flipped
// spelling names the same inode. Nothing is created. A flipped name that is
// absent means Sensitive; anything unanswerable is Unknown.
#[cfg(not(target_os = "macos"))]
fn case_answer(path: &Path) -> Case {
    probe_case(path)
}

// The probe's answer as the older boolean: anything but a positive Sensitive
// counts as insensitive (the R5-7 reading, which the probe tests pin).
#[cfg(test)]
fn probe_case_insensitive(path: &Path) -> bool {
    probe_case(path) != Case::Sensitive
}

#[cfg(test)]
fn case_insensitive(path: &Path) -> bool {
    case_answer(path) != Case::Sensitive
}

fn probe_case(path: &Path) -> Case {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::MetadataExt;
    // Unknown whenever the probe cannot answer (R5-7, R6-1): a path that does
    // not resolve, a walk that reaches the root without a lettered name, a
    // lookup that fails for any reason but absence, and a walk that would
    // cross a device boundary (the parent directory would answer for another
    // volume). Callers decide which way Unknown fails.
    let Ok(mut meta) = fs::symlink_metadata(path) else {
        return Case::Unknown;
    };
    let mut current = path.to_path_buf();
    loop {
        let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
            return Case::Unknown;
        };
        let Ok(parent_meta) = fs::symlink_metadata(parent) else {
            return Case::Unknown;
        };
        if parent_meta.dev() != meta.dev() {
            return Case::Unknown;
        }
        let bytes = name.as_bytes();
        if bytes.iter().any(u8::is_ascii_alphabetic) {
            let flipped: Vec<u8> = bytes
                .iter()
                .map(|b| {
                    if b.is_ascii_lowercase() {
                        b.to_ascii_uppercase()
                    } else {
                        b.to_ascii_lowercase()
                    }
                })
                .collect();
            let variant = parent.join(std::ffi::OsString::from_vec(flipped));
            return match fs::symlink_metadata(variant) {
                Ok(folded) if folded.dev() == meta.dev() && folded.ino() == meta.ino() => {
                    Case::Insensitive
                }
                Ok(_) => Case::Sensitive,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Case::Sensitive,
                Err(_) => Case::Unknown,
            };
        }
        current = parent.to_path_buf();
        meta = parent_meta;
    }
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
        .open(path)
        .refuse_at("estate::exclusive")?;
    // SAFETY: the owned file descriptor remains open for the lock lifetime.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(crate::refuse::io(
            &std::io::Error::last_os_error(),
            "estate::exclusive",
        ));
    }
    Ok(Exclusive(file))
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .refuse_at("estate::hash_file")?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 65536];
    loop {
        let count = file.read(&mut buffer).refuse_at("estate::hash_file")?;
        if count == 0 {
            break;
        }
        crate::counters::add_len(crate::counters::Counter::HashFileRead, count);
        crate::counters::update(
            &mut hasher,
            crate::counters::Counter::HashFile,
            buffer.get(..count).ok_or(BulkloadRefusal::FrameCodec)?,
        );
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
    Ok(path.try_exists().refuse_at("estate::retained_base")?
        && base.identity
            == crate::freshness::StatIdentity::from_metadata(
                &fs::symlink_metadata(path).refuse_at("estate::retained_base")?,
            ))
}

fn prepare_base(item: &Item, group: &str, state: &Path, corpus: &Path) -> Result<Base> {
    let record = corpus.join(format!("shared-{group}.base"));
    if record.try_exists().refuse_at("estate::prepare_base")? {
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
        if !attempt.try_exists().refuse_at("estate::prepare_base")? {
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
    if published.try_exists().refuse_at("estate::prepare_base")? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(&bundle, &published).refuse_at("estate::prepare_base")?;
    }
    // Git's successful pack write is not a durability guarantee. Flush the
    // payload before write() publishes and directory-syncs its dependency.
    fs::File::open(&published)
        .refuse_at("estate::prepare_base")?
        .sync_file_counted()
        .refuse_at("estate::prepare_base")?;
    let base = Base {
        bundle: name,
        digest,
        identity: crate::freshness::StatIdentity::from_metadata(
            &fs::symlink_metadata(published).refuse_at("estate::prepare_base")?,
        ),
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
        /// The link this pass may chain onto (WP2), if the retained bundle
        /// is chainable.
        chain: Option<Prior>,
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
    if !record.try_exists().refuse_at("estate::retained_capture")? {
        return Ok(Retained::None);
    }
    let previous: Capture = read(record)?;
    if !filename(&previous.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let bundle = corpus.join(&previous.bundle);
    if !bundle.try_exists().refuse_at("estate::retained_capture")?
        || previous.identity
            != crate::freshness::StatIdentity::from_metadata(
                &fs::symlink_metadata(&bundle).refuse_at("estate::retained_capture")?,
            )
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
    let chained = prior_sidecar(corpus, &previous.bundle)
        .try_exists()
        .refuse_at("estate::retained_capture")?;
    // A chained bundle is a hit only while its whole chain is retained: a
    // broken chain recaptures (self-contained) instead of standing as custody
    // no restore can satisfy.
    let restorable =
        !chained || chain_links(corpus, &previous.bundle, LinkBinding::Custody).is_ok();
    if previous.key == key && drift.is_empty() && settled && restorable {
        if !chained && git_carry::shared::requires_base(&bundle)? {
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
    let chain = chainable(corpus, &previous, &bundle)?;
    Ok(Retained::Extend {
        bundle,
        started_ns: recorded.map(|recorded| recorded.started_ns),
        extends,
        chain,
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
        if !candidate
            .try_exists()
            .refuse_at("estate::attempt_directory")?
        {
            return Ok(candidate);
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    }
}

/// The bytes one item's capture is expected to publish (#101): the larger of
/// its census's regular-file bytes (the worktree payload before compression)
/// and the retained bundle it extends, if any (a lower bound on its history).
/// An estimate, not a promise: compression can make the bundle smaller, and
/// history the retained bundle does not hold can make it larger.
fn estimated_bundle(parts: &git_carry::KeyParts, retained: Option<&Path>) -> Result<u64> {
    let retained = match retained {
        Some(bundle) => fs::metadata(bundle)
            .refuse_at("estate::estimated_bundle")?
            .len(),
        None => 0,
    };
    Ok(parts.census_bytes().max(retained))
}

/// CORPUS free space for one capture pass (#101, OI-1002-Q11).
///
/// Each item reserves its estimated bundle before its export writes; the
/// estimate plus every byte still reserved by in-flight items (`jobs` is at
/// most 2) is checked against a fresh `statvfs` of CORPUS under the
/// process-wide floor. A fresh probe per item sees what completed items
/// already wrote, so a completed item's reservation is released, not kept.
/// Bytes an in-flight item has already written are counted twice (on disk
/// and reserved), which errs towards refusing.
struct CorpusSpace<'a> {
    corpus: &'a Path,
    floor: u8,
    probe: fn(&Path) -> Result<crate::space::Space>,
    reserved: Mutex<u64>,
}

impl<'a> CorpusSpace<'a> {
    fn live(corpus: &'a Path) -> Self {
        Self {
            corpus,
            floor: crate::space::min_free_percent(),
            probe: crate::space::probe,
            reserved: Mutex::new(0),
        }
    }

    /// Reserve `bytes` on CORPUS until the returned guard drops.
    ///
    /// # Errors
    /// `DESTINATION_SPACE_INSUFFICIENT` when they do not fit beside every
    /// in-flight reservation, or the probe's refusal.
    fn reserve(&self, bytes: u64) -> Result<Reservation<'_, 'a>> {
        let mut reserved = self
            .reserved
            .lock()
            .map_err(|_| BulkloadRefusal::ContractSelfInconsistent)?;
        if bytes > 0 {
            let space = (self.probe)(self.corpus)?;
            crate::space::check(reserved.saturating_add(bytes), space, self.floor)?;
            *reserved = reserved.saturating_add(bytes);
        }
        drop(reserved);
        Ok(Reservation { owner: self, bytes })
    }
}

/// One in-flight item's CORPUS reservation; released when it drops, whether
/// the item captured or refused.
struct Reservation<'s, 'a> {
    owner: &'s CorpusSpace<'a>,
    bytes: u64,
}

impl Drop for Reservation<'_, '_> {
    fn drop(&mut self) {
        // A poisoned lock means another item panicked, which R33 forbids;
        // there is nothing left to release into.
        if let Ok(mut reserved) = self.owner.reserved.lock() {
            *reserved = reserved.saturating_sub(self.bytes);
        }
    }
}

// What must hold before an item's key reads its source: it is not a partial
// clone (S2, WP1 PR 1), and every nest planned as its own item captured in
// this pass (R-N114). Returns those planned nests.
fn preflight(
    item: &Item,
    owners: &Owners,
    refused: &Mutex<std::collections::BTreeSet<PathBuf>>,
) -> Result<Vec<PathBuf>> {
    git_carry::refuse_partial_clone(&item.source)?;
    let planned = planned_nests(item, owners);
    carriers_captured(item, &planned, refused)?;
    Ok(planned)
}

#[allow(clippy::too_many_arguments)] // Pass-wide state is caller-owned.
fn capture_item(
    item: &Item,
    state: &Path,
    corpus: &Path,
    base: Option<&Base>,
    policy: git_carry::CapturePolicy,
    owners: &Owners,
    refused: &Mutex<std::collections::BTreeSet<PathBuf>>,
    space: &CorpusSpace<'_>,
) -> Result<Completion> {
    const SITE: &str = "estate::capture_item";
    let identity = id(item)?;
    let record = corpus.join(format!("{identity}.capture"));
    let planned = preflight(item, owners, refused)?;
    // The opaque key cannot say what moved. Keep its typed parts so the
    // post-capture re-read can separate tolerable drift from Git authority.
    // The parts carry the nested custody, so a reuse hit names exactly the
    // nests the retained capture recorded (R-N73, B4).
    let parts = git_carry::capture_key_parts_with_planned(&item.source, policy, &planned)?;
    let nested = nest_lines(item, owners, parts.nested_repositories());
    let key = parts.digest()?;
    let authority = parts.authority()?;
    let (retained, started_ns, extends, link) =
        match retained_capture(&record, corpus, &parts, key, authority)? {
            Retained::Hit => {
                return Ok(Completion::clean("capture-reused-after-census").naming(nested));
            }
            Retained::Extend {
                bundle,
                started_ns,
                extends,
                chain,
            } => (Some(bundle), started_ns, extends, chain),
            Retained::None => (None, None, false, None),
        };
    // WP2: without a plan base, pack only what is new since the retained
    // capture, by declaring its source-held tips as prerequisites.
    let (link, chain) = chain_offer(corpus, link, base.is_none())?;
    // #101 (OI-1002-Q11): before this item's export writes a byte, charge its
    // estimated bundle to CORPUS. An item that does not fit refuses
    // DESTINATION_SPACE_INSUFFICIENT as its own receipt; the pass goes on.
    let _reservation = space.reserve(estimated_bundle(&parts, retained.as_deref())?)?;
    let (reuse, unrecorded) = reuse_offer(retained.as_deref(), started_ns);
    // A future-stamped seat blocked the whole-capture reuse above, and will on
    // every pass until the clock passes it: say so (round-3 N5).
    let future = (retained.is_some() && parts.stamped_after(git_carry::pass_start_ns()))
        .then_some(git_carry::ReuseUnavailable::FutureStamp);
    let attempt = attempt_directory(state, &identity, key)?;
    let prerequisite = base.map(|base| base_path(corpus, base)).transpose()?;
    let export = match git_carry::export_repository_with_custody(
        &item.source,
        &attempt,
        &git_carry::ExportOptions {
            prerequisite: prerequisite.as_deref(),
            policy,
            reuse,
            planned: &planned,
            chain: chain.as_deref(),
        },
    )? {
        git_carry::Exported::Captured(export) => *export,
        git_carry::Exported::ObjectStoreRewritten(drift) => {
            return Ok(Completion::deferred(&drift, nested))
        }
    };
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
    publish_prior(corpus, &name, link.as_ref(), export.chained)?;
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
        &fs::canonicalize(&item.source).refuse_at(SITE)?,
        git_carry::mid_pass::Stage::RecordWritten,
    );
    if drift.is_empty() && drift_sidecar.try_exists().refuse_at(SITE)? {
        // A clean pass can reproduce a drifted pass's bundle byte for byte
        // when the drift lay only before the export's snapshot. Retire the
        // stale record only after the clean completion is durable: a crash in
        // between leaves the capture drifted, which costs one more pass and
        // never a stale reuse.
        fs::remove_file(&drift_sidecar).refuse_at(SITE)?;
        fs::File::open(corpus)
            .refuse_at(SITE)?
            .sync_dir_counted()
            .refuse_at(SITE)?;
    }
    Ok(Completion {
        outcome: captured_outcome(&drift, extends),
        drift: drift.lines(),
        bytes_read: export.bytes_read,
        reuse_unavailable: unrecorded
            .or(future)
            .or(export.reuse_unavailable)
            .map(git_carry::ReuseUnavailable::code),
        nested,
    })
}

// The receipt outcome of a capture that exported: any drift wins, then a
// clean pass that extended a drifted capture, then a plain capture.
const fn captured_outcome(drift: &git_carry::CaptureDrift, extends: bool) -> &'static str {
    if !drift.is_empty() {
        "captured-with-drift"
    } else if extends {
        "capture-extended-from-drift"
    } else {
        "captured"
    }
}

// Where a chain link's bundle lives in the corpus.
fn link_path(corpus: &Path, link: &Prior) -> Result<PathBuf> {
    base_path(
        corpus,
        &Base {
            bundle: link.bundle.clone(),
            digest: link.digest,
            identity: link.identity,
        },
    )
}

// The link a capture chains onto and its corpus path: none under a plan base.
fn chain_offer(
    corpus: &Path,
    link: Option<Prior>,
    unbased: bool,
) -> Result<(Option<Prior>, Option<PathBuf>)> {
    let link = link.filter(|_| unbased);
    let path = link
        .as_ref()
        .map(|link| link_path(corpus, link))
        .transpose()?;
    Ok((link, path))
}

// The chain link of a capture's bundle, durable before its record names it
// (WP2), as `.base` is for a shared plan base.
fn publish_prior(corpus: &Path, name: &str, link: Option<&Prior>, chained: bool) -> Result<()> {
    match (link, chained) {
        (Some(link), true) => {
            // Identical bundle bytes declare identical prerequisites, so an
            // intact chain already recorded for this name stands as it is.
            let sidecar = prior_sidecar(corpus, name);
            if !(sidecar.try_exists().refuse_at("estate::publish_prior")?
                && chain_links(corpus, name, LinkBinding::Custody).is_ok())
            {
                if link.bundle == name {
                    return Err(BulkloadRefusal::ContractSelfInconsistent);
                }
                write(&sidecar, link)?;
            }
            Ok(())
        }
        (None, true) => Err(BulkloadRefusal::ContractSelfInconsistent),
        (_, false) => Ok(()),
    }
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
    if published.try_exists().refuse_at("estate::publish_bundle")? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(bundle, &published).refuse_at("estate::publish_bundle")?;
    }
    // Completion may survive a crash only after its bundle bytes are durable.
    // Counted, as main counts every sync (#57).
    fs::File::open(&published)
        .refuse_at("estate::publish_bundle")?
        .sync_file_counted()
        .refuse_at("estate::publish_bundle")?;
    let metadata = fs::symlink_metadata(&published).refuse_at("estate::publish_bundle")?;
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
            let (done, reason, refusal) = match operation(item) {
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
                    (done, reason, None)
                }
                Err(error) => {
                    refused.store(true, std::sync::atomic::Ordering::Relaxed);
                    (
                        Completion::clean("refused"),
                        Some(error.to_string()),
                        Some(error),
                    )
                }
            };
            receipt(&Receipt {
                item: id(item)?,
                source: item.source.clone(),
                outcome: done.outcome,
                reason,
                refusal,
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

// WP3 PR 3: the durable outcome is the typed record, its refusal recorded
// at `site` (the verb).
fn emit(
    state: &Path,
    site: &'static str,
    row: &Receipt,
    receipt: &impl Fn(&Receipt) -> Result<()>,
) -> Result<()> {
    let record = crate::outcome::OutcomeRecord::of_receipt(
        row.source.clone(),
        row.outcome,
        row.refusal.as_ref(),
        row.reason.clone(),
        site,
    )?;
    write(
        &state.join(format!("{}.outcome", row.item)),
        &record.persisted(),
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
    capture_in(
        plan,
        state,
        corpus,
        jobs,
        policy,
        &CorpusSpace::live(corpus),
        receipt,
    )
}

// [`capture_with_policy`] once both directories exist, against `space`.
fn capture_in(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    policy: git_carry::CapturePolicy,
    space: &CorpusSpace<'_>,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    let contents: Plan = read(plan)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let groups = capture_groups(&contents)?;
    let owners = owners(&contents)?;
    // R-N114, round 4 N5: an item whose source lies inside another item's
    // source captures first, level by level from the deepest, so the outer
    // knows whether each planned carrier's own capture refused in this pass.
    let refused = Mutex::new(std::collections::BTreeSet::new());
    let operation = |item: &Item| {
        let result = group_base(item, &groups, state, corpus).and_then(|base| {
            capture_item(
                item,
                state,
                corpus,
                base.as_ref(),
                policy,
                &owners,
                &refused,
                space,
            )
        });
        if result.is_err() {
            refused
                .lock()
                .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
                .insert(item.source.clone());
        }
        result
    };
    let mut levels = std::collections::BTreeMap::<usize, Plan>::new();
    for item in &contents.items {
        let depth = contents
            .items
            .iter()
            .filter(|other| item.source.starts_with(&other.source) && item.source != other.source)
            .count();
        levels.entry(depth).or_default().items.push(item.clone());
    }
    let mut outcome = Ok(());
    for level in levels.values().rev() {
        if let Err(error) = execute(level, jobs, &operation, &|row| {
            emit(state, "estate::capture", row, receipt)
        }) {
            outcome = Err(error);
        }
    }
    outcome
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
    if !item
        .repository
        .try_exists()
        .refuse_at("estate::import_base")?
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
    if staged.digest() != base.digest || git_carry::shared::requires_base(staged.path())? {
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
    let record = corpus.join(format!("{identity}.capture"));
    // Round 4 N5: an item whose capture refused has no record. That is a
    // typed refusal, never a bare IO errno.
    if !record.try_exists().refuse_at("estate::apply_item")? {
        return Err(BulkloadRefusal::SealedObjectMissing);
    }
    let captured: Capture = read(&record)?;
    if !filename(&captured.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    // The nests a capture did not carry ride into every receipt that names
    // its bundle, so an apply never presents them as restored.
    let nested = nest_lines(item, owners, &retained_nested(corpus, &captured.bundle)?);
    // A capture that drifted under its export does not hold the drifted
    // seats' bytes. Its bundle says so in-band, and apply refuses it on that
    // marker, fail-closed, before any base import, journal write or
    // destination is touched, whether or not the corpus sidecar survived; the
    // next capture
    // pass extends it clean. Key-only drift leaves a coherent snapshot, which
    // applies. R-N29 (apply proceeds on an occupied destination, recording
    // uncaptured seats) is deferred to W6 git carry v2 (bulkload#48).
    let journal = journal_path(state, &identity, source, &captured.digest);
    if journal.try_exists().refuse_at("estate::apply_item")? {
        let done: String = read(&journal)?;
        let outcome = match done.as_str() {
            "workspace-restored" => "previous-workspace-restoration-not-revalidated",
            "refs-imported" => "previous-ref-custody-not-workspace-parity",
            _ => return Err(BulkloadRefusal::ReceiptBindingInvalid),
        };
        return Ok(Completion::clean(outcome).naming(nested));
    }
    // After the journal (round-4 P2): a re-apply of a done item stages and
    // copies nothing. Otherwise one private stage next to the corpus is the
    // only copy this apply reads: the marker check, the digest (computed while
    // copying) and the restore all see the same bytes (round-3 N4), and the
    // restore verbs below do not check again.
    // R5-4: a carrier bundle the record names but the corpus no longer holds
    // is a typed refusal, never a bare IO errno.
    let published = corpus.join(&captured.bundle);
    if !published.try_exists().refuse_at("estate::apply_item")? {
        return Err(BulkloadRefusal::SealedObjectMissing);
    }
    let staged = git_carry::stage_bundle(&published).map_err(|error| {
        if error == BulkloadRefusal::Io(Some(libc::ENOENT)) {
            BulkloadRefusal::SealedObjectMissing
        } else {
            error
        }
    })?;
    if staged.digest() != captured.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    // S4 (#162): a workspace is never laid down from a bare capture. Refused
    // here, before a chain is flattened or a plan base imported, so the
    // typed cause is never masked and nothing reaches the destination.
    if item.workspace.is_some() {
        git_carry::refuse_bare_capture(&staged)?;
    }
    // WP2: a chained capture restores from its verified, flattened chain; a
    // capture on a shared plan base imports that base first.
    let staged = if prior_sidecar(corpus, &captured.bundle)
        .try_exists()
        .refuse_at("estate::apply_item")?
    {
        git_carry::chain::flatten(
            staged,
            &chain_links(corpus, &captured.bundle, LinkBinding::Digest)?,
        )?
    } else {
        import_base(item, &captured, staged.path(), corpus, source, imported)?;
        staged
    };
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

/// The outcome `git-repair-missing-index` records into a state dir (#95).
///
/// Its journal says `refs-imported`:
/// the repair imports the capture's ref custody exactly as apply does, and
/// creates the missing index; it lays down no working bytes.
pub const INDEX_REPAIRED: &str = "index-repaired";

/// Where a repair records its outcome (#95): the plan and corpus that name
/// the repaired capture, and the private state directory that holds apply's
/// outcome records and journals.
#[derive(Debug, Clone, Copy)]
pub struct RepairLedger<'a> {
    pub plan: &'a Path,
    pub corpus: &'a Path,
    pub state: &'a Path,
}

// The one planned item whose current capture is exactly the bundle being
// repaired (by digest) and whose repository or workspace is `repository`.
// An unreadable capture record names no item here; it refuses on its own in
// apply and in the closure report.
//
// #132: an item that plans a workspace never binds. A repair lays down no
// working bytes, so its `refs-imported` journal would close nothing (the
// closure report leaves it unaccounted) and would make a later estate-apply
// replay `previous-ref-custody-not-workspace-parity` instead of restoring the
// workspace. Such an item refuses here, before the repair writes anything,
// and stays pending for estate-apply.
fn repair_item(
    plan: &Plan,
    corpus: &Path,
    repository: &Path,
    digest: &[u8; 32],
) -> Result<(Item, String, [u8; 32])> {
    let same = |path: &Path| fs::canonicalize(path).is_ok_and(|path| path == repository);
    let mut found = None;
    for item in &plan.items {
        let identity = id(item)?;
        let Ok(captured) = read::<Capture>(&corpus.join(format!("{identity}.capture"))) else {
            continue;
        };
        if captured.digest != *digest
            || !(same(&item.repository) || item.workspace.as_deref().is_some_and(same))
        {
            continue;
        }
        if found.is_some() || item.workspace.is_some() {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        found = Some((item.clone(), identity, captured.digest));
    }
    found.ok_or(BulkloadRefusal::ReceiptBindingInvalid)
}

/// `git-repair-missing-index` with apply-style records (#95, OI-1002-Q5).
///
/// Binds the bundle to the one planned item whose current capture record in
/// CORPUS has the bundle's digest and whose repository (or workspace) is
/// `repository`, under the state directory's estate lock, then repairs. An
/// item that plans a workspace never binds (#132): its restore belongs to
/// estate-apply, and a repair journal would block it. Once
/// the index is published and durable it writes the item's exact
/// current-capture journal (`refs-imported`, unless one is already there) and
/// then its `{item}.outcome` = `index-repaired`, so `closure-report` reads
/// the item as referenced-only natively. A repair that refuses after binding
/// records `refused` with the typed code, unless the item's current-capture
/// journal already exists: an item already closed by an earlier repair or
/// apply keeps its records (a second repair of a repaired index refuses).
///
/// # Errors
/// `RECEIPT_BINDING_INVALID` when the bundle names no planned item, more
/// than one, or an item that plans a workspace, before anything is written;
/// then everything [`git_carry::repair_missing_index`] refuses.
pub fn repair_missing_index(
    bundle: &Path,
    repository: &Path,
    source: &str,
    receipt: &Path,
    ledger: &RepairLedger<'_>,
) -> Result<()> {
    private_directory(ledger.state)?;
    let _lock = exclusive(&ledger.state.join("estate.lock"))?;
    let contents: Plan = read(ledger.plan)?;
    let canonical = fs::canonicalize(repository).refuse_at("estate::repair_missing_index")?;
    let mut bound = None;
    let result =
        git_carry::repair_missing_index_bound(bundle, repository, source, receipt, |digest| {
            bound = Some(repair_item(&contents, ledger.corpus, &canonical, digest)?);
            Ok(())
        });
    let Some((item, identity, digest)) = bound else {
        return result;
    };
    let journal = journal_path(ledger.state, &identity, source, &digest);
    let outcome = ledger.state.join(format!("{identity}.outcome"));
    match result {
        Ok(()) => {
            // Durability order: the index is published and synced, then the
            // journal, then the outcome record that names it.
            if !journal
                .try_exists()
                .refuse_at("estate::repair_missing_index")?
            {
                write(&journal, &"refs-imported".to_owned())?;
            }
            let record = crate::outcome::OutcomeRecord {
                source: item.source,
                outcome: crate::outcome::Outcome::IndexRepaired,
                reason: None,
            };
            write(&outcome, &record.persisted())?;
            Ok(())
        }
        Err(refusal) => {
            if !journal
                .try_exists()
                .refuse_at("estate::repair_missing_index")?
            {
                let record = crate::outcome::OutcomeRecord {
                    source: item.source,
                    outcome: crate::outcome::Outcome::Refused(crate::outcome::Refusal::of(
                        &refusal,
                        "estate::repair_missing_index",
                    )),
                    reason: Some(refusal.to_string()),
                };
                write(&outcome, &record.persisted())?;
            }
            Err(refusal)
        }
    }
}

/// What the corpus and state directories hold for one item's current
/// capture's apply journal (OI-1001-Q2, #80 review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalState {
    /// The corpus holds no `{item}.capture` record, so no journal can be the
    /// current capture's.
    NoCapture,
    /// The capture record exists but does not decode.
    CaptureUnreadable,
    /// No state directory holds the exact current-capture journal.
    Absent,
    /// The exact current-capture journal's body; empty when it does not
    /// decode.
    Present(String),
}

/// One planned item as its durable apply ledger records it (OI-1001-Q2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The plan item's identity, as every receipt names it.
    pub item: String,
    /// The plan item's source checkout.
    pub source: PathBuf,
    /// The plan item restores a workspace (`workspace` is set).
    pub has_workspace: bool,
    /// The typed record from the last state directory holding an
    /// `{item}.outcome` record (WP3 PR 3; a legacy record is mapped), or why
    /// it proves nothing; `None` when no state directory holds one.
    pub record:
        Option<std::result::Result<crate::outcome::OutcomeRecord, crate::outcome::Unreadable>>,
    /// The apply journal for the item's current capture, at its exact path
    /// `{item}-{blake3(SOURCE)}-{capture digest}.done`.
    pub journal: JournalState,
    /// The current capture's bundle digest (lowercase hex), when the corpus
    /// holds a readable `{item}.capture` record. An attestation row binds to
    /// it (#133).
    pub capture: Option<String>,
}

/// A plan's items joined with their outcome records and apply journals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    /// One entry per distinct plan item, in plan order.
    pub entries: Vec<LedgerEntry>,
    /// Item identities with an outcome record but no place in the plan.
    pub foreign_records: Vec<String>,
    /// `.done` journal names that are no planned item's current-capture
    /// journal: a foreign item's, or a stale one from an earlier capture.
    pub unmatched_journals: Vec<String>,
}

fn is_identity(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read a plan's durable apply ledger.
///
/// Joins the plan with the corpus's capture records and each state
/// directory's `{item}.outcome` records and apply journals. A journal counts
/// only at the exact path apply writes for the item's current capture and
/// `source` label, so a stale journal from an earlier capture proves
/// nothing. Later state directories override earlier ones' outcome records,
/// as a later apply attempt overrides an earlier one. Reads only; it never
/// takes the apply lock or writes anything.
///
/// # Errors
/// Refuses a malformed plan or an unreadable state directory.
pub fn ledger(plan: &Path, corpus: &Path, source: &str, states: &[PathBuf]) -> Result<Ledger> {
    type Record = std::result::Result<crate::outcome::OutcomeRecord, crate::outcome::Unreadable>;
    let contents: Plan = read(plan)?;
    let mut records = std::collections::BTreeMap::<String, Record>::new();
    let mut journals = std::collections::BTreeSet::<String>::new();
    for state in states {
        for entry in fs::read_dir(state).refuse_at("estate::ledger")? {
            let entry = entry.refuse_at("estate::ledger")?;
            if !entry.file_type().refuse_at("estate::ledger")?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(item) = name
                .strip_suffix(".outcome")
                .filter(|item| is_identity(item))
            {
                records.insert(item.to_owned(), crate::outcome::read_record(&entry.path()));
            } else if name.strip_suffix(".done").is_some() {
                journals.insert(name.to_owned());
            }
        }
    }
    let mut ledger = Ledger::default();
    let mut planned = std::collections::BTreeSet::new();
    let mut matched = std::collections::BTreeSet::new();
    for item in &contents.items {
        let identity = id(item)?;
        if !planned.insert(identity.clone()) {
            continue;
        }
        let record_path = corpus.join(format!("{identity}.capture"));
        let mut capture = None;
        let journal = if !record_path.try_exists().refuse_at("estate::ledger")? {
            JournalState::NoCapture
        } else if let Ok(captured) = read::<Capture>(&record_path) {
            capture = Some(
                blake3::Hash::from_bytes(captured.digest)
                    .to_hex()
                    .as_str()
                    .to_owned(),
            );
            let expected = journal_path(Path::new(""), &identity, source, &captured.digest);
            let name = expected.to_string_lossy().into_owned();
            let found = states
                .iter()
                .rev()
                .map(|state| state.join(&name))
                .find(|path| path.is_file());
            matched.insert(name);
            found.map_or(JournalState::Absent, |path| {
                JournalState::Present(read::<String>(&path).unwrap_or_default())
            })
        } else {
            JournalState::CaptureUnreadable
        };
        let record = records.get(&identity);
        ledger.entries.push(LedgerEntry {
            source: item.source.clone(),
            has_workspace: item.workspace.is_some(),
            record: record.cloned(),
            journal,
            capture,
            item: identity,
        });
    }
    ledger.foreign_records = records
        .into_keys()
        .filter(|item| !planned.contains(item))
        .collect();
    ledger.unmatched_journals = journals
        .into_iter()
        .filter(|name| !matched.contains(name))
        .collect();
    Ok(ledger)
}

// The apply journal for one item, source and capture digest. Its presence
// means the item's restore or import completed; its body is that outcome.
fn journal_path(state: &Path, identity: &str, source: &str, digest: &[u8; 32]) -> PathBuf {
    state.join(format!(
        "{identity}-{}-{}.done",
        blake3::hash(source.as_bytes()).to_hex(),
        blake3::Hash::from_bytes(*digest).to_hex()
    ))
}

/// One pending item's planned writes and its bundle size.
type ItemSpace = (Vec<(PathBuf, u64)>, u64);

/// One pending item's planned writes: `(path, bytes)` per destination, and
/// its bundle size for the staging peak. `None` when it is not pending.
fn item_space(item: &Item, corpus: &Path, state: &Path, source: &str) -> Result<Option<ItemSpace>> {
    let identity = id(item)?;
    let record = corpus.join(format!("{identity}.capture"));
    if !record.try_exists().refuse_at("estate::item_space")? {
        return Ok(None);
    }
    let captured: Capture = read(&record)?;
    if !filename(&captured.bundle)
        || journal_path(state, &identity, source, &captured.digest)
            .try_exists()
            .refuse_at("estate::item_space")?
    {
        return Ok(None);
    }
    // A chained capture lands its whole chain's objects (WP2).
    let bundle = chain_links(corpus, &captured.bundle, LinkBinding::Digest)?
        .iter()
        .try_fold(
            fs::metadata(corpus.join(&captured.bundle))
                .refuse_at("estate::item_space")?
                .len(),
            |total, (link, _)| {
                Ok::<_, BulkloadRefusal>(
                    total.saturating_add(fs::metadata(link).refuse_at("estate::item_space")?.len()),
                )
            },
        )?;
    // Objects land in the repository; a checkout lands in the workspace.
    // For a linked worktree those are two places, possibly two filesystems.
    let mut writes = vec![(item.repository.clone(), bundle)];
    if let Some(workspace) = item.workspace.as_ref().filter(|w| **w != item.repository) {
        writes.push((workspace.clone(), bundle));
    }
    Ok(Some((writes, bundle)))
}

/// OI-1001-Q2: the bytes an apply would write, per destination filesystem,
/// before it writes any of them.
///
/// Each pending item (a capture record and bundle present, no journal yet)
/// charges its bundle size to its repository's filesystem (objects) and,
/// for a linked worktree, to its workspace's too (checkout): a lower bound,
/// since a checkout expands past its pack. Staging copies a bundle and its
/// base next to the corpus, at most `jobs` items at once, so the corpus
/// filesystem is charged twice the `jobs` largest bundles. An item whose
/// record, bundle or target cannot be read is skipped here and refuses on its
/// own, by type, in apply (R33): one bad item never aborts the others.
fn space_plan(
    plan: &Plan,
    corpus: &Path,
    state: &Path,
    source: &str,
    jobs: usize,
) -> Vec<(PathBuf, u64)> {
    let mut by_device = std::collections::BTreeMap::<u64, (PathBuf, u64)>::new();
    let locate = |path: &Path| -> Result<(u64, PathBuf)> {
        let probe = crate::space::existing_ancestor(path)?;
        Ok((
            fs::metadata(&probe).refuse_at("estate::space_plan")?.dev(),
            probe,
        ))
    };
    let mut staged = Vec::new();
    for item in &plan.items {
        let Ok(Some((writes, bundle))) = item_space(item, corpus, state, source) else {
            continue;
        };
        let Ok(located) = writes
            .iter()
            .map(|(path, bytes)| locate(path).map(|found| (found, *bytes)))
            .collect::<Result<Vec<_>>>()
        else {
            continue;
        };
        for ((device, probe), bytes) in located {
            let entry = by_device.entry(device).or_insert((probe, 0));
            entry.1 = entry.1.saturating_add(bytes);
        }
        staged.push(bundle);
    }
    staged.sort_unstable_by(|left, right| right.cmp(left));
    let peak = staged
        .iter()
        .take(jobs)
        .fold(0u64, |total, bytes| total.saturating_add(*bytes))
        .saturating_mul(2);
    if peak > 0 {
        if let Ok((device, probe)) = locate(corpus) {
            let entry = by_device.entry(device).or_insert((probe, 0));
            entry.1 = entry.1.saturating_add(peak);
        }
    }
    by_device.into_values().collect()
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
    // OI-1001-Q2: refuse before any item writes when the planned bytes would
    // take a destination filesystem under its free-space floor.
    for (probe, planned) in space_plan(&contents, corpus, state, source, jobs) {
        crate::space::preflight(&probe, planned)?;
    }
    let mut locks = std::collections::BTreeMap::new();
    let mut groups = std::collections::BTreeMap::new();
    let imported = ImportedBases::default();
    for item in &contents.items {
        // A standalone restore's repository is its own workspace: if it is
        // already there, that is a collision the restore refuses by type
        // (R-N114), not Git administration to serialise on.
        let standalone = item.workspace.as_ref() == Some(&item.repository);
        let common = if !standalone && item.repository.try_exists().refuse_at("estate::apply")? {
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
        // Compared in target_key form (round 4 N4), so a case or `..`
        // variant of an enclosing target still orders after it.
        let depth = match &item.workspace {
            None => 0,
            Some(workspace) => {
                let key = target_key(workspace)?;
                let mut depth = 0;
                for other in contents
                    .items
                    .iter()
                    .filter_map(|other| other.workspace.as_ref())
                {
                    if nests_within(&key, &target_key(other)?) {
                        depth += 1;
                    }
                }
                depth
            }
        };
        levels.entry(depth).or_default().items.push(item.clone());
    }
    let mut outcome = Ok(());
    for level in levels.values() {
        if let Err(error) = execute(level, jobs, &operation, &|row| {
            emit(state, "estate::apply", row, receipt)
        }) {
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

    // OI-1001-Q2: the space plan charges pending bundles before apply writes,
    // and the closure report reads apply's own ledger back, failing closed on
    // a planned item apply never recorded.
    #[test]
    fn space_plan_and_closure_report_read_the_durable_ledger() {
        use crate::closure::{Disposition, Report};
        let root = std::env::temp_dir().join(format!("tcfs-estate-closure-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let applied = root.join("applied");
        fs::DirBuilder::new().mode(0o700).create(&applied).unwrap();
        let report = |states: &[PathBuf]| {
            Report::from_ledger(&ledger(&plan, &corpus, "neo", states).unwrap())
        };

        // Before apply: one pending bundle, charged to the target's
        // filesystem and twice (bundle and base staging) to the corpus's;
        // same device here.
        let contents: Plan = read(&plan).unwrap();
        let identity = id(contents.items.first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{identity}.capture"))).unwrap();
        let bundle = fs::metadata(corpus.join(&record.bundle)).unwrap().len();
        let planned = space_plan(&contents, &corpus, &applied, "neo", 1);
        assert_eq!(planned.len(), 1);
        let (probe, bytes) = planned.first().unwrap();
        assert_eq!(*bytes, 3 * bundle);
        let space = crate::space::probe(probe).unwrap();
        assert_eq!(
            crate::space::check(*bytes, space, 100),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        assert_eq!(crate::space::check(*bytes, space, 0), Ok(()));

        // Before apply the ledger holds nothing: unaccounted, fail.
        let before = report(std::slice::from_ref(&applied));
        assert_eq!((before.planned(), before.unaccounted), (1, 1));
        assert_eq!(before.gate(), Err(BulkloadRefusal::ClosureUnaccounted));

        apply(&plan, &corpus, &applied, "neo", 1, &|_| Ok(())).unwrap();
        let after = report(std::slice::from_ref(&applied));
        assert_eq!((after.applied, after.unaccounted), (1, 0));
        assert!(after.passes() && after.unmatched_journals.is_empty());
        // A done item is not charged again.
        assert!(space_plan(&contents, &corpus, &applied, "neo", 1).is_empty());
        // The journal is bound to the apply's SOURCE label.
        let other_label = Report::from_ledger(
            &ledger(&plan, &corpus, "sting", std::slice::from_ref(&applied)).unwrap(),
        );
        assert_eq!(
            other_label.rows.first().unwrap().disposition,
            Disposition::Unaccounted("journal-missing")
        );

        // #80 review M3: a journal from an earlier capture proves nothing.
        // Rebind the record to another digest: the old journal goes stale
        // and is listed, and the item is unaccounted.
        let rebound = Capture {
            digest: [7; 32],
            ..read::<Capture>(&corpus.join(format!("{identity}.capture"))).unwrap()
        };
        let record_path = corpus.join(format!("{identity}.capture"));
        let original = fs::read(&record_path).unwrap();
        fs::remove_file(&record_path).unwrap();
        write(&record_path, &rebound).unwrap();
        let recaptured = report(std::slice::from_ref(&applied));
        assert_eq!(
            recaptured.rows.first().unwrap().disposition,
            Disposition::Unaccounted("journal-missing")
        );
        assert_eq!(recaptured.unmatched_journals.len(), 1);
        fs::remove_file(&record_path).unwrap();
        fs::write(&record_path, original).unwrap();

        // A newly planned item apply has not run yet is unaccounted, and the
        // capture state's records are not apply outcomes.
        let second = root.join("second");
        fs::create_dir(&second).unwrap();
        git(&second, &["init", "--template="]);
        add(&plan, &second, &root.join("second-target"), None).unwrap();
        let both = report(&[state.clone(), applied]);
        assert_eq!((both.planned(), both.applied, both.unaccounted), (2, 1, 1));
        assert_eq!(
            both.rows.last().unwrap().disposition,
            Disposition::Unaccounted("no-outcome-record")
        );
        let capture_only = report(std::slice::from_ref(&state));
        assert_eq!(
            capture_only.rows.first().unwrap().disposition,
            Disposition::Unaccounted("not-an-apply-outcome")
        );
        fs::remove_dir_all(root).unwrap();
    }

    // #80 review M4 (R33): one item whose record cannot be read is skipped by
    // the space plan, so it refuses on its own in apply; the rest are still
    // planned and the apply is not aborted by the plan.
    #[test]
    fn space_plan_skips_an_unreadable_item_and_charges_linked_repositories() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-space-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let mut sources = Vec::new();
        for name in ["good", "bad"] {
            let source = root.join(name);
            fs::create_dir(&source).unwrap();
            git(&source, &["init", "--template="]);
            fs::write(source.join("file"), name.as_bytes()).unwrap();
            git(&source, &["add", "file"]);
            git(&source, &["commit", "-m", name]);
            sources.push(source);
        }
        let plan = root.join("plan");
        let repository = root.join("repository");
        let (good_source, bad_source) = (sources.first().unwrap(), sources.last().unwrap());
        add(&plan, good_source, &repository, Some(&root.join("linked"))).unwrap();
        add(&plan, bad_source, &root.join("bad-target"), None).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let contents: Plan = read(&plan).unwrap();
        let good: Capture = read(&corpus.join(format!(
            "{}.capture",
            id(contents.items.first().unwrap()).unwrap()
        )))
        .unwrap();
        let bundle = fs::metadata(corpus.join(&good.bundle)).unwrap().len();
        let bad_record = corpus.join(format!(
            "{}.capture",
            id(contents.items.last().unwrap()).unwrap()
        ));
        fs::remove_file(&bad_record).unwrap();
        fs::write(&bad_record, b"not a capture record").unwrap();
        let applied = root.join("applied");
        fs::DirBuilder::new().mode(0o700).create(&applied).unwrap();

        // Linked worktree: repository (objects) + workspace (checkout), plus
        // the doubled staging peak; the unreadable item adds nothing.
        let planned = space_plan(&contents, &corpus, &applied, "neo", 2);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned.first().unwrap().1, 4 * bundle);

        // The unreadable item refuses by itself in apply, as a value.
        let outcomes = Mutex::new(Vec::new());
        let _ = apply(&plan, &corpus, &applied, "neo", 1, &|row| {
            outcomes
                .lock()
                .unwrap()
                .push((row.source.clone(), row.outcome, row.reason.clone()));
            Ok(())
        });
        let outcomes = outcomes.into_inner().unwrap();
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        assert!(outcomes
            .iter()
            .any(|(source, outcome, reason)| source == bad_source
                && *outcome == "refused"
                && reason.as_deref() == Some("FRAME_CODEC")));
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

    // WP1 PR 1 (S2): a partial-clone item refuses as its own typed receipt
    // before its key reads anything, and no capture record is written.
    #[test]
    fn a_partial_clone_item_refuses_before_its_key_reads_it() {
        let (root, origin, _, _, corpus) = drifting_plan("partial");
        git(&origin, &["config", "uploadpack.allowFilter", "true"]);
        let clone = root.join("clone");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                "--template=",
                "--no-checkout",
                "--filter=blob:none",
                &format!("file://{}", origin.display()),
                clone.to_str().unwrap(),
            ],
        );
        let plan = root.join("partial-plan");
        add(&plan, &clone, &root.join("partial-destination"), None).unwrap();
        let state = root.join("state");
        let rows = Mutex::new(Vec::new());
        let result = capture(&plan, &state, &corpus, 1, &|row| {
            rows.lock().unwrap().push((row.outcome, row.reason.clone()));
            Ok(())
        });
        assert!(result.is_err(), "a refused item fails the pass");
        assert_eq!(
            rows.into_inner().unwrap(),
            vec![("refused", Some("GIT_SOURCE_PARTIAL_CLONE".to_owned()))]
        );
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        assert!(!corpus.join(format!("{item}.capture")).exists());
        fs::remove_dir_all(root).unwrap();
    }

    // S4 (#162): a bare repository item (a mirror) captures and applies as
    // ref custody. Its key used to read the absent index and refuse with a
    // bare IO (errno 2), which no closure report can account for.
    #[test]
    fn a_bare_repository_item_captures_and_applies_as_ref_custody() {
        let (root, origin, _, _, corpus) = drifting_plan("bare");
        let mirror = root.join("mirror.git");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                "--bare",
                origin.to_str().unwrap(),
                mirror.to_str().unwrap(),
            ],
        );
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let plan = root.join("bare-plan");
        add(&plan, &mirror, &repository, None).unwrap();
        let state = root.join("state");
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.len(), 1);
        let (outcome, reason, drift, bytes_read) = rows.first().unwrap();
        assert_eq!(
            (*outcome, reason.as_deref(), drift.len(), *bytes_read),
            ("captured", None, 0, 0)
        );
        let applied = root.join("applied");
        let outcomes = Mutex::new(Vec::new());
        apply(&plan, &corpus, &applied, "neo", 1, &|row| {
            outcomes
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone()));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            outcomes.into_inner().unwrap(),
            vec![("refs-imported", None)]
        );
        let main = Command::new("git")
            .arg("-C")
            .arg(&mirror)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let imported = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["for-each-ref", "--format=%(objectname) %(refname)"])
            .arg("refs/carry/v1/neo/")
            .output()
            .unwrap();
        let main = String::from_utf8(main.stdout).unwrap();
        let imported = String::from_utf8(imported.stdout).unwrap();
        assert!(
            imported
                .lines()
                .any(|line| line.starts_with(main.trim()) && line.ends_with("/head")),
            "{imported}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // S4 (#162): a bare repository item planned with a workspace, standalone
    // or linked, captures as ref custody, then refuses at apply as a typed
    // GIT_BARE_CAPTURE_WORKSPACE before anything is laid down: no workspace
    // directory, no imported ref, no journal. A bare capture has no index or
    // worktree, so a checkout from it would stage every HEAD path as deleted.
    #[test]
    fn a_bare_repository_item_with_a_workspace_refuses_typed_at_apply() {
        let (root, origin, _, _, corpus) = drifting_plan("bare-workspace");
        let mirror = root.join("mirror.git");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                "--bare",
                origin.to_str().unwrap(),
                mirror.to_str().unwrap(),
            ],
        );
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let standalone = root.join("standalone");
        let linked = root.join("linked");
        for (name, target, workspace) in [
            ("standalone", &standalone, &standalone),
            ("linked", &repository, &linked),
        ] {
            let plan = root.join(format!("{name}-plan"));
            add(&plan, &mirror, target, Some(workspace)).unwrap();
            let state = root.join(format!("{name}-state"));
            let rows = receipts(&plan, &state, &corpus).unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| (row.0, row.1.as_deref()))
                    .collect::<Vec<_>>(),
                vec![("captured", None)],
                "{name}"
            );
            let applied = root.join(format!("{name}-applied"));
            let outcomes = Mutex::new(Vec::new());
            let result = apply(&plan, &corpus, &applied, "neo", 1, &|row| {
                outcomes
                    .lock()
                    .unwrap()
                    .push((row.outcome, row.reason.clone()));
                Ok(())
            });
            assert!(result.is_err(), "{name}");
            assert_eq!(
                outcomes.into_inner().unwrap(),
                vec![("refused", Some("GIT_BARE_CAPTURE_WORKSPACE".to_owned()))],
                "{name}"
            );
            assert!(!workspace.exists(), "{name}: nothing is laid down");
        }
        let refs = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["for-each-ref"])
            .output()
            .unwrap();
        assert!(refs.stdout.is_empty(), "nothing was imported");
        fs::remove_dir_all(root).unwrap();
    }

    // S4 (#162): a bare hub and its linked worktree share a common dir, so
    // their items share a plan base. The bare item, planned with a
    // standalone workspace, still refuses GIT_BARE_CAPTURE_WORKSPACE at
    // apply: the check runs before the base import, which would otherwise
    // mask it (`GIT_DESTINATION_OCCUPIED`).
    #[test]
    fn a_bare_item_on_a_plan_base_refuses_its_workspace_before_the_base() {
        let (root, origin, _, _, corpus) = drifting_plan("bare-base");
        let hub = root.join("hub.git");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                "--bare",
                origin.to_str().unwrap(),
                hub.to_str().unwrap(),
            ],
        );
        let linked = root.join("hub-worktree");
        git(
            &hub,
            &["worktree", "add", "--quiet", linked.to_str().unwrap()],
        );
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let standalone = root.join("standalone");
        let plan = root.join("base-plan");
        add(&plan, &hub, &standalone, Some(&standalone)).unwrap();
        add(&plan, &linked, &repository, None).unwrap();
        let state = root.join("state");
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert!(
            rows.iter()
                .all(|row| row.0 == "captured" && row.1.is_none()),
            "{rows:?}"
        );
        let item = inspect(&plan)
            .unwrap()
            .into_iter()
            .find(|item| item.workspace.is_some())
            .unwrap();
        let record: Capture =
            read(&corpus.join(format!("{}.capture", id(&item).unwrap()))).unwrap();
        assert!(
            git_carry::shared::requires_base(&corpus.join(&record.bundle)).unwrap(),
            "the bare capture is on the plan base"
        );
        let applied = root.join("applied");
        let outcomes = Mutex::new(Vec::new());
        let result = apply(&plan, &corpus, &applied, "neo", 1, &|row| {
            outcomes
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone()));
            Ok(())
        });
        assert!(result.is_err());
        let mut outcomes = outcomes.into_inner().unwrap();
        outcomes.sort();
        assert_eq!(
            outcomes,
            vec![
                ("refs-imported", None),
                ("refused", Some("GIT_BARE_CAPTURE_WORKSPACE".to_owned())),
            ]
        );
        assert!(!standalone.exists(), "nothing is laid down");
        fs::remove_dir_all(root).unwrap();
    }

    // WP1 PR 4 (S5, arch 8): a lane deletes a branch and prunes under the
    // pass. The private repository reads the source through `alternates`, so
    // its bundle child fails on the pruned commit; the pack listing moved, so
    // the item is drift custody, not a refusal, and the next pass captures.
    #[test]
    fn an_object_store_rewrite_under_the_pass_is_drift_custody() {
        let (root, source, _, plan, corpus) = drifting_plan("object-store");
        git(&source, &["checkout", "-q", "-b", "doomed"]);
        fs::write(source.join("doomed-only"), b"reachable only from doomed").unwrap();
        git(&source, &["add", "doomed-only"]);
        git(&source, &["commit", "-q", "-m", "doomed"]);
        git(&source, &["checkout", "-q", "-"]);
        let inside = fs::canonicalize(&source).unwrap();
        git_carry::mid_pass::arm(&source, move || {
            git(&inside, &["branch", "-D", "doomed"]);
            git(&inside, &["reflog", "expire", "--expire=now", "--all"]);
            git(&inside, &["repack", "-a", "-d", "-q"]);
            git(&inside, &["prune", "--expire=now"]);
        });
        let state = root.join("state");
        let rows = receipts(&plan, &state, &corpus).expect("Ok(()): custody, not a refusal");
        assert_eq!(rows.len(), 1);
        let (outcome, reason, drift, _) = rows.first().unwrap();
        assert_eq!(*outcome, "deferred-with-drift");
        assert_eq!(reason.as_deref(), Some("drift=1"));
        assert_eq!(
            *drift,
            vec!["ObjectStoreRewritten \"objects/pack\"".to_owned()]
        );
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        assert!(!corpus.join(format!("{item}.capture")).exists());
        // The next pass sees the rewritten store and captures it clean.
        let second = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(second.first().unwrap().0, "captured");
        assert!(corpus.join(format!("{item}.capture")).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    // WP1 PR 4: the same failure with an unchanged pack listing is not a
    // rewrite, and keeps its refusal. Since WP3 PR 2 that refusal is the
    // bundle child's own GIT_CHILD_FAILED with its stderr class (the pruned
    // commit is a bad object), the refusal custody absorbs only under a moved
    // listing.
    #[test]
    fn a_git_child_failure_without_a_rewrite_still_refuses() {
        let (root, source, _, plan, corpus) = drifting_plan("no-rewrite");
        git(&source, &["checkout", "-q", "-b", "doomed"]);
        fs::write(source.join("doomed-only"), b"reachable only from doomed").unwrap();
        git(&source, &["add", "doomed-only"]);
        git(&source, &["commit", "-q", "-m", "doomed"]);
        git(&source, &["checkout", "-q", "-"]);
        let inside = fs::canonicalize(&source).unwrap();
        git_carry::mid_pass::arm(&source, move || {
            // Prune the loose commit without touching `objects/pack`.
            git(&inside, &["branch", "-D", "doomed"]);
            git(&inside, &["reflog", "expire", "--expire=now", "--all"]);
            git(&inside, &["prune", "--expire=now"]);
        });
        let state = root.join("state");
        let rows = Mutex::new(Vec::new());
        let result = capture(&plan, &state, &corpus, 1, &|row| {
            rows.lock().unwrap().push((row.outcome, row.reason.clone()));
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            rows.into_inner().unwrap(),
            vec![(
                "refused",
                Some("GIT_CHILD_FAILED stderr_class=bad_object".to_owned())
            )]
        );
        fs::remove_dir_all(root).unwrap();
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
        // The durable outcome is the typed record (WP3 PR 3; it was the
        // legacy string tuple, which the reader still maps).
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let durable = crate::outcome::read_record(&state.join(format!("{item}.outcome"))).unwrap();
        assert_eq!(durable.source, fs::canonicalize(&source).unwrap());
        assert_eq!(durable.outcome, crate::outcome::Outcome::CapturedWithDrift);
        assert_eq!(durable.reason.as_deref(), Some("drift=3"));
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
        let heads = recorded_heads(&plan, &corpus, &source);
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

    // The ref lines the item's recorded capture carries (its header, or its
    // ref table's expansion, OI-1003-Q54); a chained bundle's prerequisites
    // are read from the source's object store.
    fn recorded_heads(plan: &Path, corpus: &Path, source: &Path) -> String {
        let item = id(inspect(plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let objects = Command::new("git")
            .arg("-C")
            .arg(source)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ])
            .output()
            .unwrap();
        let objects = String::from_utf8(objects.stdout).unwrap();
        git_carry::carried_heads(
            &corpus.join(&record.bundle),
            Some(Path::new(objects.trim_end())),
        )
        .unwrap()
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

    // Round-4 P2: re-applying an item that is already done reads the journal
    // first and stages nothing, so it never copies (or even needs) the bundle.
    #[test]
    fn a_reapply_of_a_done_item_stages_nothing() {
        let (root, _, _, plan, corpus) = drifting_plan("reapply");
        let state = root.join("state");
        receipts(&plan, &state, &corpus).unwrap();
        let applied = root.join("applied");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).unwrap();
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let bundle = corpus.join(&record.bundle);
        let held = root.join("held.bundle");
        fs::rename(&bundle, &held).unwrap();
        let outcomes = Mutex::new(Vec::new());
        apply(&plan, &corpus, &applied, "neo", 2, &|row| {
            outcomes.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec!["previous-workspace-restoration-not-revalidated"]
        );
        fs::rename(&held, &bundle).unwrap();
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

    // Round 4 N4 (R-N114), R6-1: targets are compared as target_key forms.
    // A `..` target that physically lands inside another item's target, and
    // a case variant of one on a case-insensitive volume, refuse at add; the
    // nest at its own place in another case is accepted.
    #[test]
    fn overlapping_targets_compare_canonical_and_case_folded() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-fold-{}", std::process::id()));
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
        fs::create_dir(root.join("elsewhere")).unwrap();
        let target = root.join("target");
        let plan = root.join("plan");
        add(&plan, &outer, &target, Some(&target)).unwrap();
        let sneaky = root.join("elsewhere/../target/file");
        let dotdot = add(&plan, &other, &sneaky, Some(&sneaky));
        let insensitive = {
            fs::write(root.join("probe"), b"").unwrap();
            let folded = root.join("PROBE").exists();
            fs::remove_file(root.join("probe")).unwrap();
            folded
        };
        let variant = root.join("TARGET/x");
        let cased = add(&plan, &other, &variant, Some(&variant));
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(dotdot, Err(BulkloadRefusal::GitDestinationOccupied));
        if insensitive {
            assert_eq!(cased, Err(BulkloadRefusal::GitDestinationOccupied));
        } else {
            assert_eq!(cased, Ok(()));
        }
    }

    // R6-1 (R-N83, R-N123): an unanswerable case probe must never relax a
    // planned-nest refusal. The unanswerable path is forced without any
    // host device boundary: `/` is a letterless walk to the root, so the
    // probe cannot answer there. The R5-3 plan (nest at Vendor/inner, planned
    // at T/vendor/inner) refuses whether the answer is Sensitive or Unknown,
    // and is only relaxed by a positive Insensitive answer on both keys.
    #[test]
    fn an_unanswerable_case_probe_never_relaxes_a_planned_nest_refusal() {
        assert_eq!(probe_case(Path::new("/")), Case::Unknown);
        let outer_source = Path::new("/src/outer");
        let inner_source = Path::new("/src/outer/Vendor/inner");
        let target = PathBuf::from("/mnt/T");
        let inner = target.join("vendor/inner");
        let plan = |case: Case, other_case: Case| {
            overlapping(
                &(inner.clone(), case),
                inner_source,
                &(target.clone(), other_case),
                outer_source,
            )
        };
        assert!(plan(Case::Sensitive, Case::Sensitive));
        assert!(plan(Case::Unknown, Case::Unknown));
        assert!(plan(Case::Unknown, Case::Insensitive));
        assert!(plan(Case::Insensitive, Case::Unknown));
        assert!(plan(Case::Sensitive, Case::Insensitive));
        assert!(!plan(Case::Insensitive, Case::Insensitive));
        // A same-case plan still passes on an unanswerable volume.
        let same = Path::new("/src/outer/vendor/inner");
        assert!(!overlapping(
            &(inner.clone(), Case::Unknown),
            same,
            &(target.clone(), Case::Unknown),
            outer_source,
        ));
        // Unanswerable still fails toward detecting a case-variant overlap
        // between unrelated items.
        assert!(overlapping(
            &(PathBuf::from("/mnt/t/x"), Case::Unknown),
            Path::new("/src/other"),
            &(target.clone(), Case::Unknown),
            outer_source,
        ));
        // Only positive Sensitive answers on both sides keep case-distinct
        // targets distinct.
        assert!(!overlapping(
            &(PathBuf::from("/mnt/t/x"), Case::Sensitive),
            Path::new("/src/other"),
            &(target, Case::Sensitive),
            outer_source,
        ));
    }

    // Round 4 N4 on both platforms: the read-only case probe agrees with a
    // created-file ground truth on whatever volume the test runs on (case-
    // insensitive APFS on macOS, case-sensitive ext4 on Linux CI).
    #[test]
    fn the_read_only_case_probe_matches_the_volume() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-case-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let named = root.join("CaseProbe");
        fs::create_dir(&named).unwrap();
        fs::write(root.join("probe"), b"").unwrap();
        let folds = root.join("PROBE").exists();
        let probed = probe_case_insensitive(&named);
        let digits = root.join("1234");
        fs::create_dir(&digits).unwrap();
        // A name with no letter is decided by its nearest lettered ancestor.
        let through_parent = probe_case_insensitive(&digits);
        let at_root = probe_case_insensitive(&root);
        let platform = case_insensitive(&named);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(probed, folds);
        assert_eq!(through_parent, at_root);
        assert_eq!(platform, folds);
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

// #53 round-5 reviewer probes of 895cd8a's read-only case probe (R-N114),
// kept as regression tests. The case-sensitive directory comes from RV5_CS
// (an hdiutil case-sensitive APFS image), its mount root from RV5_CS_ROOT;
// those probes skip when unset. Under R5-7 an unanswerable probe (a missing
// path, or a walk that would cross a device boundary) answers insensitive,
// which can only add overlap refusals; the two probes that asserted the
// opposite now assert that.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod review_pr53e_probe {
    use super::*;

    fn cs() -> Option<(PathBuf, PathBuf)> {
        Some((
            fs::canonicalize(std::env::var_os("RV5_CS")?).ok()?,
            fs::canonicalize(std::env::var_os("RV5_CS_ROOT")?).ok()?,
        ))
    }

    fn scratch(base: &Path, name: &str) -> PathBuf {
        let root = base.join(format!("rv5p-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    // A symlinked nearest existing ancestor: target_key resolves it, so the
    // probe sees the real directory on the real volume.
    #[test]
    fn rv5_probe_symlinked_ancestor_is_resolved_before_probing() {
        let Some((cs, _)) = cs() else { return };
        let root = scratch(&cs, "symlink");
        let real = root.join("Real");
        fs::create_dir(&real).unwrap();
        let tmp = scratch(&std::env::temp_dir(), "symlink-ci");
        std::os::unix::fs::symlink(&real, tmp.join("Link")).unwrap();
        let (key, _) = target_key(&tmp.join("Link/Sub/Leaf")).unwrap();
        let (probe_real, probe_link) = (
            probe_case_insensitive(&real),
            probe_case_insensitive(&tmp.join("Link")),
        );
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&tmp).unwrap();
        eprintln!("key={key:?} probe(real on cs)={probe_real} probe(link on ci)={probe_link}");
        assert_eq!(
            key,
            real.join("Sub/Leaf"),
            "not folded: the real volume is case-sensitive"
        );
        assert!(!probe_real);
    }

    // A case-flipped name that exists as a different entry on a case-sensitive
    // volume is a different inode: still case-sensitive.
    #[test]
    fn rv5_probe_flipped_name_existing_as_another_entry() {
        let Some((cs, _)) = cs() else { return };
        let root = scratch(&cs, "flipped");
        fs::create_dir(root.join("Tgt")).unwrap();
        fs::create_dir(root.join("tGT")).unwrap();
        fs::write(root.join("File"), b"a").unwrap();
        fs::write(root.join("fILE"), b"b").unwrap();
        let dirs = probe_case_insensitive(&root.join("Tgt"));
        let files = probe_case_insensitive(&root.join("File"));
        // A hard link under the flipped name (files only) is the same inode.
        fs::remove_file(root.join("fILE")).unwrap();
        fs::hard_link(root.join("File"), root.join("fILE")).unwrap();
        let hardlinked = probe_case_insensitive(&root.join("File"));
        fs::remove_dir_all(&root).unwrap();
        eprintln!("dirs={dirs} files={files} hardlinked={hardlinked}");
        assert!(!dirs && !files);
    }

    // A component that does not exist yet: nothing resolves, no fold, and
    // target_key probes only its existing ancestor.
    #[test]
    fn rv5_probe_missing_component() {
        let tmp = scratch(&std::env::temp_dir(), "missing");
        let missing = probe_case_insensitive(&tmp.join("NotThere"));
        let (key, _) = target_key(&tmp.join("NotThere/Deeper")).unwrap();
        let ci = probe_case_insensitive(&tmp);
        fs::remove_dir_all(&tmp).unwrap();
        eprintln!("missing={missing} tmp-probe={ci} key={key:?}");
        // R5-7: nothing resolves, so the probe cannot answer: insensitive.
        assert!(missing);
    }

    // A letterless directory directly under a case-sensitive volume's mount
    // root, whose mount point sits on a case-insensitive parent: the nearest
    // lettered component is the mount point, looked up in the parent volume.
    #[test]
    fn rv5_probe_across_a_mount_boundary() {
        let Some((_, cs_root)) = cs() else { return };
        let letterless = cs_root.join("1234");
        fs::create_dir_all(&letterless).unwrap();
        let probed = probe_case_insensitive(&letterless);
        let at_root = probe_case_insensitive(&cs_root);
        eprintln!(
            "probe(cs_root/1234)={probed} probe(cs_root)={at_root} (volume is case-sensitive)"
        );
        // R5-7: the walk would look the mount point up in the parent volume,
        // which answers for the wrong volume. It stops at the device change
        // and fails toward detecting overlap instead.
        assert!(probed && at_root);
    }
}

// The #53 round-6 reviewer demonstrator (R6-1), kept as a regression test and
// adapted to the three-valued case answer: an unanswerable probe (here, a
// real device boundary when the host has one) must not re-admit the R5-3
// case-mismatched nest plan.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod review_pr53f_r6 {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn r6_unanswerable_probe_refolds_the_source_relation() {
        let boundary = ["/Users", "/nix", "/home", "/tmp", "/boot", "/mnt"]
            .iter()
            .map(Path::new)
            .find(|p| {
                let (Ok(m), Ok(pm)) = (
                    fs::symlink_metadata(p),
                    fs::symlink_metadata(p.parent().unwrap()),
                ) else {
                    return false;
                };
                m.dev() != pm.dev()
            });
        if let Some(b) = boundary {
            assert_eq!(probe_case(b), Case::Unknown, "{b:?}");
        }
        let outer_src = Path::new("/src/outer");
        let inner_src = Path::new("/src/outer/Vendor/inner");
        let t = PathBuf::from("/mnt/T");
        let answered = overlapping(
            &(t.join("vendor/inner"), Case::Sensitive),
            inner_src,
            &(t.clone(), Case::Sensitive),
            outer_src,
        );
        let unanswerable = overlapping(
            &(t.join("vendor/inner"), Case::Unknown),
            inner_src,
            &(t, Case::Unknown),
            outer_src,
        );
        assert!(answered);
        assert!(
            unanswerable,
            "unanswerable probe re-admits the R5-3 case-mismatched nest plan"
        );
    }
}

// The #53 round-7 reviewer demonstrator (R7-1; R-N71, R-N83, R-N123),
// kept as regression tests.
#[cfg(test)]
mod review_pr53g_r7 {
    use super::*;

    const ALL: [Case; 3] = [Case::Sensitive, Case::Insensitive, Case::Unknown];

    fn ov(t: &str, ts: &str, ca: Case, o: &str, os: &str, cb: Case) -> bool {
        overlapping(
            &(PathBuf::from(t), ca),
            Path::new(ts),
            &(PathBuf::from(o), cb),
            Path::new(os),
        )
    }

    // Every answer pairing, both argument orders: the R5-3 plan refuses
    // unless both answers are positively Insensitive; the same-case plan
    // always passes.
    #[test]
    fn r7_pairing_table_both_directions() {
        for a in ALL {
            for b in ALL {
                let both_i = a == Case::Insensitive && b == Case::Insensitive;
                let fwd = ov(
                    "/mnt/T/vendor/inner",
                    "/src/outer/Vendor/inner",
                    a,
                    "/mnt/T",
                    "/src/outer",
                    b,
                );
                let rev = ov(
                    "/mnt/T",
                    "/src/outer",
                    a,
                    "/mnt/T/vendor/inner",
                    "/src/outer/Vendor/inner",
                    b,
                );
                eprintln!("R5-3 plan ({a:?},{b:?}) fwd_refused={fwd} rev_refused={rev}");
                assert_eq!(fwd, !both_i, "fwd {a:?} {b:?}");
                assert_eq!(rev, !both_i, "rev {a:?} {b:?}");
                let same_f = ov(
                    "/mnt/T/vendor/inner",
                    "/src/outer/vendor/inner",
                    a,
                    "/mnt/T",
                    "/src/outer",
                    b,
                );
                let same_r = ov(
                    "/mnt/T",
                    "/src/outer",
                    a,
                    "/mnt/T/vendor/inner",
                    "/src/outer/vendor/inner",
                    b,
                );
                assert!(!same_f && !same_r, "same-case plan refused at {a:?} {b:?}");
            }
        }
    }

    // Exhaustive monotonicity over a small universe: substituting a definite
    // answer for any Unknown never yields a refusal the Unknown result lacks.
    #[test]
    fn r7_unknown_is_never_less_refusing_than_a_definite_answer() {
        let targets = [
            "/m/T", "/m/t", "/m/T/v/i", "/m/t/v/i", "/m/T/V/i", "/m/t/V/i", "/m/T/x", "/m/t/X",
        ];
        let sources = ["/s/o", "/s/o/v/i", "/s/o/V/i", "/s/p"];
        let subst = |c: Case| -> Vec<Case> {
            if c == Case::Unknown {
                vec![Case::Sensitive, Case::Insensitive]
            } else {
                vec![c]
            }
        };
        let mut checked = 0usize;
        for t in targets {
            for ts in sources {
                for o in targets {
                    for os in sources {
                        for a in ALL {
                            for b in ALL {
                                let got = ov(t, ts, a, o, os, b);
                                for a2 in subst(a) {
                                    for b2 in subst(b) {
                                        let definite = ov(t, ts, a2, o, os, b2);
                                        assert!(got || !definite, "{t} {ts} {a:?} / {o} {os} {b:?}: definite ({a2:?},{b2:?}) refuses, unknown admits");
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!("monotonicity pairs checked: {checked}");
    }

    // R7-1: when the targets nest only case-folded (the outer prefix differs
    // in case), the relative path falls back to the FOLDED relative, so the
    // source relation is compared folded under (Unknown, Unknown) -- the
    // exact-comparison contract is not kept. Control: the same relative case
    // mismatch with a same-case prefix is refused.
    #[test]
    fn r7_fallback_folds_the_relation_when_the_prefix_differs_in_case() {
        let u = Case::Unknown;
        let control = ov(
            "/data/T/VENDOR/inner",
            "/src/outer/vendor/inner",
            u,
            "/data/T",
            "/src/outer",
            u,
        );
        assert!(control);
        // The seven pairs that are neither (Sensitive, Sensitive) nor
        // (Insensitive, Insensitive) nest the targets case-folded and compare
        // the relation exactly, so the relative VENDOR/inner never matches
        // vendor/inner however the outer prefix is spelled: refused, both
        // argument orders. Under (Sensitive, Sensitive) /data/t and /data/T
        // are different places, so there is nothing to refuse; under
        // (Insensitive, Insensitive) the relation matches case-folded.
        for a in ALL {
            for b in ALL {
                let both_s = a == Case::Sensitive && b == Case::Sensitive;
                let both_i = a == Case::Insensitive && b == Case::Insensitive;
                let expected = !both_s && !both_i;
                let fwd = ov(
                    "/data/t/VENDOR/inner",
                    "/src/outer/vendor/inner",
                    a,
                    "/data/T",
                    "/src/outer",
                    b,
                );
                let rev = ov(
                    "/data/T",
                    "/src/outer",
                    a,
                    "/data/t/VENDOR/inner",
                    "/src/outer/vendor/inner",
                    b,
                );
                assert_eq!(fwd, expected, "R7-1 fwd ({a:?},{b:?})");
                assert_eq!(rev, expected, "R7-1 rev ({a:?},{b:?})");
            }
        }
    }
}

// #101 (OI-1002-Q11) and #95 (OI-1002-Q5): per-item CORPUS space refusal in
// estate-capture, and git-repair-missing-index records read natively by the
// closure report.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod closure_lane_20261002 {
    use super::*;
    use crate::closure::{classify, Disposition};
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

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tcfs-estate-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn repository(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let source = root.join(name);
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), bytes).unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-q", "-m", "base"]);
        source
    }

    // 1 TiB filesystem with 32 KiB available: a small checkout fits, a
    // 64 KiB one does not.
    #[allow(clippy::unnecessary_wraps)]
    fn tight(_: &Path) -> Result<crate::space::Space> {
        Ok(crate::space::Space {
            total: 1 << 40,
            available: 32 * 1024,
        })
    }

    #[test]
    fn one_item_refuses_for_space_while_another_in_the_pass_captures() {
        let root = fresh("space-101");
        let small = repository(&root, "small", b"small");
        let large = repository(&root, "large", &vec![7u8; 64 * 1024]);
        let plan = root.join("plan");
        add(&plan, &small, &root.join("small-dest"), None).unwrap();
        add(&plan, &large, &root.join("large-dest"), None).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        private_directory(&state).unwrap();
        private_directory(&corpus).unwrap();
        let space = CorpusSpace {
            corpus: &corpus,
            floor: 0,
            probe: tight,
            reserved: Mutex::new(0),
        };
        let rows = Mutex::new(Vec::new());
        let outcome = capture_in(
            &plan,
            &state,
            &corpus,
            2,
            git_carry::CapturePolicy::default(),
            &space,
            &|row| {
                rows.lock()
                    .unwrap()
                    .push((row.source.clone(), row.outcome, row.reason.clone()));
                Ok(())
            },
        );
        // Any refused item makes the pass refuse, after every item ran.
        assert_eq!(outcome, Err(BulkloadRefusal::ContractSelfInconsistent));
        let mut rows = rows.into_inner().unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (
                    large.clone(),
                    "refused",
                    Some("DESTINATION_SPACE_INSUFFICIENT".to_owned())
                ),
                (small, "captured", None),
            ]
        );
        // Every reservation was released.
        assert_eq!(*space.reserved.lock().unwrap(), 0);
        // The refused item's durable receipt carries the typed code, so the
        // closure report counts it as a typed refusal pending review (S4).
        let read = ledger(&plan, &corpus, "neo", std::slice::from_ref(&state)).unwrap();
        let refused = read
            .entries
            .iter()
            .find(|entry| entry.source == large)
            .unwrap();
        assert_eq!(
            classify(refused),
            Disposition::RefusedPendingReview("DESTINATION_SPACE_INSUFFICIENT".into())
        );
        // The next pass, with room, captures the refused item and reuses the
        // other.
        let rows = Mutex::new(Vec::new());
        capture(&plan, &state, &corpus, 2, &|row| {
            rows.lock().unwrap().push((row.source.clone(), row.outcome));
            Ok(())
        })
        .unwrap();
        let rows = rows.into_inner().unwrap();
        assert!(rows.contains(&(large, "captured")), "{rows:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repaired_items_record_apply_style_outcomes_read_natively() {
        let root = fresh("repair-95");
        let source = repository(&root, "source", b"base");
        fs::write(source.join("file"), b"staged edit").unwrap();
        git(&source, &["add", "file"]);
        let destination = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &destination, None).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let contents: Plan = read(&plan).unwrap();
        let identity = id(contents.items.first().unwrap()).unwrap();
        let captured: Capture = read(&corpus.join(format!("{identity}.capture"))).unwrap();
        let bundle = corpus.join(&captured.bundle);
        // The destination already holds the payload; only its index is gone.
        git_carry::restore_bundle(&bundle, &destination, "neo").unwrap();
        fs::remove_file(destination.join(".git/index")).unwrap();
        let applied = root.join("applied");
        let led = RepairLedger {
            plan: &plan,
            corpus: &corpus,
            state: &applied,
        };
        // An unplanned repository binds no item: refused, nothing recorded.
        let elsewhere = repository(&root, "elsewhere", b"other");
        assert_eq!(
            repair_missing_index(&bundle, &elsewhere, "neo", &root.join("r0"), &led),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert!(!applied.join(format!("{identity}.outcome")).exists());

        repair_missing_index(&bundle, &destination, "neo", &root.join("r1"), &led).unwrap();
        let entry = |states: &[PathBuf]| {
            ledger(&plan, &corpus, "neo", states)
                .unwrap()
                .entries
                .into_iter()
                .next()
                .unwrap()
        };
        let repaired = entry(std::slice::from_ref(&applied));
        assert_eq!(
            repaired
                .record
                .as_ref()
                .and_then(|record| record.as_ref().ok())
                .map(|record| record.outcome.name()),
            Some(INDEX_REPAIRED)
        );
        assert_eq!(
            repaired.journal,
            JournalState::Present("refs-imported".into())
        );
        assert_eq!(classify(&repaired), Disposition::ReferencedOnly);

        // A second repair refuses (the index exists) and keeps the records.
        assert!(
            repair_missing_index(&bundle, &destination, "neo", &root.join("r2"), &led).is_err()
        );
        assert_eq!(
            classify(&entry(std::slice::from_ref(&applied))),
            Disposition::ReferencedOnly
        );
        // A later estate-apply sees the journal and replays nothing.
        let rows = Mutex::new(Vec::new());
        apply(&plan, &corpus, &applied, "neo", 1, &|row| {
            rows.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            rows.into_inner().unwrap(),
            vec!["previous-ref-custody-not-workspace-parity"]
        );

        // A repair that refuses after binding records the typed refusal.
        let refused_state = root.join("refused");
        let refused_led = RepairLedger {
            state: &refused_state,
            ..led
        };
        assert!(
            repair_missing_index(&bundle, &destination, "neo", &root.join("r3"), &refused_led)
                .is_err()
        );
        let refused = entry(std::slice::from_ref(&refused_state));
        assert!(
            matches!(classify(&refused), Disposition::RefusedPendingReview(_)),
            "{refused:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // #132: a repair run on the main repository of an item that plans a
    // linked workspace binds through `item.repository`. It must refuse
    // before writing anything, so a later estate-apply still restores the
    // workspace instead of replaying a refs-only journal.
    #[test]
    fn a_repair_never_binds_a_workspace_item_and_apply_still_restores_it() {
        let root = fresh("repair-132");
        let source = repository(&root, "source", b"base");
        fs::write(source.join("file"), b"staged edit").unwrap();
        git(&source, &["add", "file"]);
        let repository = root.join("repository");
        let workspace = root.join("linked");
        let plan = root.join("plan");
        add(&plan, &source, &repository, Some(&workspace)).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let contents: Plan = read(&plan).unwrap();
        let identity = id(contents.items.first().unwrap()).unwrap();
        let captured: Capture = read(&corpus.join(format!("{identity}.capture"))).unwrap();
        let bundle = corpus.join(&captured.bundle);
        // The main repository holds the captured HEAD with its index gone:
        // exactly what a repair accepts.
        git_carry::restore_bundle(&bundle, &repository, "neo").unwrap();
        fs::remove_file(repository.join(".git/index")).unwrap();
        let applied = root.join("applied");
        let led = RepairLedger {
            plan: &plan,
            corpus: &corpus,
            state: &applied,
        };
        let receipt = root.join("r1");
        assert_eq!(
            repair_missing_index(&bundle, &repository, "neo", &receipt, &led),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // Nothing was written: no receipt, no outcome, no journal, and the
        // index is still missing.
        assert!(!receipt.exists());
        assert!(!repository.join(".git/index").exists());
        let written: Vec<_> = fs::read_dir(&applied)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "estate.lock")
            .collect();
        assert!(written.is_empty(), "{written:?}");
        // The workspace is still pending, and estate-apply restores it.
        let rows = Mutex::new(Vec::new());
        apply(&plan, &corpus, &applied, "neo", 1, &|row| {
            rows.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        assert_eq!(rows.into_inner().unwrap(), vec!["workspace-restored"]);
        assert_eq!(fs::read(workspace.join("file")).unwrap(), b"staged edit");
        let entry = ledger(&plan, &corpus, "neo", std::slice::from_ref(&applied))
            .unwrap()
            .entries
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(classify(&entry), Disposition::Applied);
        fs::remove_dir_all(root).unwrap();
    }
}

// WP2 PR 2: auto-prerequisite chains (OI-1003-Q15, R-N13).
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod wp2_chain {
    use super::*;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
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
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?}");
    }

    // Whole-capture reuse needs seats older than one timestamp tick (R-N76).
    fn settle() {
        std::thread::sleep(std::time::Duration::from_nanos(
            u64::try_from(git_carry::RACY_GRANULARITY_NS).unwrap() + 100_000_000,
        ));
    }

    const CHAIN_HISTORY: usize = 64 * 1024;

    // Incompressible, so a re-packed history is visible in a bundle's size.
    fn chain_noise(len: usize, mut state: u32) -> Vec<u8> {
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    struct ChainFixture {
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
        plan: PathBuf,
        state: PathBuf,
        corpus: PathBuf,
    }

    impl Drop for ChainFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn chain_fixture(name: &str) -> ChainFixture {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "tcfs-estate-chain-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template=", "-b", "main"]);
        fs::write(source.join("history"), chain_noise(CHAIN_HISTORY, 11)).unwrap();
        fs::write(source.join("file-0"), b"base").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "base"]);
        let target = root.join("target");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        ChainFixture {
            state: root.join("state"),
            corpus: root.join("corpus"),
            source,
            target,
            plan,
            root,
        }
    }

    fn chain_record(fixture: &ChainFixture) -> Capture {
        let item = inspect(&fixture.plan).unwrap().remove(0);
        read(
            &fixture
                .corpus
                .join(format!("{}.capture", id(&item).unwrap())),
        )
        .unwrap()
    }

    // 0 for a self-contained bundle, else one more than its prior's depth.
    fn chain_depth(corpus: &Path, bundle: &str) -> u32 {
        let sidecar = prior_sidecar(corpus, bundle);
        if sidecar.exists() {
            read::<Prior>(&sidecar).unwrap().depth + 1
        } else {
            0
        }
    }

    fn heads(bundle: &Path) -> Vec<String> {
        let listed = Command::new("git")
            .args(["bundle", "list-heads"])
            .arg(bundle)
            .output()
            .unwrap();
        assert!(listed.status.success());
        let mut lines: Vec<String> = String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort_unstable();
        lines
    }

    fn head_of(repo: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[derive(Debug, Clone)]
    enum ChainStep {
        Commit(u8, u32),
        Branch(u8),
        Untracked(u32),
        Stage(u8, u32),
    }

    fn chain_step() -> impl proptest::strategy::Strategy<Value = ChainStep> {
        use proptest::prelude::*;
        prop_oneof![
            (0u8..3, any::<u32>()).prop_map(|(file, seed)| ChainStep::Commit(file, seed)),
            (0u8..2).prop_map(ChainStep::Branch),
            any::<u32>().prop_map(ChainStep::Untracked),
            (0u8..3, any::<u32>()).prop_map(|(file, seed)| ChainStep::Stage(file, seed)),
        ]
    }

    fn apply_step(source: &Path, step: &ChainStep) {
        match step {
            ChainStep::Commit(file, seed) => {
                let name = format!("file-{file}");
                fs::write(source.join(&name), format!("commit {seed}")).unwrap();
                git(source, &["add", &name]);
                git(source, &["commit", "--allow-empty", "-m", "step"]);
            }
            ChainStep::Branch(lane) => {
                git(source, &["branch", "-f", &format!("lane-{lane}"), "HEAD"]);
            }
            ChainStep::Untracked(seed) => {
                fs::write(source.join("notes"), format!("untracked {seed}")).unwrap();
            }
            ChainStep::Stage(file, seed) => {
                let name = format!("file-{file}");
                fs::write(source.join(&name), format!("staged {seed}")).unwrap();
                git(source, &["add", &name]);
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(crate::test_support::prop_config(6))]

        /// P-CHAIN (WP2, S3 + S4): ∀ sequences of commits, branch moves,
        /// staged and untracked edits, each followed by a capture:
        /// - every rerun chains on the retained capture and never re-packs
        ///   the incompressible history blob (its bundle stays far smaller);
        /// - the chain only grows by one per changed pass (bounded depth);
        /// - the flattened chain advertises exactly the refs a standalone
        ///   capture of the same state advertises;
        /// - apply restores the source's HEAD and bytes from the chain.
        #[test]
        fn a_capture_chain_packs_only_what_is_new_and_restores_exactly(
            steps in proptest::collection::vec(chain_step(), 1..=4)
        ) {
            let fixture = chain_fixture("prop");
            capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| Ok(())).unwrap();
            let first = chain_record(&fixture);
            proptest::prop_assert_eq!(chain_depth(&fixture.corpus, &first.bundle), 0);
            let full = fs::metadata(fixture.corpus.join(&first.bundle)).unwrap().len();
            proptest::prop_assert!(full >= CHAIN_HISTORY as u64);
            let mut previous = (first.bundle, 0u32);
            for step in &steps {
                apply_step(&fixture.source, step);
                capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| Ok(())).unwrap();
                let record = chain_record(&fixture);
                let depth = chain_depth(&fixture.corpus, &record.bundle);
                if record.bundle == previous.0 {
                    // Byte-identical to the retained bundle: same custody.
                    proptest::prop_assert_eq!(depth, previous.1);
                } else {
                    proptest::prop_assert_eq!(depth, previous.1 + 1, "{:?}", step);
                }
                let size = fs::metadata(fixture.corpus.join(&record.bundle)).unwrap().len();
                proptest::prop_assert!(
                    size < (CHAIN_HISTORY / 4) as u64,
                    "a chained bundle re-packed history: {} bytes after {:?}", size, step
                );
                previous = (record.bundle, depth);
            }
            let head = fixture.corpus.join(&previous.0);
            let flat = git_carry::chain::flatten(
                git_carry::stage_bundle(&head).unwrap(),
                &chain_links(&fixture.corpus, &previous.0, LinkBinding::Digest).unwrap(),
            )
            .unwrap();
            let standalone = git_carry::export_repository_with_policy(
                &fixture.source,
                &fixture.root.join("standalone"),
                None,
                git_carry::CapturePolicy::default(),
            )
            .unwrap();
            proptest::prop_assert_eq!(heads(flat.path()), heads(&standalone.bundle));
            proptest::prop_assert_eq!(heads(flat.path()), heads(&head));
            apply(&fixture.plan, &fixture.corpus, &fixture.root.join("applied"), "neo", 1, &|_| Ok(())).unwrap();
            proptest::prop_assert_eq!(head_of(&fixture.target), head_of(&fixture.source));
            for name in ["history", "file-0", "file-1", "file-2", "notes"] {
                let expected = fs::read(fixture.source.join(name)).ok();
                proptest::prop_assert_eq!(fs::read(fixture.target.join(name)).ok(), expected, "{}", name);
            }
        }
    }

    #[test]
    fn the_chain_re_bases_at_the_depth_limit() {
        let fixture = chain_fixture("limit");
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let mut depths = vec![chain_depth(&fixture.corpus, &chain_record(&fixture).bundle)];
        for pass in 0..=git_carry::chain::CHAIN_DEPTH_LIMIT {
            apply_step(&fixture.source, &ChainStep::Commit(0, pass));
            capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
                Ok(())
            })
            .unwrap();
            depths.push(chain_depth(&fixture.corpus, &chain_record(&fixture).bundle));
        }
        // 0 (first), 1..=LIMIT chained, then a self-contained re-base.
        let mut expected: Vec<u32> = (0..=git_carry::chain::CHAIN_DEPTH_LIMIT).collect();
        expected.push(0);
        assert_eq!(depths, expected);
        let record = chain_record(&fixture);
        assert!(!git_carry::shared::requires_base(&fixture.corpus.join(&record.bundle)).unwrap());
        apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(head_of(&fixture.target), head_of(&fixture.source));
    }

    #[test]
    fn a_broken_chain_refuses_apply_by_type_and_the_next_capture_re_bases() {
        let fixture = chain_fixture("broken");
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let root = chain_record(&fixture).bundle;
        apply_step(&fixture.source, &ChainStep::Commit(1, 7));
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let chained = chain_record(&fixture).bundle;
        assert_eq!(chain_depth(&fixture.corpus, &chained), 1);
        let held = fixture.root.join("held-root");
        fs::rename(fixture.corpus.join(&root), &held).unwrap();
        let reasons = Mutex::new(Vec::new());
        assert!(apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|row| {
                reasons.lock().unwrap().push(row.reason.clone());
                Ok(())
            }
        )
        .is_err());
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![Some(BulkloadRefusal::SealedObjectMissing.to_string())]
        );
        assert!(
            !fixture.target.exists(),
            "nothing is restored from a broken chain"
        );
        // The chained record is no longer a hit: the next pass writes a
        // self-contained bundle, which applies.
        settle();
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let rebased = chain_record(&fixture).bundle;
        assert_ne!(rebased, chained);
        assert_eq!(chain_depth(&fixture.corpus, &rebased), 0);
        apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied-2"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(head_of(&fixture.target), head_of(&fixture.source));
    }

    #[test]
    fn a_replaced_chain_link_refuses_by_type() {
        let fixture = chain_fixture("replaced");
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let root = chain_record(&fixture).bundle;
        apply_step(&fixture.source, &ChainStep::Commit(2, 9));
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let chained = chain_record(&fixture).bundle;
        // Same bytes, new inode: the recorded identity no longer matches.
        let path = fixture.corpus.join(&root);
        let bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            chain_links(&fixture.corpus, &chained, LinkBinding::Custody).err(),
            Some(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // Capture never extends or reuses it, but the bytes are the recorded
        // ones, so restore (bound by name and digest) still applies.
        assert!(chain_links(&fixture.corpus, &chained, LinkBinding::Digest).is_ok());
        apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(head_of(&fixture.target), head_of(&fixture.source));
    }

    // A chained capture of `fixture` (depth 1), with its corpus copied as
    // `pull` copies one to the apply host: every file a new inode, and a new
    // ctime, so no link sits at its recorded `StatIdentity`.
    fn chained_and_pulled(fixture: &ChainFixture) -> (String, String, PathBuf) {
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let root = chain_record(fixture).bundle;
        apply_step(&fixture.source, &ChainStep::Commit(1, 5));
        capture(&fixture.plan, &fixture.state, &fixture.corpus, 1, &|_| {
            Ok(())
        })
        .unwrap();
        let chained = chain_record(fixture).bundle;
        assert_eq!(chain_depth(&fixture.corpus, &chained), 1);
        let pulled = fixture.root.join("pulled");
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&fixture.corpus)
            .arg(&pulled)
            .status()
            .unwrap();
        assert!(copied.success());
        (root, chained, pulled)
    }

    /// R-N72 restore custody: a chain captured on one host applies from a
    /// pulled copy of its corpus on another. Links bind by name and digest
    /// at apply; the capture-side identity check refuses only the reuse.
    #[test]
    fn a_chained_capture_applies_from_a_pulled_corpus() {
        let fixture = chain_fixture("pulled");
        let (_, chained, pulled) = chained_and_pulled(&fixture);
        assert_eq!(
            chain_links(&pulled, &chained, LinkBinding::Custody).err(),
            Some(BulkloadRefusal::ReceiptBindingInvalid),
            "the copy must move every link off its recorded identity"
        );
        // The space preflight charges the whole chain from the copy, too.
        let item = inspect(&fixture.plan).unwrap().remove(0);
        let (_, charged) = item_space(&item, &pulled, &fixture.root.join("applied"), "neo")
            .unwrap()
            .expect("the pulled chained capture is pending");
        assert!(charged > fs::metadata(pulled.join(&chained)).unwrap().len());
        let reasons = Mutex::new(Vec::new());
        apply(
            &fixture.plan,
            &pulled,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|row| {
                reasons.lock().unwrap().push(row.reason.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*reasons.lock().unwrap(), vec![None]);
        assert_eq!(head_of(&fixture.target), head_of(&fixture.source));
        for name in ["history", "file-0", "file-1"] {
            assert_eq!(
                fs::read(fixture.target.join(name)).unwrap(),
                fs::read(fixture.source.join(name)).unwrap(),
                "{name}"
            );
        }
    }

    /// Digest binding is still binding: a pulled link whose bytes are not
    /// the recorded ones refuses `DIGEST_MISMATCH` and restores nothing.
    #[test]
    fn a_pulled_chain_link_with_other_bytes_refuses_by_type() {
        let fixture = chain_fixture("pulled-tampered");
        let (root, _, pulled) = chained_and_pulled(&fixture);
        let link = pulled.join(&root);
        let mut bytes = fs::read(&link).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&link, bytes).unwrap();
        let reasons = Mutex::new(Vec::new());
        assert!(apply(
            &fixture.plan,
            &pulled,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|row| {
                reasons.lock().unwrap().push(row.reason.clone());
                Ok(())
            }
        )
        .is_err());
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![Some(BulkloadRefusal::DigestMismatch.to_string())]
        );
        assert!(!fixture.target.exists(), "nothing is restored");
    }
}
