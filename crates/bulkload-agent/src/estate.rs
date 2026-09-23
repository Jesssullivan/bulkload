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
    /// One line per foreign nested repository or gitlink the capture names
    /// as custody and does not carry (R-N73), from
    /// [`git_carry::NestedRepository::receipt_line`]. Empty for a repository
    /// without nests. The durable statement is the `{bundle}.nested` sidecar.
    pub nested: Vec<String>,
}

/// One item's completed operation and the nests its capture did not carry.
struct Completion {
    outcome: &'static str,
    /// Receipt lines, one per nest, already naming any carrying item.
    nested: Vec<String>,
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
    // The key covers the nested custody, so a reuse hit names exactly the
    // nests the retained capture recorded.
    let (key, nested) =
        git_carry::reusable_capture_key_with_planned(&item.source, policy, &planned)?;
    if record.try_exists()? {
        let previous: Capture = read(&record)?;
        if !filename(&previous.bundle) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let bundle = corpus.join(&previous.bundle);
        if previous.key == key
            && bundle.try_exists()?
            && previous.identity
                == crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(&bundle)?)
        {
            if git_carry::shared::requires_base(&bundle)? {
                let bound: Base = read(&corpus.join(format!("{}.base", previous.bundle)))?;
                if !retained_base(corpus, &bound)? {
                    return Err(BulkloadRefusal::ReceiptBindingInvalid);
                }
            }
            return Ok(Completion {
                outcome: "capture-reused-after-census",
                nested: nest_lines(item, owners, &nested),
            });
        }
    }
    let mut generation = 0u64;
    let attempt = loop {
        let candidate = state.join(format!(
            "{identity}-{}-{generation}",
            blake3::Hash::from_bytes(key).to_hex()
        ));
        if !candidate.try_exists()? {
            break candidate;
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    };
    // Failed private attempts are retained, never silently overwritten.
    let prerequisite = base.map(|base| base_path(corpus, base)).transpose()?;
    let export = git_carry::export_repository_with_planned(
        &item.source,
        &attempt,
        prerequisite.as_deref(),
        policy,
        &planned,
    )?;
    if key != git_carry::reusable_capture_key_with_planned(&item.source, policy, &planned)?.0
        || export.nested_repositories != nested
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let bundle = export.bundle;
    let digest = hash_file(&bundle)?;
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
        fs::hard_link(&bundle, &published)?;
    }
    // Completion may survive a crash only after its bundle bytes are durable.
    fs::File::open(&published)?.sync_all()?;
    let metadata = fs::symlink_metadata(&published)?;
    if let Some(base) = base {
        // Publish dependency custody before the unchanged completion codec.
        write(&corpus.join(format!("{name}.base")), base)?;
    }
    // A separate sidecar, as `.base` is: the Capture postcard is positional
    // and gains no field. Written only when there is custody to record.
    if !nested.is_empty() {
        write(&corpus.join(format!("{name}.nested")), &nested)?;
    }
    write(
        &record,
        &Capture {
            key,
            bundle: name,
            digest,
            identity: crate::freshness::StatIdentity::from_metadata(&metadata),
        },
    )?;
    Ok(Completion {
        outcome: "captured",
        nested: nest_lines(item, owners, &nested),
    })
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
            let (outcome, reason, nested) = match operation(item) {
                Ok(done) => {
                    // The count rides in the layout-safe reason (and so in the
                    // durable `.outcome` tuple); the rows ride in the sidecar
                    // and the in-process receipt lines.
                    let reason =
                        (!done.nested.is_empty()).then(|| format!("nested={}", done.nested.len()));
                    (done.outcome, reason, done.nested)
                }
                Err(error) => {
                    refused.store(true, std::sync::atomic::Ordering::Relaxed);
                    ("refused", Some(error.to_string()), Vec::new())
                }
            };
            receipt(&Receipt {
                item: id(item)?,
                source: item.source.clone(),
                outcome,
                reason,
                nested,
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

fn import_base(
    item: &Item,
    captured: &Capture,
    corpus: &Path,
    source: &str,
    imported: &ImportedBases,
) -> Result<()> {
    let bundle = corpus.join(&captured.bundle);
    if !git_carry::shared::requires_base(&bundle)? {
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
    if hash_file(&path)? != base.digest || git_carry::shared::requires_base(&path)? {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    git_carry::import_bundle(&item.repository, &path, source)?;
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
        return Ok(Completion { outcome, nested });
    }
    let bundle = corpus.join(&captured.bundle);
    if hash_file(&bundle)? != captured.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    import_base(item, &captured, corpus, source, imported)?;
    let outcome = if let Some(workspace) = &item.workspace {
        if item.repository == *workspace {
            git_carry::restore_bundle(&bundle, workspace, source)?;
        } else {
            git_carry::restore_linked(&bundle, &item.repository, workspace, source)?;
        }
        "workspace-restored"
    } else {
        git_carry::import_bundle(&item.repository, &bundle, source)?;
        "refs-imported"
    };
    write(&journal, &outcome.to_owned())?;
    Ok(Completion { outcome, nested })
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
        capture(&plan, &state, &corpus, 1, &record).unwrap();
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
}
