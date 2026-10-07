//! One common Git closure plus workspace-specific prerequisite bundles.
//!
//! Shared bases are transport dependencies, not a replacement for each
//! workspace's staged, dirty, ignored and filesystem metadata capture.

use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::decide::{self, Basis, Policy};
use super::{capture_refs, git, input, oid, output, prepare_private, refs, set_ref, text};
use crate::{BulkloadRefusal, Result};

/// The most bytes a v1 bundle header may hold.
///
/// That is its signature, prerequisite and ref lines and the blank line that
/// ends it. Every reader refuses a longer one, and every writer measures its
/// header against it before it writes, so no capture is recorded that a
/// reader refuses for size. Over it is `GIT_INVENTORY_OVER_CAP`, never
/// `GIT_INVENTORY_MALFORMED` (OI-1003-Q54, #178).
pub const HEADER_CAP: usize = 16 * 1024 * 1024;

/// The longest header line a reader accepts. A longer one is malformed.
const HEADER_LINE: u64 = 1024 * 1024;

// A header that has consumed `consumed` bytes is over the cap.
const fn over_cap(consumed: usize) -> bool {
    consumed > HEADER_CAP
}

/// A commit a capture bundle declares as a prerequisite (`-<oid>` in its
/// header, `^<oid>` to the walk): a well-formed object name, checked once
/// when it enters the prerequisite path, so the writer never declares a
/// malformed one (Q42 lane L6a).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Oid(String);

impl Oid {
    /// `value` as a prerequisite; `GIT_INVENTORY_MALFORMED` unless it is a
    /// SHA-1 or SHA-256 object name.
    pub(super) fn new(value: &str) -> Result<Self> {
        if oid(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(BulkloadRefusal::GitInventoryMalformed)
        }
    }

    /// The object name.
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn stash_history(repo: &Path, inventory: &str) -> Result<Vec<u8>> {
    if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]))
    } else {
        Ok(Vec::new())
    }
}

/// Capture refs, stash history and HEAD once without walking workspace payloads.
///
/// This optimistic snapshot does not freeze writers. Subsequent workspace
/// captures may include new commits absent from this base; those travel in
/// their delta. The caller owns digest binding, retention and import ordering.
///
/// # Errors
/// Refuses changing refs/stashes/HEAD, invalid Git state or occupied capture paths.
pub fn export_base(repo: &Path, capture: &Path) -> Result<PathBuf> {
    let repo = fs::canonicalize(repo).refuse_at("git_carry::shared::export_base")?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(capture)
        .refuse_at("git_carry::shared::export_base")?;
    let capture = fs::canonicalize(capture).refuse_at("git_carry::shared::export_base")?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let inventory = refs(&repo)?;
    let boundary = super::shallow::frontier(&repo)?;
    let stashes = stash_history(&repo, &inventory)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let private = prepare_private(&repo, &capture)?;
    if !boundary.is_empty() {
        super::metadata(&private, "shallow-frontier-v1", &boundary)?;
    }
    capture_refs(&private, &inventory, &stashes)?;
    set_ref(&private, "refs/carry-export/shared-base-head", &head)?;
    let bundle = capture.join("base.bundle");
    write_bundle(&private, &bundle, None)?;
    if inventory != refs(&repo)?
        || boundary != super::shallow::frontier(&repo)?
        || stashes != stash_history(&repo, &inventory)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(bundle)
}

// The commits a plan base's refs name, peeled, as the private repository
// holds them: an item bundle's prerequisites.
//
// One `cat-file --batch-check` answers every head (#178: two children per
// head cost about 4 ms each, 0.79 ks of CPU at 96,850 heads and 2 items).
// For each head it asks the object itself, which must be held, and the
// object peeled to a commit: a ref may legally name a tree or blob, which is
// no prerequisite and stays in the item's pack. A ref table (OI-1003-Q54)
// is the base's own commit, in its pack only, so it is never a
// prerequisite: its tips carry what it maps. Any other name outside
// `refs/carry-export/` is malformed, as it was before the table.
fn prerequisite_commits(private: &Path, base: &Path) -> Result<BTreeSet<Oid>> {
    output(git(private).args(["bundle", "verify"]).arg(base))?;
    let mut request = String::new();
    let mut asked = 0usize;
    for (value, name) in super::chain::advertised(base)? {
        if super::ref_table::is_table(&name) {
            continue;
        }
        if !name.starts_with("refs/carry-export/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        writeln!(request, "{value}\n{value}^{{commit}}")
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        asked = asked.saturating_add(1);
    }
    let answer = input(
        git(private).args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        request.as_bytes(),
    )?;
    let answer =
        std::str::from_utf8(&answer).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut lines = answer.lines();
    let mut commits = BTreeSet::new();
    for _ in 0..asked {
        let (Some(object), Some(peeled)) = (lines.next(), lines.next()) else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        // A missing name answers `<name> missing`: a base head this
        // repository does not hold cannot be declared.
        let held = object
            .split_once(' ')
            .is_some_and(|(value, kind)| oid(value) && kind != "missing");
        if !held {
            return Err(BulkloadRefusal::GitInventoryMissingPrerequisite);
        }
        if let Some((commit, "commit")) = peeled.split_once(' ') {
            commits.insert(Oid::new(commit)?);
        }
    }
    if lines.next().is_some() || commits.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(commits)
}

/// Whether a bundle declares prerequisite commits that require retained custody.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub fn requires_base(bundle: &Path) -> Result<bool> {
    Ok(!prerequisites(bundle)?.is_empty())
}

/// The prerequisite object names a bundle's header declares (`-<oid>` lines),
/// in header order. Empty for a self-contained bundle.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub fn prerequisites(bundle: &Path) -> Result<Vec<String>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)
        .refuse_at("git_carry::shared::prerequisites")?;
    let mut source = BufReader::new(file);
    let mut consumed = 0usize;
    let mut prerequisite = Vec::new();
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(HEADER_LINE)
            .read_until(b'\n', &mut line)
            .refuse_at("git_carry::shared::prerequisites")?;
        if consumed == 0 && line != b"# v2 git bundle\n" && line != b"# v3 git bundle\n" {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if over_cap(consumed) {
            return Err(BulkloadRefusal::GitInventoryOverCap);
        }
        if count == 0 || !line.ends_with(b"\n") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            return Ok(prerequisite);
        }
        if let Some(rest) = line.strip_prefix(b"-") {
            let name = rest
                .split(|b| *b == b' ' || *b == b'\n')
                .next()
                .and_then(|name| std::str::from_utf8(name).ok())
                .filter(|name| oid(name))
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            prerequisite.push(name.to_owned());
        }
    }
}

/// What packing one capture cost (WP2, OI-1003-Q15). Also added to the
/// process counters `write_source_pack_bytes`, `write_source_pack_objects`
/// and `read_source_pack_readback_bytes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackStats {
    /// Bytes git wrote for the capture's pack (the bundle as git created it,
    /// or the raw pack of a shallow envelope). For a self-contained pack this
    /// is the logical measure of what git read from the object stores to
    /// write it. A thin pack (a grouped item or a chained link, OI-1003-Q42)
    /// also read every preferred delta base it deltaed against, which it does
    /// not hold, so this bounds what was written, not what was read;
    /// `storage_read` sees those reads, as a lower bound.
    pub bytes: u64,
    /// Objects in that pack, from its header.
    pub objects: u64,
    /// The packing child's own storage reads: a lower bound (counters notes).
    pub storage_read: u64,
}

impl PackStats {
    /// Measure a pack git just wrote at `path` (a bundle, or a raw pack when
    /// `raw`), add it to the process counters and return it.
    pub(super) fn record(path: &Path, raw: bool, storage_read: u64) -> Result<Self> {
        use crate::counters::{add, Counter};
        let stats = Self {
            bytes: fs::symlink_metadata(path)
                .refuse_at("git_carry::shared::record")?
                .len(),
            objects: pack_object_count(path, raw)?,
            storage_read,
        };
        add(Counter::SourcePackWrite, stats.bytes);
        add(Counter::SourcePackObjects, stats.objects);
        add(Counter::SourcePackReadback, stats.storage_read);
        Ok(stats)
    }
}

// Read a bundle's header through the blank line that ends it, leaving
// `source` at its pack. Returns the header's length.
fn skip_header(source: &mut BufReader<fs::File>, site: &'static str) -> Result<u64> {
    let mut consumed = 0usize;
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(HEADER_LINE)
            .read_until(b'\n', &mut line)
            .refuse_at(site)?;
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if over_cap(consumed) {
            return Err(BulkloadRefusal::GitInventoryOverCap);
        }
        if count == 0 || !line.ends_with(b"\n") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            return u64::try_from(consumed).map_err(|_| BulkloadRefusal::BudgetExceeded);
        }
    }
}

fn open_nofollow(path: &Path, site: &'static str) -> Result<BufReader<fs::File>> {
    Ok(BufReader::new(
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .refuse_at(site)?,
    ))
}

/// The length of `bundle`'s pack: the file less its header.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub(super) fn bundle_pack_len(bundle: &Path) -> Result<u64> {
    const SITE: &str = "git_carry::shared::bundle_pack_len";
    let mut source = open_nofollow(bundle, SITE)?;
    let header = skip_header(&mut source, SITE)?;
    let length = source.get_ref().metadata().refuse_at(SITE)?.len();
    length
        .checked_sub(header)
        .ok_or(BulkloadRefusal::GitInventoryMalformed)
}

// The object count a pack header declares: `PACK`, version 2 or 3, count, all
// big-endian. A bundle's pack follows its header's blank line.
fn pack_object_count(path: &Path, raw: bool) -> Result<u64> {
    const SITE: &str = "git_carry::shared::pack_object_count";
    let mut source = open_nofollow(path, SITE)?;
    if !raw {
        skip_header(&mut source, SITE)?;
    }
    let mut header = [0u8; 12];
    source.read_exact(&mut header).refuse_at(SITE)?;
    let (magic, rest) = header.split_at(4);
    let (version, count) = rest.split_at(4);
    if magic != b"PACK" || !matches!(version, [0, 0, 0, 2 | 3]) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let count: [u8; 4] = count
        .try_into()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    Ok(u64::from(u32::from_be_bytes(count)))
}

// The bundle signature for `private`'s object format: v2 for SHA-1, v3 with
// its capability line for SHA-256.
fn signature(private: &Path) -> Result<&'static [u8]> {
    match text(git(private).args(["rev-parse", "--show-object-format"]))?.as_str() {
        "sha1" => Ok(b"# v2 git bundle\n"),
        "sha256" => Ok(b"# v3 git bundle\n@object-format=sha256\n"),
        _ => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

// Refuse a header of `length` bytes over the cap, before anything is written.
const fn within_cap(length: usize) -> Result<()> {
    if over_cap(length) {
        return Err(BulkloadRefusal::GitInventoryOverCap);
    }
    Ok(())
}

// The header `bundle create --all` writes for `private`: its signature, one
// `<oid> <name>` line per private ref and the blank line. A bare private
// repository's HEAD is unborn, so it adds no line.
fn full_header_len(private: &Path) -> Result<usize> {
    let listed = refs(private)?;
    let lines = listed
        .lines()
        .try_fold(0usize, |total, line| {
            total.checked_add(line.len().checked_add(1)?)
        })
        .ok_or(BulkloadRefusal::BudgetExceeded)?;
    signature(private)?
        .len()
        .checked_add(lines)
        .and_then(|length| length.checked_add(1))
        .ok_or(BulkloadRefusal::BudgetExceeded)
}

// A self-contained bundle of every private ref: `git bundle create` through
// the measured child path. A failed child refuses GIT_CHILD_FAILED with its
// stderr class (WP3, R-N121). Its header is measured before git writes a
// byte, so an over-cap capture is refused here, never written and refused by
// its reader (`PackStats::record` reads it back under the same cap).
pub(super) fn write_full(private: &Path, bundle: &Path) -> Result<PackStats> {
    within_cap(full_header_len(private)?)?;
    let storage_read = super::pack_child(
        git(private)
            .args(["bundle", "create"])
            .arg(bundle)
            .arg("--all")
            .stdout(Stdio::null()),
        None,
    )?;
    PackStats::record(bundle, false, storage_read)
}

/// A retained capture bundle a capture may chain on (WP2, see `chain`), and
/// the source whose object store must hold its tips.
#[derive(Debug, Clone, Copy)]
pub(super) struct Link<'a> {
    /// The retained capture bundle.
    pub prior: &'a Path,
    /// The checkout it captured.
    pub source: &'a Path,
}

/// What a capture's decision offers its writer, as [`super::ExportOptions`]
/// carries it: a shared plan base and a retained capture to chain on.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Offer<'a> {
    /// The plan base whose commit closure the capture may exclude.
    pub base: Option<&'a Path>,
    /// The retained capture whose source-held tips may become prerequisites.
    pub link: Option<Link<'a>>,
}

impl<'a> Offer<'a> {
    /// `ExportOptions`' plan `base` and retained capture `prior` of `source`.
    pub(super) fn of(base: Option<&'a Path>, prior: Option<&'a Path>, source: &'a Path) -> Self {
        Self {
            base,
            link: prior.map(|prior| Link { prior, source }),
        }
    }
}

/// Write a capture bundle on what the decision core decides for `offer`
/// (`decide::decide`, OI-1003-Q43), once this pass has read what only it can
/// read: whether the source is shallow (the private repository carries its
/// frontier), and, only when the decision rests on it, whether the source
/// holds a tip of the link (`chain::source_held_tips`). Under v1's policy a
/// plan base wins over a link, a shallow source is always self-contained
/// and a source that holds none of the link's tips gets a self-contained
/// bundle.
///
/// Returns the pack cost and whether the bundle declares the link's tips as
/// prerequisites. A self-contained bundle (a shallow envelope, nothing
/// offered, no source-held tip, or a thin header over `cap`, see
/// [`write_excluding_tip_trees`]) does not, and nor does a plan base's
/// delta. A decision v1 cannot write (L6b's chain under a plan base) refuses
/// `CONTRACT_SELF_INCONSISTENT`, as does a decision that is not an export.
pub(super) fn write_capture(
    private: &Path,
    bundle: &Path,
    offer: Offer<'_>,
    cap: usize,
) -> Result<(PackStats, bool)> {
    let boundary = super::shallow::frontier(private)?;
    let mut tips = BTreeSet::new();
    let plan = decide::decide_offered(
        offer.base.is_some(),
        offer.link,
        !boundary.is_empty(),
        Policy::V1,
        |link| {
            tips = super::chain::source_held_tips(link.source, link.prior)?;
            Ok(!tips.is_empty())
        },
    )?;
    match plan.basis {
        // A shallow frontier is not a bundle prerequisite. Preserve the entire
        // locally available shallow closure as explicit custody instead.
        Basis::SelfContained if !boundary.is_empty() => Ok((
            super::shallow::write_bundle(private, bundle, &boundary)?,
            false,
        )),
        Basis::SelfContained => Ok((write_full(private, bundle)?, false)),
        Basis::Base => {
            let base = offer
                .base
                .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
            let commits = prerequisite_commits(private, base)?;
            Ok((write_thin(private, bundle, &commits, cap)?.0, false))
        }
        Basis::Chain => write_excluding_tip_trees(private, bundle, &tips, cap),
        Basis::BaseAndChain => Err(BulkloadRefusal::ContractSelfInconsistent),
    }
}

/// The chain writer: [`write_capture`] offered `prior` alone, with the thin
/// header held to `cap` bytes (at most [`HEADER_CAP`]). REFS-SCALE lowers
/// the cap to reach the fallback without 100,000 distinct objects.
#[cfg(test)]
pub(super) fn write_chained_capped(
    private: &Path,
    bundle: &Path,
    source: &Path,
    prior: &Path,
    cap: usize,
) -> Result<(PackStats, bool)> {
    write_capture(
        private,
        bundle,
        Offer {
            base: None,
            link: Some(Link { prior, source }),
        },
        cap,
    )
}

// The header a thin bundle declaring `commits` writes: its signature, one
// `-<oid> shared base` line per prerequisite, one `<oid> <name>` line per
// private ref, and the blank line. 54 B per prerequisite on SHA-1 (78 B on
// SHA-256) on top of a self-contained header's 111 B (159 B) per distinct
// object, so a thin header reaches the cap first: at about 101,680 distinct
// commit tips on SHA-1 (237 B each on SHA-256: about 70,790), against about
// 151,100 (105,500) self-contained.
fn thin_header(private: &Path, commits: &BTreeSet<Oid>) -> Result<Vec<u8>> {
    const SITE: &str = "git_carry::shared::thin_header";
    let mut header = signature(private)?.to_vec();
    for value in commits {
        writeln!(header, "-{value} shared base").refuse_at(SITE)?;
    }
    writeln!(header, "{}\n", refs(private)?).refuse_at(SITE)?;
    Ok(header)
}

// A thin bundle declaring `commits` as prerequisites, which deltas against
// them; `true` with its pack cost. Both prerequisite kinds come here, as the
// decision core's basis says (`write_capture`): a shared plan base's tips
// and a prior capture's source-held tips. Each commit enters the
// prerequisite path as an [`Oid`], so a malformed name refuses
// `GIT_INVENTORY_MALFORMED` before anything is written.
//
// **Over the cap, self-contained.** The final header, prerequisites
// included, is measured before the walk and before any byte is written
// (#178: a grouped item's rewritten header was recorded 33,880 B over the
// cap and refused by every later reader). A thin header over `cap` (never
// more than the readers' [`HEADER_CAP`]) is not refused: the capture is
// written self-contained instead (`write_full`, `false`), whose header has no
// prerequisite line and fits wherever the source's distinct objects do.
// Refusing would strand the item: every later pass is offered the same
// prerequisites (a chain that never grows never reaches the depth reset, and
// a plan base never changes), while a self-contained bundle carries it. Only
// a capture whose self-contained header is itself over the cap is refused,
// `GIT_INVENTORY_OVER_CAP` from `write_full`, before it writes.
//
// What its pack omits is what the walk marks uninteresting, P64 in
// `tests/git_group_minimality.rs`: every commit `commits` reach, and every
// tree and blob under the tree of an excluded tip, of a ref tip they reach
// (HEAD among them, through `--all`), or of an edge (an excluded parent of a
// packed commit). That is not the full object closure of `commits`. A blob
// they hold only deeper in history (a `checkout <old> -- path`, a revert, a
// stash of old content) is packed again, and so is the capture's own
// snapshot payload (untracked and ignored files), which no prerequisite
// holds. OI-1003-Q42 reading (a); the P64 rows pin both costs.
//
// `git bundle create ^tip` only marks the trees of edge commits (parents of
// packed commits) uninteresting. A capture's staged and worktree commits are
// parentless, so their trees would re-pack every blob they share with HEAD:
// all tracked content, every pass (Q42: 1.83 MB for a 3.5 KB head move, and
// a 64 MiB base blob in every item of a group). `rev-list
// --objects-edge-aggressive` marks the tree of every excluded tip, and of
// every ref tip they reach, uninteresting instead. The header (signature,
// prerequisites, every private ref) is written here. `bundle verify` in the
// caller checks the result like any other bundle. Both children go through
// the measured, classified child path: a failed one refuses GIT_CHILD_FAILED
// with its stderr class (WP3, R-N121).
//
// The walk's `-<oid>` edge lines stay on pack-objects' stdin: it reads each
// edge's tree as a preferred base, so an edited blob packs as a delta against
// the copy its path holds there, a REF_DELTA to an object the pack omits
// (OI-1003-Q42). That pack is thin by construction; a restore's fetch, or
// `chain::flatten`'s, completes it with `index-pack --fix-thin` from the
// prerequisites it already holds. So does the next pass's blob-reuse fetch
// (`retained_blobs`), which reads those bases from the source object store
// and counts them. `--thin` itself is not passed: it implies
// pack-objects' internal rev-list, which refuses edge lines (`not a rev`).
// Each preferred base is read from the object stores to delta against and
// never written, so `PackStats::bytes` bounds this pack, not its reads.
// pack-objects keeps only the first `pack.window` (10) edges as bases. The
// walk prints the edge parents of new commits first, then the uninteresting
// ref tips in ref order, where `refs/carry-export/head` sorts before every
// source ref. So HEAD's tree is a base unless ten edge parents precede it.
//
// A failed write removes its pending object list and header, so a refused
// pass leaves no partial file beside the bundle path.
pub(super) fn write_excluding_tip_trees(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<String>,
    cap: usize,
) -> Result<(PackStats, bool)> {
    let commits = commits
        .iter()
        .map(|value| Oid::new(value))
        .collect::<Result<BTreeSet<Oid>>>()?;
    write_thin(private, bundle, &commits, cap)
}

// [`write_excluding_tip_trees`] on commits already checked as prerequisites.
fn write_thin(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<Oid>,
    cap: usize,
) -> Result<(PackStats, bool)> {
    let header = thin_header(private, commits)?;
    if header.len() > cap.min(HEADER_CAP) {
        return Ok((write_full(private, bundle)?, false));
    }
    let written = write_excluding_tip_trees_pending(private, bundle, commits, &header);
    if written.is_err() {
        for leftover in ["objects-pending", "header-pending"] {
            // Best effort: the refusal being returned is the one that matters,
            // and a path never created is already clean.
            let _ = fs::remove_file(bundle.with_extension(leftover));
        }
    }
    Ok((written?, true))
}

fn write_excluding_tip_trees_pending(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<Oid>,
    header: &[u8],
) -> Result<PackStats> {
    const SITE: &str = "git_carry::shared::write_excluding_tip_trees_pending";
    let listing = bundle.with_extension("objects-pending");
    let list = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&listing)
        .refuse_at(SITE)?;
    let exclusions = commits.iter().fold(String::new(), |mut result, value| {
        result.push('^');
        result.push_str(value.as_str());
        result.push('\n');
        result
    });
    let list_read = super::pack_child(
        git(private)
            .args(["rev-list", "--objects-edge-aggressive", "--all", "--stdin"])
            .stdout(Stdio::from(list.try_clone().refuse_at(SITE)?)),
        Some(exclusions.as_bytes()),
    )?;
    // Edge lines (`-<oid>`) name excluded commits; pack-objects reads them as
    // preferred delta bases, never as objects to pack.
    let mut objects = Vec::new();
    fs::File::open(&listing)
        .refuse_at(SITE)?
        .read_to_end(&mut objects)
        .refuse_at(SITE)?;
    fs::remove_file(&listing).refuse_at(SITE)?;
    let pending = bundle.with_extension("header-pending");
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .refuse_at(SITE)?;
    target.write_all(header).refuse_at(SITE)?;
    target.flush().refuse_at(SITE)?;
    let pack_read = super::pack_child(
        git(private)
            .args(["pack-objects", "--stdout", "--delta-base-offset"])
            .stdout(Stdio::from(target.try_clone().refuse_at(SITE)?)),
        Some(&objects),
    )?;
    target.sync_file_counted().refuse_at(SITE)?;
    fs::rename(&pending, bundle).refuse_at(SITE)?;
    fs::File::open(bundle.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at(SITE)?
        .sync_dir_counted()
        .refuse_at(SITE)?;
    PackStats::record(bundle, false, list_read.saturating_add(pack_read))
}

/// Write a capture bundle with no link to chain on ([`write_capture`]): a
/// shallow envelope, a self-contained bundle, or, with a shared plan `base`,
/// a thin bundle whose prerequisites are the base's commit tips
/// (self-contained when that thin header would be over the cap,
/// [`write_excluding_tip_trees`]). Its pack holds no commit the base reaches
/// and no object under the tree of a base tip, of a capture ref the base
/// reaches (HEAD's) or of an edge parent. It can still hold an object the
/// base holds only deeper in its history, and it holds the capture's
/// untracked and ignored payload on every pass (OI-1003-Q42, P64; see
/// `write_excluding_tip_trees`).
pub(super) fn write_bundle(
    private: &Path,
    bundle: &Path,
    base: Option<&Path>,
) -> Result<PackStats> {
    Ok(write_capture(private, bundle, Offer { base, link: None }, HEADER_CAP)?.0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) {
        output(
            git(repo)
                .args([
                    "-c",
                    "user.name=Bulkload test",
                    "-c",
                    "user.email=bulkload@localhost",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
        )
        .unwrap();
    }

    #[test]
    fn shared_base_preserves_workspace_refs_and_requires_imported_history() {
        let root = std::env::temp_dir().join(format!(
            "tcfs-shared-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        run(&source, &["init", "--template="]);
        let mut random = 1u32;
        let history: Vec<u8> = (0..262_144)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random.to_le_bytes().first().copied().unwrap()
            })
            .collect();
        fs::write(source.join("history"), &history).unwrap();
        fs::write(source.join("file"), b"base").unwrap();
        run(&source, &["add", "."]);
        run(&source, &["commit", "-m", "base"]);
        let head = text(git(&source).args(["rev-parse", "HEAD"])).unwrap();
        let base = export_base(&source, &root.join("base")).unwrap();
        fs::write(source.join("file"), b"staged").unwrap();
        run(&source, &["add", "file"]);
        fs::write(source.join("file"), b"dirty").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"private ignored payload").unwrap();
        let status = output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap();
        let delta =
            super::super::export_repository_with_prerequisite(&source, &root.join("delta"), &base)
                .unwrap();
        let standalone =
            super::super::export_repository(&source, &root.join("standalone")).unwrap();
        assert!(fs::metadata(&delta).unwrap().len() < fs::metadata(&standalone).unwrap().len());
        assert!(
            text(git(&source).args(["bundle", "list-heads"]).arg(&delta))
                .unwrap()
                .lines()
                .any(|line| line == format!("{head} refs/carry-export/head"))
        );
        assert_eq!(
            status,
            output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap()
        );
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        run(&destination, &["init", "--template="]);
        assert!(super::super::import_bundle(&destination, &delta, "neo").is_err());
        super::super::import_bundle(&destination, &base, "neo").unwrap();
        let restored = root.join("restored");
        super::super::restore_linked(&delta, &destination, &restored, "neo").unwrap();
        assert_eq!(
            head,
            text(git(&restored).args(["rev-parse", "HEAD"])).unwrap()
        );
        assert_eq!(fs::read(restored.join("history")).unwrap(), history);
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        assert_eq!(
            output(git(&restored).args(["show", ":file"])).unwrap(),
            b"staged"
        );
        assert_eq!(
            fs::read(restored.join("ignored")).unwrap(),
            b"private ignored payload"
        );
        let linked_source = root.join("linked-source");
        output(
            git(&source)
                .args(["worktree", "add", "--detach"])
                .arg(&linked_source)
                .arg(&head),
        )
        .unwrap();
        fs::write(linked_source.join("file"), b"other workspace").unwrap();
        let second = super::super::export_repository_with_prerequisite(
            &linked_source,
            &root.join("second-delta"),
            &base,
        )
        .unwrap();
        let second_restored = root.join("second-restored");
        super::super::restore_linked(&second, &destination, &second_restored, "neo").unwrap();
        assert_eq!(
            fs::read(second_restored.join("file")).unwrap(),
            b"other workspace"
        );
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        fs::remove_dir_all(root).unwrap();
    }
}
