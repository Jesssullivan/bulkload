//! Estate adopt (#226; OI-1003-Q171/Q172, Linear TIN-4543; design
//! `docs/agent-notes/2026-10-15-estate-adopt-design.md`): `estate-apply
//! --adopt` makes an existing checkout or repository that bulkload never
//! landed bulkload's by proof, never by moving it.
//!
//! A11a, one workspace item, in this order:
//!
//! 1. the checkout must be this item's to adopt ([`foreign_shape`]): its
//!    path and the plan's repository spelled canonically (never through a
//!    symlink), its repository the plan's, a standalone item's checkout a
//!    main worktree, and its git directory naming it back. Otherwise
//!    `GIT_DESTINATION_OCCUPIED`, and nothing is written;
//! 2. a private scratch repository beside the staged bundle borrows the
//!    destination's objects (`alternates`, read-only) and takes the capture;
//! 3. the destination repository must be the capture's
//!    ([`super::super::related_repository`]): an unrelated one stays
//!    `GIT_DESTINATION_OCCUPIED` and nothing is written;
//! 4. the proof, read-only and taken before any lock or write: HEAD, the
//!    index entries, a standalone item's `info/exclude`, and every carried
//!    seat's kind, mode and content (each print taken before and after its
//!    read must agree; the ledger's racy instant is taken before the first
//!    read). Any difference is `GIT_DESTINATION_DIVERGED`, and nothing is
//!    written;
//! 5. under the destination lock: still no workspace ledger (another apply
//!    may have landed or adopted it since; `GIT_DESTINATION_OCCUPIED`), any
//!    pending ref intent settled, the proved prints checked again
//!    (metadata only), the capture imported (`estate.adopt.after_import`);
//! 6. the adoption record and the repository marker, with the proved HEAD
//!    branch's R2 row (`estate.adopt.after_marker`);
//! 7. the native ref step: R1 creates, R2 adopts, R3 keeps, R10 leaves,
//!    checked-out branches skipped; the stash by T0, T1 or T2
//!    (`estate.adopt.after_refs`);
//! 8. the workspace ledger from the proof's prints
//!    (`estate.adopt.after_ledger`), then the caller's journal.
//!
//! Adopt never writes a seat, the index, HEAD or configuration, and never
//! deletes a ref. A crash leaves either nothing new but objects and
//! provenance (before the marker), or a marker and ref rows naming only
//! values equal to the capture's, and no workspace ledger; the rerun with
//! `--adopt` proves again and finishes. A11b re-keys a ledger naming another
//! item, only when that item is not the plan's and the capture is the same
//! tree's ([`rekey_refusal`]), after its normal proof; A11d adopts a
//! repository a refs-only item imports into, and refuses an unrelated one.

use crate::refuse::RefuseAt as _;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::super::batch_objects::BatchObjects;
use super::super::landed::{
    self, Adopted, Head, Kind, LandedExclude, LandedRef, LandedWorkspace, RefLedger, SeatPrint,
};
use super::super::native_refs::{self, Custody};
use super::super::{git, safe_destination, text};
use super::{
    exclude_digest, head_of, holds, index_digest, lstat, read_exclude, settle_repository, sides,
    Applied, Landing, Meta, Place, Target,
};
use crate::{BulkloadRefusal, Result};

/// A private repository beside a staged capture (in its private directory,
/// removed with it), holding the capture and borrowing a destination's
/// objects read-only: what the adopt proof and `estate-diff` read the
/// capture from, so nothing reaches the destination before the proof.
#[derive(Debug)]
pub(in crate::git_carry) struct Scratch {
    /// The scratch checkout (non-bare, its worktree empty).
    pub(in crate::git_carry) repo: PathBuf,
    /// The capture's provenance instance in it, the same name the
    /// destination's import gives it.
    pub(in crate::git_carry) instance: String,
}

impl Scratch {
    /// Build a scratch repository for `staged` (and its plan `base`),
    /// borrowing `objects` (a destination's object directory) when given.
    ///
    /// A base shared by its stage (`estate-diff`, M2r2 item 3) is imported
    /// once per destination object store into a scratch repository of its
    /// own, and this one borrows that import (its objects as one more
    /// alternate, its refs created here as the import made them) instead
    /// of indexing the whole base again; every other base is imported
    /// here.
    ///
    /// # Errors
    /// Whatever the import refuses.
    pub(in crate::git_carry) fn new(
        staged: &super::super::StagedBundle,
        objects: Option<&Path>,
        base: Option<&super::super::StagedBundle>,
        source: &str,
    ) -> Result<Self> {
        const SITE: &str = "git_carry::converge::adopt::scratch";
        let dir = fresh_dir(staged.private_dir(), "adopt")?;
        let repo = dir.join("repo");
        let listed = text(git(&dir).args(["bundle", "list-heads"]).arg(staged.path()))?;
        let format = super::super::bundle_object_format(&listed)?;
        let objects = objects
            .map(|objects| fs::canonicalize(objects).refuse_at(SITE))
            .transpose()?;
        let seed = match base {
            Some(base) => borrowed_base(base, objects.as_deref(), format, source)?,
            None => None,
        };
        fs::create_dir(&repo).refuse_at(SITE)?;
        super::super::init_repository(&repo, format, false)?;
        let lenders: Vec<&Path> = objects
            .as_deref()
            .into_iter()
            .chain(seed.as_ref().map(|seed| seed.objects.as_path()))
            .collect();
        alternates(&repo, &lenders)?;
        match (base, &seed) {
            (Some(_), Some(seed)) => {
                if !seed.refs.is_empty() {
                    super::super::input(
                        git(&repo).args(["update-ref", "--no-deref", "--stdin", "-z"]),
                        &seed.refs,
                    )?;
                }
                crate::counters::bump(crate::counters::Counter::GitScratchBaseBorrowed);
            }
            (Some(base), None) => {
                super::super::import_verified(&repo, base.path(), source)?;
                crate::counters::bump(crate::counters::Counter::GitScratchBaseImports);
            }
            (None, _) => {}
        }
        let (_, instance) = super::super::import_verified_instance(&repo, staged.path(), source)?;
        Ok(Self { repo, instance })
    }

    /// The capture's metadata in the scratch repository.
    ///
    /// # Errors
    /// `SEALED_OBJECT_MISSING` for a capture with no worktree or HEAD (a
    /// bare one).
    pub(in crate::git_carry) fn meta(&self, source: &str) -> Result<Meta> {
        Meta::read(&self.repo, source, &self.instance)
    }

    /// Every tip the capture carries: its HEAD and its native refs.
    ///
    /// # Errors
    /// Whatever reading the provenance refuses.
    pub(in crate::git_carry) fn tips(&self, source: &str) -> Result<Vec<String>> {
        let mut tips: Vec<String> = native_refs::carried(&self.repo, source, &self.instance)?
            .into_values()
            .collect();
        if let Ok(meta) = self.meta(source) {
            tips.push(meta.get("head")?.to_owned());
        }
        tips.sort();
        tips.dedup();
        Ok(tips)
    }

    /// The entries digest of the index a landing of `meta` installs
    /// ([`super::installed_index`]'s value), built in the scratch checkout:
    /// the staged tree read into a private index, and every carried
    /// intent-to-add entry added over an empty placeholder of its mode.
    ///
    /// # Errors
    /// Whatever `read-tree` or the intent-to-add restore refuses.
    pub(in crate::git_carry) fn capture_index(&self, meta: &Meta) -> Result<[u8; 32]> {
        const SITE: &str = "git_carry::converge::adopt::capture_index";
        let index = self.repo.join(".git/bulkload-adopt-index");
        let _ = fs::remove_file(&index);
        super::super::read_tree(
            git(&self.repo).env("GIT_INDEX_FILE", &index),
            &[&format!("{}^{{tree}}", meta.get("staged")?)],
        )?;
        let heads = meta.heads();
        for entry in super::super::carried_intent_to_add(&self.repo, &heads)? {
            let relative = Path::new(OsStr::from_bytes(&entry.rel_path));
            if !relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            let path = self.repo.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).refuse_at(SITE)?;
            }
            if entry.mode == 0o120_000 {
                std::os::unix::fs::symlink("placeholder", &path).refuse_at(SITE)?;
            } else {
                fs::write(&path, b"").refuse_at(SITE)?;
                let mode = if entry.mode & 0o111 == 0 {
                    0o644
                } else {
                    0o755
                };
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).refuse_at(SITE)?;
            }
        }
        super::super::restore_intent_to_add(&self.repo, Some(&index), &heads)?;
        index_digest(&self.repo, Some(&index))
    }
}

/// The object directory of the repository at `repo`, absolute.
// A new directory `{prefix}-{n}` in `parent`, the first `n` free.
fn fresh_dir(parent: &Path, prefix: &str) -> Result<PathBuf> {
    let mut n = 0u32;
    loop {
        let candidate = parent.join(format!("{prefix}-{n}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                n = n
                    .checked_add(1)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            }
            Err(error) => {
                return Err(crate::refuse::io(
                    &error,
                    "git_carry::converge::adopt::fresh_dir",
                ))
            }
        }
    }
}

// Write `repo`'s `objects/info/alternates`: one line per lender, in order
// (none, no file). A lender is a canonical object directory.
fn alternates(repo: &Path, lenders: &[&Path]) -> Result<()> {
    if lenders.is_empty() {
        return Ok(());
    }
    let mut lines = String::new();
    for lender in lenders {
        let line = lender
            .to_str()
            .filter(|line| !line.contains(['\n', '\r']) && !line.starts_with('"'))
            .ok_or(BulkloadRefusal::PathNotPortable)?;
        lines.push_str(line);
        lines.push('\n');
    }
    fs::write(repo.join(".git/objects/info/alternates"), lines)
        .refuse_at("git_carry::converge::adopt::alternates")
}

// What a scratch over the shared `base` borrows for this object store,
// format and source (M2r2 item 3): the import made at the first ask and
// kept in the base's stage, or its refusal. `None` for a base its stage
// does not share, or whose import a borrower cannot take (a shallow
// frontier): the caller imports the base itself.
fn borrowed_base(
    base: &super::super::StagedBundle,
    objects: Option<&Path>,
    format: &'static str,
    source: &str,
) -> Result<Option<super::super::BaseSeed>> {
    let Some(shared) = &base.shared else {
        return Ok(None);
    };
    let mut imports = shared
        .lock()
        .map_err(|_| BulkloadRefusal::ContractSelfInconsistent)?;
    if let Some(found) = imports.iter().find(|import| {
        import.objects.as_deref() == objects && import.format == format && import.source == source
    }) {
        return found.seed.clone();
    }
    let seed = import_shared(base, objects, format, source);
    imports.push(super::super::SharedBase {
        objects: objects.map(Path::to_path_buf),
        format,
        source: source.to_owned(),
        seed: seed.clone(),
    });
    seed
}

// The shared import itself: a scratch repository in the base's private
// directory, built as an unshared scratch builds its own (same format,
// same lender, the same `import_verified`), whose objects and refs a
// borrower then takes.
fn import_shared(
    base: &super::super::StagedBundle,
    objects: Option<&Path>,
    format: &'static str,
    source: &str,
) -> Result<Option<super::super::BaseSeed>> {
    const SITE: &str = "git_carry::converge::adopt::import_shared";
    let dir = fresh_dir(base.private_dir(), "shared-base")?;
    let repo = dir.join("repo");
    fs::create_dir(&repo).refuse_at(SITE)?;
    super::super::init_repository(&repo, format, false)?;
    alternates(&repo, &objects.into_iter().collect::<Vec<_>>())?;
    super::super::import_verified(&repo, base.path(), source)?;
    crate::counters::bump(crate::counters::Counter::GitScratchBaseImports);
    // A shallow frontier is repository state alternates do not carry.
    if fs::symlink_metadata(repo.join(".git/shallow")).is_ok() {
        return Ok(None);
    }
    let listed = super::super::output(
        git(&repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]),
    )?;
    let mut refs = Vec::new();
    for line in listed
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let space = line
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let (value, name) = line.split_at(space);
        let name = name
            .get(1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if name.contains(&0) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        refs.extend_from_slice(b"create ");
        refs.extend_from_slice(name);
        refs.push(0);
        refs.extend_from_slice(value);
        refs.push(0);
    }
    Ok(Some(super::super::BaseSeed {
        objects: fs::canonicalize(repo.join(".git/objects")).refuse_at(SITE)?,
        refs,
    }))
}

pub(super) fn objects_of(repo: &Path) -> Result<PathBuf> {
    let objects = text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]))?;
    if objects.contains(['\n', '\r']) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(PathBuf::from(objects))
}

/// What an adopt proof found at a destination checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::git_carry) enum Found {
    /// Equal to the capture: the workspace ledger to record, its `item`
    /// still to be named by the caller.
    Equal(Box<LandedWorkspace>),
    /// Different: the components that differ (`head`, `index`, `exclude`,
    /// `seats=<n>` and the first seat paths).
    Differs(Vec<String>),
}

/// The adopt proof (design section 5): the destination checkout at
/// `worktree` (common directory `common`) against the capture `meta` in
/// `scratch`. Read-only at the destination: reads its HEAD, its index
/// entries (no refresh), its exclude and every carried seat's bytes, each
/// counted as `read_dest_verify_existing_bytes`. Extra destination seats,
/// the omitted rebuildable roots and the planned nests (neither is in the
/// capture's rows) are left alone.
///
/// # Errors
/// Whatever reading the destination or the scratch repository refuses.
pub(in crate::git_carry) fn prove_adoptable(
    worktree: &Path,
    common: &Path,
    scratch: &Scratch,
    meta: &Meta,
    standalone: bool,
    landing: &Landing<'_>,
) -> Result<Found> {
    // The racy instant is taken before the first read (review of
    // 2026-10-16, finding 4), as Git's racy-index check takes it: a seat
    // read early in a long proof and rewritten within its timestamp tick
    // must stay racy for the next converge, which then reads it by content.
    let recorded_ns = super::super::pass_start_ns();
    let mut differs = Vec::new();
    let head = Head {
        symbolic: meta.symbolic(&scratch.repo)?,
        oid: meta.get("head")?.to_owned(),
    };
    if head_of(worktree)? != head {
        differs.push("head".to_owned());
    }
    let index = index_digest(worktree, None)?;
    if index != scratch.capture_index(meta)? {
        differs.push("index".to_owned());
    }
    let exclude = exclude_digest(&read_exclude(common)?);
    if standalone && exclude != exclude_digest(&meta.exclude(&scratch.repo)?) {
        differs.push("exclude".to_owned());
    }
    let sides = sides(&scratch.repo, meta)?;
    let mut objects = BatchObjects::new(&scratch.repo)?;
    let mut prints = Vec::with_capacity(sides.len());
    let mut seats = Vec::new();
    for (path, side) in &sides {
        match seat(&mut objects, worktree, path, side)? {
            Some(print) => prints.push(print),
            None => seats.push(path.clone()),
        }
    }
    objects.finish()?;
    if !seats.is_empty() {
        differs.push(format!("seats={}", seats.len()));
        for path in seats.iter().take(3) {
            differs.push(format!("seat={}", path.escape_ascii()));
        }
    }
    if !differs.is_empty() {
        return Ok(Found::Differs(differs));
    }
    prints.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Found::Equal(Box::new(LandedWorkspace {
        item: landing.item.to_owned(),
        capture: landing.capture,
        observed_ns: landing.observed_ns,
        instance: scratch.instance.clone(),
        head,
        intended_branch: None,
        index,
        exclude,
        recorded_ns,
        fsid: landed::fsid(worktree)?,
        seats: prints,
    })))
}

// One carried seat: its print when it holds the capture's kind, mode and
// content under a print that held still while it was read; `None` when it
// differs.
fn seat(
    objects: &mut BatchObjects,
    worktree: &Path,
    relative: &[u8],
    side: &super::Side,
) -> Result<Option<SeatPrint>> {
    let Ok(path) = safe_destination(worktree, Path::new(OsStr::from_bytes(relative))) else {
        return Ok(None);
    };
    let Some(meta) = lstat(&path)? else {
        return Ok(None);
    };
    let Some(now) = SeatPrint::of(relative, &meta) else {
        return Ok(None);
    };
    if now.kind != side.kind || (side.kind != Kind::Symlink && meta.mode() & 0o777 != side.mode) {
        return Ok(None);
    }
    if side.kind == Kind::Directory {
        return Ok(Some(now));
    }
    if !holds(objects, &path, side)? {
        return Ok(None);
    }
    #[cfg(test)]
    super::super::mid_pass::fire(worktree, super::super::mid_pass::Stage::AdoptSeatRead);
    let settled = lstat(&path)?.and_then(|meta| SeatPrint::of(relative, &meta));
    Ok((settled.as_ref() == Some(&now)).then_some(now))
}

/// Why the checkout at `worktree` (git directory `git_dir`, common
/// directory `common`) is not one this item may adopt or re-key, read
/// before any proof (review of 2026-10-16, findings 1, 3 and 11): `None`
/// when it may; otherwise the component the refusal
/// (`GIT_DESTINATION_OCCUPIED`) stands for, which `estate-diff` prints:
///
/// - `symlinked-path`: the workspace or the plan's repository is not
///   spelled canonically (a symlink on the way, or `..`). A ledger is keyed
///   by the item, the item by its spelled paths, and the converge's own
///   path checks by the real ones: a ledger written under another spelling
///   of a tree hands that tree to a second item, and its converge never
///   settles (design note section 2: a symlink-spelled plan must not be
///   used against a landing; now enforced);
/// - `other-repository`: the checkout belongs to another repository than
///   the plan's;
/// - `linked-as-standalone`: a standalone item's checkout is a linked
///   worktree;
/// - `worktree-link`: the git directory does not name this checkout back:
///   a linked worktree whose admin directory's `gitdir` names another path
///   (moved without `git worktree repair`, which `git worktree prune`
///   would remove with its ledger; or a copy sharing another's admin
///   directory), or a main checkout whose git directory is not its own
///   `.git`.
///
/// # Errors
/// Whatever reading the repository or the paths refuses.
pub(in crate::git_carry) fn foreign_shape(
    worktree: &Path,
    repository: &Path,
    git_dir: &Path,
    common: &Path,
    standalone: bool,
) -> Result<Option<&'static str>> {
    const SITE: &str = "git_carry::converge::adopt::foreign_shape";
    if fs::canonicalize(worktree).refuse_at(SITE)? != worktree {
        return Ok(Some("symlinked-path"));
    }
    match fs::canonicalize(repository) {
        Ok(real) if real != repository => return Ok(Some("symlinked-path")),
        Ok(_) => {}
        Err(_) => return Ok(Some("other-repository")),
    }
    if !super::same_dir(common, &super::super::common_repository(repository)?) {
        return Ok(Some("other-repository"));
    }
    let linked = !super::same_dir(git_dir, common);
    if standalone && linked {
        return Ok(Some("linked-as-standalone"));
    }
    let named = if linked {
        // The admin directory's backlink: `<worktree>/.git`, absolute, or
        // relative to the admin directory (`worktree.useRelativePaths`).
        let Ok(text) = fs::read_to_string(git_dir.join("gitdir")) else {
            return Ok(Some("worktree-link"));
        };
        let named = Path::new(text.trim_end_matches(['\n', '\r']));
        if named.is_absolute() {
            named.to_path_buf()
        } else {
            git_dir.join(named)
        }
    } else {
        git_dir.to_path_buf()
    };
    let backlink = fs::canonicalize(named);
    let own = fs::canonicalize(worktree.join(".git"));
    Ok(match (backlink, own) {
        (Ok(backlink), Ok(own)) if backlink == own => None,
        _ => Some("worktree-link"),
    })
}

/// Why a workspace ledger naming another item (`ledger`) may not be
/// re-keyed to this one (A11b; review of 2026-10-16, findings 1, 2 and 8):
/// `None` when it may. A re-key relabels the same tree, never hands a
/// landing to another source:
///
/// - the checkout's shape ([`foreign_shape`]): never under a symlink's
///   spelling;
/// - `item-in-plan`: the ledger's item is one of this run's plan
///   (`items`), so two items of one plan never take a tree from each other;
/// - `other-tree`: unless this capture is the ledger's own, or its HEAD is
///   the ledger's HEAD or descends from it (read in a scratch repository
///   borrowing the destination's objects; descent implies a related
///   repository). A plan that names another source at a landed path (a
///   typo, two items aliasing one tree) is refused here.
///
/// Read-only at the destination.
///
/// # Errors
/// Whatever reading the destination or the capture refuses.
#[allow(clippy::too_many_arguments)]
pub(in crate::git_carry) fn rekey_refusal(
    staged: &super::super::StagedBundle,
    worktree: &Path,
    repository: &Path,
    (git_dir, common): (&Path, &Path),
    standalone: bool,
    base: Option<&super::super::StagedBundle>,
    source: &str,
    ledger: &LandedWorkspace,
    landing: &Landing<'_>,
    items: &[String],
) -> Result<Option<&'static str>> {
    if let Some(shape) = foreign_shape(worktree, repository, git_dir, common, standalone)? {
        return Ok(Some(shape));
    }
    if items.contains(&ledger.item) {
        return Ok(Some("item-in-plan"));
    }
    if ledger.capture == landing.capture {
        return Ok(None);
    }
    let scratch = Scratch::new(staged, Some(&objects_of(worktree)?), base, source)?;
    let head = scratch.meta(source)?.get("head")?.to_owned();
    if ledger.head.oid.is_empty() || !ancestor(&scratch.repo, &ledger.head.oid, &head)? {
        return Ok(Some("other-tree"));
    }
    Ok(None)
}

/// Whether `older` is an ancestor of `newer`, or equal to it (both
/// readable in `repo`).
///
/// # Errors
/// When Git cannot be run.
pub(in crate::git_carry) fn ancestor(repo: &Path, older: &str, newer: &str) -> Result<bool> {
    let status = git(repo)
        .args(["merge-base", "--is-ancestor", older, newer])
        .output()
        .map_err(|error| crate::refuse::io(&error, "git_carry::converge::adopt::ancestor"))?;
    Ok(status.status.success())
}

// Whether the checkout still is what the proof found (metadata only): the
// same HEAD, index entries, exclude and seat prints. Run under the lock,
// after the proof that read the bytes.
fn still(
    worktree: &Path,
    common: &Path,
    standalone: bool,
    ledger: &LandedWorkspace,
) -> Result<bool> {
    if head_of(worktree)? != ledger.head || index_digest(worktree, None)? != ledger.index {
        return Ok(false);
    }
    if standalone && exclude_digest(&read_exclude(common)?) != ledger.exclude {
        return Ok(false);
    }
    for print in &ledger.seats {
        let Ok(path) = safe_destination(worktree, Path::new(OsStr::from_bytes(&print.path))) else {
            return Ok(false);
        };
        let now = lstat(&path)?.and_then(|meta| SeatPrint::of(&print.path, &meta));
        if !now.is_some_and(|now| now.same(print)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A11a: adopt the checkout at `place` (no workspace ledger) for the
/// capture `staged`.
///
/// # Errors
/// `GIT_DESTINATION_OCCUPIED` when the checkout is not this item's to
/// adopt ([`foreign_shape`]), belongs to an unrelated repository, or has a
/// workspace ledger by the time the lock is held;
/// `GIT_DESTINATION_DIVERGED` when it differs from the capture (all with
/// nothing written); whatever the import, the ref step or a record write
/// refuses.
pub(super) fn workspace(
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    place: &Place<'_>,
) -> Result<Applied> {
    let worktree = place.worktree;
    let source = place.source;
    if foreign_shape(
        worktree,
        target.repository,
        &place.git_dir,
        &place.common,
        place.standalone,
    )?
    .is_some()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let scratch = Scratch::new(staged, Some(&objects_of(worktree)?), place.base, source)?;
    if !super::super::related_repository(worktree, &scratch.repo, &scratch.tips(source)?)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let meta = scratch.meta(source)?;
    let mut ledger = match prove_adoptable(
        worktree,
        &place.common,
        &scratch,
        &meta,
        place.standalone,
        landing,
    )? {
        Found::Equal(ledger) => *ledger,
        Found::Differs(_) => return Err(BulkloadRefusal::GitDestinationDiverged),
    };
    #[cfg(test)]
    super::super::mid_pass::fire(worktree, super::super::mid_pass::Stage::AdoptBeforeLock);
    let _lock = landed::lock(&place.common, source)?;
    // The caller found no workspace ledger before the lock; another apply
    // (another STATE or plan) may have landed or adopted the checkout
    // since, under its own item (review of 2026-10-16, finding 5).
    if landed::exists(&place.workspace_dir().join(landed::WORKSPACE_LANDED))? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let mut custody = settle_repository(worktree, &place.common, source)?;
    if !still(worktree, &place.common, place.standalone, &ledger)? {
        return Err(BulkloadRefusal::GitDestinationDiverged);
    }
    if let Some(base) = place.base {
        super::super::import_verified(worktree, base.path(), source)?;
    }
    let (_, instance) = super::super::import_verified_instance(worktree, staged.path(), source)?;
    if instance != scratch.instance {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    ledger.instance = instance;
    fault_point!(EstateAdoptAfterImport);
    let t = landing.observed_ns;
    let ref_path = place.ref_dir().join(landed::REFS_LANDED);
    let mut refs = landed::read::<RefLedger>(&ref_path)?.unwrap_or_default();
    if !refs.created {
        mark(worktree, &place.common, source, &ledger.instance, landing)?;
        refs.created = true;
        custody.count("repository-adopted");
    }
    if place.standalone && refs.exclude.is_none() {
        refs.exclude = Some(LandedExclude {
            digest: ledger.exclude,
            observed_ns: t,
        });
    }
    // The HEAD branch is proved equal to the capture's (R2): its row makes
    // it bulkload's, so a later converge's attach rule and R5 move it.
    if let Some(branch) = &ledger.head.symbolic {
        let (native, _) = native_refs::native_refs(worktree)?;
        let live = refs.refs.get(branch).and_then(|row| row.oid.clone());
        if live.is_none() && native.get(branch) == Some(&ledger.head.oid) {
            refs.refs.insert(
                branch.clone(),
                LandedRef {
                    oid: Some(ledger.head.oid.clone()),
                    observed_ns: t,
                },
            );
            custody.count("ref-adopted");
        }
    }
    landed::ensure_dir(&place.common, source)?;
    landed::write(&ref_path, &refs)?;
    fault_point!(EstateAdoptAfterMarker);
    let meta1 = Meta::read(worktree, source, &ledger.instance)?;
    place.refs(
        &meta1,
        t,
        ledger.head.symbolic.as_deref(),
        None,
        &mut custody,
    )?;
    fault_point!(EstateAdoptAfterRefs);
    let seats = u64::try_from(ledger.seats.len()).unwrap_or(u64::MAX);
    let directory = landed::ensure_dir(&place.git_dir, source)?;
    landed::write(&directory.join(landed::WORKSPACE_LANDED), &ledger)?;
    fault_point!(EstateAdoptAfterLedger);
    custody.count("adopted");
    custody.add("adopted-seats", seats);
    Ok(Applied::adopted(&custody))
}

// The adoption record, then (by the caller) the marker: written in the
// common ledger directory, before the marker, so a marker never stands
// without a record of how it was set.
fn mark(
    repo: &Path,
    common: &Path,
    source: &str,
    instance: &str,
    landing: &Landing<'_>,
) -> Result<()> {
    let (native, _) = native_refs::native_refs(repo)?;
    let carried = native_refs::carried(repo, source, instance)?;
    let equal = carried
        .iter()
        .filter(|(name, value)| native.get(*name) == Some(*value))
        .count();
    let directory = landed::ensure_dir(common, source)?;
    landed::write(
        &directory.join(landed::ADOPTED),
        &Adopted {
            item: landing.item.to_owned(),
            capture: landing.capture,
            observed_ns: landing.observed_ns,
            recorded_ns: super::super::pass_start_ns(),
            native: u64::try_from(native.len()).unwrap_or(u64::MAX),
            equal: u64::try_from(equal).unwrap_or(u64::MAX),
        },
    )
}

/// A11b: re-key a workspace ledger that names another item: only under
/// `--adopt` and when [`rekey_refusal`] finds none
/// (`GIT_DESTINATION_OCCUPIED` otherwise), then after its normal proof
/// (`GIT_DESTINATION_MODIFIED` when it fails), one atomic rewrite of the
/// item key, nothing else.
///
/// # Errors
/// As above, or the ledger's write.
pub(super) fn rekey(
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    place: &Place<'_>,
    (ledger_path, ledger): (&Path, &mut LandedWorkspace),
    landing: &Landing<'_>,
    policy: &super::Policy<'_>,
) -> Result<()> {
    if !policy.adopt
        || rekey_refusal(
            staged,
            place.worktree,
            target.repository,
            (&place.git_dir, &place.common),
            place.standalone,
            place.base,
            place.source,
            ledger,
            landing,
            policy.items,
        )?
        .is_some()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let meta0 = Meta::read(place.worktree, place.source, &ledger.instance)?;
    let old = super::handoff::landed_sides(place.worktree, &place.git_dir, place.source, &meta0)?;
    super::prove(
        place.worktree,
        &place.common,
        ledger,
        place.standalone,
        &old,
    )?;
    landing.item.clone_into(&mut ledger.item);
    landed::write(ledger_path, &*ledger)
}

/// A11d: a refs-only item's repository bulkload did not create, its
/// capture just imported as provenance (`instance`): adopted when it is
/// the capture's, or empty (the marker, then the native ref step; an
/// unborn HEAD of a bare repository put on the captured branch). An
/// unrelated repository keeps the provenance import, as without
/// `--adopt`, and refuses (review of 2026-10-16, finding 7): the operator
/// asked for adoption and no native ref landed, so the item is pending
/// review, never counted applied.
///
/// # Errors
/// `GIT_DESTINATION_OCCUPIED` for an unrelated repository (no marker, no
/// record, no native ref written); whatever the ref step or a record
/// write refuses.
pub(super) fn repository(
    repo: &Path,
    common: &Path,
    source: &str,
    instance: &str,
    landing: &Landing<'_>,
) -> Result<Applied> {
    let carried: Vec<String> = native_refs::carried(repo, source, instance)?
        .into_values()
        .collect();
    if !super::super::related_repository(repo, repo, &carried)? {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let mut custody = Custody::default();
    let _lock = landed::lock(common, source)?;
    custody.merge(settle_repository(repo, common, source)?);
    let ref_dir = landed::ledger_dir(common, source);
    let ref_path = ref_dir.join(landed::REFS_LANDED);
    let mut refs = landed::read::<RefLedger>(&ref_path)?.unwrap_or_default();
    if !refs.created {
        mark(repo, common, source, instance, landing)?;
        refs.created = true;
        landed::write(&ref_path, &refs)?;
        custody.count("repository-adopted");
    }
    fault_point!(EstateAdoptAfterMarker);
    let mut skip = native_refs::checked_out(repo)?;
    skip.extend(super::intent_branches(repo, source)?);
    let (_, more) = native_refs::step(
        repo,
        &ref_dir,
        &refs,
        source,
        instance,
        landing.observed_ns,
        &skip,
        None,
        true,
    )?;
    custody.merge(more);
    fault_point!(EstateAdoptAfterRefs);
    if super::super::settle_bare_head(
        repo,
        super::super::carried_symbolic(repo, source, instance)?.as_deref(),
    )? {
        custody.count("head-symbolic-set");
    }
    custody.count("adopted");
    Ok(Applied::adopted(&custody))
}
