//! Explicit reviewed Git batches. Capture reuse still performs a metadata census.
//!
//! Ref custody and usable restored workspaces are separate outcomes. Completed
//! restores are never replayed over subsequent operator edits: a re-apply of
//! a changed capture converges a landed checkout only when its ledger proves
//! nobody changed it, and refuses `GIT_DESTINATION_MODIFIED` otherwise
//! (`git_carry::converge`, estate converge 2026-10-08).

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

use crate::git_carry::decide;
use crate::{git_carry, BulkloadRefusal, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// What a chained capture's bundle depends on (R-N72): every bundle a
/// restore must hold beside the head, each with its recorded digest.
#[derive(Debug, Default)]
struct Chain {
    /// The bundles it chains on, oldest first.
    links: Vec<(PathBuf, [u8; 32])>,
    /// The plan bases the head and its links are bound to (L6b's fix 2),
    /// the oldest link's first, each once. A v1 chain binds none.
    bases: Vec<(PathBuf, [u8; 32])>,
}

// The base `{bundle}.base` binds, if the capture published that sidecar.
fn bound(corpus: &Path, bundle: &str) -> Result<Option<Base>> {
    let sidecar = corpus.join(format!("{bundle}.base"));
    if !sidecar.try_exists().refuse_at("estate::bound")? {
        return Ok(None);
    }
    Ok(Some(read(&sidecar)?))
}

// One bound base of a chain, checked as `chain_links` checks a link: it
// must be in the corpus and, under `Custody`, at its recorded identity.
fn bind_base(
    corpus: &Path,
    base: &Base,
    binding: LinkBinding,
    bases: &mut Vec<(PathBuf, [u8; 32])>,
) -> Result<()> {
    let path = base_path(corpus, base)?;
    if !path.try_exists().refuse_at("estate::bind_base")? {
        return Err(BulkloadRefusal::SealedObjectMissing);
    }
    if binding == LinkBinding::Custody && !retained_base(corpus, base)? {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let entry = (path, base.digest);
    if !bases.contains(&entry) {
        bases.push(entry);
    }
    Ok(())
}

/// The bundles a chained capture's bundle depends on: its links, oldest
/// first, and the plan bases they and the head are bound to, each with its
/// recorded digest. Both are empty for a bundle without a `.prior` sidecar.
///
/// Every link must be retained (and, under [`LinkBinding::Custody`], at its
/// recorded identity), and depths must fall by exactly one per link to a
/// depth-0 root within [`git_carry::chain::CHAIN_DEPTH_LIMIT`]; the walk is
/// therefore bounded. Digests and prerequisites are checked when the chain is
/// flattened.
///
/// **Bound bases (L6b's fix 2).** The head and every link that has a
/// `.prior` is bound to the base its `.base` sidecar names, if it has one.
/// The root is bound to its `.base` only when its header declares
/// prerequisites: a root written self-contained (a shallow source, or a thin
/// header over the cap) needs no base. Each bound base must be retained
/// exactly as a link must, so under `Custody` a chain whose base is gone or
/// replaced is not intact: it is neither a hit nor extended. One group has
/// one base, so the set has one entry; it has more only when a base record
/// went missing and a later capture was bound to a new base while its chain
/// still binds the old one (OI-1003-Q63 D5).
///
/// # Errors
/// `SEALED_OBJECT_MISSING` for a link or a bound base the corpus no longer
/// holds, `RECEIPT_BINDING_INVALID` for inconsistent depths or (under
/// `Custody`) a replaced link or base, and `PATH_ESCAPES_ROOT` for a link or
/// base name that is not a corpus file name.
fn chain_links(corpus: &Path, bundle: &str, binding: LinkBinding) -> Result<Chain> {
    let mut links = Vec::new();
    let mut bases = Vec::new();
    let mut current = bundle.to_owned();
    let mut expected: Option<u32> = None;
    loop {
        let sidecar = prior_sidecar(corpus, &current);
        if !sidecar.try_exists().refuse_at("estate::chain_links")? {
            // `expected` is the depth `current`'s own link must record: a
            // chained bundle that lost its link breaks the chain. Only an
            // unchained head ends the walk here; a root ends it below.
            return match expected {
                None => Ok(Chain::default()),
                Some(_) => Err(BulkloadRefusal::ReceiptBindingInvalid),
            };
        }
        // `current` is chained: the base it is bound to, if any.
        if let Some(base) = bound(corpus, &current)? {
            bind_base(corpus, &base, binding, &mut bases)?;
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
        if prior.depth == 0 {
            if prior_sidecar(corpus, &prior.bundle)
                .try_exists()
                .refuse_at("estate::chain_links")?
            {
                return Err(BulkloadRefusal::ReceiptBindingInvalid);
            }
            // The root: bound to its base only when it is that base's delta.
            if let Some(base) = bound(corpus, &prior.bundle)? {
                if git_carry::shared::requires_base(&path)? {
                    bind_base(corpus, &base, binding, &mut bases)?;
                }
            }
            links.push((path, prior.digest));
            links.reverse();
            bases.reverse();
            return Ok(Chain { links, bases });
        }
        links.push((path, prior.digest));
        expected = Some(prior.depth - 1);
        current = prior.bundle;
    }
}

/// What the decision core needs to know to chain a new capture onto
/// `previous` (`decide`): its shape (a chain link, a shared plan base's
/// delta, or self-contained), whether its chain is intact under
/// [`LinkBinding::Custody`] (every link and every bound base retained), and
/// its depth. Whether it chains is the decision's: never on a broken chain,
/// and only below the depth limit (the capture at the limit re-bases).
fn chainable(
    corpus: &Path,
    previous: &Capture,
    bundle: &Path,
) -> Result<(decide::Shape, bool, u32)> {
    if prior_sidecar(corpus, &previous.bundle)
        .try_exists()
        .refuse_at("estate::chainable")?
    {
        return Ok(
            match chain_links(corpus, &previous.bundle, LinkBinding::Custody) {
                Ok(chain) => (
                    decide::Shape::Chained,
                    true,
                    u32::try_from(chain.links.len())
                        .map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                ),
                // A broken chain is neither a hit nor extended.
                Err(_) => (decide::Shape::Chained, false, 0),
            },
        );
    }
    // No `.prior`: `chain_links` finds no link to break.
    Ok(if git_carry::shared::requires_base(bundle)? {
        (decide::Shape::Based, true, 0)
    } else {
        (decide::Shape::Unchained, true, 0)
    })
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
    /// An apply's ref and seat custody counts (estate converge), as
    /// `key=value` words carried in the receipt's reason.
    custody: Option<String>,
}

impl Completion {
    const fn clean(outcome: &'static str) -> Self {
        Self {
            outcome,
            drift: Vec::new(),
            bytes_read: 0,
            reuse_unavailable: None,
            nested: Vec::new(),
            custody: None,
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

// #226 item 5: the `{bundle}.omissions` sidecar, the rebuildable roots a
// capture omitted with their measured entries and bytes. Written beside the
// other sidecars before the record; a reuse hit keeps its capture's.
fn omissions_sidecar(corpus: &Path, bundle: &str) -> PathBuf {
    corpus.join(format!("{bundle}.omissions"))
}

/// The rebuildable roots a capture omitted, as its `.omissions` sidecar
/// records them (#226 item 5); `None` for a capture taken before the
/// sidecar existed, or one whose sidecar is gone or unreadable: unknown,
/// never zero.
fn retained_omissions(corpus: &Path, bundle: &str) -> Option<Vec<git_carry::RebuildableOmission>> {
    let path = omissions_sidecar(corpus, bundle);
    if !path.is_file() {
        return None;
    }
    read(&path).ok()
}

/// An item's omissions for the closure report, and why they are unknown when
/// they are: the capture's `.omissions` sidecar; without one (a capture from
/// before it, `3fe1f075`), what the bundle carries in band, its omission
/// list read by oid from the item's destination repository once its import
/// ran, when `read_destinations` (M2 round 2, d). Reads only.
fn item_omissions(
    item: &Item,
    corpus: &Path,
    bundle: &str,
    read_destinations: bool,
) -> (
    Option<Vec<git_carry::RebuildableOmission>>,
    Option<&'static str>,
) {
    let sidecar = omissions_sidecar(corpus, bundle);
    if sidecar.is_file() {
        return read(&sidecar).map_or((None, Some("sidecar-unreadable")), |omitted| {
            (Some(omitted), None)
        });
    }
    let holder = read_destinations.then_some(item.repository.as_path());
    match git_carry::in_band::omissions(&corpus.join(bundle), holder) {
        git_carry::in_band::InBand::Known(omitted) => (Some(omitted), None),
        git_carry::in_band::InBand::Unknown(reason) => (None, Some(reason)),
    }
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

/// A capture's `{bundle}.reuse` sidecar (Q42 lane L7, OI-1003-Q42): the
/// regular seats its bundle holds, each with its blob, bound to the bundle
/// by digest. A later pass reuses those blobs from this list, looked up in
/// the source object store, and fetches the bundle only on a miss (P69).
///
/// **Advisory.** The bundle stays complete and self-describing: its own
/// `filesystem-v1` rows and worktree tree say the same, and a restore never
/// reads this file. A reader that does not know the sidecar (any engine
/// before lane L7) ignores it and fetches the bundle, as it always did.
///
/// **Order (P70).** It is durable before the `{item}.capture` record that
/// names its bundle, like every other sidecar, so a record written by this
/// engine never lacks its manifest. A crash in between leaves a manifest
/// whose bundle no record names: harmless, because a manifest is only ever
/// looked up under the name the record gives, and is used only when its
/// digest is the record's.
#[derive(Serialize, Deserialize)]
struct ReuseManifest {
    // blake3 of the bundle the seats were listed from: the record's digest.
    digest: [u8; 32],
    seats: Vec<git_carry::ManifestSeat>,
}

/// The largest manifest written or read. A capture whose list is larger
/// publishes none, and its next pass fetches the bundle, as before lane L7.
const REUSE_MANIFEST_CAP: usize = 256 * 1024 * 1024;

fn reuse_sidecar(corpus: &Path, bundle: &str) -> PathBuf {
    corpus.join(format!("{bundle}.reuse"))
}

/// What a retained capture's `.reuse` sidecar offers the next pass.
enum Listing {
    /// No sidecar: a capture from before lane L7 (or one over the cap). Its
    /// bundle is fetched for reuse, as it always was.
    Absent,
    /// The seats of a manifest bound to the record's bundle.
    Bound(Vec<git_carry::ManifestSeat>),
    /// A sidecar that cannot be read, does not decode or names another
    /// bundle. Nothing is reused from it (`reuse_unavailable=
    /// manifest-mismatch`); it is never a refusal of the capture.
    Mismatch,
}

// The manifest of the capture `previous` records, read in full and counted
// (`read_reuse_manifest_bytes`). Never an error: the manifest is advisory.
fn retained_manifest(corpus: &Path, previous: &Capture) -> Listing {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(reuse_sidecar(corpus, &previous.bundle))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Listing::Absent,
        Err(_) => return Listing::Mismatch,
    };
    let mut bytes = Vec::new();
    let limit = u64::try_from(REUSE_MANIFEST_CAP).map_or(u64::MAX, |cap| cap.saturating_add(1));
    let read = file.take(limit).read_to_end(&mut bytes);
    crate::counters::add_len(crate::counters::Counter::ReuseManifestRead, bytes.len());
    if read.is_err() || bytes.len() > REUSE_MANIFEST_CAP {
        return Listing::Mismatch;
    }
    match postcard::from_bytes::<ReuseManifest>(&bytes) {
        Ok(manifest) if manifest.digest == previous.digest => Listing::Bound(manifest.seats),
        Ok(_) | Err(_) => Listing::Mismatch,
    }
}

// The manifest a plan that reuses the retained capture's blobs reads; no
// other plan reads one.
fn offered_manifest(corpus: &Path, plan: decide::Plan, retained: Option<&Retained>) -> Listing {
    match (plan.reuse, retained) {
        (decide::ReuseEligibility::BlobReuse, Some(held)) => {
            retained_manifest(corpus, &held.previous)
        }
        _ => Listing::Absent,
    }
}

// The reuse manifest of the bundle published as `name` with `digest`, durable
// before the record names that bundle (P70). A shallow capture has none, and
// a list over the cap is not published.
fn publish_reuse(
    corpus: &Path,
    name: &str,
    digest: [u8; 32],
    seats: Option<Vec<git_carry::ManifestSeat>>,
) -> Result<()> {
    let Some(seats) = seats else {
        return Ok(());
    };
    let bytes = postcard::to_allocvec(&ReuseManifest { digest, seats })
        .map_err(|_| BulkloadRefusal::FrameCodec)?;
    if bytes.len() > REUSE_MANIFEST_CAP {
        return Ok(());
    }
    write_bytes(&reuse_sidecar(corpus, name), &bytes)
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
    write_bytes(
        path,
        &postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// [`write`] of a record already encoded. Also the transfer closure
// record's writer (`transfer_outcome::write`, WP3 PR 4).
pub(crate) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
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
    file.write_all(bytes).refuse_at("estate::write")?;
    file.sync_file_counted().refuse_at("estate::write")?;
    fs::rename(&temporary, path).refuse_at("estate::write")?;
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at("estate::write")?
        .sync_dir_counted()
        .refuse_at("estate::write")?;
    Ok(())
}

// [`write`] for a record that must never replace another: the temporary
// file is linked to `path`, which fails if `path` exists, where `write`'s
// rename would replace it. `RECEIPT_BINDING_INVALID` when `path` exists; it
// is left as it is.
fn write_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    const SITE: &str = "estate::write_new";
    let bytes = postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)?;
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
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    };
    file.write_all(&bytes).refuse_at(SITE)?;
    file.sync_file_counted().refuse_at(SITE)?;
    let linked = fs::hard_link(&temporary, path);
    // The temporary name is this call's own; the record is the link.
    fs::remove_file(&temporary).refuse_at(SITE)?;
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at(SITE)?
        .sync_dir_counted()
        .refuse_at(SITE)?;
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

/// An item's identity (`estate::id`): the name its capture record, outcome
/// record and journal carry, and what `estate-carry-files --item` takes.
///
/// # Errors
/// `FRAME_CODEC` when the item does not encode.
pub fn item_id(item: &Item) -> Result<String> {
    id(item)
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

// The decision on a group whose base record names a lost bundle. It comes
// before any item's record is read: the decision core's first rule reads
// only the base, so every item of the group refuses
// `RECEIPT_BINDING_INVALID` (P67 draws every other input of that row).
fn lost_base() -> BulkloadRefusal {
    let inputs = decide::Inputs::new(true, decide::BaseState::Lost, decide::Policy::CODE);
    match decide::decide(&inputs) {
        decide::Decision::Refuse(refusal) => refusal.into(),
        decide::Decision::Hit | decide::Decision::Export(_) => {
            BulkloadRefusal::ContractSelfInconsistent
        }
    }
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
        return Err(lost_base());
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
    #[cfg(test)]
    git_carry::mid_pass::fire(
        &fs::canonicalize(&item.source).refuse_at("estate::prepare_base")?,
        git_carry::mid_pass::Stage::BasePublished,
    );
    // Never replace a base record (R-N72): every delta bound to the base it
    // names depends on it. A record that appeared since the check above was
    // written by another capture of this corpus; it stands, and this pass
    // refuses rather than bind its items to a base the record does not name.
    write_new(&record, &base)?;
    Ok(base)
}

/// A retained capture of this checkout, at its recorded identity: what
/// acting on the decision needs beside the decision's inputs.
struct Retained {
    /// Its record.
    previous: Capture,
    /// Its bundle in the corpus. Every seat at an unchanged, non-racy
    /// `StatIdentity` may be reused from it at zero source bytes (R25).
    bundle: PathBuf,
    /// Its recorded pass start, absent for a capture from before that was
    /// recorded.
    started_ns: Option<i128>,
    /// It drifted and the difference is confined to the ref inventory and
    /// the worktree census, so this pass completes it.
    extends: bool,
}

/// The item's capture record as the decision core reads it: `inputs` with
/// the record's state and, for a retained bundle, its key, drift, pass start
/// and chain filled in; and the retained capture, if any. Nothing here
/// decides. The retained bundle's bound base is read later, and only when
/// the decision rests on it (`decide::reads_prev_base`).
fn retained_capture(
    record: &Path,
    corpus: &Path,
    parts: &git_carry::KeyParts,
    key: [u8; 32],
    authority: [u8; 32],
    inputs: decide::Inputs,
) -> Result<(decide::Inputs, Option<Retained>)> {
    if !record.try_exists().refuse_at("estate::retained_capture")? {
        return Ok((
            decide::Inputs {
                retained: decide::RetainedState::NoRecord,
                ..inputs
            },
            None,
        ));
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
        return Ok((
            decide::Inputs {
                retained: decide::RetainedState::BundleGone,
                ..inputs
            },
            None,
        ));
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
    // A chained bundle is a hit only while its whole chain is retained: a
    // broken chain recaptures (self-contained) instead of standing as custody
    // no restore can satisfy.
    let (shape, chain_intact, depth) = chainable(corpus, &previous, &bundle)?;
    let extends = !drift.is_empty()
        && recorded
            .as_ref()
            .is_some_and(|recorded| recorded.authority == authority);
    let inputs = decide::Inputs {
        retained: decide::RetainedState::Held,
        key_equal: previous.key == key,
        drifted: !drift.is_empty(),
        settled,
        pass_start: recorded.is_some(),
        shape,
        chain_intact,
        depth,
        // v1 never re-roots, so a chain's root is as old as it is deep.
        age: depth,
        ..inputs
    };
    Ok((
        inputs,
        Some(Retained {
            previous,
            bundle,
            started_ns: recorded.map(|recorded| recorded.started_ns),
            extends,
        }),
    ))
}

// The bases a retained bundle is bound to: none, all retained at their
// recorded identities, or one lost. A bundle with no `.prior` is bound to
// the base its own `{bundle}.base` names. With no such sidecar it is bound
// to none only when its header declares no prerequisites (a self-contained
// capture). A bundle that declares prerequisites and has neither sidecar is
// a based capture whose custody record is gone: its base is lost, exactly
// as apply reads it (`import_base` refuses `SEALED_OBJECT_MISSING`), so it
// is never a reuse hit and never a chain link. A chained bundle is bound to
// its own base and to every link's (`chain_links`), and one lost base loses
// it.
fn bound_base(corpus: &Path, bundle: &str) -> Result<decide::PrevBase> {
    if prior_sidecar(corpus, bundle)
        .try_exists()
        .refuse_at("estate::bound_base")?
    {
        return match chain_links(corpus, bundle, LinkBinding::Custody) {
            Ok(chain) if chain.bases.is_empty() => Ok(decide::PrevBase::None),
            Ok(_) => Ok(decide::PrevBase::Retained),
            // A base or a link that is gone or replaced: nothing restores it.
            Err(BulkloadRefusal::ReceiptBindingInvalid | BulkloadRefusal::SealedObjectMissing) => {
                Ok(decide::PrevBase::Lost)
            }
            Err(refusal) => Err(refusal),
        };
    }
    Ok(match bound(corpus, bundle)? {
        None if git_carry::shared::requires_base(&corpus.join(bundle))? => decide::PrevBase::Lost,
        None => decide::PrevBase::None,
        Some(base) if retained_base(corpus, &base)? => decide::PrevBase::Retained,
        Some(_) => decide::PrevBase::Lost,
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

// The blob-reuse offer the plan makes (R25): the retained capture with its
// recorded pass start and, when its `.reuse` sidecar is bound to it, its
// manifest (Q42 lane L7); or why none is made. Without a recorded pass start
// no retained seat can be proved non-racy, and a manifest that does not
// match its capture proves nothing about it.
fn reuse_offer<'a>(
    plan: decide::Plan,
    retained: Option<&'a Retained>,
    listing: &'a Listing,
) -> Result<(
    Option<git_carry::RetainedCapture<'a>>,
    Option<git_carry::ReuseUnavailable>,
)> {
    Ok(match plan.reuse {
        decide::ReuseEligibility::NoRetained => (None, None),
        decide::ReuseEligibility::BlobReuse => {
            let retained = retained.ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
            let manifest = match listing {
                Listing::Mismatch => {
                    return Ok((None, Some(git_carry::ReuseUnavailable::ManifestMismatch)));
                }
                Listing::Absent => None,
                Listing::Bound(seats) => Some(seats.as_slice()),
            };
            (
                Some(git_carry::RetainedCapture {
                    bundle: &retained.bundle,
                    started_ns: retained
                        .started_ns
                        .ok_or(BulkloadRefusal::ContractSelfInconsistent)?,
                    manifest,
                }),
                None,
            )
        }
        decide::ReuseEligibility::PassStartUnrecorded => {
            (None, Some(git_carry::ReuseUnavailable::PassStartUnrecorded))
        }
    })
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
    probe: fn(&Path) -> Result<crate::space::Limits>,
    reserved: Mutex<u64>,
}

impl<'a> CorpusSpace<'a> {
    fn live(corpus: &'a Path) -> Self {
        Self {
            corpus,
            floor: crate::space::min_free_percent(),
            probe: crate::space::probe_limits,
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
            let limits = (self.probe)(self.corpus)?;
            let planned = reserved.saturating_add(bytes);
            crate::space::check_at(self.corpus, (planned, planned), limits, self.floor)?;
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

// The decision on one item (OI-1003-Q43), from the group's `base` and the
// item's record: a reuse hit, a typed refusal, or an export and what it may
// depend on. The writer decides again on what its pass observes
// (`git_carry::shared::write_capture`). Returns the decision, the inputs it
// read (among them the retained bundle's depth, which a chain link records)
// and the retained capture.
fn decide_capture(
    record: &Path,
    corpus: &Path,
    parts: &git_carry::KeyParts,
    key: [u8; 32],
    authority: [u8; 32],
    base: Option<&Base>,
    policy: decide::Policy,
) -> Result<(decide::Decision, decide::Inputs, Option<Retained>)> {
    let state = if base.is_some() {
        decide::BaseState::Retained
    } else {
        decide::BaseState::NoGroup
    };
    let (recorded, retained) = retained_capture(
        record,
        corpus,
        parts,
        key,
        authority,
        decide::Inputs::new(base.is_some(), state, policy),
    )?;
    // A hit on a bundle bound to a base needs that base retained. The core
    // asks for it only then, which is only for a held record.
    let (decision, inputs) = decide::decide_recorded(recorded, || {
        let held = retained
            .as_ref()
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        bound_base(corpus, &held.previous.bundle)
    })?;
    Ok((decision, inputs, retained))
}

#[allow(clippy::too_many_arguments)] // Pass-wide state is caller-owned.
#[allow(clippy::too_many_lines)]
fn capture_item(
    item: &Item,
    state: &Path,
    corpus: &Path,
    base: Option<&Base>,
    (policy, decision): (git_carry::CapturePolicy, decide::Policy),
    owners: &Owners,
    refused: &Mutex<std::collections::BTreeSet<PathBuf>>,
    space: &CorpusSpace<'_>,
) -> Result<Completion> {
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
    let (decision, inputs, retained) =
        decide_capture(&record, corpus, &parts, key, authority, base, decision)?;
    let plan = match decision {
        decide::Decision::Hit => {
            return Ok(Completion::clean("capture-reused-after-census").naming(nested));
        }
        decide::Decision::Refuse(refusal) => return Err(refusal.into()),
        decide::Decision::Export(plan) => plan,
    };
    let extends = retained.as_ref().is_some_and(|held| held.extends);
    // WP2: pack only what is new since the retained capture, by declaring its
    // source-held tips as prerequisites, when the plan chains on it. Under a
    // plan base too (L6b's fix 2): the writer then declares both.
    let (link, chain) = chain_offer(corpus, plan, retained.as_ref(), &inputs)?;
    let retained_bundle = retained.as_ref().map(|held| held.bundle.as_path());
    // #101 (OI-1002-Q11): before this item's export writes a byte, charge its
    // estimated bundle to CORPUS. An item that does not fit refuses
    // DESTINATION_SPACE_INSUFFICIENT as its own receipt; the pass goes on.
    let _reservation = space.reserve(estimated_bundle(&parts, retained_bundle)?)?;
    // Q42 lane L7: the retained capture's manifest, read only when the plan
    // reuses its blobs. The pass then reads the bundle only on a miss.
    let listing = offered_manifest(corpus, plan, retained.as_ref());
    let (reuse, unoffered) = reuse_offer(plan, retained.as_ref(), &listing)?;
    // A future-stamped seat blocked the whole-capture reuse above, and will on
    // every pass until the clock passes it: say so (round-3 N5).
    let future = (retained.is_some() && parts.stamped_after(git_carry::pass_start_ns()))
        .then_some(git_carry::ReuseUnavailable::FutureStamp);
    let attempt = attempt_directory(state, &identity, key)?;
    // The plan base, when the plan's basis names it.
    let base = base.filter(|_| plan.basis.based());
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
    fault_point!(EstateAfterBundlePublish);
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
    // #226 item 5: the rebuildable roots this capture omitted, counted, so
    // closure reads "equal except N counted roots" without the bundle.
    write(&omissions_sidecar(corpus, &name), &export.omitted)?;
    // Q42 lane L7 (P70): the reuse manifest, bound to the bundle by its
    // digest and durable before the record, like every sidecar above. A
    // crash here leaves a manifest no record names, which nothing reads.
    fault_point!(EstateBeforeReuseSidecar);
    publish_reuse(corpus, &name, digest, export.manifest)?;
    fault_point!(EstateAfterReuseSidecar);
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
    fault_point!(EstateAfterCaptureRecord);
    #[cfg(test)]
    git_carry::mid_pass::fire(
        &fs::canonicalize(&item.source).refuse_at("estate::capture_item")?,
        git_carry::mid_pass::Stage::RecordWritten,
    );
    if drift.is_empty() {
        retire_drift(corpus, &drift_sidecar)?;
    }
    Ok(Completion {
        outcome: captured_outcome(&drift, extends),
        drift: drift.lines(),
        bytes_read: export.bytes_read,
        reuse_unavailable: unoffered
            .or(future)
            .or(export.reuse_unavailable)
            .map(git_carry::ReuseUnavailable::code),
        nested,
        custody: None,
    })
}

// A clean pass can reproduce a drifted pass's bundle byte for byte when the
// drift lay only before the export's snapshot. Retire the stale drift record
// only after the clean completion is durable: a crash in between leaves the
// capture drifted, which costs one more pass and never a stale reuse.
fn retire_drift(corpus: &Path, drift_sidecar: &Path) -> Result<()> {
    // The site its caller always recorded these under.
    const SITE: &str = "estate::capture_item";
    if drift_sidecar.try_exists().refuse_at(SITE)? {
        fs::remove_file(drift_sidecar).refuse_at(SITE)?;
        fs::File::open(corpus)
            .refuse_at(SITE)?
            .sync_dir_counted()
            .refuse_at(SITE)?;
    }
    Ok(())
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

// The link a capture chains onto and its corpus path, as the decision's plan
// says (WP2): the retained bundle at its own depth for a chained basis,
// under a plan base or not (L6b's fix 2), none otherwise. A plan the code
// cannot carry out refuses CONTRACT_SELF_INCONSISTENT: a re-root on the
// chain's root (Q46, L8), or a depth that is not one more than the link's.
//
// One link is never offered: a based bundle (no `.prior`) whose bound base
// is gone or replaced. The decision does not read a based link's base on
// this path, and nothing restores that bundle, so a capture chained on it
// could not restore either. Without the link the writer writes what v1
// wrote: the plan base's delta, or a self-contained bundle. A chained
// link's bases are already part of its chain's custody (`chain_links`).
fn chain_offer(
    corpus: &Path,
    plan: decide::Plan,
    retained: Option<&Retained>,
    inputs: &decide::Inputs,
) -> Result<(Option<Prior>, Option<PathBuf>)> {
    let depth = inputs.depth;
    let link = match (plan.basis, plan.rebase) {
        (_, decide::Rebase::Reroot) => {
            return Err(BulkloadRefusal::ContractSelfInconsistent);
        }
        (decide::Basis::Chain | decide::Basis::BaseAndChain, _) => {
            let held = retained.ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
            if depth.checked_add(1) != Some(plan.depth) {
                return Err(BulkloadRefusal::ContractSelfInconsistent);
            }
            let unrestorable = inputs.shape == decide::Shape::Based
                && bound_base(corpus, &held.previous.bundle)? == decide::PrevBase::Lost;
            (!unrestorable).then(|| Prior {
                bundle: held.previous.bundle.clone(),
                digest: held.previous.digest,
                identity: held.previous.identity,
                depth,
            })
        }
        (decide::Basis::SelfContained | decide::Basis::Base, _) => None,
    };
    let path = link
        .as_ref()
        .map(|link| link_path(corpus, link))
        .transpose()?;
    Ok((link, path))
}

// The chain link of a capture's bundle, durable before its record names it
// (WP2), as `.base` is for a shared plan base.
//
// An export can reproduce, byte for byte, a bundle the corpus already holds
// under the same content name. Identical bytes declare identical
// prerequisites, so that bundle's recorded custody answers for it whenever
// that custody is intact, and three rules keep the record from ever naming
// a chain nothing can restore:
//
// 1. **Never a cycle.** A name that is the link, or is already in the
//    link's own chain, gets no `.prior`: the bundle is that earlier capture
//    (nothing it carries moved since; under a plan base the tips between
//    are the base's own commits, L6b's fix 2), and its custody as a root or
//    a shallower link stands. Writing the link there would close
//    root -> link -> root.
// 2. **An intact chain stands.** A `.prior` already recorded for the name
//    whose chain is intact under `Custody` is left as it is.
// 3. **A stale link never outlives its chain.** When the export declared
//    no link's tips (a base's delta, or self-contained) and the name still
//    carries a `.prior` whose chain is broken, that sidecar is removed and
//    the removal made durable: the bytes were just written without the
//    link, so the bundle restores from its base alone.
fn publish_prior(corpus: &Path, name: &str, link: Option<&Prior>, chained: bool) -> Result<()> {
    const SITE: &str = "estate::publish_prior";
    let sidecar = prior_sidecar(corpus, name);
    let recorded = sidecar.try_exists().refuse_at(SITE)?;
    let intact = recorded && chain_links(corpus, name, LinkBinding::Custody).is_ok();
    match (link, chained) {
        (Some(link), true) => {
            if link.bundle == name {
                return Ok(());
            }
            let published = corpus.join(name);
            if chain_links(corpus, &link.bundle, LinkBinding::Custody)?
                .links
                .iter()
                .any(|(path, _)| *path == published)
            {
                return Ok(());
            }
            if !intact {
                write(&sidecar, link)?;
            }
            Ok(())
        }
        (None, true) => Err(BulkloadRefusal::ContractSelfInconsistent),
        (_, false) => {
            if recorded && !intact {
                fs::remove_file(&sidecar).refuse_at(SITE)?;
                fs::File::open(corpus)
                    .refuse_at(SITE)?
                    .sync_dir_counted()
                    .refuse_at(SITE)?;
            }
            Ok(())
        }
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
// and the key parts. The reuse manifest follows them (`publish_reuse`).
// Returns the drift sidecar's path, present or not.
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
                        done.custody.clone(),
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
    // S4 (#231): every item ran; each refusal is in its own record.
    if refused.load(std::sync::atomic::Ordering::Relaxed) {
        Err(BulkloadRefusal::ItemsRefused)
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
        (policy, decide::Policy::CODE),
        &CorpusSpace::live(corpus),
        receipt,
    )
}

// [`capture_with_policy`] once both directories exist, against `space`,
// deciding under `decision`. Every caller outside the tests passes the
// code's one policy, `decide::Policy::CODE` (OI-1003-Q63 D4: no flag); the
// back-compat tests pass `decide::Policy::V1` to write the corpus a pass
// from before L6b left.
fn capture_in(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    (policy, decision): (git_carry::CapturePolicy, decide::Policy),
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
        let result = group_base(item, &groups, state, corpus)
            .and_then(|base| {
                capture_item(
                    item,
                    state,
                    corpus,
                    base.as_ref(),
                    (policy, decision),
                    &owners,
                    &refused,
                    space,
                )
            })
            .map_err(|refusal| capture_refusal(refusal, item, state));
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
            outcome = first_fault(outcome, error);
        }
    }
    outcome
}

// The verb's refusal across its levels (S4, #231): the first that is not
// the run-level `ITEMS_REFUSED`, which never hides a level that aborted.
fn first_fault(outcome: Result<()>, error: BulkloadRefusal) -> Result<()> {
    match outcome {
        Ok(()) | Err(BulkloadRefusal::ItemsRefused) => Err(error),
        fault => fault,
    }
}

type ImportedBases = Mutex<std::collections::BTreeSet<(PathBuf, [u8; 32])>>;

// S4 (#220): a Git child that ran out of space names no directory (its
// stderr is never carried, R-N121), but its verb knows where its children
// write. Apply's write into the destination repository and its corpus
// stages, both charged by apply's space preflight: the destination's space,
// `DESTINATION_SPACE_INSUFFICIENT`.
fn charged_space(refusal: BulkloadRefusal) -> BulkloadRefusal {
    match refusal {
        BulkloadRefusal::SpaceExhausted(None) => {
            // Judged by no limit: no earlier check's numbers belong to it.
            crate::space::forget_refusal();
            BulkloadRefusal::DestinationSpaceInsufficient
        }
        other => other,
    }
}

// A capture's refusal with the facts only the verb knows. S4 (#220): a
// capture's Git children write only under PRIVATE_STATE (the attempt and
// plan-base directories), which no preflight charges, so an unnamed
// out-of-space refusal names it. S4 (#219): a missed object in a source
// whose alternates name a store Git cannot open (a lender moved or deleted)
// names that alternates file.
fn capture_refusal(refusal: BulkloadRefusal, item: &Item, state: &Path) -> BulkloadRefusal {
    use std::os::unix::ffi::OsStrExt as _;
    match refusal {
        BulkloadRefusal::SpaceExhausted(None) => {
            BulkloadRefusal::SpaceExhausted(Some(state.as_os_str().as_bytes().to_vec()))
        }
        other => git_carry::missing_alternate(&item.source, &other).map_or(other, |file| {
            BulkloadRefusal::GitSourceAlternates(file.as_os_str().as_bytes().to_vec())
        }),
    }
}

// A missing corpus file is a typed refusal, never a bare IO errno (S4).
fn sealed(refusal: BulkloadRefusal) -> BulkloadRefusal {
    if refusal == BulkloadRefusal::Io(Some(libc::ENOENT)) {
        BulkloadRefusal::SealedObjectMissing
    } else {
        refusal
    }
}

// A plan base staged for an import (R-N72): bound by its corpus name and its
// recorded digest, and self-contained. A base the corpus no longer holds
// refuses `SEALED_OBJECT_MISSING` (#181), as a missing capture bundle does.
fn stage_base(corpus: &Path, base: &Base) -> Result<git_carry::StagedBundle> {
    let path = base_path(corpus, base)?;
    if !path.try_exists().refuse_at("estate::stage_base")? {
        return Err(BulkloadRefusal::SealedObjectMissing);
    }
    let staged = git_carry::stage_bundle(&path).map_err(sealed)?;
    if staged.digest() != base.digest || git_carry::shared::requires_base(staged.path())? {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    Ok(staged)
}

// A plan base a linked item's apply imports itself (its repository's
// common directory and the base's digest), recorded in `ImportedBases`
// only once that apply succeeded.
type DeferredBase = Option<(PathBuf, [u8; 32])>;

// `bundle` is the staged copy of the capture this apply restores; `bare`
// whether it is a capture of a bare repository.
fn import_base(
    item: &Item,
    captured: &Capture,
    (bundle, bare_capture): (&Path, bool),
    corpus: &Path,
    source: &str,
    imported: &ImportedBases,
) -> Result<(Option<git_carry::StagedBundle>, DeferredBase)> {
    if !git_carry::shared::requires_base(bundle)? {
        return Ok((None, None));
    }
    // A delta whose `.base` sidecar is gone names no base to import.
    let base = bound(corpus, &captured.bundle)?.ok_or(BulkloadRefusal::SealedObjectMissing)?;
    // Estate converge: a standalone item takes its plan base where its
    // capture lands, in its private stage at a first landing or before a
    // converge's import, so one plan lands a main and its linked worktrees.
    // So does a bare refs-only capture into a path that does not exist
    // (#226 item 4): the bare repository bulkload makes takes it in its
    // stage.
    let fresh_bare = item.workspace.is_none()
        && bare_capture
        && fs::symlink_metadata(&item.repository).is_err_and(|error| {
            // M2 finding 1: under a file nothing can be; the landing's
            // parent check types it.
            error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ENOTDIR)
        });
    if fresh_bare
        || item
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace == &item.repository)
    {
        return Ok((Some(stage_base(corpus, &base)?), None));
    }
    // A linked or refs-only item imports into its existing repository.
    // S4 (#183): a destination repository that is missing, or a directory
    // that holds none, is named as such, never as an occupied one.
    git_carry::destination_repository(&item.repository)?;
    let key = (git_carry::common_repository(&item.repository)?, base.digest);
    if imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .contains(&key)
    {
        return Ok((None, None));
    }
    let staged = stage_base(corpus, &base)?;
    // Dogfood of 2026-10-16, F2 (OI-1003-Q171, Q172, Q173): a linked item
    // imports its base itself, after every refusal it can decide without a
    // write (a first landing's `check_linked`, an adopt proof, a converge
    // proof), never here before them.
    if item.workspace.is_some() {
        return Ok((Some(staged), Some(key)));
    }
    git_carry::import_staged(&item.repository, &staged, source)?;
    imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .insert(key);
    Ok((None, None))
}

#[allow(clippy::too_many_lines)]
fn apply_item(
    item: &Item,
    corpus: &Path,
    state: &Path,
    source: &str,
    (imported, owners): (&ImportedBases, &Owners),
    (options, items): (&ApplyOptions, &[String]),
) -> Result<Completion> {
    let identity = id(item)?;
    let record = corpus.join(format!("{identity}.capture"));
    // Round 4 N5: an item whose capture refused has no record. That is a
    // typed refusal, never a bare IO errno; and it names the absent capture
    // (S4, #219), never a sealed object a capture recorded and lost.
    if !record.try_exists().refuse_at("estate::apply_item")? {
        // M2r2 b: an item `estate-carry-files` carried as files, with its
        // carry still proved, keeps that outcome; a re-apply never turns it
        // back into CAPTURE_ABSENT.
        let carried_now = matches!(
            crate::outcome::read_record(&state.join(format!("{identity}.outcome"))),
            Ok(crate::outcome::OutcomeRecord {
                outcome: crate::outcome::Outcome::CarriedAsFiles,
                ..
            })
        );
        if carried_now
            && matches!(
                carried::prove(&[state.to_path_buf()], &identity, source, item),
                Some(Ok(_))
            )
        {
            return Ok(Completion::clean(carried::CARRIED_OUTCOME));
        }
        return Err(BulkloadRefusal::CaptureAbsent);
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
    // uncaptured seats) is deferred (bulkload#48); carry v2, the W6 engine it
    // was deferred to, is deleted (OI-1003-Q44, OI-1003-Q56).
    let journal = journal_path(state, &identity, source, &captured.digest);
    // A3 (estate converge): this item's own converge a crash interrupted is
    // settled before its journal is trusted. A ref intent another item of
    // the repository left is settled here without proving or restaging this
    // item (review 2 finding 7). Metadata only: no-op reruns stay no-ops.
    if journal.try_exists().refuse_at("estate::apply_item")? {
        let pending =
            git_carry::converge::pending(item.workspace.as_deref(), &item.repository, source)?;
        let done: String = read(&journal)?;
        // `--advance-ancestor` (operator ruling 2026-10-19): a beside journal
        // of this capture does not end an item whose live checkout may be
        // advanced now (or whose advance a crash cut short); one whose
        // advance would refuse keeps it, its refusal recorded for closure.
        let retry = done == "workspace-beside"
            && options.advance_ancestor
            && advance_retry(item, state, (&identity, source), &captured.digest)?;
        // A beside retirement a crash cut short is finished by a rerun,
        // with or without the flag, and one put back for a transient
        // reason (a write or a process inside it) is tried again.
        let retiring = retire_record_path(state, &identity, source)
            .try_exists()
            .refuse_at("estate::apply_item")?
            || (done == "workspace-restored"
                && git_carry::converge::advance::read_record(&advance_record_path(
                    state, &identity, source,
                ))?
                .is_some_and(|record| {
                    record.item == identity
                        && record.capture == captured.digest
                        && record.outcome == "advanced"
                        && record.beside == "pending"
                }));
        if !pending.workspace && !retry && !retiring {
            let outcome = match done.as_str() {
                "workspace-restored" => "previous-workspace-restoration-not-revalidated",
                "workspace-beside" => "previous-workspace-beside-not-revalidated",
                "workspace-refs-in" => "previous-workspace-refs-in-not-revalidated",
                "refs-imported" => "previous-ref-custody-not-workspace-parity",
                _ => return Err(BulkloadRefusal::ReceiptBindingInvalid),
            };
            let mut done = Completion::clean(outcome).naming(nested);
            if pending.refs {
                done.custody = git_carry::converge::settle_refs(
                    item.workspace.as_deref(),
                    &item.repository,
                    source,
                )?;
            }
            return Ok(done);
        }
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
    let staged = git_carry::stage_bundle(&published).map_err(sealed)?;
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
    // capture on a shared plan base imports that base first. L6b's fix 2
    // (R-N72): a chain under a plan base binds every base and every link by
    // name and digest, and the flatten imports every bound base before the
    // oldest link.
    let (staged, base, deferred) = if prior_sidecar(corpus, &captured.bundle)
        .try_exists()
        .refuse_at("estate::apply_item")?
    {
        let chain = chain_links(corpus, &captured.bundle, LinkBinding::Digest)?;
        (
            git_carry::chain::flatten(staged, &chain.bases, &chain.links)?,
            None,
            None,
        )
    } else {
        let (base, deferred) = import_base(
            item,
            &captured,
            (staged.path(), staged.is_bare()),
            corpus,
            source,
            imported,
        )?;
        (staged, base, deferred)
    };
    // Estate converge: a first landing restores native refs and records
    // ledgers; a landed checkout whose capture changed converges when its
    // ledger proves it unchanged (`git_carry::converge`).
    // M2 finding 1: a first landing makes its absent parents (0755) and
    // records them here.
    let parents = parents_path(state, &identity, source);
    // M2 round 2, item 2: `--exclude-union` records its appends here.
    let union = exclude_union_path(state, &identity, source);
    let landing = git_carry::converge::Landing {
        item: &identity,
        observed_ns: retained_parts(corpus, &captured.bundle)?.map_or(0, |parts| parts.started_ns),
        capture: captured.digest,
        parents: Some(&parents),
        exclude_union: options.exclude_union.then_some(union.as_path()),
        beside: false,
    };
    let target = git_carry::converge::Target {
        repository: &item.repository,
        workspace: item.workspace.as_deref(),
        source,
        base: base.as_ref(),
    };
    // M2 item 7: a ref named `refs/<ns>` itself would take the refs-in
    // namespace; refused before the item writes anything. A path Git cannot
    // read as a repository is left to the landing's own refusal.
    if options.refs_in && item.repository.exists() {
        let suffix = options
            .beside
            .as_deref()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let ns = git_carry::beside::namespace(suffix)?;
        if matches!(
            git_carry::beside::namespace_taken(&item.repository, &ns),
            Ok(true)
        ) {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
    }
    let policy = git_carry::converge::Policy {
        adopt: options.adopt,
        origins: &options.activate_origin,
        items,
        advance: options
            .beside
            .as_deref()
            .filter(|_| options.advance_ancestor),
        beside: options.beside.as_deref(),
    };
    // `--advance-ancestor` (review of #289, finding 3): a beside
    // retirement a crash cut short is finished, or its tree put back,
    // before the item's workspace step, whatever that step answers; its
    // outcome is recorded for closure (a tree left under its private name
    // is listed pending, never silent).
    let retired_before = settle_retirement(state, (&identity, source))?;
    let (outcome, mut applied) = if item.workspace.is_some() {
        let landed_before = landed_before(state, &identity, source, &journal)?;
        let (outcome, applied) = match git_carry::converge::apply_workspace(
            &staged,
            &target,
            &landing,
            landed_before,
            &policy,
        ) {
            Ok(applied) => ("workspace-restored", applied),
            // M2 round 2 item c: the planned checkout exists, its adopt
            // proof refused (nothing written), and it stays as it is; the
            // capture lands beside it. Review B2: no other refusal does,
            // but these. 2026-10-19 (the td01 batch): a non-repo directory
            // at a linked item's planned path that does not prove equal, or
            // a standalone item's, is left as it is and the capture lands
            // beside it too; the code is the beside landing's recorded
            // reason. `--advance-ancestor` (operator rulings 2026-10-19):
            // each refused advance or switch is recorded in STATE for
            // closure (pending review) and the capture lands beside the
            // live checkout in this same pass, as without the flag.
            Err(reason)
                if options.adopt
                    && (reason == BulkloadRefusal::GitDestinationDiverged
                        || reason == BulkloadRefusal::GitDestinationNonrepoDiffers
                        || reason.is_advance_refusal()) =>
            {
                let Some(suffix) = options.beside.as_deref() else {
                    return Err(reason);
                };
                // A standalone item's non-repo directory holds no repository
                // to land beside into or import refs into: refused typed,
                // pending review, nothing written.
                if reason == BulkloadRefusal::GitDestinationNonrepoDiffers
                    && item.workspace.as_deref() == Some(item.repository.as_path())
                {
                    return Err(reason);
                }
                let refused = reason.is_advance_refusal().then(|| reason.clone());
                if let (Some(refused), Some(workspace)) = (&refused, item.workspace.as_deref()) {
                    record_advance(
                        state,
                        (&identity, source, captured.digest),
                        workspace,
                        refused,
                    )?;
                    // A process inside the live checkout (the coordinator's
                    // decision on #289, C19): retried by the next pass, and
                    // no beside written while one already exists.
                    if refused.is_advance_in_use()
                        && fs::symlink_metadata(git_carry::beside::path(workspace, suffix)).is_ok()
                    {
                        return Err(refused.clone());
                    }
                }
                // Operator ruling 2026-10-16 21:24Z ("refs in always; a tree
                // beside ONLY where neo has uncommitted work"): a capture
                // with no tracked change whose HEAD the refs bring in lands
                // no tree beside; its untracked and ignored files are
                // reconciled into the live checkout, file by file
                // (`git_carry::converge::clean`). Only under `--refs-in`:
                // without it the refs would not go in.
                // A capture the assessment cannot read lands beside, as it
                // did before the rule; so does one whose HEAD the refs-in
                // that follows did not bring in after all (a namespace ref
                // held at another value, or one the operator removed).
                // A non-repo live directory holds no checkout to reconcile
                // into: it lands beside.
                let clean =
                    if options.refs_in && reason != BulkloadRefusal::GitDestinationNonrepoDiffers {
                        git_carry::converge::clean::assess(&staged, &target)
                            .ok()
                            .flatten()
                    } else {
                        None
                    };
                let cleaned = clean.as_ref().map_or(Ok(None), |clean| {
                    clean_item(
                        clean,
                        (item, &staged),
                        (&target, &landing),
                        (state, &identity, suffix),
                    )
                });
                let landed = cleaned.and_then(|cleaned| {
                    cleaned.map_or_else(
                        || {
                            beside_item(
                                &staged,
                                &target,
                                &landing,
                                (state, &policy, suffix, &reason),
                            )
                            .map(|applied| ("workspace-beside", applied))
                        },
                        Ok,
                    )
                });
                match landed {
                    Ok((outcome, mut applied)) => {
                        if let Some(refused) = &refused {
                            let words = format!("advance-refused={}", refused.code());
                            applied.custody = Some(match applied.custody.take() {
                                Some(more) => format!("{more} {words}"),
                                None => words,
                            });
                        }
                        (outcome, applied)
                    }
                    // Gap C12 (A-115): the tree waits for review beside the
                    // live checkout, but the item's refs go in now, branches,
                    // tags and stash entries, create-only under `refs/<ns>/`
                    // (`--refs-in`), and closure names them on the refused
                    // row. The item's refusal is the beside one, whatever
                    // the refs-in answers: a refs-in that refuses records
                    // nothing for this capture, and closure shows none.
                    Err(refusal) => {
                        if options.refs_in {
                            let _imported: Result<String> = refs_in(
                                item,
                                &staged,
                                &target,
                                (state, &identity, suffix),
                                (captured.digest, RefsIn::STASH),
                            );
                        }
                        return Err(refusal);
                    }
                }
            }
            Err(refusal) => return Err(refusal),
        };
        if let Some(key) = deferred {
            imported
                .lock()
                .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
                .insert(key);
        }
        (outcome, applied)
    } else {
        (
            "refs-imported",
            git_carry::converge::apply_refs(&staged, &target, &landing, &policy)?,
        )
    };
    // M2 item 7 (`estate-move`): refs-in after an adopt, a landing or a
    // converge, before the journal, so a crash is finished by the rerun. A
    // beside landing took every captured ref under the namespace already.
    // An advanced live checkout's stash is the live one's, never touched,
    // so the captured stash entries go in as `refs/<ns>-stash/<n>` too.
    // A clean source's refs went in before its files (`clean_item`).
    if options.refs_in && outcome != "workspace-beside" && outcome != "workspace-refs-in" {
        let suffix = options
            .beside
            .as_deref()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        // An advanced checkout (now or by an earlier pass) keeps its own
        // stash: the capture's entries go in under the namespace.
        let stash = applied.advanced
            || match item.workspace.as_deref() {
                Some(workspace) => {
                    git_carry::converge::advance::advanced_at(workspace, source)?.is_some()
                }
                None => false,
            };
        let words = refs_in(
            item,
            &staged,
            &target,
            (state, &identity, suffix),
            (
                captured.digest,
                RefsIn {
                    stash,
                    follow: false,
                },
            ),
        )?;
        applied.custody = Some(match applied.custody.take() {
            Some(more) => format!("{more} {words}"),
            None => words,
        });
    }
    // `--advance-ancestor`: after an advance (now, or finished after a
    // crash), its record in STATE, then the beside landing it made
    // redundant retired once proved equal to the live tree; a retirement a
    // crash cut short is finished first. Before the journal, so a crash is
    // finished by the rerun.
    if outcome == "workspace-restored" {
        if let Some(words) = advanced_item(
            item,
            (&staged, &target, &landing, &policy),
            (state, &identity, source, captured.digest),
            (options, applied.advanced, retired_before),
        )? {
            applied.custody = Some(match applied.custody.take() {
                Some(more) => format!("{more} {words}"),
                None => words,
            });
        }
    }
    write(&journal, &outcome.to_owned())?;
    let mut done = Completion::clean(outcome).naming(nested);
    done.custody = applied
        .custody
        .map(|words| {
            if applied.converged {
                format!("converged {words}")
            } else {
                words
            }
        })
        .or_else(|| applied.converged.then(|| "converged".to_owned()));
    Ok(done)
}

// Operator ruling 2026-10-16 21:24Z: the clean capture `clean` lands no
// tree beside the live checkout. Its refs go in first, every captured ref
// and stash entry under the namespace, this item's own earlier refs moved
// to the source's values by compare-and-swap (no tree beside holds them
// now: the namespace does); then its HEAD must be reachable from a live
// ref, or `None` (the caller lands beside). Then its untracked and ignored
// files are reconciled into the live checkout
// ([`git_carry::converge::clean::land`]), recorded at
// `{item}-{blake3(SOURCE)}.files` in STATE. A beside landing an earlier
// pass made is left as it was and named, for `estate-beside-retire`.
fn clean_item(
    clean: &git_carry::converge::clean::Clean,
    (item, staged): (&Item, &git_carry::StagedBundle),
    (target, landing): (
        &git_carry::converge::Target<'_>,
        &git_carry::converge::Landing<'_>,
    ),
    (state, identity, suffix): (&Path, &str, &str),
) -> Result<Option<(&'static str, git_carry::converge::Applied)>> {
    let words = refs_in(
        item,
        staged,
        target,
        (state, identity, suffix),
        (landing.capture, RefsIn::FOLLOW),
    )?;
    if !git_carry::converge::clean::reachable_in(target.repository, clean.head())? {
        return Ok(None);
    }
    let record = files_record_path(state, landing.item, target.source);
    let mut applied = git_carry::converge::clean::land(clean, target, landing, (&record, suffix))?;
    applied.custody = Some(match applied.custody.take() {
        Some(more) => format!("{more} {words}"),
        None => words,
    });
    if let Some(workspace) = item.workspace.as_deref() {
        let beside = git_carry::beside::path(workspace, suffix);
        if fs::symlink_metadata(&beside).is_ok() {
            applied.custody = applied
                .custody
                .map(|words| format!("{words} beside-existing={}", beside.display()));
        }
    }
    Ok(Some(("workspace-refs-in", applied)))
}

/// How [`refs_in`] takes the captured refs.
#[derive(Debug, Clone, Copy)]
struct RefsIn {
    /// Each carried stash entry too, as `refs/<ns>-stash/<n>`.
    stash: bool,
    /// A clean source with no tree beside (operator ruling 2026-10-16
    /// 21:24Z): every captured ref (not only branches and tags), and this
    /// item's own earlier namespace refs moved to the source's values by
    /// compare-and-swap, as a beside landing moves its own.
    follow: bool,
}

impl RefsIn {
    const STASH: Self = Self {
        stash: true,
        follow: false,
    };
    const FOLLOW: Self = Self {
        stash: true,
        follow: true,
    };
}

/// Where an item's clean-source file reconcile is recorded (operator
/// ruling 2026-10-16 21:24Z): `{item}-{blake3(source)}.files` in STATE, a
/// [`git_carry::converge::files::FilesRecord`].
#[must_use]
pub fn files_record_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.files",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// What an item's `.files` record says of its current capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesSummary {
    /// The tracked files and links the source's HEAD holds (in by ref).
    pub tracked: u64,
    /// How the untracked and ignored entries ended.
    pub tally: git_carry::converge::files::Tally,
}

// The `.files` summary of `identity`'s current capture (`capture`,
// lowercase hex) from the last state directory holding its record: `Err`
// when it names another item or capture, is unfinished or left an entry
// occupied; `None` with no record.
fn files_summary(
    states: &[PathBuf],
    identity: &str,
    source: &str,
    capture: Option<&str>,
) -> Option<std::result::Result<FilesSummary, &'static str>> {
    let path = states
        .iter()
        .rev()
        .map(|state| files_record_path(state, identity, source))
        .find(|path| path.is_file())?;
    let Ok(Some(record)) = git_carry::converge::files::read_record(&path) else {
        return Some(Err("files-record-unreadable"));
    };
    let current = capture.is_some_and(|capture| {
        blake3::Hash::from_bytes(record.capture).to_hex().as_str() == capture
    });
    Some(
        if record.item == identity && current && record.done && record.occupied.is_empty() {
            Ok(FilesSummary {
                tracked: record.tracked,
                tally: record.tally,
            })
        } else {
            Err("files-record-mismatch")
        },
    )
}

/// Whether the last `.beside` record of `identity` in `states` proves its
/// beside landing of the current capture (`capture`, lowercase hex):
/// `None` with no record (review of #266, B4).
fn beside_proof(
    states: &[PathBuf],
    identity: &str,
    source: &str,
    capture: Option<&str>,
) -> Option<std::result::Result<(), &'static str>> {
    let path = states
        .iter()
        .rev()
        .map(|state| beside_record_path(state, identity, source))
        .find(|path| path.is_file())?;
    let Ok(Some(record)) = git_carry::beside::read_record(&path) else {
        return Some(Err("beside-record-unreadable"));
    };
    let current = capture.is_some_and(|capture| {
        blake3::Hash::from_bytes(record.capture).to_hex().as_str() == capture
    });
    Some(
        if record.item == identity && current && record.pending.is_empty() && record.kept.is_empty()
        {
            Ok(())
        } else {
            Err("beside-record-mismatch")
        },
    )
}

/// Where an item's beside landing is recorded (M2 round 2 item c):
/// `{item}-{blake3(source)}.beside` in STATE, a
/// [`git_carry::beside::BesideRecord`].
#[must_use]
pub fn beside_record_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.beside",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// `estate-apply --adopt --beside SUFFIX` (M2 round 2 item c,
/// [`git_carry::beside`]): the planned checkout's adopt proof refused
/// `GIT_DESTINATION_DIVERGED`, so the capture lands at `<workspace>SUFFIX`
/// ([`git_carry::beside::land`]): a first linked landing into the planned
/// repository, detached, its native refs never stepped; on a later pass
/// this item's own beside landing converges, another item's re-keys and
/// converges, an equal standalone checkout no ledger names is adopted, and
/// anything else refuses `GIT_BESIDE_FOREIGN` (gap C12, A-115). Then the
/// captured refs go in under `refs/<ns>/`, recorded write-ahead in STATE:
/// created, or moved where this item's own earlier ref still holds the
/// value it created ([`git_carry::beside::plan_own`]).
fn beside_item(
    staged: &git_carry::StagedBundle,
    target: &git_carry::converge::Target<'_>,
    landing: &git_carry::converge::Landing<'_>,
    (state, policy, suffix, reason): (
        &Path,
        &git_carry::converge::Policy<'_>,
        &str,
        &BulkloadRefusal,
    ),
) -> Result<git_carry::converge::Applied> {
    use std::os::unix::ffi::OsStrExt as _;
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let ns = git_carry::beside::namespace(suffix)?;
    let beside = git_carry::beside::path(workspace, suffix);
    // Review of #266, A3: the namespace is checked before anything lands.
    if git_carry::beside::namespace_taken(target.repository, &ns)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let beside_landing = git_carry::converge::Landing {
        beside: true,
        ..*landing
    };
    let record_path = beside_record_path(state, landing.item, target.source);
    let prior = git_carry::beside::read_record(&record_path)?;
    // Gap C12: a beside checkout this item landed (its record names the
    // item, this path and a capture) that is absent now was folded or
    // retired by the operator's agent; it is never laid down again.
    let retired = prior.as_ref().is_some_and(|record| {
        record.item == landing.item
            && record.path == beside.as_os_str().as_bytes()
            && record.capture != [0; 32]
    });
    let landed =
        git_carry::beside::land(staged, target, &beside_landing, policy, (&beside, retired))?;
    // Review B1: what the beside checkout holds is read from its own
    // ledger, and must be this item's landing of this capture.
    let ledger = git_carry::beside::ledger_of(&beside, target.source)?
        .filter(|ledger| ledger.item == landing.item && ledger.capture == landing.capture)
        .ok_or_else(|| BulkloadRefusal::GitBesideForeign(beside.as_os_str().as_bytes().to_vec()))?;
    // The capture's provenance instance in the planned repository: a
    // linked beside landing imported it there; a standalone one (adopted,
    // re-keyed or converged in its own repository) holds it there, so it
    // is imported here (with its plan base), as a refs-only item would.
    let instance = if landed.standalone {
        if let Some(base) = target.base {
            git_carry::import_staged(target.repository, base, target.source)?;
        }
        git_carry::beside::import_instance(target.repository, staged, target.source)?
    } else {
        ledger.instance.clone()
    };
    let mut record = prior.unwrap_or_default();
    // Review B5: refs a crashed run created are named by its pending list.
    git_carry::beside::settle(target.repository, &mut record)?;
    landing.item.clone_into(&mut record.item);
    record.path = beside.as_os_str().as_bytes().to_vec();
    suffix.clone_into(&mut record.suffix);
    record.capture = ledger.capture;
    record.instance.clone_from(&instance);
    // Gap C12: the refs this item created before (its beside landing's and
    // its refs-in's) are its own; one still at the value it created moves
    // to the new capture's, one removed since is never created again.
    // A refs-in a crash cut short names its refs pending: settled here, in
    // memory, so they count as this item's own.
    let mut refs_in =
        git_carry::beside::read_refs_in(&refs_in_record_path(state, landing.item, target.source))?
            .unwrap_or_default();
    git_carry::beside::settle_refs_in(target.repository, &mut refs_in)?;
    let own = git_carry::beside::own_refs(&record.created, &refs_in.created);
    let wanted =
        git_carry::beside::wanted(target.repository, (target.source, &instance, &ns), &own)?;
    let planned = git_carry::beside::plan_own(target.repository, &wanted, &own)?;
    record.kept.clone_from(&planned.kept);
    record.pending = planned.writes();
    git_carry::beside::write_record(&record_path, &record)?;
    git_carry::beside::write(target.repository, &planned)?;
    fault_point!(EstateApplyBesideRefs);
    for written in std::mem::take(&mut record.pending) {
        git_carry::beside::upsert(&mut record.created, written);
    }
    git_carry::beside::write_record(&record_path, &record)?;
    // Review B4: a captured ref kept at another value is not landed; the
    // item stays refused (pending review) until the namespace is resolved.
    if !planned.kept.is_empty() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    // 2026-10-19: why the capture went beside (the planned checkout's
    // `GIT_DESTINATION_DIVERGED`, or a non-repo directory's
    // `GIT_DESTINATION_NONREPO_DIFFERS`), for closure's `beside_reason`.
    write(
        &beside_reason_path(state, landing.item, target.source),
        &reason.code().to_owned(),
    )?;
    let words = format!(
        "beside-reason={} beside-{}=1 beside-{}=1 beside-ref-created={} beside-ref-present={} beside-ref-moved={} beside-ref-retired={} beside-ref-kept=0",
        reason.code(),
        landed.how.word(),
        if landed.standalone {
            "standalone"
        } else {
            "linked"
        },
        planned.create.len(),
        planned.present.len(),
        planned.moved.len(),
        planned.retired.len(),
    );
    let mut applied = landed.applied;
    applied.custody = Some(match applied.custody.take() {
        Some(more) => format!("{words} {more}"),
        None => words,
    });
    Ok(applied)
}

/// Where the reason an item's capture landed beside is recorded.
///
/// 2026-10-19: `{item}-{blake3(source)}.beside-reason` in STATE, the
/// refusal code that sent it there (`GIT_DESTINATION_DIVERGED`,
/// `GIT_DESTINATION_NONREPO_DIFFERS`).
#[must_use]
pub fn beside_reason_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.beside-reason",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// Where an item's refs-in is recorded (M2 item 7, `estate-move`):
/// `{item}-{blake3(source)}.refs-in` in STATE, a
/// [`git_carry::beside::RefsInRecord`].
#[must_use]
pub fn refs_in_record_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.refs-in",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// Where an item's `--advance-ancestor` outcome is recorded (operator
/// ruling 2026-10-19): `{item}-{blake3(source)}.advance` in STATE, a
/// [`git_carry::converge::advance::AdvanceRecord`].
#[must_use]
pub fn advance_record_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.advance",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// Where an item's beside retirement under way is recorded:
/// `{item}-{blake3(source)}.retire` in STATE, a
/// [`git_carry::beside::retire::RetireIntent`].
#[must_use]
pub fn retire_record_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.retire",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

// Record that `capture`'s advance of the live checkout at `workspace` was
// refused with `refusal` (closure holds the item pending review, its
// detail printed), unless the record already says so for the same live
// HEAD and untracked files.
fn record_advance(
    state: &Path,
    (identity, source, capture): (&str, &str, [u8; 32]),
    workspace: &Path,
    refusal: &BulkloadRefusal,
) -> Result<()> {
    use git_carry::converge::advance;
    let path = advance_record_path(state, identity, source);
    let (live, branch) = advance::live_head(workspace).unwrap_or_default();
    // The colliding paths, so a rerun proves the item again once one is
    // gone (an ignored one is in no untracked digest).
    let paths = match refusal {
        BulkloadRefusal::GitAdvanceUntrackedCollides(listed)
        | BulkloadRefusal::GitSwitchUntrackedCollides(listed) => listed
            .split(|byte| *byte == b'\n')
            .filter(|path| !path.is_empty() && !path.starts_with(b"+"))
            .map(<[u8]>::to_vec)
            .collect(),
        _ => Vec::new(),
    };
    let record = advance::AdvanceRecord {
        item: identity.to_owned(),
        capture,
        live,
        branch,
        outcome: refusal.code().to_owned(),
        detail: refusal.to_string(),
        paths,
        untracked: advance::untracked_digest(workspace, source)
            .ok()
            .flatten()
            .unwrap_or_default(),
        ..advance::AdvanceRecord::default()
    };
    if advance::read_record(&path)?.as_ref() != Some(&record) {
        advance::write_record(&path, &record)?;
    }
    Ok(())
}

// `--advance-ancestor`: whether an item whose beside journal closed its
// current capture is proved again ([`git_carry::converge::advance::gate`]):
// not when the live path is no checkout, nor when an advance would refuse
// now (recorded), nor when this capture's advance was refused for a reason
// the capture, the live HEAD and its untracked files alone decide (not an
// ancestor, another branch, a collision) and none of them moved since.
fn advance_retry(
    item: &Item,
    state: &Path,
    (identity, source): (&str, &str),
    capture: &[u8; 32],
) -> Result<bool> {
    use git_carry::converge::advance;
    use std::os::unix::ffi::OsStrExt as _;
    let Some(workspace) = item.workspace.as_deref() else {
        return Ok(false);
    };
    match advance::gate(workspace, source)? {
        advance::Gate::Leave => Ok(false),
        advance::Gate::Refused(refusal) => {
            record_advance(state, (identity, source, *capture), workspace, &refusal)?;
            Ok(false)
        }
        advance::Gate::Try(untracked) => {
            let decided = advance::read_record(&advance_record_path(state, identity, source))?
                .filter(|record| {
                    record.item == identity
                        && record.capture == *capture
                        && record.untracked == untracked
                        && matches!(
                            record.outcome.as_str(),
                            "GIT_ADVANCE_NOT_ANCESTOR"
                                | "GIT_ADVANCE_BRANCH_MISMATCH"
                                | "GIT_ADVANCE_UNTRACKED_COLLIDES"
                                | "GIT_SWITCH_UNTRACKED_COLLIDES"
                        )
                });
            Ok(match decided {
                Some(record) => {
                    advance::live_head(workspace).ok() != Some((record.live, record.branch))
                        || !record.paths.iter().all(|path| {
                            workspace
                                .join(std::ffi::OsStr::from_bytes(path))
                                .symlink_metadata()
                                .is_ok()
                        })
                }
                None => true,
            })
        }
    }
}

// `--advance-ancestor`, after an item's workspace outcome: the advance
// record in STATE (`advanced`, with the live HEAD it moved from), a
// retirement a crash cut short finished, then (when this pass advanced or
// finished an advance a flagged pass began) the item's beside landing
// retired once proved equal to the live tree. Its receipt words, `None`
// when the item was not advanced.
#[allow(clippy::too_many_lines)]
fn advanced_item(
    item: &Item,
    (staged, target, landing, policy): (
        &git_carry::StagedBundle,
        &git_carry::converge::Target<'_>,
        &git_carry::converge::Landing<'_>,
        &git_carry::converge::Policy<'_>,
    ),
    (state, identity, source, capture): (&Path, &str, &str, [u8; 32]),
    (options, advanced, pending): (
        &ApplyOptions,
        bool,
        Option<git_carry::beside::retire::Retired>,
    ),
) -> Result<Option<String>> {
    use git_carry::converge::advance;
    use std::fmt::Write as _;
    let Some(workspace) = item.workspace.as_deref() else {
        return Ok(None);
    };
    let record_path = advance_record_path(state, identity, source);
    let retire_path = retire_record_path(state, identity, source);
    if !advanced && pending.is_none() {
        return Ok(None);
    }
    let held = advance::read_record(&record_path)?
        .filter(|record| record.item == identity && record.capture == capture);
    let mut record = match held {
        Some(record) if record.outcome == "advanced" => record,
        _ => {
            let done = advance::advanced_at(workspace, source)?;
            advance::AdvanceRecord {
                item: identity.to_owned(),
                capture,
                live: done
                    .as_ref()
                    .map(|done| done.from.oid.clone())
                    .unwrap_or_default(),
                branch: done
                    .as_ref()
                    .and_then(|done| done.from.symbolic.clone())
                    .unwrap_or_default(),
                outcome: "advanced".to_owned(),
                beside: "pending".to_owned(),
                reason: String::new(),
                kept: done.as_ref().map_or(0, |done| {
                    u64::try_from(done.left.kept.len()).unwrap_or(u64::MAX)
                }),
                besides: done.as_ref().map_or(0, |done| {
                    u64::try_from(done.left.besides.len()).unwrap_or(u64::MAX)
                }),
                switched: done.as_ref().is_some_and(advance::Advanced::switched),
                ..advance::AdvanceRecord::default()
            }
        }
    };
    advance::write_record(&record_path, &record)?;
    // A retirement put back for a transient reason before the workspace
    // step is tried again now.
    let pending = pending.filter(|done| !transient(done));
    let mut carried = git_carry::beside::retire::Carried::default();
    let decided = match (pending, options.beside.as_deref()) {
        (Some(done), _) => Some(Some(done)),
        // The advance was begun under `--advance-ancestor`: its beside
        // landing is retired even by a rerun that finishes it without.
        (None, Some(suffix)) if record.beside == "pending" => Some(
            retire_beside(
                workspace,
                (staged, target, landing, policy),
                (
                    &retire_path,
                    &exclude_union_path(state, identity, source),
                    identity,
                    capture,
                ),
                suffix,
            )?
            .map(|(done, carry)| {
                carried = carry;
                done
            }),
        ),
        (None, _) => None,
    };
    if let Some(done) = decided {
        note_retired(&mut record, done.as_ref());
        advance::write_record(&record_path, &record)?;
    }
    // What a standalone beside carried into the live repository first, created
    // only where absent (the coordinator's decision on #289).
    let mut carried_words = String::new();
    for (name, count) in [
        ("advance-config-added", carried.config),
        ("advance-exclude-appended", carried.exclude),
    ] {
        if count > 0 {
            let _ = write!(carried_words, " {name}={count}");
        }
    }
    Ok(Some(format!(
        "advance-file-besides={} advance-beside={}{}{}{carried_words}",
        record.besides,
        record.beside,
        if record.reason.is_empty() {
            String::new()
        } else {
            format!(" advance-beside-reason={}", record.reason)
        },
        if record.switched {
            format!(" advance-switched-from={}@{}", record.branch, record.live)
        } else {
            String::new()
        }
    )))
}

// Whether a retirement was put back for a reason the next try may not
// meet (a write while it was proved, a process inside it): its tree is
// back at its path, its record gone, and it is tried again.
fn transient(done: &git_carry::beside::retire::Retired) -> bool {
    matches!(done.reason(), "raced" | "in-use" | "written-since")
}

// What became of an advance's beside landing (`None`: the beside path held
// nothing), in its record: `retired`; `absent`; `pending` (put back for a
// transient reason, tried again); else `kept`. A beside not retired is a
// duplicate: closure holds it pending review with
// `GIT_ADVANCE_BESIDE_KEPT` and the reason (the coordinator's decision 4
// on #289).
fn note_retired(
    record: &mut git_carry::converge::advance::AdvanceRecord,
    done: Option<&git_carry::beside::retire::Retired>,
) {
    use git_carry::beside::retire::Retired;
    let (beside, reason) = match done {
        Some(Retired::Retired) => ("retired", ""),
        None => ("absent", ""),
        Some(Retired::Kept(reason)) if reason == "gone" => ("absent", ""),
        Some(kept @ Retired::Kept(reason)) => (
            if transient(kept) { "pending" } else { "kept" },
            reason.as_str(),
        ),
    };
    beside.clone_into(&mut record.beside);
    reason.clone_into(&mut record.reason);
    record.detail = if reason.is_empty() {
        String::new()
    } else {
        BulkloadRefusal::GitAdvanceBesideKept(reason.as_bytes().to_vec()).to_string()
    };
}

// `--advance-ancestor` (review of #289, finding 3): finish the beside
// retirement a crash cut short (or put its tree back) before the item's
// workspace step, and record its outcome in the item's advance record.
fn settle_retirement(
    state: &Path,
    (identity, source): (&str, &str),
) -> Result<Option<git_carry::beside::retire::Retired>> {
    use git_carry::converge::advance;
    let Some(done) =
        git_carry::beside::retire::settle(&retire_record_path(state, identity, source))?
    else {
        return Ok(None);
    };
    let path = advance_record_path(state, identity, source);
    if let Some(mut record) = advance::read_record(&path)?
        .filter(|record| record.item == identity && record.outcome == "advanced")
    {
        note_retired(&mut record, Some(&done));
        advance::write_record(&path, &record)?;
    }
    Ok(Some(done))
}

// Retire the beside landing at `<workspace>SUFFIX` an advance made
// redundant ([`git_carry::beside::retire`]): absent, nothing to do; else
// brought to the capture first, as every beside pass brings it (gap C12:
// converged, re-keyed or adopted by proof, never laid down), kept when
// that refuses or when its ledger does not then name this item and
// capture (never one the item does not own), and retired only once proved
// equal to the live tree.
fn retire_beside(
    workspace: &Path,
    (staged, target, landing, policy): (
        &git_carry::StagedBundle,
        &git_carry::converge::Target<'_>,
        &git_carry::converge::Landing<'_>,
        &git_carry::converge::Policy<'_>,
    ),
    (retire_path, exclude_path, identity, capture): (&Path, &Path, &str, [u8; 32]),
    suffix: &str,
) -> Result<
    Option<(
        git_carry::beside::retire::Retired,
        git_carry::beside::retire::Carried,
    )>,
> {
    use git_carry::beside::retire::{self, Carried, Retired};
    let beside = git_carry::beside::path(workspace, suffix);
    if fs::symlink_metadata(&beside).is_err() {
        return Ok(None);
    }
    let beside_landing = git_carry::converge::Landing {
        beside: true,
        ..*landing
    };
    if let Err(refusal) =
        git_carry::beside::land(staged, target, &beside_landing, policy, (&beside, false))
    {
        return Ok(Some((
            Retired::Kept(format!("refused:{}", refusal.code())),
            Carried::default(),
        )));
    }
    let owned = git_carry::beside::ledger_of(&beside, target.source)?
        .is_some_and(|ledger| ledger.item == identity && ledger.capture == capture);
    if !owned {
        return Ok(Some((
            Retired::Kept("not-owned".to_owned()),
            Carried::default(),
        )));
    }
    // The live checkout's own untracked files the advance left as they
    // were are never the beside landing's.
    let left = git_carry::converge::advance::advanced_at(workspace, target.source)?
        .map(|done| done.left)
        .unwrap_or_default();
    let ns = git_carry::beside::namespace(suffix)?;
    retire::retire(
        (retire_path, exclude_path),
        &beside,
        workspace,
        (identity, capture),
        (&left, &ns, target.source),
    )
    .map(Some)
}

/// `estate-apply --refs-in` (M2 item 7).
///
/// The in-place lane's `refs-in.py` made native (operator ruling "refs in,
/// tree beside", A-20261016-072): after this item adopted, landed or converged its repository, each
/// captured branch and tag the repository lacks or holds at another value
/// goes in create-only as `refs/<ns>/heads/<name>` (`refs/<ns>/tags/<name>`),
/// `ns` the `--beside` suffix's namespace. Recorded write-ahead in STATE
/// like a beside landing's refs; no ref is moved or deleted. A ref of that
/// name already held at another value (an earlier pass's, the source has
/// moved since) is kept, never moved, recorded and counted
/// (`refs-in-kept=N`), as the design's step 7 reports `exists`: the
/// source's value stays in the capture's provenance refs. A ref named
/// `refs/<ns>` itself refuses the item before it writes anything
/// (`apply_item`). With `stash` (a refused beside landing, gap C12), each
/// carried stash entry goes in too, as `refs/<ns>-stash/<n>` (newest
/// first), create-only like the rest. A name this item created before (its
/// `.refs-in` or `.beside` record) that is absent now was removed by the
/// operator and is never created again (`refs-in-retired`). Returns the
/// receipt's custody words.
fn refs_in(
    item: &Item,
    staged: &git_carry::StagedBundle,
    target: &git_carry::converge::Target<'_>,
    (state, identity, suffix): (&Path, &str, &str),
    (capture, how): ([u8; 32], RefsIn),
) -> Result<String> {
    let ns = git_carry::beside::namespace(suffix)?;
    let repository = target.repository;
    if git_carry::beside::namespace_taken(repository, &ns)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    // The capture's provenance instance: the workspace ledger this apply
    // just proved or wrote names it; a refs-only item (or a ledger that
    // does not name this item's capture) imports it again, as a beside
    // landing's adopted standalone checkout does.
    let ledger = match item.workspace.as_deref() {
        Some(workspace) => git_carry::beside::ledger_of(workspace, target.source)?
            .filter(|ledger| ledger.item == identity && ledger.capture == capture),
        None => None,
    };
    let instance = if let Some(ledger) = ledger {
        ledger.instance
    } else {
        if let Some(base) = target.base {
            git_carry::import_staged(repository, base, target.source)?;
        }
        git_carry::beside::import_instance(repository, staged, target.source)?
    };
    let record_path = refs_in_record_path(state, identity, target.source);
    let mut record = git_carry::beside::read_refs_in(&record_path)?.unwrap_or_default();
    git_carry::beside::settle_refs_in(repository, &mut record)?;
    identity.clone_into(&mut record.item);
    ns.clone_into(&mut record.ns);
    record.capture = capture;
    record.instance.clone_from(&instance);
    // Gap C12: a name this item created before (here or in its beside
    // landing) and the operator removed since is never created again
    // (`refs-in-retired`); create-only, so one of its own the source moved
    // since is kept, as any other differing name is. Stash entries are
    // decided by commit (review of #282, F2): a removed one is never
    // created again under another index, and a new one takes a free index.
    let beside_created =
        git_carry::beside::read_record(&beside_record_path(state, identity, target.source))?
            .map(|record| record.created)
            .unwrap_or_default();
    let own = git_carry::beside::own_refs(&beside_created, &record.created);
    let mut wanted = if how.follow {
        git_carry::beside::refs_in_wanted_every(repository, target.source, &instance, &ns)?
    } else {
        git_carry::beside::refs_in_wanted(repository, target.source, &instance, &ns)?
    };
    if how.stash {
        wanted.extend(git_carry::beside::stash_wanted(
            repository,
            (target.source, &instance, &ns),
            &own,
            false,
        )?);
    }
    let mut planned = git_carry::beside::plan_own(repository, &wanted, &own)?;
    if !how.follow {
        planned.kept.append(&mut planned.moved);
    }
    record.kept.clone_from(&planned.kept);
    record.pending = planned.writes();
    git_carry::beside::write_refs_in(&record_path, &record)?;
    git_carry::beside::write(repository, &planned)?;
    for written in std::mem::take(&mut record.pending) {
        git_carry::beside::upsert(&mut record.created, written);
    }
    git_carry::beside::write_refs_in(&record_path, &record)?;
    let moved = if how.follow {
        format!(" refs-in-moved={}", planned.moved.len())
    } else {
        String::new()
    };
    Ok(format!(
        "refs-in-created={} refs-in-present={}{moved} refs-in-kept={} refs-in-retired={}",
        planned.create.len(),
        planned.present.len(),
        planned.kept.len(),
        planned.retired.len()
    ))
}

// Finding 13: whether STATE holds a journal of this item for a capture
// other than the current one, so a destination now absent was removed.
fn landed_before(state: &Path, identity: &str, source: &str, current: &Path) -> Result<bool> {
    let prefix = format!("{identity}-{}-", blake3::hash(source.as_bytes()).to_hex());
    for entry in fs::read_dir(state).refuse_at("estate::landed_before")? {
        let entry = entry.refuse_at("estate::landed_before")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".done") && entry.path() != current {
            return Ok(true);
        }
    }
    Ok(false)
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
    /// The rebuildable roots the current capture omitted (#226 item 5),
    /// from its `.omissions` sidecar or, for a capture from before the
    /// sidecar, from what its bundle carries in band
    /// ([`git_carry::in_band`], M2 round 2, d); `None` when that is unknown.
    pub omitted: Option<Vec<git_carry::RebuildableOmission>>,
    /// The item's proved carry as files (M2r2 b): its
    /// `{item}-{blake3(SOURCE)}.carried` record from the last state
    /// directory holding one, with the transfer record it names checked
    /// ([`carried::prove`]), or why it proves nothing. `None` when no state
    /// directory holds one.
    pub carried: Option<std::result::Result<carried::CarriedProof, &'static str>>,
    /// M2r2 c: whether the item's `{item}-{blake3(SOURCE)}.beside` record
    /// (the last state directory holding one) proves its beside landing:
    /// it names the item and its current capture, with no ref pending or
    /// kept. `None` when no state directory holds one.
    pub beside: Option<std::result::Result<(), &'static str>>,
    /// Gap C12: what the item's `{item}-{blake3(SOURCE)}.refs-in` record
    /// (the last state directory holding one) says of its current capture,
    /// when it names the item and that capture with no ref pending: the
    /// refs `--refs-in` imported, which closure prints on a refused row (a
    /// beside landing refused, its refs in all the same). `None` otherwise.
    pub refs_in: Option<RefsInSummary>,
    /// `--advance-ancestor` (operator ruling 2026-10-19): what the item's
    /// `{item}-{blake3(SOURCE)}.advance` record (the last state directory
    /// holding one) says of its current capture, when it names the item
    /// and that capture: advanced (and what became of its beside landing)
    /// or refused with a `GIT_ADVANCE_*` code, which closure holds pending
    /// review on a beside row. `None` otherwise.
    pub advance: Option<AdvanceSummary>,
    /// Operator ruling 2026-10-16 21:24Z: what the item's
    /// `{item}-{blake3(SOURCE)}.files` record (the last state directory
    /// holding one) says of its current capture, landed with no tree beside:
    /// every untracked and ignored entry counted, none left occupied. `None`
    /// when no state directory holds one.
    pub files: Option<std::result::Result<FilesSummary, &'static str>>,
    /// 2026-10-19: the reason the item's capture landed beside (its
    /// `.beside-reason` record in the last state directory holding one),
    /// which closure prints on an `applied-beside` row.
    pub beside_reason: Option<String>,
    /// Why `omitted` is unknown: `no-capture`, `capture-unreadable`,
    /// `sidecar-unreadable`, or the in-band reader's reason
    /// (`header-unreadable`, `value-not-read`, `value-not-held`,
    /// `value-unreadable`, `envelope-not-read`, `envelope-not-held`,
    /// `envelope-unreadable`, `holder-partial-clone`,
    /// `holder-git-too-old`).
    pub unknown_because: Option<&'static str>,
}

/// What an item's `--advance-ancestor` record says of its current capture
/// (operator ruling 2026-10-19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvanceSummary {
    /// `advanced`, or the `GIT_ADVANCE_*` code that refused the advance.
    pub outcome: String,
    /// After an advance, its beside landing: `pending`, `absent`,
    /// `retired` or `kept`; empty after a refusal.
    pub beside: String,
    /// Why the beside landing was kept.
    pub reason: String,
    /// How many captured entries the advance left to the live checkout.
    pub kept: u64,
    /// A refusal's detail (the colliding paths, or a kept beside's
    /// reason, escaped).
    pub detail: String,
    /// How many captured files landed as file-level besides.
    pub besides: u64,
    /// The live HEAD the advance was decided against: its commit and its
    /// branch (empty when detached).
    pub live: String,
    pub branch: String,
    /// The advance switched the live checkout from `branch` to the
    /// capture's (operator ruling 2026-10-19, "Switch when clean").
    pub switched: bool,
    /// A beside retirement of the item is under way in STATE (its tree
    /// under its private name; review of #289, finding 3).
    pub retiring: bool,
}

impl AdvanceSummary {
    /// The `GIT_ADVANCE_*` or `GIT_SWITCH_*` code that refused the advance,
    /// when one did; or, after an advance, `GIT_ADVANCE_BESIDE_KEPT` when
    /// the beside landing it made redundant was not retired (kept, put
    /// back to be tried again, or a retirement under way): a duplicate,
    /// pending review.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        if self.outcome == "advanced" {
            return (self.retiring || matches!(self.beside.as_str(), "kept" | "pending"))
                .then_some("GIT_ADVANCE_BESIDE_KEPT");
        }
        ((self.outcome.starts_with("GIT_ADVANCE_") || self.outcome.starts_with("GIT_SWITCH_"))
            && crate::outcome::is_typed_code(&self.outcome))
        .then_some(self.outcome.as_str())
    }
}

/// The refs an item's refs-in imported for its current capture (gap C12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefsInSummary {
    /// The namespace, `refs/<ns>/` (the `--beside` suffix without its
    /// leading `.`).
    pub ns: String,
    /// Refs it created, create-only.
    pub created: u64,
    /// Refs it kept, held at another value.
    pub kept: u64,
}

// The refs-in summary of `identity`'s current capture (`capture`,
// lowercase hex) from the last state directory holding its record.
fn refs_in_summary(
    states: &[PathBuf],
    identity: &str,
    source: &str,
    capture: Option<&str>,
) -> Option<RefsInSummary> {
    let path = states
        .iter()
        .rev()
        .map(|state| refs_in_record_path(state, identity, source))
        .find(|path| path.is_file())?;
    let record = git_carry::beside::read_refs_in(&path).ok()??;
    let current = capture.is_some_and(|capture| {
        blake3::Hash::from_bytes(record.capture).to_hex().as_str() == capture
    });
    (record.item == identity && current && record.pending.is_empty()).then(|| RefsInSummary {
        ns: record.ns,
        created: u64::try_from(record.created.len()).unwrap_or(u64::MAX),
        kept: u64::try_from(record.kept.len()).unwrap_or(u64::MAX),
    })
}

// `--advance-ancestor`: what the last `.advance` record of `identity` in
// `states` says of the current capture (`capture`, lowercase hex), when it
// names both.
fn advance_summary(
    states: &[PathBuf],
    identity: &str,
    source: &str,
    capture: Option<&str>,
) -> Option<AdvanceSummary> {
    let path = states
        .iter()
        .rev()
        .map(|state| advance_record_path(state, identity, source))
        .find(|path| path.is_file())?;
    let record = git_carry::converge::advance::read_record(&path).ok()??;
    let current = capture.is_some_and(|capture| {
        blake3::Hash::from_bytes(record.capture).to_hex().as_str() == capture
    });
    let retiring = states
        .iter()
        .any(|state| retire_record_path(state, identity, source).is_file());
    (record.item == identity && current).then_some(AdvanceSummary {
        outcome: record.outcome,
        beside: record.beside,
        reason: record.reason,
        kept: record.kept,
        detail: record.detail,
        besides: record.besides,
        live: record.live,
        branch: record.branch,
        switched: record.switched,
        retiring,
    })
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
    ledger_with(plan, corpus, source, states, false)
}

/// [`ledger`], optionally reading omission lists from destinations.
///
/// With `read_destinations` (`closure-report
/// --omissions-from-destination`, M2 round 2, d) a capture from before the
/// `.omissions` sidecar whose bundle names an omission list has that list
/// read by oid from the item's destination repository's objects (reads
/// only, `--no-optional-locks`). Without it the bundle header alone
/// decides: no omission ref is known zero, a named list stays unknown
/// (`value-not-read`).
///
/// # Errors
/// As [`ledger`].
#[allow(clippy::too_many_lines)]
pub fn ledger_with(
    plan: &Path,
    corpus: &Path,
    source: &str,
    states: &[PathBuf],
    read_destinations: bool,
) -> Result<Ledger> {
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
        let mut omitted = None;
        let unknown_because;
        let journal = if !record_path.try_exists().refuse_at("estate::ledger")? {
            unknown_because = Some("no-capture");
            JournalState::NoCapture
        } else if let Ok(captured) = read::<Capture>(&record_path) {
            if filename(&captured.bundle) {
                (omitted, unknown_because) =
                    item_omissions(item, corpus, &captured.bundle, read_destinations);
            } else {
                unknown_because = Some("capture-unreadable");
            }
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
            unknown_because = Some("capture-unreadable");
            JournalState::CaptureUnreadable
        };
        let record = records.get(&identity);
        let carried = carried::prove(states, &identity, source, item);
        let beside = beside_proof(states, &identity, source, capture.as_deref());
        let refs_in = refs_in_summary(states, &identity, source, capture.as_deref());
        let advance = advance_summary(states, &identity, source, capture.as_deref());
        let files = files_summary(states, &identity, source, capture.as_deref());
        let beside_reason = states
            .iter()
            .rev()
            .map(|state| beside_reason_path(state, &identity, source))
            .find(|path| path.is_file())
            .and_then(|path| read::<String>(&path).ok());
        ledger.entries.push(LedgerEntry {
            source: item.source.clone(),
            has_workspace: item.workspace.is_some(),
            record: record.cloned(),
            journal,
            capture,
            omitted,
            carried,
            beside,
            refs_in,
            advance,
            files,
            beside_reason,
            unknown_because,
            item: identity,
        });
    }
    check_riders(&mut ledger);
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

// M2r2 b: a refs-only item's carry rides on a planned workspace item of the
// same source whose own outcome is carried-as-files and proves.
fn check_riders(ledger: &mut Ledger) {
    let riding: Vec<(usize, String)> = ledger
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match &entry.carried {
            Some(Ok(carried::CarriedProof { via: Some(via), .. })) => Some((index, via.clone())),
            _ => None,
        })
        .collect();
    for (index, via) in riding {
        let Some(source) = ledger.entries.get(index).map(|entry| entry.source.clone()) else {
            continue;
        };
        let proved = ledger.entries.iter().any(|sibling| {
            sibling.item == via
                && sibling.has_workspace
                && sibling.source == source
                && matches!(
                    sibling.record,
                    Some(Ok(crate::outcome::OutcomeRecord {
                        outcome: crate::outcome::Outcome::CarriedAsFiles,
                        ..
                    }))
                )
                && matches!(
                    sibling.carried,
                    Some(Ok(carried::CarriedProof { via: None, .. }))
                )
        });
        if let (false, Some(entry)) = (proved, ledger.entries.get_mut(index)) {
            entry.carried = Some(Err("carried-via-unproved"));
        }
    }
}

/// Where an item's first landing records the parents it made (M2 finding 1).
///
/// `{item}-{blake3(source)}.parents` in STATE, a
/// [`git_carry::parents::CreatedParents`]. Per item and source, never per
/// capture: a parent made once is not made again.
#[must_use]
pub fn parents_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.parents",
        blake3::hash(source.as_bytes()).to_hex()
    ))
}

/// Where an item's `--exclude-union` appends are recorded (M2 round 2,
/// item 2): `{item}-{blake3(source)}.exclude-union` in STATE, a
/// [`git_carry::exclude_union::ExcludeUnion`]. Per item and source.
#[must_use]
pub fn exclude_union_path(state: &Path, identity: &str, source: &str) -> PathBuf {
    state.join(format!(
        "{identity}-{}.exclude-union",
        blake3::hash(source.as_bytes()).to_hex()
    ))
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

/// What a space plan already charged or found held, so one bundle is
/// charged to one repository once (M2 round 2, item 1).
#[derive(Default)]
struct Charged {
    /// `(repository, bundle)` pairs charged or proven held.
    seen: std::collections::BTreeSet<(PathBuf, PathBuf)>,
    /// Whether each `(repository, bundle)` pair's repository holds it
    /// whole, held or not (review B6: one test per pair).
    held: std::collections::BTreeMap<(PathBuf, PathBuf), bool>,
}

impl Charged {
    /// Whether the destination `repository` already holds every object
    /// `bundle` brings (the test its import makes before it takes
    /// anything); `false` when it does not exist or cannot be read.
    fn held(&mut self, repository: &Path, bundle: &Path) -> bool {
        let key = (repository.to_path_buf(), bundle.to_path_buf());
        if let Some(held) = self.held.get(&key) {
            return *held;
        }
        let held = repository.try_exists().unwrap_or(false)
            && git_carry::bundle_held(repository, bundle).unwrap_or(false);
        self.held.insert(key, held);
        held
    }
}

/// What the workspace ledger at `workspace` says it holds for `source`:
/// `Some(capture digest)` when it records a landed checkout, `None` when
/// there is none or it cannot be read.
fn landed_capture(workspace: &Path, source: &str) -> Option<[u8; 32]> {
    use git_carry::landed;
    let git_dir = landed::worktree_git_dir(workspace).ok()??;
    let ledger = landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_LANDED);
    landed::read::<landed::LandedWorkspace>(&ledger)
        .ok()?
        .map(|landed| landed.capture)
}

/// One pending item's planned writes: `(path, bytes)` per destination, and
/// its bundle size for the staging peak. `None` when it is not pending.
///
/// Objects (the capture's bundle, its chain links and plan bases) are
/// charged to the repository only for a bundle the destination repository
/// does not already hold whole, and only once per repository across the
/// plan (M2 round 2, item 1). The checkout is charged apart from the
/// objects (review B3), to the workspace (a standalone's is its
/// repository): nothing when its workspace ledger records this capture;
/// the capture's own bundle (the bytes it brings beyond its chain) when the
/// ledger records an earlier one, which a converge moves forward; the
/// whole chain when no checkout was recorded (a first landing, or one a
/// crash interrupted after its objects or its `worktree add`). Bundle bytes
/// are compressed: a lower bound on a checkout.
fn item_space(
    item: &Item,
    corpus: &Path,
    state: &Path,
    source: &str,
    charged: &mut Charged,
) -> Result<Option<ItemSpace>> {
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
    // A chained capture lands its whole chain's objects (WP2), and the
    // objects of every plan base the chain is bound to (L6b's fix 2).
    let chain = chain_links(corpus, &captured.bundle, LinkBinding::Digest)?;
    let head = corpus.join(&captured.bundle);
    let mut bundles = vec![head.clone()];
    bundles.extend(
        chain
            .links
            .iter()
            .chain(&chain.bases)
            .map(|(link, _)| link.clone()),
    );
    let mut bundle = 0_u64;
    let mut objects = 0_u64;
    for path in &bundles {
        let size = fs::metadata(path).refuse_at("estate::item_space")?.len();
        bundle = bundle.saturating_add(size);
        if charged.held(&item.repository, path) {
            continue;
        }
        if charged.seen.insert((item.repository.clone(), path.clone())) {
            objects = objects.saturating_add(size);
        }
    }
    // Objects land in the repository; a checkout lands in the workspace.
    // For a linked worktree those are two places, possibly two filesystems.
    let mut writes = vec![(item.repository.clone(), objects)];
    if let Some(workspace) = item.workspace.as_ref() {
        let checkout = match landed_capture(workspace, source) {
            Some(landed) if landed == captured.digest => 0,
            Some(_) => fs::metadata(&head).refuse_at("estate::item_space")?.len(),
            None => bundle,
        };
        writes.push((workspace.clone(), checkout));
    }
    Ok(Some((writes, bundle)))
}

/// OI-1001-Q2: the bytes an apply would write, per existing destination
/// path, probed, before it writes any of them; [`crate::space::check_planned`]
/// sums them per filesystem and per project quota.
///
/// Each pending item (a capture record and bundle present, no journal yet)
/// charges what [`item_space`] finds missing at its destination. Staging
/// copies a bundle and its base next to the corpus, at most `jobs` items at
/// once, so the corpus is charged twice the `jobs` largest pending bundles.
/// Stated margins, not charged: a pack's index files and a thin pack's
/// completion beside the objects, a linked worktree's index, a chain's
/// links staged beside its head (up to three copies of a chain at once),
/// and a checkout's expansion past its compressed bundle. An item whose
/// record, bundle or target cannot be read or probed is skipped here and
/// refuses on its own, by type, in apply (R33): one bad item never aborts
/// the others.
fn space_plan(
    plan: &Plan,
    corpus: &Path,
    state: &Path,
    source: &str,
    jobs: usize,
) -> Vec<crate::space::Planned> {
    let mut planned = Vec::new();
    let mut charged = Charged::default();
    let mut staged = Vec::new();
    for item in &plan.items {
        let Ok(Some((writes, bundle))) = item_space(item, corpus, state, source, &mut charged)
        else {
            continue;
        };
        let Ok(located) = writes
            .iter()
            .filter(|(_, bytes)| *bytes > 0)
            .map(|(path, bytes)| crate::space::locate(path, *bytes))
            .collect::<Result<Vec<_>>>()
        else {
            continue;
        };
        planned.extend(located);
        staged.push(bundle);
    }
    staged.sort_unstable_by(|left, right| right.cmp(left));
    let peak = staged
        .iter()
        .take(jobs)
        .fold(0u64, |total, bytes| total.saturating_add(*bytes))
        .saturating_mul(2);
    if peak > 0 {
        if let Ok(corpus) = crate::space::locate(corpus, peak) {
            planned.push(corpus);
        }
    }
    planned
}

/// What one `estate-apply` run may do beyond the default (#226,
/// OI-1003-Q171/Q172). Explicit per run, never inferred; each receipt it
/// changes says so.
// Each bool is one independent command-line flag.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOptions {
    /// `--adopt`: an existing checkout or repository bulkload never landed
    /// becomes bulkload's by proof (rows A11a and A11d), and a workspace
    /// ledger naming another plan's item is re-keyed after its proof when
    /// the capture is the landed tree's (A11b). A checkout that differs
    /// refuses `GIT_DESTINATION_DIVERGED`; one that is not the item's to
    /// adopt (spelled through a symlink, another or an unrelated
    /// repository, a stale worktree backlink) `GIT_DESTINATION_OCCUPIED`.
    pub adopt: bool,
    /// `--activate-origin PREFIX` (repeatable): a first landing activates
    /// an scp-style or `ssh://` origin starting with one of these; a local
    /// path never ([`git_carry::origin_prefix_allowed`]).
    pub activate_origin: Vec<String>,
    /// `--side-root ROOT` (repeatable; M2r2 item e; needs `adopt`): after
    /// the preflights and before any item lands, each planned path spelled
    /// through a symlink whose single hop names a directory inside ROOT is
    /// seated there ([`seat`]): the directory takes the live path by one
    /// exchange (same inode, no byte copied) and the side-root path becomes
    /// a link back, recorded in STATE for `estate-unseat`.
    pub side_roots: Vec<PathBuf>,
    /// `--exclude-union` (M2 round 2, item 2): a first linked landing whose
    /// repository's common `info/exclude` is not the capture's appends the
    /// captured lines it lacks, recorded in STATE, instead of refusing
    /// `GIT_IGNORE_POLICY_CONFLICT`; only where the union cannot change
    /// what either policy means (no negation on either side) and no
    /// bulkload ledger owns the exclude ([`git_carry::exclude_union`]).
    pub exclude_union: bool,
    /// `--beside SUFFIX` (M2 round 2 item c; only with `--adopt`): a
    /// workspace item whose existing planned checkout the adopt proof
    /// refuses `GIT_DESTINATION_DIVERGED` lands beside it instead, at the
    /// workspace path with SUFFIX appended, with the captured refs created
    /// under `refs/<ns>/`; the planned checkout is left as it was
    /// ([`git_carry::beside`]).
    pub beside: Option<String>,
    /// `--refs-in` (M2 item 7, `estate-move`; needs `adopt` and `beside`):
    /// after an item adopts, lands or converges, each captured branch and
    /// tag its repository lacks or holds at another value is created under
    /// `refs/<ns>/` (the `--beside` namespace), create-only, recorded in
    /// STATE ([`git_carry::beside::RefsInRecord`]).
    pub refs_in: bool,
    /// `--advance-ancestor` (operator ruling 2026-10-19, "Yes, advance
    /// those"; needs `adopt` and `beside`): a live checkout bulkload did
    /// not land that differs from the capture is advanced in place when it
    /// is clean, its HEAD an ancestor of the captured HEAD and its branch
    /// the captured one, free to move ([`git_carry::converge::advance`]);
    /// any other such checkout refuses with a typed `GIT_ADVANCE_*` code,
    /// recorded in STATE for closure, and lands beside in the same pass.
    /// After an advance, the item's beside landing is retired once proved
    /// equal to the live tree ([`git_carry::beside::retire`]).
    pub advance_ancestor: bool,
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
    apply_with(
        plan,
        corpus,
        state,
        source,
        jobs,
        &ApplyOptions::default(),
        receipt,
    )
}

/// [`apply`] under explicit [`ApplyOptions`] (#226).
///
/// # Errors
/// As [`apply`]; `FIELD_DOMAIN_VIOLATION` for an `--activate-origin`
/// prefix that is not an scp-style or `ssh://` URL prefix.
pub fn apply_with(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source: &str,
    jobs: usize,
    options: &ApplyOptions,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    apply_reporting(
        plan,
        corpus,
        state,
        source,
        (jobs, options),
        receipt,
        &mut |_| Ok(()),
    )
}

/// `estate-unseat STATE [LIVE ...]` (M2r2 item e): undo every seat
/// recorded in `state`, or those of `lives`, under the apply's STATE lock;
/// one line per seat undone, handed to `line`.
///
/// # Errors
/// `REQUIRED_FIELD_MISSING` for a LIVE path with no seat record (nothing is
/// undone); [`seat::unseat`]'s refusal, at the first seat refused.
pub fn unseat(
    state: &Path,
    lives: &[PathBuf],
    line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let _lock = exclusive(&state.join("estate.lock"))?;
    let records = seat::records(state)?;
    if lives
        .iter()
        .any(|live| !records.iter().any(|(_, record)| &record.live == live))
    {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    for (path, record) in records {
        if !lives.is_empty() && !lives.contains(&record.live) {
            continue;
        }
        seat::unseat(&path, &record)?;
        line(&format!(
            "{} plan=\"{}\"",
            seat_line(&record.live, &record.target, "unseated", None),
            record.plan.display().to_string().escape_debug()
        ))?;
    }
    Ok(())
}

// One seat's line on the apply's output (M2r2 item e).
fn seat_line(
    live: &Path,
    target: &Path,
    outcome: &str,
    refusal: Option<&BulkloadRefusal>,
) -> String {
    format!(
        "seat live=\"{}\" target=\"{}\" outcome={outcome} reason={}",
        live.display().to_string().escape_debug(),
        target.display().to_string().escape_debug(),
        refusal.map_or("none", BulkloadRefusal::code),
    )
}

/// [`apply_with`], with each hand-off H step's line (TIN-6569,
/// OI-1003-Q183: `item=<id> handoff=<written|unchanged|CODE>`) handed to
/// `handoff_line` after the last level.
///
/// # Errors
/// As [`apply_with`]; a refusal of `handoff_line` itself.
#[allow(clippy::too_many_lines)]
pub fn apply_reporting(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source: &str,
    (jobs, options): (usize, &ApplyOptions),
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
    handoff_line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !options
        .activate_origin
        .iter()
        .all(|prefix| git_carry::origin_prefix_allowed(prefix))
    {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    // Review B2: `--beside` acts only on an adopt proof's refusal.
    if let Some(suffix) = &options.beside {
        if !options.adopt {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        }
        git_carry::beside::namespace(suffix)?;
    }
    if options.refs_in && options.beside.is_none() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    // `--advance-ancestor` acts on an adopt proof's difference, and falls
    // back to the beside landing.
    if options.advance_ancestor && (!options.adopt || options.beside.is_none()) {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    // A seated tree is an existing checkout: only `--adopt` lands into it.
    if !options.side_roots.is_empty() && !options.adopt {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    private_directory(state)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let contents: Plan = read(plan)?;
    // OI-1001-Q2: refuse before any item writes when the planned bytes would
    // take a destination filesystem under its free-space floor.
    crate::space::check_planned(
        &space_plan(&contents, corpus, state, source, jobs),
        crate::space::min_free_percent(),
    )?;
    // M2r2 item e, after the preflights and before any item: finish every
    // seat or undo a crash interrupted (a record that cannot be finished is
    // a refused line, never the apply's refusal), then seat the plan's
    // symlinked live paths into the side roots. A seat stays when its items
    // then refuse (`estate-unseat` undoes it).
    let roots = seat::roots(&options.side_roots)?;
    for (record, done) in seat::resume(state)? {
        match done {
            Ok(seat::Seated::Present) => {}
            Ok(done) => handoff_line(&seat_line(&record.live, &record.target, done.word(), None))?,
            Err(refusal) => handoff_line(&seat_line(
                &record.live,
                &record.target,
                "refused",
                Some(&refusal),
            ))?,
        }
    }
    let owner = std::path::absolute(plan).refuse_at("estate::apply")?;
    for candidate in seat::candidates(&contents.items, &roots) {
        let line = match &candidate.found {
            seat::Found::Seatable { target, .. } => {
                match seat::seat(state, &owner, &contents.items, &roots, &candidate.live) {
                    Ok(done) => seat_line(&candidate.live, target, done.word(), None),
                    Err(refusal) => seat_line(&candidate.live, target, "refused", Some(&refusal)),
                }
            }
            seat::Found::Refused(refusal) => {
                seat_line(&candidate.live, Path::new(""), "refused", Some(refusal))
            }
        };
        handoff_line(&line)?;
    }
    let mut locks = std::collections::BTreeMap::new();
    let mut groups = std::collections::BTreeMap::new();
    let imported = ImportedBases::default();
    for item in &contents.items {
        // Estate converge: one key per repository, standalone or not, so a
        // main and its linked worktrees never hold two mutexes for one
        // repository. A repository not there yet keys by its path, which is
        // also what its linked items key by until it lands. S4 (#183): a
        // destination Git cannot read as a repository (an empty directory,
        // an unfinished `git init`) keys by its path too, and its item
        // refuses on its own; it never aborts the verb before any item runs.
        let common = if item.repository.try_exists().refuse_at("estate::apply")? {
            git_carry::common_repository(&item.repository)
                .unwrap_or_else(|_| item.repository.clone())
        } else {
            item.repository.clone()
        };
        groups.insert(id(item)?, common.clone());
        locks.entry(common).or_insert_with(|| Mutex::new(()));
    }
    let owners = owners(&contents)?;
    // A11b never re-keys a ledger naming one of the plan's own items
    // (review of 2026-10-16, finding 2).
    let items = contents
        .items
        .iter()
        .map(id)
        .collect::<Result<Vec<String>>>()?;
    let touched = Mutex::new((
        std::collections::BTreeSet::<PathBuf>::new(),
        std::collections::BTreeSet::<String>::new(),
    ));
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
        let done = apply_item(
            item,
            corpus,
            state,
            source,
            (&imported, &owners),
            (options, &items),
        )
        .map_err(charged_space);
        // TIN-6569: a repository whose item did more than a journal no-op
        // gets the H step after the last level; a refused item's own
        // workspace never does (design 6.4: the converge left it as it was).
        if done.as_ref().map_or(true, |done| {
            // A kept carry (M2r2 b) is a no-op, like a previous journal.
            !done.outcome.starts_with("previous-") && done.outcome != carried::CARRIED_OUTCOME
        }) {
            let mut touched = touched
                .lock()
                .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
            touched.0.insert(common.clone());
            if done.is_err() {
                touched.1.insert(id(item)?);
            }
        }
        done
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
        // A refs-only item is placed by its repository's path (M2 item 1
        // review): a bare landing below a planned workspace that has not
        // landed yet would otherwise make that workspace's directory as its
        // parent, and the workspace would then find its path occupied.
        let place = item.workspace.as_ref().unwrap_or(&item.repository);
        let key = target_key(place)?;
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
        levels.entry(depth).or_default().items.push(item.clone());
    }
    // Estate converge: an item whose repository is another planned item's
    // standalone workspace runs after that item's level, so mains land (and
    // converge) before their linked worktrees and refs-only items, with no
    // Plan A/B split.
    let levels = after_mains(levels)?;
    let mut outcome = Ok(());
    for level in levels.values() {
        if let Err(error) = execute(level, jobs, &operation, &|row| {
            emit(state, "estate::apply", row, receipt)
        }) {
            outcome = first_fault(outcome, error);
        }
    }
    // TIN-6569 (OI-1003-Q183), the H step (design 6.4): after every level,
    // with every outcome record written.
    let (touched, refused) = touched
        .into_inner()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
    handoff::after_levels(
        &contents,
        (plan, corpus, state),
        source,
        &|item| {
            let identity = id(item)?;
            Ok(groups
                .get(&identity)
                .is_some_and(|common| touched.contains(common))
                && !refused.contains(&identity))
        },
        handoff_line,
    )?;
    outcome
}

// Move every item whose repository is a planned standalone workspace to a
// level after that workspace's own.
fn after_mains(
    levels: std::collections::BTreeMap<usize, Plan>,
) -> Result<std::collections::BTreeMap<usize, Plan>> {
    let mut mains = Vec::new();
    for (depth, plan) in &levels {
        for item in &plan.items {
            if item.workspace.as_ref() == Some(&item.repository) {
                mains.push((target_key(&item.repository)?, *depth));
            }
        }
    }
    let mut placed = std::collections::BTreeMap::<usize, Plan>::new();
    for (depth, plan) in levels {
        for item in plan.items {
            let mut at = depth;
            if item.workspace.as_ref() != Some(&item.repository) {
                let key = target_key(&item.repository)?;
                for (main, main_depth) in &mains {
                    let (this, that) = comparable(&key, main);
                    if this == that {
                        at = at.max(main_depth + 1);
                    }
                }
            }
            placed.entry(at).or_default().items.push(item);
        }
    }
    Ok(placed)
}

/// Corpus custody as the fault harness reads it after a crash (P70).
/// Compiled only with `fault-injection`; read-only.
#[cfg(feature = "fault-injection")]
pub mod custody_probe {
    use super::{
        filename, hash_file, id, read, reuse_sidecar, BulkloadRefusal, Capture, Path, Plan, Result,
        ReuseManifest,
    };
    use crate::refuse::RefuseAt as _;

    /// A manifest's seats: each path with the blob listed for it.
    pub type Seats = Vec<(Vec<u8>, String)>;

    /// For each plan item, in plan order, the bundle its `{item}.capture`
    /// record names, or `None` without a record.
    ///
    /// # Errors
    /// Refuses an unreadable plan or record.
    pub fn recorded(plan: &Path, corpus: &Path) -> Result<Vec<Option<String>>> {
        let contents: Plan = read(plan)?;
        contents
            .items
            .iter()
            .map(|item| {
                let record = corpus.join(format!("{}.capture", id(item)?));
                if !record.try_exists().refuse_at("estate::custody_probe")? {
                    return Ok(None);
                }
                Ok(Some(read::<Capture>(&record)?.bundle))
            })
            .collect()
    }

    /// The reuse manifest published for `bundle`: each seat's path and
    /// blob, or `None` without a `.reuse` sidecar.
    ///
    /// # Errors
    /// `RECEIPT_BINDING_INVALID` for a manifest that is not bound to the
    /// bytes at `bundle`'s name; refuses an unreadable bundle or sidecar.
    pub fn manifest(corpus: &Path, bundle: &str) -> Result<Option<Seats>> {
        if !filename(bundle) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let sidecar = reuse_sidecar(corpus, bundle);
        if !sidecar.try_exists().refuse_at("estate::custody_probe")? {
            return Ok(None);
        }
        let manifest: ReuseManifest = read(&sidecar)?;
        if manifest.digest != hash_file(&corpus.join(bundle))? {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Ok(Some(
            manifest
                .seats
                .into_iter()
                .map(|seat| (seat.row.rel_path, seat.object))
                .collect(),
        ))
    }
}

// `estate-apply --side-root` and `estate-unseat` (M2r2 item e).
pub mod seat;

// `estate-diff` (#226 item 1).
pub mod carried;
mod diff;
pub use diff::{diff, DiffOptions, Summary as DiffSummary, SCHEMA as DIFF_SCHEMA};

// `estate-handoff` (TIN-6569, OI-1003-Q183).
mod handoff;
pub use handoff::{handoff, HandoffMode, HandoffOptions, HandoffSummary, SCHEMA as HANDOFF_SCHEMA};
mod retarget;
pub use retarget::{mapped as retarget_mapped, retarget, Moved as Retargeted};

// `estate-move` and `estate-move-source` (M2 item 7).
pub mod moving;

// `estate-beside-retire` (operator ruling 2026-10-16 21:24Z).
pub mod beside_retire;

// S4 refusal sweep (2026-10-08): #219, #220, #162, #183.
#[cfg(test)]
mod s4_sweep_tests;

// P87 (#38, S5 under OI-1003-Q152): HEAD and index drift is custody.
#[cfg(test)]
mod drift_custody_tests;

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

    // P67's v1 row `Held ... Based Lost -> Refuse`, through the code: the
    // group's base record is retained, but a retained based bundle is bound
    // (`{bundle}.base`) to a base that is lost. That happens when the group's
    // base record was regenerated after its old base was lost. Every other
    // condition of a reuse hit holds, so only the lazily read bound base
    // (`decide::decide_recorded`, `bound_base`) keeps the record from being
    // reused: each item refuses RECEIPT_BINDING_INVALID and its record stays.
    #[test]
    fn a_retained_based_bundle_bound_to_a_lost_base_is_never_a_reuse_hit() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-bound-{}", std::process::id()));
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
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let plan = root.join("plan");
        add(
            &plan,
            &source,
            &repository,
            Some(&root.join("first-target")),
        )
        .unwrap();
        add(
            &plan,
            &second,
            &repository,
            Some(&root.join("second-target")),
        )
        .unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        // Whole-capture reuse needs seats older than one timestamp tick (R-N76).
        settle();
        let outcomes = |corpus: &Path| {
            let rows = Mutex::new(Vec::new());
            let result = capture(&plan, &state, corpus, 2, &|row| {
                rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                Ok(())
            });
            (result, rows.into_inner().unwrap())
        };
        capture(&plan, &state, &corpus, 2, &|_| Ok(())).unwrap();
        // The records are reuse hits while their bound base is retained.
        let (result, rows) = outcomes(&corpus);
        result.unwrap();
        assert_eq!(rows, vec![("capture-reused-after-census", None); 2]);
        let items = inspect(&plan).unwrap();
        let records: Vec<(PathBuf, Vec<u8>)> = items
            .iter()
            .map(|item| {
                let path = corpus.join(format!("{}.capture", id(item).unwrap()));
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let mut bound = Vec::new();
        for (path, _) in &records {
            let record: Capture = read(path).unwrap();
            assert!(git_carry::shared::requires_base(&corpus.join(&record.bundle)).unwrap());
            let base: Base = read(&corpus.join(format!("{}.base", record.bundle))).unwrap();
            bound.push(base);
        }
        let named = |base: &Base| (base.bundle.clone(), base.digest, base.identity);
        assert_eq!(bound.first().map(named), bound.last().map(named));
        let lost = bound.first().unwrap();
        // The old base is lost, and the group's base record regenerated: the
        // next pass's `prepare_base` exports and records a retained base.
        let groups: Vec<PathBuf> = fs::read_dir(&corpus)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let name = path.file_name().unwrap().to_str().unwrap();
                name.starts_with("shared-")
                    && Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension == "base")
            })
            .collect();
        assert_eq!(groups.len(), 1);
        fs::rename(corpus.join(&lost.bundle), root.join("lost-base")).unwrap();
        fs::remove_file(groups.first().unwrap()).unwrap();
        let (result, rows) = outcomes(&corpus);
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![("refused", Some("RECEIPT_BINDING_INVALID".to_owned())); 2]
        );
        // The group's base record is back and retained, and it is not the
        // base the records are bound to.
        let regenerated: Base = read(groups.first().unwrap()).unwrap();
        assert!(retained_base(&corpus, &regenerated).unwrap());
        assert_ne!(regenerated.identity, lost.identity);
        assert!(!retained_base(&corpus, lost).unwrap());
        // Neither record was replaced: the missing custody stays visible.
        for (path, bytes) in &records {
            assert_eq!(&fs::read(path).unwrap(), bytes);
        }
        fs::remove_dir_all(root).unwrap();
    }

    // OI-1001-Q2: the space plan charges pending bundles before apply writes,
    // and the closure report reads apply's own ledger back, failing closed on
    // a planned item apply never recorded.
    #[test]
    #[allow(clippy::too_many_lines)]
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
        // filesystem for its objects and again for its checkout (review B4),
        // and twice (bundle and base staging) to the corpus's; same device
        // here.
        let contents: Plan = read(&plan).unwrap();
        let identity = id(contents.items.first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{identity}.capture"))).unwrap();
        let bundle = fs::metadata(corpus.join(&record.bundle)).unwrap().len();
        let planned = space_plan(&contents, &corpus, &applied, "neo", 1);
        // Objects and checkout at the target's nearest existing ancestor,
        // and the corpus.
        assert_eq!(planned.len(), 3);
        let bytes: u64 = planned.iter().map(|want| want.bytes).sum();
        assert_eq!(bytes, 4 * bundle);
        let probe = &planned.first().unwrap().path;
        let space = crate::space::probe(probe).unwrap();
        assert_eq!(
            crate::space::check(bytes, space, 100),
            Err(BulkloadRefusal::DestinationSpaceInsufficient)
        );
        assert_eq!(crate::space::check(bytes, space, 0), Ok(()));

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
        // M2 round 2, item 1: the re-captured item is pending again, but its
        // landed repository already holds every object of its bundle, so
        // its objects are charged nothing; its checkout, which records an
        // earlier capture, is charged the capture's own bundle (a converge
        // moves it forward, review B3), and the corpus its staging.
        let charged = |plan: Vec<crate::space::Planned>| -> Vec<(PathBuf, u64)> {
            plan.into_iter()
                .map(|want| (want.path, want.bytes))
                .collect()
        };
        assert_eq!(
            charged(space_plan(&contents, &corpus, &applied, "neo", 1)),
            vec![(target.clone(), bundle), (corpus.clone(), 2 * bundle)]
        );
        fs::remove_file(&record_path).unwrap();
        fs::write(&record_path, original).unwrap();
        // The workspace ledger records the landed capture under the label.
        assert_eq!(landed_capture(&target, "neo"), Some(record.digest));
        assert_eq!(landed_capture(&target, "sting"), None);

        // Review B3 (probe C): every object held is not "nothing left to
        // write". An apply a crash cut after its import, before its
        // checkout was recorded (no journal, no workspace ledger), charges
        // its whole checkout again, though the repository charges nothing.
        let journal = journal_path(&applied, &identity, "neo", &record.digest);
        let done = fs::read(&journal).unwrap();
        fs::remove_file(&journal).unwrap();
        let git_dir = git_carry::landed::worktree_git_dir(&target)
            .unwrap()
            .unwrap();
        let ledger_file = git_carry::landed::ledger_dir(&git_dir, "neo")
            .join(git_carry::landed::WORKSPACE_LANDED);
        let landed_bytes = fs::read(&ledger_file).unwrap();
        fs::remove_file(&ledger_file).unwrap();
        assert_eq!(
            charged(space_plan(&contents, &corpus, &applied, "neo", 1)),
            vec![(target, bundle), (corpus.clone(), 2 * bundle)]
        );
        // With the ledger back the checkout is this capture's: only staging.
        fs::write(&ledger_file, landed_bytes).unwrap();
        assert_eq!(
            charged(space_plan(&contents, &corpus, &applied, "neo", 1)),
            vec![(corpus.clone(), 2 * bundle)]
        );
        fs::write(&journal, done).unwrap();

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
        // Objects and checkout at the root (the targets' nearest existing
        // ancestor), and the corpus.
        assert_eq!(planned.len(), 3);
        assert_eq!(
            planned.iter().map(|want| want.bytes).sum::<u64>(),
            4 * bundle
        );

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
    // uncaptured seats) is deferred (bulkload#48); carry v2, the W6 engine it
    // was deferred to, is deleted (OI-1003-Q44, OI-1003-Q56).
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
    // never leave a whole-capture Hit on a bundle carrying B. The exclude
    // file and the configuration refuse. HEAD (detached or symbolic) and the
    // index are drift custody since #38 (S5 under OI-1003-Q152): the bundle
    // carries B, which the source held at the export's snapshot, the moves
    // are named, the key is poisoned, and the next pass carries A.
    #[allow(clippy::too_many_lines)]
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
            // Review finding 4 (#38): an agent switching a nested worktree
            // away and back across the two unguarded windows. The export's
            // census holds the switched HEAD, both keys the original one.
            "nested-worktree" => {
                let nested = inside.join(".claude/worktrees/agent-x");
                fs::create_dir_all(inside.join(".claude/worktrees")).unwrap();
                r3_git_out(
                    &inside,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "agent-x",
                        nested.to_str().unwrap(),
                    ],
                );
                // agent-y: another commit of the same tree, so the switch
                // moves the nested HEAD (its custody) and no seat.
                #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax.
                let tree = rev_parse(&inside, "HEAD^{tree}");
                let y = r3_git_out(&inside, &["commit-tree", &tree, "-p", &a, "-m", "y"])
                    .trim()
                    .to_owned();
                git(&inside, &["branch", "agent-y", &y]);
                let n2 = nested.clone();
                (
                    Box::new(move || git(&nested, &["switch", "-q", "agent-y"])),
                    Box::new(move || git(&n2, &["switch", "-q", "agent-x"])),
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
        // Every receipt of the first capture, with its drift rows.
        let rows = Mutex::new(Vec::new());
        let _ = capture(&plan, &state, &corpus, 2, &|row| {
            rows.lock()
                .unwrap()
                .push((row.outcome, row.reason.clone(), row.drift.clone()));
            Ok(())
        });
        let drifted = rows.into_inner().unwrap();
        let first: Vec<(&str, Option<String>)> = drifted
            .iter()
            .map(|(outcome, reason, _)| (*outcome, reason.clone()))
            .collect();
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
                // The bundle's nested custody names agent-y: never a Hit.
                "nested-worktree" => false,
                #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax.
                _ => r3_git_out(&scratch, &["ls-tree", "refs/b/staged^{tree}"])
                    .contains("100644 blob"),
            };
            assert!(carried, "{class}: a whole-capture Hit on a stale bundle");
        }
        match class {
            "detached-head" | "symbolic-head" | "index" | "nested-worktree" => {
                let name = match class {
                    "index" => "IndexRewritten \"index\"",
                    "nested-worktree" => "NestedWorktreeMoved \".claude/worktrees/agent-x\"",
                    _ => "HeadMoved \"HEAD\"",
                };
                assert_eq!(first.len(), 1, "{class}: {first:?}");
                assert_eq!(
                    first.first().unwrap().0,
                    "captured-with-drift",
                    "{class}: {first:?}"
                );
                assert!(
                    drifted.first().unwrap().2.contains(&name.to_owned()),
                    "{class}: {drifted:?}"
                );
                assert!(!hit, "{class}: a drifted capture is never a Hit");
                // The next pass carries A, the source's state again.
                assert!(
                    matches!(
                        second.first().unwrap().0,
                        "captured" | "capture-extended-from-drift"
                    ),
                    "{class}: {second:?}"
                );
            }
            _ => assert_eq!(
                first,
                vec![("refused", Some("GIT_AUTHORITY_CHANGED".to_owned()))],
                "{class}: authority moved inside the capture window"
            ),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r3_detached_head_round_trip_across_unguarded_windows_is_drift_custody() {
        r3_authority_round_trip("detached-head");
    }

    #[test]
    fn r3_symbolic_head_round_trip_across_unguarded_windows_is_drift_custody() {
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
    fn r3_nested_worktree_round_trip_across_unguarded_windows_is_drift_custody() {
        r3_authority_round_trip("nested-worktree");
    }

    #[test]
    fn r3_index_round_trip_across_unguarded_windows_is_drift_custody() {
        r3_authority_round_trip("index");
    }

    // N1, stash: refs/stash keeps its value throughout; only the reflog below
    // it is rewritten and restored, so the export's snapshot (B) lacks stash
    // S1 while both keys (A) hold it. Since OI-1003-Q161 the stash is drift
    // custody like HEAD and the index: the move is named (`StashMoved`), the
    // key is poisoned, so the stale bundle is never a whole-capture Hit, and
    // the next pass carries A, S1 included. Before Q161 it refused
    // GIT_AUTHORITY_CHANGED.
    #[test]
    fn r3_stash_reflog_round_trip_across_unguarded_windows_is_drift_custody() {
        use git_carry::mid_pass::Stage;
        let (root, source, _, plan, corpus) = drifting_plan("r3-stash");
        let state = root.join("state");
        fs::write(source.join("tracked"), b"stash one").unwrap();
        git(&source, &["stash", "push", "-q"]);
        let s1 = rev_parse(&source, "refs/stash");
        fs::write(source.join("tracked"), b"stash two").unwrap();
        git(&source, &["stash", "push", "-q"]);
        let s2 = rev_parse(&source, "refs/stash");
        // The stash list A, messages included: stored back under the same
        // messages it is A again (OI-1003-Q169: a message is identity).
        let a = r3_git_out(
            &source,
            &["reflog", "show", "--format=%H %gs", "refs/stash"],
        );
        let mut messages = a
            .lines()
            .map(|line| line.split_once(' ').unwrap().1.to_owned());
        let (m2, m1) = (messages.next().unwrap(), messages.next().unwrap());
        let inside = fs::canonicalize(&source).unwrap();
        let (i1, i2) = (inside.clone(), inside);
        let (s1c, s2c) = (s1.clone(), s2.clone());
        settle();
        git_carry::mid_pass::arm_at(&source, Stage::Snapshot, move || {
            git(&i1, &["stash", "drop", "-q", "stash@{1}"]);
        });
        git_carry::mid_pass::arm_at(&source, Stage::AfterPass, move || {
            git(&i2, &["stash", "clear"]);
            git(&i2, &["stash", "store", "-m", &m1, &s1c]);
            git(&i2, &["stash", "store", "-m", &m2, &s2c]);
        });
        let rows = Mutex::new(Vec::new());
        let _ = capture(&plan, &state, &corpus, 2, &|row| {
            rows.lock()
                .unwrap()
                .push((row.outcome, row.reason.clone(), row.drift.clone()));
            Ok(())
        });
        let first = rows.into_inner().unwrap();
        assert_eq!(
            r3_git_out(
                &source,
                &["reflog", "show", "--format=%H %gs", "refs/stash"]
            ),
            a,
            "the source is back at A"
        );
        assert!(a.starts_with(&format!("{s2} ")), "{a}");
        assert_eq!(first.len(), 1, "{first:?}");
        let (outcome, _, drift) = first.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift", "{first:?}");
        assert!(
            drift.contains(&"StashMoved \"refs/stash\"".to_owned()),
            "{first:?}"
        );
        // The drifted capture's bundle is B: it lost S1.
        assert!(!recorded_heads(&plan, &corpus, &source).contains(&format!("stashes/{s1}")));
        settle();
        let second = attempt(&plan, &state, &corpus);
        assert_ne!(
            second.first().unwrap().0,
            "capture-reused-after-census",
            "a whole-capture Hit on a bundle that lost stash S1"
        );
        assert!(
            matches!(
                second.first().unwrap().0,
                "captured" | "capture-extended-from-drift"
            ),
            "{second:?}"
        );
        let heads = recorded_heads(&plan, &corpus, &source);
        assert!(heads.contains(&format!("stashes/{s1}")), "{heads}");
        assert!(heads.contains(&format!("stashes/{s2}")), "{heads}");
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

    // #216 (OI-1003-Q129, OI-1003-Q135): a standalone item cloned from a
    // local path (origin `/abs/path`, the xoruby estate's ~200 clones)
    // restores complete instead of refusing GIT_AUTHORITY_CHANGED after its
    // working tree was written. The origin is preserved inert, named in the
    // activation receipt, and the destination has no `origin` remote; the
    // journal is written, so a rerun replays rather than refusing
    // GIT_DESTINATION_OCCUPIED.
    #[test]
    fn a_standalone_item_with_a_local_path_origin_restores_with_the_origin_inert() {
        let root =
            std::env::temp_dir().join(format!("tcfs-estate-inert-origin-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let upstream = root.join("upstream");
        fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "--template="]);
        fs::write(upstream.join("tracked"), b"base").unwrap();
        git(&upstream, &["add", "tracked"]);
        git(&upstream, &["commit", "-m", "base"]);
        let source = root.join("source");
        git(
            &root,
            &[
                "clone",
                "-q",
                "--template=",
                upstream.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        fs::write(source.join("tracked"), b"dirty").unwrap();
        fs::write(source.join("untracked"), b"kept").unwrap();
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let (state, corpus) = (root.join("state"), root.join("corpus"));
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let applied = root.join("applied");
        let outcomes = Mutex::new(Vec::new());
        let record = |row: &Receipt| {
            outcomes
                .lock()
                .unwrap()
                .push((row.outcome, row.refusal.clone()));
            Ok(())
        };
        apply(&plan, &corpus, &applied, "neo", 1, &record).unwrap();
        apply(&plan, &corpus, &applied, "neo", 1, &record).unwrap();
        assert_eq!(
            outcomes.into_inner().unwrap(),
            vec![
                ("workspace-restored", None),
                ("previous-workspace-restoration-not-revalidated", None)
            ]
        );
        assert_eq!(fs::read(target.join("tracked")).unwrap(), b"dirty");
        assert_eq!(fs::read(target.join("untracked")).unwrap(), b"kept");
        let config = |key: &str| {
            Command::new("git")
                .arg("-C")
                .arg(&target)
                .args(["config", "--local", key])
                .output()
                .unwrap()
        };
        assert!(!config("remote.origin.url").status.success());
        assert!(!config("remote.origin.fetch").status.success());
        let (activated, preserved, _): (Vec<String>, Vec<String>, Option<(String, String)>) =
            postcard::from_bytes(
                &fs::read(target.join(".git/carry-config/configuration-activation.postcard"))
                    .unwrap(),
            )
            .unwrap();
        assert!(preserved.iter().any(|key| key == "remote.origin.url"));
        assert!(!activated
            .iter()
            .any(|key| key.starts_with("remote.origin.")));
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

    /// M2 round 2, d: the in-place batch i02 (900 items) was captured on
    /// neo by an engine (`5beb0314`) older than the `.omissions` sidecar
    /// (`3fe1f075`), so `closure-report` listed every item's omissions
    /// `unknown`. Each bundle still names what it omitted in band. This
    /// fixture is that corpus: two captures whose sidecars are gone, one
    /// that omitted `node_modules` and one that omitted nothing. Before
    /// apply the omitting one is unknown (`value-not-held`: its list is in
    /// a pack nothing has indexed) and the other is known empty from its
    /// header; after apply both are known, the list read by oid from the
    /// destination repository, equal to what the sidecar said.
    #[test]
    fn a_corpus_from_before_the_omissions_sidecar_is_classified_in_band() {
        use crate::closure::Report;
        let root = fresh("omissions-in-band");
        let omitting = repository(&root, "omitting", b"tracked");
        fs::create_dir_all(omitting.join("node_modules/pkg")).unwrap();
        fs::write(omitting.join("node_modules/pkg/index.js"), b"rebuildable").unwrap();
        let quiet = repository(&root, "quiet", b"quiet");
        let plan = root.join("plan");
        add(
            &plan,
            &omitting,
            &root.join("omitting-dest"),
            Some(&root.join("omitting-dest")),
        )
        .unwrap();
        add(
            &plan,
            &quiet,
            &root.join("quiet-dest"),
            Some(&root.join("quiet-dest")),
        )
        .unwrap();
        let (state, corpus, applied) = (
            root.join("state"),
            root.join("corpus"),
            root.join("applied"),
        );
        private_directory(&state).unwrap();
        private_directory(&corpus).unwrap();
        private_directory(&applied).unwrap();
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let report_with = |read: bool| {
            Report::from_ledger(
                &ledger_with(&plan, &corpus, "neo", std::slice::from_ref(&applied), read).unwrap(),
            )
            .omissions
        };
        let report = || report_with(true);
        // With the sidecars, as the engine writes them today.
        let with_sidecars = report();
        assert_eq!((with_sidecars.known, with_sidecars.unknown.len()), (2, 0));
        assert_eq!(
            with_sidecars
                .rows
                .iter()
                .map(|row| row.1.clone())
                .collect::<Vec<_>>(),
            vec![b"node_modules".to_vec()]
        );
        // The pre-sidecar corpus: every `.omissions` sidecar gone.
        for entry in fs::read_dir(&corpus).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "omissions")
            {
                fs::remove_file(path).unwrap();
            }
        }
        let before = report();
        assert_eq!((before.known, before.unknown.len()), (1, 1), "{before:?}");
        assert_eq!(
            before.unknown_because.get("value-not-held"),
            Some(&1),
            "{before:?}"
        );
        assert!(before.rows.is_empty());
        apply(&plan, &corpus, &applied, "neo", 1, &|_| Ok(())).unwrap();
        let after = report();
        assert_eq!((after.known, after.unknown.len()), (2, 0), "{after:?}");
        assert_eq!(after.rows, with_sidecars.rows);
        assert!(after.unknown_because.is_empty());
        // Without destination reads (the default) the header decides alone:
        // the named list stays unknown and says it was not read.
        let header_only = report_with(false);
        assert_eq!((header_only.known, header_only.unknown.len()), (1, 1));
        assert_eq!(header_only.unknown_because.get("value-not-read"), Some(&1));
        fs::remove_dir_all(root).unwrap();
    }

    // 1 TiB filesystem with 32 KiB available: a small checkout fits, a
    // 64 KiB one does not.
    #[allow(clippy::unnecessary_wraps)]
    fn tight(_: &Path) -> Result<crate::space::Limits> {
        Ok(crate::space::Limits::filesystem(crate::space::Space {
            total: 1 << 40,
            available: 32 * 1024,
        }))
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
            (git_carry::CapturePolicy::default(), decide::Policy::CODE),
            &space,
            &|row| {
                rows.lock()
                    .unwrap()
                    .push((row.source.clone(), row.outcome, row.reason.clone()));
                Ok(())
            },
        );
        // Any refused item makes the pass refuse, after every item ran,
        // with the run-level code, never a contract fault (#231).
        assert_eq!(outcome, Err(BulkloadRefusal::ItemsRefused));
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
            let chain = chain_links(&fixture.corpus, &previous.0, LinkBinding::Digest).unwrap();
            assert!(chain.bases.is_empty(), "an ungrouped chain binds no base");
            let flat = git_carry::chain::flatten(
                git_carry::stage_bundle(&head).unwrap(),
                &chain.bases,
                &chain.links,
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
        let (_, charged) = item_space(
            &item,
            &pulled,
            &fixture.root.join("applied"),
            "neo",
            &mut Charged::default(),
        )
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

// Q42 lane L6b, fix 2 (OI-1003-Q62, OI-1003-Q63, R-N72): a grouped item's
// capture chains on its prior capture under the group's plan base. P68
// (`tests/git_grouped_chain.rs`) holds the byte bound through the verbs;
// these tests hold the custody: the bound bases, the base record that is
// never replaced, the typed refusals, and corpora written before L6b.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod l6b_grouped_chain {
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

    // Whole-capture reuse needs seats older than one timestamp tick (R-N76).
    fn settle() {
        std::thread::sleep(std::time::Duration::from_nanos(
            u64::try_from(git_carry::RACY_GRANULARITY_NS).unwrap() + 100_000_000,
        ));
    }

    type Rows = Vec<(&'static str, Option<String>)>;

    struct Fixture {
        root: PathBuf,
        /// The main checkout, on `main`.
        source: PathBuf,
        /// (source checkout, restore target), one per item: main, then in a
        /// group the `wt` branch worktree.
        items: Vec<(PathBuf, PathBuf)>,
        plan: PathBuf,
        state: PathBuf,
        corpus: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // A main checkout with an `extra` branch one commit off main, and, when
    // `grouped`, a `wt` branch worktree: two items on one plan base.
    fn fixture(name: &str, grouped: bool) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("tcfs-estate-l6b-{name}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template=", "-b", "main"]);
        fs::write(source.join("file"), b"base\n").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        git(&source, &["checkout", "-q", "-b", "extra"]);
        fs::write(source.join("extra"), b"only on extra\n").unwrap();
        git(&source, &["add", "extra"]);
        git(&source, &["commit", "-m", "extra"]);
        git(&source, &["checkout", "-q", "main"]);
        let plan = root.join("plan");
        let mut items = Vec::new();
        if grouped {
            let wt = root.join("wt");
            git(
                &source,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "wt",
                    wt.to_str().unwrap(),
                    "main",
                ],
            );
            let repository = root.join("repository");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--template="]);
            for (checkout, target) in [(&source, "target-main"), (&wt, "target-wt")] {
                let target = root.join(target);
                add(&plan, checkout, &repository, Some(&target)).unwrap();
                items.push((checkout.clone(), target));
            }
        } else {
            let target = root.join("target");
            add(&plan, &source, &target, Some(&target)).unwrap();
            items.push((source.clone(), target));
        }
        Fixture {
            state: root.join("state"),
            corpus: root.join("corpus"),
            source,
            items,
            plan,
            root,
        }
    }

    impl Fixture {
        // One capture pass deciding under `decision`: the code's policy, or
        // v1's to write the corpus a pass from before L6b left.
        fn pass(&self, decision: decide::Policy) -> (Result<()>, Rows) {
            private_directory(&self.state).unwrap();
            private_directory(&self.corpus).unwrap();
            let rows = Mutex::new(Vec::new());
            let result = capture_in(
                &self.plan,
                &self.state,
                &self.corpus,
                1,
                (git_carry::CapturePolicy::default(), decision),
                &CorpusSpace::live(&self.corpus),
                &|row| {
                    rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                    Ok(())
                },
            );
            (result, rows.into_inner().unwrap())
        }

        fn captured(&self, decision: decide::Policy) {
            let (result, rows) = self.pass(decision);
            result.unwrap();
            assert!(
                rows.iter().all(|(outcome, _)| *outcome == "captured"),
                "{rows:?}"
            );
        }

        // A commit on main. In a group the ref it moves is every item's, so
        // every item's key changes.
        fn commit(&self, round: u32) {
            fs::write(self.source.join("file"), format!("round {round}\n")).unwrap();
            git(
                &self.source,
                &["commit", "-q", "-am", &format!("round {round}")],
            );
        }

        fn record(&self, index: usize) -> Capture {
            let item = inspect(&self.plan).unwrap().remove(index);
            read(&self.corpus.join(format!("{}.capture", id(&item).unwrap()))).unwrap()
        }

        fn prior(&self, bundle: &str) -> Option<Prior> {
            let sidecar = prior_sidecar(&self.corpus, bundle);
            sidecar.exists().then(|| read(&sidecar).unwrap())
        }

        fn apply(&self, state: &str) -> (Result<()>, Rows) {
            let rows = Mutex::new(Vec::new());
            let result = apply(
                &self.plan,
                &self.corpus,
                &self.root.join(state),
                "neo",
                1,
                &|row| {
                    rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                    Ok(())
                },
            );
            (result, rows.into_inner().unwrap())
        }

        fn restored(&self, state: &str) {
            let (result, rows) = self.apply(state);
            assert!(result.is_ok(), "{result:?}: {rows:?}");
            for (checkout, target) in &self.items {
                assert_eq!(head_of(target), head_of(checkout), "{}", target.display());
                assert_eq!(
                    fs::read(target.join("file")).unwrap(),
                    fs::read(checkout.join("file")).unwrap(),
                    "{}",
                    target.display()
                );
            }
        }

        // An empty destination: no restore target, and a fresh repository
        // for a group's items to restore into.
        fn reset(&self) {
            for (_, target) in &self.items {
                let _ = fs::remove_dir_all(target);
            }
            let repository = self.root.join("repository");
            if repository.exists() {
                fs::remove_dir_all(&repository).unwrap();
                fs::create_dir(&repository).unwrap();
                git(&repository, &["init", "--template="]);
            }
        }

        // Apply into an empty destination under the fresh state `state`.
        fn restored_afresh(&self, state: &str) {
            self.reset();
            self.restored(state);
        }

        // The group's one base record in the corpus.
        fn base_record(&self) -> PathBuf {
            let groups = capture_groups(&read::<Plan>(&self.plan).unwrap()).unwrap();
            assert_eq!(groups.bases.len(), 1);
            let group = groups.bases.keys().next().unwrap();
            self.corpus.join(format!("shared-{group}.base"))
        }
    }

    // Back-compat: a v1 grouped record is a delta on the plan base alone.
    // The next changed capture chains on it under the base, at depth 1, and
    // the restore imports the base, then the v1 bundle, then the head.
    #[test]
    fn a_v1_grouped_based_record_chains_under_its_base_on_the_next_changed_capture() {
        let fixture = fixture("v1-based", true);
        fixture.captured(decide::Policy::V1);
        fixture.commit(1);
        fixture.captured(decide::Policy::V1);
        let v1: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        for record in &v1 {
            // What v1 wrote: a based bundle with no chain link.
            assert!(
                git_carry::shared::requires_base(&fixture.corpus.join(&record.bundle)).unwrap()
            );
            assert!(fixture.prior(&record.bundle).is_none());
            assert!(bound(&fixture.corpus, &record.bundle).unwrap().is_some());
        }
        fixture.commit(2);
        fixture.captured(decide::Policy::CODE);
        let base: Base = read(&fixture.base_record()).unwrap();
        for (index, old) in v1.iter().enumerate() {
            let record = fixture.record(index);
            let prior = fixture
                .prior(&record.bundle)
                .expect("chained on the v1 bundle");
            assert_eq!(
                (&prior.bundle, prior.digest, prior.depth),
                (&old.bundle, old.digest, 0)
            );
            let chain = chain_links(&fixture.corpus, &record.bundle, LinkBinding::Digest).unwrap();
            assert_eq!(
                chain.links,
                vec![(fixture.corpus.join(&old.bundle), old.digest)]
            );
            assert_eq!(
                chain.bases,
                vec![(fixture.corpus.join(&base.bundle), base.digest)]
            );
            // The space preflight charges the head, its link and its base.
            let item = inspect(&fixture.plan).unwrap().remove(index);
            let (_, bytes) = item_space(
                &item,
                &fixture.corpus,
                &fixture.root.join("none"),
                "neo",
                &mut Charged::default(),
            )
            .unwrap()
            .unwrap();
            let size = |name: &str| fs::metadata(fixture.corpus.join(name)).unwrap().len();
            assert_eq!(
                bytes,
                size(&record.bundle) + size(&old.bundle) + size(&base.bundle)
            );
        }
        fixture.restored("applied");
    }

    // Back-compat: a v1 ungrouped chain (`.prior`, no `.base`) is still a
    // reuse hit under the code's policy, which reads its bound bases and
    // finds none, and it extends as a plain chain.
    #[test]
    fn a_v1_ungrouped_chain_keeps_hitting_and_extends_as_a_plain_chain() {
        let fixture = fixture("v1-chain", false);
        fixture.captured(decide::Policy::V1);
        fixture.commit(1);
        settle();
        fixture.captured(decide::Policy::V1);
        let v1 = fixture.record(0);
        assert_eq!(fixture.prior(&v1.bundle).map(|prior| prior.depth), Some(0));
        assert!(bound(&fixture.corpus, &v1.bundle).unwrap().is_none());
        assert_eq!(
            bound_base(&fixture.corpus, &v1.bundle).unwrap(),
            decide::PrevBase::None
        );
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        result.unwrap();
        assert_eq!(rows, vec![("capture-reused-after-census", None)]);
        fixture.commit(2);
        fixture.captured(decide::Policy::CODE);
        let record = fixture.record(0);
        let prior = fixture.prior(&record.bundle).expect("chained");
        assert_eq!((&prior.bundle, prior.depth), (&v1.bundle, 1));
        assert!(bound(&fixture.corpus, &record.bundle).unwrap().is_none());
        let chain = chain_links(&fixture.corpus, &record.bundle, LinkBinding::Digest).unwrap();
        assert_eq!((chain.links.len(), chain.bases.len()), (2, 0));
        fixture.restored("applied");
    }

    // Back-compat: v1 records bound to a base that is lost, under a base
    // record regenerated since, still refuse RECEIPT_BINDING_INVALID while
    // their key holds, and the records stay (the missing custody is kept
    // visible). Once the source moves, the next capture is the new base's
    // delta alone: it never chains on a bundle nothing can restore.
    #[test]
    fn v1_records_bound_to_a_lost_base_refuse_and_are_never_chained_on() {
        let fixture = fixture("v1-lost", true);
        settle();
        fixture.captured(decide::Policy::V1);
        let v1: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        let lost = bound(&fixture.corpus, &v1[0].bundle).unwrap().unwrap();
        fs::rename(fixture.corpus.join(&lost.bundle), fixture.root.join("lost")).unwrap();
        fs::remove_file(fixture.base_record()).unwrap();
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![("refused", Some("RECEIPT_BINDING_INVALID".to_owned())); 2]
        );
        for (index, old) in v1.iter().enumerate() {
            assert_eq!(fixture.record(index).bundle, old.bundle);
            assert_eq!(
                bound_base(&fixture.corpus, &old.bundle).unwrap(),
                decide::PrevBase::Lost
            );
        }
        fixture.commit(1);
        fixture.captured(decide::Policy::CODE);
        let regenerated: Base = read(&fixture.base_record()).unwrap();
        for index in 0..2 {
            let record = fixture.record(index);
            assert!(
                fixture.prior(&record.bundle).is_none(),
                "never chained on it"
            );
            let base = bound(&fixture.corpus, &record.bundle).unwrap().unwrap();
            assert_eq!(base.bundle, regenerated.bundle);
        }
        fixture.restored("applied");
    }

    // R-N72: a base record is never replaced. A record that appears between
    // `prepare_base`'s check and its write (another capture of this corpus)
    // stands byte for byte, and the pass refuses RECEIPT_BINDING_INVALID
    // rather than bind its items to a base the record does not name.
    #[test]
    fn prepare_base_never_replaces_a_record_that_appears_concurrently() {
        let fixture = fixture("no-replace", true);
        private_directory(&fixture.corpus).unwrap();
        let record = fixture.base_record();
        let other = postcard::to_allocvec(&Base {
            bundle: "shared-another-capture.bundle".to_owned(),
            digest: [7; 32],
            identity: crate::freshness::StatIdentity::from_metadata(
                &fs::symlink_metadata(&fixture.corpus).unwrap(),
            ),
        })
        .unwrap();
        // Whichever item prepares the group's base, the record appears just
        // before its write.
        for (checkout, _) in &fixture.items {
            let (record, other) = (record.clone(), other.clone());
            git_carry::mid_pass::arm_at(
                checkout,
                git_carry::mid_pass::Stage::BasePublished,
                move || {
                    if !record.exists() {
                        fs::write(&record, &other).unwrap();
                    }
                },
            );
        }
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![("refused", Some("RECEIPT_BINDING_INVALID".to_owned())); 2]
        );
        assert_eq!(fs::read(&record).unwrap(), other, "the record stands");
        // No pending file is left beside it, and no item was captured.
        let leftovers: Vec<String> = fs::read_dir(&fixture.corpus)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("pending") || name.ends_with(".capture"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // The record write itself: a second write to the same path refuses
        // and leaves the first.
        let path = fixture.root.join("record");
        write_new(&path, &1u32).unwrap();
        assert_eq!(
            write_new(&path, &2u32).err(),
            Some(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(read::<u32>(&path).unwrap(), 1);
    }

    // #181, R-N72: a plan base the corpus no longer holds refuses an apply
    // SEALED_OBJECT_MISSING, never a bare IO errno: for a base's delta
    // (`import_base`) and for a chain under the base (`chain_links`,
    // `flatten`). Nothing reaches the destination.
    #[test]
    fn a_missing_plan_base_refuses_apply_by_name() {
        for chained in [false, true] {
            let fixture = fixture(
                if chained {
                    "base-gone-chain"
                } else {
                    "base-gone"
                },
                true,
            );
            fixture.captured(decide::Policy::CODE);
            if chained {
                fixture.commit(1);
                fixture.captured(decide::Policy::CODE);
            }
            for index in 0..2 {
                let record = fixture.record(index);
                assert_eq!(fixture.prior(&record.bundle).is_some(), chained);
            }
            let base: Base = read(&fixture.base_record()).unwrap();
            let held = fixture.root.join("held-base");
            fs::rename(fixture.corpus.join(&base.bundle), &held).unwrap();
            let (result, rows) = fixture.apply("applied");
            assert!(result.is_err());
            assert_eq!(
                rows,
                vec![("refused", Some("SEALED_OBJECT_MISSING".to_owned())); 2],
                "chained={chained}"
            );
            for (_, target) in &fixture.items {
                assert!(!target.exists(), "nothing is restored without the base");
            }
            // A flatten handed the missing base refuses the same way.
            if chained {
                let record = fixture.record(0);
                let link = fixture.prior(&record.bundle).unwrap();
                let refused = git_carry::chain::flatten(
                    git_carry::stage_bundle(&fixture.corpus.join(&record.bundle)).unwrap(),
                    &[(fixture.corpus.join(&base.bundle), base.digest)],
                    &[(fixture.corpus.join(&link.bundle), link.digest)],
                );
                assert_eq!(refused.err(), Some(BulkloadRefusal::SealedObjectMissing));
            }
            fs::rename(&held, fixture.corpus.join(&base.bundle)).unwrap();
            fixture.restored("applied");
        }
    }

    // OI-1003-Q63 D5: the group's base record goes missing while a record
    // still binds the old base. The next pass exports a new base, as v1
    // did, and its captures chain on records bound to the old one, so a
    // chain binds two bases and the restore imports both: the old base
    // holds a commit the link declares and the new base lacks, and the new
    // base holds a commit the head declares and the old base lacks.
    //
    // Cross-base custody: a flatten given only one of the two refuses, and
    // once the old base is lost the chain is not intact, so the next
    // capture is the new base's delta alone.
    #[test]
    fn a_chain_bound_to_two_bases_restores_from_both() {
        let fixture = fixture("two-bases", true);
        settle();
        fixture.captured(decide::Policy::CODE);
        let old: Base = read(&fixture.base_record()).unwrap();
        let first: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        // `extra`'s commit is now only in the old base and the first
        // captures' prerequisites; main's new commit is only in the new base.
        git(&fixture.source, &["branch", "-q", "-D", "extra"]);
        fixture.commit(1);
        fs::remove_file(fixture.base_record()).unwrap();
        settle();
        fixture.captured(decide::Policy::CODE);
        let new: Base = read(&fixture.base_record()).unwrap();
        assert_ne!(new.bundle, old.bundle);
        let bases = vec![
            (fixture.corpus.join(&old.bundle), old.digest),
            (fixture.corpus.join(&new.bundle), new.digest),
        ];
        for (index, link) in first.iter().enumerate() {
            let record = fixture.record(index);
            let prior = fixture
                .prior(&record.bundle)
                .expect("chained on the first capture");
            assert_eq!(prior.bundle, link.bundle);
            let chain = chain_links(&fixture.corpus, &record.bundle, LinkBinding::Custody).unwrap();
            assert_eq!(chain.bases, bases, "the link's base, then the head's");
            assert_eq!(
                bound_base(&fixture.corpus, &record.bundle).unwrap(),
                decide::PrevBase::Retained
            );
            // Both bases, the link's first: the chain flattens to one
            // self-contained bundle.
            let flat = git_carry::chain::flatten(
                git_carry::stage_bundle(&fixture.corpus.join(&record.bundle)).unwrap(),
                &chain.bases,
                &chain.links,
            )
            .unwrap();
            assert!(!git_carry::shared::requires_base(flat.path()).unwrap());
            // Either base alone leaves a prerequisite unsatisfied.
            for one in &bases {
                let refused = git_carry::chain::flatten(
                    git_carry::stage_bundle(&fixture.corpus.join(&record.bundle)).unwrap(),
                    std::slice::from_ref(one),
                    &chain.links,
                );
                assert_eq!(
                    refused.err(),
                    Some(BulkloadRefusal::GitInventoryMissingPrerequisite),
                    "{}",
                    one.0.display()
                );
            }
        }
        // The old base is lost: every chain that binds it is broken.
        let held = fixture.root.join("held-old-base");
        fs::rename(fixture.corpus.join(&old.bundle), &held).unwrap();
        let chained: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        for record in &chained {
            assert_eq!(
                chain_links(&fixture.corpus, &record.bundle, LinkBinding::Custody).err(),
                Some(BulkloadRefusal::SealedObjectMissing)
            );
            assert_eq!(
                bound_base(&fixture.corpus, &record.bundle).unwrap(),
                decide::PrevBase::Lost
            );
        }
        let (result, rows) = fixture.apply("applied");
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![("refused", Some("SEALED_OBJECT_MISSING".to_owned())); 2]
        );
        // Not a hit and not extended: the unchanged source is recaptured as
        // the new base's delta, which restores.
        fixture.captured(decide::Policy::CODE);
        for (index, broken) in chained.iter().enumerate() {
            let record = fixture.record(index);
            assert_ne!(record.bundle, broken.bundle);
            assert!(fixture.prior(&record.bundle).is_none());
            let base = bound(&fixture.corpus, &record.bundle).unwrap().unwrap();
            assert_eq!(base.bundle, new.bundle);
        }
        fixture.restored("applied");
    }

    // A capture that reproduces its chain's ROOT byte for byte (A -> B -> A:
    // an untracked file added, then removed) is that root. Under a plan base
    // the chained export of pass 3 declares the base's commits and the
    // link's held tips, which are the base's own commits, so its bytes are
    // the first capture's. The root's custody stands: it gets no `.prior`
    // (the link's chain already holds it, so one would close a cycle
    // root -> link -> root that `chain_links` refuses), and every pass
    // restores.
    #[test]
    fn a_capture_that_reproduces_its_chain_root_stands_as_the_root() {
        let fixture = fixture("root-again", true);
        let seat = fixture.source.join("new");
        let restored = |state: &str| {
            fixture.restored_afresh(state);
            assert_eq!(fixture.items[0].1.join("new").exists(), seat.exists());
        };
        let moved = |outcome: &'static str| {
            settle();
            let (result, rows) = fixture.pass(decide::Policy::CODE);
            result.unwrap();
            // Only main's worktree moved; `wt` is a hit.
            assert_eq!(
                rows,
                vec![(outcome, None), ("capture-reused-after-census", None)]
            );
        };
        settle();
        fixture.captured(decide::Policy::CODE);
        let root = fixture.record(0);
        assert!(fixture.prior(&root.bundle).is_none());
        restored("applied-1");
        // A -> B.
        fs::write(&seat, b"new\n").unwrap();
        moved("captured");
        let link = fixture.record(0);
        let prior = fixture.prior(&link.bundle).expect("chained on the root");
        assert_eq!((&prior.bundle, prior.depth), (&root.bundle, 0));
        restored("applied-2");
        // B -> A: the root again, three passes over.
        fs::remove_file(&seat).unwrap();
        for pass in 3..6 {
            moved(if pass == 3 {
                "captured"
            } else {
                "capture-reused-after-census"
            });
            let record = fixture.record(0);
            assert_eq!((&record.bundle, record.digest), (&root.bundle, root.digest));
            assert!(
                fixture.prior(&record.bundle).is_none(),
                "pass {pass}: the root is never chained on its own link"
            );
            let chain = chain_links(&fixture.corpus, &record.bundle, LinkBinding::Custody).unwrap();
            assert!(chain.links.is_empty() && chain.bases.is_empty());
            assert_eq!(
                bound_base(&fixture.corpus, &record.bundle).unwrap(),
                decide::PrevBase::Retained
            );
            restored(&format!("applied-{pass}"));
        }
        // The root is a root still: the next changed capture chains on it
        // at depth 0, and restores.
        fs::write(&seat, b"new again\n").unwrap();
        moved("captured");
        let again = fixture.record(0);
        let prior = fixture.prior(&again.bundle).unwrap();
        assert_eq!((&prior.bundle, prior.depth), (&root.bundle, 0));
        restored("applied-6");
    }

    // The same reproduction deeper in a chain (A -> B -> C -> B): the
    // capture is a link already in its own link's chain, so it keeps the
    // `.prior` and the depth it was recorded with.
    #[test]
    fn a_capture_that_reproduces_a_link_of_its_own_chain_keeps_that_links_custody() {
        let fixture = fixture("link-again", true);
        let (one, two) = (fixture.source.join("one"), fixture.source.join("two"));
        let pass = || {
            settle();
            let (result, rows) = fixture.pass(decide::Policy::CODE);
            result.unwrap();
            assert_eq!(
                rows,
                vec![("captured", None), ("capture-reused-after-census", None)]
            );
            fixture.record(0)
        };
        settle();
        fixture.captured(decide::Policy::CODE);
        let root = fixture.record(0);
        fs::write(&one, b"one\n").unwrap();
        let first = pass();
        fs::write(&two, b"two\n").unwrap();
        let second = pass();
        let prior = fixture.prior(&second.bundle).unwrap();
        assert_eq!((&prior.bundle, prior.depth), (&first.bundle, 1));
        fs::remove_file(&two).unwrap();
        let again = pass();
        assert_eq!(again.bundle, first.bundle);
        let prior = fixture.prior(&again.bundle).unwrap();
        assert_eq!((&prior.bundle, prior.depth), (&root.bundle, 0));
        let chain = chain_links(&fixture.corpus, &again.bundle, LinkBinding::Custody).unwrap();
        assert_eq!(chain.links.len(), 1);
        fixture.restored_afresh("applied");
        assert!(fixture.items[0].1.join("one").exists());
        assert!(!fixture.items[0].1.join("two").exists());
    }

    // Cross-base custody when the recapture reproduces the broken head. The
    // head's link tips are all commits of the new base (no branch is
    // deleted here), so once the old base is lost the unchanged source's
    // recapture, the new base's delta, has the head's bytes and so its
    // name. The stale `.prior` is removed before the record names the
    // bundle again: the record never names a chain nothing can restore, and
    // the capture restores from the new base alone.
    #[test]
    fn a_recapture_that_reproduces_a_broken_chains_head_drops_its_stale_link() {
        let fixture = fixture("two-bases-same-head", true);
        settle();
        fixture.captured(decide::Policy::CODE);
        let old: Base = read(&fixture.base_record()).unwrap();
        fixture.commit(1);
        fs::remove_file(fixture.base_record()).unwrap();
        settle();
        fixture.captured(decide::Policy::CODE);
        let new: Base = read(&fixture.base_record()).unwrap();
        assert_ne!(new.bundle, old.bundle);
        let heads: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        for head in &heads {
            assert!(fixture.prior(&head.bundle).is_some());
            let chain = chain_links(&fixture.corpus, &head.bundle, LinkBinding::Custody).unwrap();
            assert_eq!(chain.bases.len(), 2, "the old base and the new");
        }
        fixture.restored_afresh("applied-both");
        // The old base is lost: the chains are broken and apply refuses.
        fs::rename(
            fixture.corpus.join(&old.bundle),
            fixture.root.join("held-old-base"),
        )
        .unwrap();
        fixture.reset();
        let (result, rows) = fixture.apply("applied-lost");
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![("refused", Some("SEALED_OBJECT_MISSING".to_owned())); 2]
        );
        // The recapture reproduces each head; none keeps its stale link.
        for pass in 0..2 {
            settle();
            let (result, rows) = fixture.pass(decide::Policy::CODE);
            result.unwrap();
            let outcome = if pass == 0 {
                "captured"
            } else {
                "capture-reused-after-census"
            };
            assert_eq!(rows, vec![(outcome, None); 2]);
            for (index, head) in heads.iter().enumerate() {
                let record = fixture.record(index);
                assert_eq!(
                    (&record.bundle, record.digest),
                    (&head.bundle, head.digest),
                    "the new base's delta is the head, byte for byte"
                );
                assert!(fixture.prior(&record.bundle).is_none(), "pass {pass}");
                let base = bound(&fixture.corpus, &record.bundle).unwrap().unwrap();
                assert_eq!(base.bundle, new.bundle);
                assert_eq!(
                    bound_base(&fixture.corpus, &record.bundle).unwrap(),
                    decide::PrevBase::Retained
                );
            }
            fixture.restored_afresh(&format!("applied-recaptured-{pass}"));
        }
    }

    // A based bundle (its header declares the plan base's commits, and it
    // has no `.prior`) whose own `.base` sidecar is gone has lost its base:
    // apply refuses it SEALED_OBJECT_MISSING, so the capture side never
    // hides it behind a reuse hit. While its key holds it refuses
    // RECEIPT_BINDING_INVALID, the record stays, and it is never a chain
    // link; once the source moves the next capture is the base's delta
    // alone and restores.
    #[test]
    fn a_based_bundle_whose_base_sidecar_is_gone_is_never_a_hit_or_a_link() {
        let fixture = fixture("sidecar-gone", true);
        settle();
        fixture.captured(decide::Policy::CODE);
        let first: Vec<Capture> = (0..2).map(|index| fixture.record(index)).collect();
        let bundle = fixture.corpus.join(&first[0].bundle);
        assert!(git_carry::shared::requires_base(&bundle).unwrap());
        fs::remove_file(fixture.corpus.join(format!("{}.base", first[0].bundle))).unwrap();
        assert_eq!(
            bound_base(&fixture.corpus, &first[0].bundle).unwrap(),
            decide::PrevBase::Lost
        );
        assert_eq!(
            bound_base(&fixture.corpus, &first[1].bundle).unwrap(),
            decide::PrevBase::Retained
        );
        settle();
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![
                ("refused", Some("RECEIPT_BINDING_INVALID".to_owned())),
                ("capture-reused-after-census", None)
            ]
        );
        assert_eq!(fixture.record(0).bundle, first[0].bundle);
        // What the refusal stands for: apply cannot restore that item.
        let (result, rows) = fixture.apply("applied-lost");
        assert!(result.is_err());
        assert_eq!(
            rows,
            vec![
                ("refused", Some("SEALED_OBJECT_MISSING".to_owned())),
                ("workspace-restored", None)
            ]
        );
        // The source moves: item 0 is the base's delta alone, never chained
        // on the bundle nothing restores; item 1 chains as usual.
        fixture.commit(1);
        fixture.captured(decide::Policy::CODE);
        let record = fixture.record(0);
        assert!(
            fixture.prior(&record.bundle).is_none(),
            "never chained on it"
        );
        assert!(bound(&fixture.corpus, &record.bundle).unwrap().is_some());
        let other = fixture.record(1);
        assert_eq!(
            fixture.prior(&other.bundle).map(|prior| prior.bundle),
            Some(first[1].bundle.clone())
        );
        fixture.restored_afresh("applied");
        // A self-contained bundle with no sidecar is bound to no base.
        let alone = self::fixture("sidecar-none", false);
        alone.captured(decide::Policy::CODE);
        let record = alone.record(0);
        assert!(!git_carry::shared::requires_base(&alone.corpus.join(&record.bundle)).unwrap());
        assert_eq!(
            bound_base(&alone.corpus, &record.bundle).unwrap(),
            decide::PrevBase::None
        );
    }

    // OI-1003-Q63 D1 (#149): a drift-marked bundle may be a chain link. The
    // pass after a drifted capture extends it clean, chained on the drifted
    // bundle under the plan base: only that bundle's source-held tips become
    // prerequisites, so the head re-packs every seat the drifted pass
    // withdrew, and the flattened restore advertises exactly the head's
    // refs, none of the link's drift markers.
    #[test]
    fn a_drift_marked_bundle_may_be_a_chain_link_under_the_plan_base() {
        let fixture = fixture("drift-link", true);
        let inside = fs::canonicalize(&fixture.source).unwrap();
        git_carry::mid_pass::arm(&fixture.source, move || {
            fs::write(inside.join("file"), b"rewritten mid-pass\n").unwrap();
            fs::write(inside.join("appeared"), b"new seat\n").unwrap();
        });
        let outcomes = |rows: &Rows| rows.iter().map(|row| row.0).collect::<Vec<_>>();
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        result.unwrap();
        assert!(outcomes(&rows).contains(&"captured-with-drift"), "{rows:?}");
        let drifted = fixture.record(0);
        assert!(fixture
            .corpus
            .join(format!("{}.drift", drifted.bundle))
            .is_file());
        let (result, rows) = fixture.pass(decide::Policy::CODE);
        result.unwrap();
        assert!(
            outcomes(&rows).contains(&"capture-extended-from-drift"),
            "{rows:?}"
        );
        let record = fixture.record(0);
        let prior = fixture
            .prior(&record.bundle)
            .expect("chained on the drifted bundle");
        assert_eq!((&prior.bundle, prior.depth), (&drifted.bundle, 0));
        assert!(bound(&fixture.corpus, &record.bundle).unwrap().is_some());
        assert!(fixture
            .corpus
            .join(format!("{}.drift", prior.bundle))
            .is_file());
        fixture.restored("applied");
        let target = &fixture.items[0].1;
        assert_eq!(
            fs::read(target.join("file")).unwrap(),
            b"rewritten mid-pass\n"
        );
        assert_eq!(fs::read(target.join("appeared")).unwrap(), b"new seat\n");
    }

    // #183 item 2: a corpus named by a relative path flattens. The scratch
    // directory is made beside the canonical corpus, so the paths this
    // process writes and the paths its git children read (from that
    // directory) are the same files.
    #[test]
    fn a_chain_in_a_relative_corpus_path_flattens() {
        let fixture = fixture("relative", false);
        fixture.captured(decide::Policy::CODE);
        fixture.commit(1);
        fixture.captured(decide::Policy::CODE);
        let record = fixture.record(0);
        // The corpus relative to the working directory, without changing it.
        let here = std::env::current_dir().unwrap();
        let mut relative: PathBuf = here.components().skip(1).map(|_| "..").collect();
        relative.extend(
            fs::canonicalize(&fixture.corpus)
                .unwrap()
                .components()
                .skip(1),
        );
        assert!(relative.is_relative());
        let chain = chain_links(&relative, &record.bundle, LinkBinding::Digest).unwrap();
        assert_eq!(chain.links.len(), 1);
        assert!(chain.links[0].0.is_relative());
        let flat = git_carry::chain::flatten(
            git_carry::stage_bundle(&relative.join(&record.bundle)).unwrap(),
            &chain.bases,
            &chain.links,
        )
        .unwrap();
        assert!(!git_carry::shared::requires_base(flat.path()).unwrap());
    }
}

// Q42 lane L7 (OI-1003-Q42, OI-1003-Q45, OI-1003-Q94): the `{bundle}.reuse`
// manifest. P69 (`tests/git_reuse_cost.rs`) holds the cost law and P70's
// crash order is the fault harness's (`tests/fault_harness/
// estate_sidecar.rs`); these tests hold the binding: a manifest that does
// not match its capture is never reused, and nothing but blob reuse reads
// one.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod l7_manifest_reuse {
    use super::*;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
            .expect("git command");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    // Identity reuse needs seats older than one timestamp tick (R-N76).
    fn settle() {
        std::thread::sleep(std::time::Duration::from_nanos(
            u64::try_from(git_carry::RACY_GRANULARITY_NS).unwrap() + 100_000_000,
        ));
    }

    const FIRST: &[u8] = b"AAAA the content one commit back";
    const SECOND: &[u8] = b"BBBB the content at HEAD, equal.";

    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
        plan: PathBuf,
        state: PathBuf,
        corpus: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // A clean checkout whose `tracked` file has two same-sized versions in
    // history, so the source object store holds a wrong blob to offer.
    fn fixture(name: &str) -> Fixture {
        assert_eq!(FIRST.len(), SECOND.len());
        let root =
            std::env::temp_dir().join(format!("bulkload-estate-l7-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(source.join("other"), b"another tracked seat").unwrap();
        fs::write(source.join("tracked"), FIRST).unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "first"]);
        fs::write(source.join("tracked"), SECOND).unwrap();
        git(&source, &["commit", "--quiet", "-am", "second"]);
        let target = root.join("target");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        Fixture {
            source,
            target,
            plan,
            state: root.join("state"),
            corpus: root.join("corpus"),
            root,
        }
    }

    impl Fixture {
        // One pass: its outcome, the bytes it read and why it reused nothing.
        fn pass(&self) -> (&'static str, u64, Option<&'static str>) {
            let rows = Mutex::new(Vec::new());
            capture(&self.plan, &self.state, &self.corpus, 1, &|row| {
                rows.lock()
                    .unwrap()
                    .push((row.outcome, row.bytes_read, row.reuse_unavailable));
                Ok(())
            })
            .unwrap();
            let mut rows = rows.into_inner().unwrap();
            assert_eq!(rows.len(), 1);
            rows.pop().unwrap()
        }

        fn record(&self) -> Capture {
            let item = id(inspect(&self.plan).unwrap().first().unwrap()).unwrap();
            read(&self.corpus.join(format!("{item}.capture"))).unwrap()
        }

        // The manifest of the capture the record names, and its path.
        fn manifest(&self) -> (PathBuf, ReuseManifest) {
            let path = reuse_sidecar(&self.corpus, &self.record().bundle);
            let manifest = read(&path).unwrap();
            (path, manifest)
        }

        // Move the capture key without touching a seat: a new branch.
        fn move_key(&self, round: u32) {
            git(&self.source, &["branch", &format!("extra-{round}")]);
        }

        fn seat_bytes(&self) -> u64 {
            ["other", "tracked"]
                .iter()
                .map(|name| fs::metadata(self.source.join(name)).unwrap().len())
                .sum()
        }
    }

    // P70: a manifest is reused only when it is the record's own bundle's.
    // Each forged sidecar below names blobs the source object store holds,
    // at the seats' exact identities, so a pass that accepted it would put
    // the wrong content under `tracked` with no miss and no read. Each must
    // instead reuse nothing, say why, read every seat, and still capture.
    #[test]
    fn p70_a_manifest_that_does_not_match_its_capture_is_never_reused() {
        let fixture = fixture("mismatch");
        settle();
        assert_eq!(fixture.pass(), ("captured", fixture.seat_bytes(), None));
        let stale = git(&fixture.source, &["rev-parse", "HEAD~1:tracked"]);
        let tree = git(&fixture.source, &["rev-parse", "HEAD:"]);
        let swapped = |manifest: &ReuseManifest, object: &str| -> Vec<git_carry::ManifestSeat> {
            manifest
                .seats
                .iter()
                .cloned()
                .map(|seat| git_carry::ManifestSeat {
                    object: if seat.row.rel_path == b"tracked" {
                        object.to_owned()
                    } else {
                        seat.object
                    },
                    ..seat
                })
                .collect()
        };
        let mismatch = ("captured", fixture.seat_bytes(), Some("manifest-mismatch"));

        // The true manifest is reused whole: nothing is read.
        fixture.move_key(0);
        assert_eq!(fixture.pass(), ("captured", 0, None));

        // Bound to another capture: the stale blob under another digest.
        let (path, manifest) = fixture.manifest();
        assert_eq!(manifest.digest, fixture.record().digest);
        assert_eq!(manifest.seats.len(), 2);
        write(
            &path,
            &ReuseManifest {
                digest: [0x5a; 32],
                seats: swapped(&manifest, &stale),
            },
        )
        .unwrap();
        fixture.move_key(1);
        assert_eq!(fixture.pass(), mismatch);

        // Not a manifest at all.
        let (path, _) = fixture.manifest();
        fs::write(&path, b"\xff\xff not a postcard manifest").unwrap();
        fixture.move_key(2);
        assert_eq!(fixture.pass(), mismatch);

        // Bound to the record's bundle, and naming an object that is not a
        // blob: it does not describe a worktree tree.
        let (path, manifest) = fixture.manifest();
        write(
            &path,
            &ReuseManifest {
                digest: manifest.digest,
                seats: swapped(&manifest, &tree),
            },
        )
        .unwrap();
        fixture.move_key(3);
        assert_eq!(fixture.pass(), mismatch);

        // Every pass published its own manifest again, and the restore is
        // exact: the content at HEAD, never the stale blob.
        let (_, manifest) = fixture.manifest();
        assert_eq!(manifest.digest, fixture.record().digest);
        apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(fs::read(fixture.target.join("tracked")).unwrap(), SECOND);
        assert_eq!(
            fs::read(fixture.target.join("other")).unwrap(),
            b"another tracked seat"
        );
    }

    // Deliverable 2: a reader that does not know the sidecar ignores it
    // safely. Restore never reads `.reuse`: a corpus whose manifests are
    // garbage, or gone, restores byte for byte; and a whole-capture hit
    // reads none either.
    #[test]
    fn a_restore_and_a_hit_never_read_the_reuse_manifest() {
        let fixture = fixture("ignored");
        settle();
        assert_eq!(fixture.pass().0, "captured");
        fixture.move_key(1);
        assert_eq!(fixture.pass(), ("captured", 0, None));
        let (path, _) = fixture.manifest();
        fs::write(&path, b"\xff garbage where a manifest was").unwrap();
        // A hit: the key is unchanged, and the garbage is never opened (a
        // pass that opened it would say `manifest-mismatch`).
        assert_eq!(fixture.pass(), ("capture-reused-after-census", 0, None));
        apply(
            &fixture.plan,
            &fixture.corpus,
            &fixture.root.join("applied-garbage"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(fs::read(fixture.target.join("tracked")).unwrap(), SECOND);
        // Gone: what a corpus from before lane L7 looks like. The next
        // changed capture fetches the bundle and reuses every seat.
        fs::remove_file(&path).unwrap();
        fixture.move_key(2);
        assert_eq!(fixture.pass(), ("captured", 0, None));
        let (_, manifest) = fixture.manifest();
        assert_eq!(manifest.digest, fixture.record().digest);
    }

    // A shallow checkout's bundle is shallow-graph custody and is never
    // reused, so its capture publishes no manifest.
    #[test]
    fn a_shallow_capture_publishes_no_manifest() {
        let fixture = fixture("shallow");
        let clone = fixture.root.join("shallow");
        git(
            &fixture.root,
            &[
                "clone",
                "--quiet",
                "--depth=1",
                "--template=",
                &format!("file://{}", fixture.source.display()),
                clone.to_str().unwrap(),
            ],
        );
        let plan = fixture.root.join("shallow-plan");
        let target = fixture.root.join("shallow-target");
        add(&plan, &clone, &target, Some(&target)).unwrap();
        let (state, corpus) = (
            fixture.root.join("shallow-state"),
            fixture.root.join("shallow-corpus"),
        );
        capture(&plan, &state, &corpus, 1, &|_| Ok(())).unwrap();
        let sidecars: Vec<PathBuf> = fs::read_dir(&corpus)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "reuse"))
            .collect();
        assert!(sidecars.is_empty(), "{sidecars:?}");
    }
}
