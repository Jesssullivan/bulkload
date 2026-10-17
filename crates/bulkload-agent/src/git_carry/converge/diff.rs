//! `estate-diff` (#226 item 1): the report mode of what `estate-apply` and
//! `--adopt` run.
//!
//! Design: `docs/agent-notes/2026-10-15-estate-adopt-design.md`, section 3.
//! The report and the apply share their functions, so they cannot
//! disagree.
//!
//! Read-only at the destination: HEAD, the index entries (never
//! refreshed), the exclude, refs, reflogs and the bytes of carried seats
//! whose print does not already prove them; every Git child runs with
//! `GIT_OPTIONAL_LOCKS=0`. The capture is read from a scratch repository in
//! the staged bundle's private directory, which borrows the destination's
//! objects through `alternates` and is removed with the stage. The source
//! is never read.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::super::landed::{self, LandedWorkspace, RefLedger};
use super::super::native_refs::{self, Class, Input};
use super::super::{git, text};
use super::adopt::{
    ancestor, foreign_shape, objects_of, prove_adoptable, rekey_refusal, Found, Scratch,
};
use super::{
    check_linked, first_landing_target, head_of, lstat, planned, prove, Landing, Meta, Target,
};
use crate::{BulkloadRefusal, Result};

/// One ref of an item's repository, as `estate-diff` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRow {
    pub name: String,
    /// `same`, `source-only`, `dest-only`, `source-ahead` (the
    /// destination's tip is an ancestor of the carried one), `dest-ahead`
    /// or `diverged`; exact, by ancestry.
    pub class: &'static str,
    /// The custody word the native ref step would report for it with the
    /// repository marker set (an apply with `--adopt`, or a repository
    /// bulkload created): `ref-created` (R1), `ref-adopted` (R2),
    /// `ref-kept-destination` (R3), `untouched` (R10), and the converge
    /// rows' words.
    pub row: &'static str,
}

/// One planned item, as `estate-diff` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemRow {
    /// `identical`, `source-only`, `source-ahead`, `dest-ahead`,
    /// `diverged` (exact, three-way against a ledger), or `differs` with a
    /// hint (two-way, no ledger).
    pub class: &'static str,
    /// For `differs`: `source-ahead`, `dest-ahead` or `diverged`, by the
    /// HEADs' ancestry only, never claimed exact.
    pub hint: Option<&'static str>,
    /// `ledger` (three-way) or `none` (two-way).
    pub baseline: &'static str,
    /// What `estate-apply` does here: `landing` (A5), and where A5's own
    /// checks refuse it, `ignore-policy-conflict`
    /// (`GIT_IGNORE_POLICY_CONFLICT`) or `parent-missing`
    /// (`GIT_DESTINATION_PARENT_MISSING`: absent parents under a symlink or
    /// a non-directory, which the apply never makes; M2 finding 1);
    /// `bare-landing`
    /// (A11c), `adoptable` (A11a, A11d), `diverged`, `occupied-foreign`,
    /// `symlinked-path` and `occupied-shape` (`GIT_DESTINATION_OCCUPIED`
    /// before any proof, `adopt::foreign_shape`), `ledgered`,
    /// `ledgered-other-item` (re-keyed by A11b with `--adopt`),
    /// `not-a-repository`, `not-at-path`.
    pub adopt: &'static str,
    /// The components that differ: `head`, `index`, `exclude`,
    /// `seats=<n>`, `seat=<path>`, `unrelated`; `parents=<n>` for a first
    /// landing that makes `n` absent parents (M2 finding 1);
    /// `exclude-union=<n>` for an `ignore-policy-conflict` that
    /// `estate-apply --exclude-union` lands by appending `n` lines (M2
    /// round 2, item 2);
    /// `repository-absent` for a
    /// first linked landing whose repository the apply's main item lands
    /// first (its exclude policy is not checked); or, for an occupied shape
    /// or a re-key `--adopt` refuses, its reason (`symlinked-path`,
    /// `other-repository`, `linked-as-standalone`, `worktree-link`,
    /// `item-in-plan`, `other-tree`).
    pub components: Vec<String>,
    /// Destination work since `--since` (evidence, never a class).
    pub evidence: Vec<String>,
    pub refs: Vec<RefRow>,
    /// What a first landing does with the captured origin: `https`,
    /// `prefix` (an `--activate-origin` prefix names it), `inert`, `none`;
    /// `kept-destination` when the destination's own configuration stays.
    pub origin: &'static str,
}

// What `estate-apply --adopt` answers for an occupied shape or a refused
// re-key (review of 2026-10-16, findings 1, 2, 3 and 11): another tree is
// `occupied-foreign`, a symlinked spelling `symlinked-path`, any other
// shape `occupied-shape`; the reason is a component. A ledger's three-way
// class stands; without one nothing was compared: class `differs` with no
// hint.
fn occupied(row: &mut ItemRow, reason: &'static str) {
    if row.baseline != "ledger" {
        row.class = "differs";
        row.hint = None;
    }
    row.adopt = match reason {
        "symlinked-path" => "symlinked-path",
        "other-tree" => "occupied-foreign",
        _ => "occupied-shape",
    };
    row.components.push(reason.to_owned());
}

// What a first landing (A5) answers (dogfood of 2026-10-16, F1): the
// apply's own pre-landing checks, never a guess. A first linked landing
// runs [`check_linked`] when its repository exists (when it does not, the
// apply's main item lands it first: only the path is checked, and the row
// says `repository-absent`). `GIT_IGNORE_POLICY_CONFLICT` is
// `ignore-policy-conflict`, `GIT_DESTINATION_PARENT_MISSING`
// `parent-missing`; any other refusal is the row's refusal. With the
// verdict, how many absent parents the apply makes (M2 finding 1), and for
// `ignore-policy-conflict` how many lines `--exclude-union` would append
// to land it (`None` where the union refuses too; M2 round 2, item 2).
fn first_landing(
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
) -> Result<(&'static str, usize, Option<usize>)> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let checked = if workspace == target.repository || planned(target.repository)?.is_none() {
        first_landing_target(staged, workspace)
    } else {
        check_linked(
            staged,
            (target.repository, target.base),
            workspace,
            target.source,
            (landing.observed_ns, false),
        )
        .map(|checked| checked.destination)
    };
    match checked {
        Ok(first) => Ok(("landing", first.absent(), None)),
        Err(BulkloadRefusal::GitIgnorePolicyConflict) => {
            // M2 round 2, item 2: what `--exclude-union` would do, decided
            // by the same check under its union rule (read-only).
            let union = check_linked(
                staged,
                (target.repository, target.base),
                workspace,
                target.source,
                (landing.observed_ns, true),
            )
            .ok()
            .map(|checked| checked.exclude_union.len());
            Ok(("ignore-policy-conflict", 0, union))
        }
        Err(BulkloadRefusal::GitDestinationParentMissing) => Ok(("parent-missing", 0, None)),
        Err(other) => Err(other),
    }
}

// The component a first landing that makes `absent` parents carries.
fn parents_component(row: &mut ItemRow, absent: usize) {
    if absent > 0 {
        row.components.push(format!("parents={absent}"));
    }
}

/// Report one workspace item (`target.workspace` is set). `items` are the
/// plan's item ids, which A11b never re-keys from.
///
/// # Errors
/// Whatever reading the destination or the capture refuses.
pub fn workspace(
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    since: Option<i128>,
    (origins, items): (&[String], &[String]),
) -> Result<ItemRow> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let standalone = workspace == target.repository;
    let mut row = ItemRow {
        baseline: "none",
        origin: "none",
        ..ItemRow::default()
    };
    if planned(workspace)?.is_none() {
        row.class = "source-only";
        let (adopt, absent, union) = first_landing(staged, target, landing)?;
        row.adopt = adopt;
        parents_component(&mut row, absent);
        if let Some(lines) = union {
            row.components.push(format!("exclude-union={lines}"));
        }
        if standalone && row.adopt == "landing" {
            let scratch = Scratch::new(staged, None, target.base, target.source)?;
            row.origin = origin(&scratch, target.source, origins)?;
        }
        if !standalone && planned(target.repository)?.is_none() {
            row.components.push("repository-absent".to_owned());
        }
        return Ok(row);
    }
    let Some(git_dir) = landed::worktree_git_dir(workspace)?.filter(|dir| dir.is_dir()) else {
        row.class = "differs";
        row.hint = Some("diverged");
        row.adopt = "not-a-repository";
        return Ok(row);
    };
    let common = landed::common_dir(&git_dir)?;
    let scratch = Scratch::new(
        staged,
        Some(&objects_of(workspace)?),
        target.base,
        target.source,
    )?;
    let meta = scratch.meta(target.source)?;
    row.origin = "kept-destination";
    row.evidence = evidence(&git_dir, &common, since)?;
    let refs = landed::read::<RefLedger>(
        &landed::ledger_dir(&common, target.source).join(landed::REFS_LANDED),
    )?
    .filter(|ledger| ledger.created)
    .unwrap_or_default();
    row.refs = ref_rows(
        workspace,
        &scratch,
        target.source,
        &refs,
        landing.observed_ns,
    )?;
    let ledger_path = landed::ledger_dir(&git_dir, target.source).join(landed::WORKSPACE_LANDED);
    if let Some(ledger) = landed::read::<LandedWorkspace>(&ledger_path)? {
        ledgered(
            &mut row,
            staged,
            target,
            landing,
            (&git_dir, &common),
            &ledger,
            items,
        )?;
        return Ok(row);
    }
    if let Some(reason) =
        foreign_shape(workspace, target.repository, &git_dir, &common, standalone)?
    {
        occupied(&mut row, reason);
        return Ok(row);
    }
    if !super::super::related_repository(workspace, &scratch.repo, &scratch.tips(target.source)?)? {
        row.class = "differs";
        row.hint = Some("diverged");
        row.adopt = "occupied-foreign";
        row.components.push("unrelated".to_owned());
        return Ok(row);
    }
    match prove_adoptable(workspace, &common, &scratch, &meta, standalone, landing)? {
        Found::Equal(_) => {
            row.class = "identical";
            row.adopt = "adoptable";
        }
        Found::Differs(components) => {
            row.class = "differs";
            row.adopt = "diverged";
            row.components = components;
            row.hint = Some(hint(
                &scratch.repo,
                &head_of(workspace)?.oid,
                meta.get("head")?,
            )?);
        }
    }
    Ok(row)
}

// The three-way row of a checkout with a workspace ledger: its class by
// the ledger's proof and capture, and, for a ledger naming another item,
// whether `--adopt` re-keys it (A11b) or refuses ([`rekey_refusal`]).
fn ledgered(
    row: &mut ItemRow,
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    (git_dir, common): (&Path, &Path),
    ledger: &LandedWorkspace,
    items: &[String],
) -> Result<()> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let standalone = workspace == target.repository;
    row.baseline = "ledger";
    row.adopt = if ledger.item == landing.item {
        "ledgered"
    } else {
        "ledgered-other-item"
    };
    let meta0 = Meta::read(workspace, target.source, &ledger.instance)?;
    let old = super::handoff::landed_sides(workspace, git_dir, target.source, &meta0)?;
    let proved = match prove(workspace, common, ledger, standalone, &old) {
        Ok(_) => true,
        Err(BulkloadRefusal::GitDestinationModified) => false,
        Err(other) => return Err(other),
    };
    let current = ledger.capture == landing.capture;
    row.class = match (proved, current) {
        (true, true) => "identical",
        (true, false) => "source-ahead",
        (false, true) => "dest-ahead",
        (false, false) => "diverged",
    };
    if !proved {
        row.components.push("modified-since-landing".to_owned());
    }
    if ledger.item != landing.item {
        if let Some(reason) = rekey_refusal(
            staged,
            workspace,
            target.repository,
            (git_dir, common),
            standalone,
            target.base,
            target.source,
            ledger,
            landing,
            items,
        )? {
            occupied(row, reason);
        }
    }
    Ok(())
}

/// Report one refs-only item (no workspace planned).
///
/// # Errors
/// Whatever reading the destination or the capture refuses.
pub fn refs_only(
    staged: &super::super::StagedBundle,
    target: &Target<'_>,
    landing: &Landing<'_>,
    since: Option<i128>,
    origins: &[String],
) -> Result<ItemRow> {
    let repo = target.repository;
    let mut row = ItemRow {
        baseline: "none",
        origin: "none",
        ..ItemRow::default()
    };
    if planned(repo)?.is_none() {
        row.class = "source-only";
        if staged.is_bare() {
            // M2 finding 1: the bare landing's own parent check.
            match super::super::parents::FirstTarget::of(repo) {
                Ok(first) => parents_component(&mut row, first.absent()),
                Err(BulkloadRefusal::GitDestinationParentMissing) => {
                    row.adopt = "parent-missing";
                    return Ok(row);
                }
                Err(other) => return Err(other),
            }
            row.adopt = "bare-landing";
            let scratch = Scratch::new(staged, None, target.base, target.source)?;
            row.origin = origin(&scratch, target.source, origins)?;
        } else {
            row.adopt = "not-at-path";
        }
        return Ok(row);
    }
    if super::super::destination_repository(repo).is_err() {
        row.class = "differs";
        row.hint = Some("diverged");
        row.adopt = "not-a-repository";
        return Ok(row);
    }
    let common = super::super::common_repository(repo)?;
    let git_dir = PathBuf::from(text(git(repo).args(["rev-parse", "--absolute-git-dir"]))?);
    let scratch = Scratch::new(staged, Some(&objects_of(repo)?), target.base, target.source)?;
    row.origin = "kept-destination";
    row.evidence = evidence(&git_dir, &common, since)?;
    let ledger = landed::read::<RefLedger>(
        &landed::ledger_dir(&common, target.source).join(landed::REFS_LANDED),
    )?
    .filter(|ledger| ledger.created);
    row.baseline = if ledger.is_some() { "ledger" } else { "none" };
    row.refs = ref_rows(
        repo,
        &scratch,
        target.source,
        &ledger.clone().unwrap_or_default(),
        landing.observed_ns,
    )?;
    let same = row.refs.iter().all(|reference| reference.class == "same");
    row.class = if same { "identical" } else { "differs" };
    if !same {
        row.hint = Some("diverged");
    }
    row.adopt = if ledger.is_some() {
        "ledgered"
    } else if super::super::related_repository(repo, &scratch.repo, &scratch.tips(target.source)?)?
    {
        "adoptable"
    } else {
        "occupied-foreign"
    };
    Ok(row)
}

/// Every worktree `repo` registers, its main first, with the branch its
/// HEAD names: what `estate-diff` reports `dest-only` when no item plans it.
///
/// # Errors
/// Whatever `git worktree list` refuses.
pub fn registered_worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    super::worktrees(repo)
}

// What a first landing of the capture does with its origin.
fn origin(scratch: &Scratch, source: &str, origins: &[String]) -> Result<&'static str> {
    let heads = match scratch.meta(source) {
        Ok(meta) => meta.heads(),
        Err(_) => carried_heads(scratch, source)?,
    };
    if super::super::capture_revision(&heads, "configuration-v1").is_err() {
        return Ok("none");
    }
    let receipt = scratch.repo.join(".git/diff-config");
    let _ = fs::create_dir(&receipt);
    let plan = super::super::plan_standalone_configuration(
        &scratch.repo,
        &heads,
        &receipt,
        None,
        origins,
    )?;
    let active = plan
        .activate
        .iter()
        .any(|(key, _)| key == "remote.origin.url");
    Ok(if active && !plan.by_prefix.is_empty() {
        "prefix"
    } else if active {
        "https"
    } else if plan
        .preserved_only
        .iter()
        .any(|key| key == "remote.origin.url")
    {
        "inert"
    } else {
        "none"
    })
}

// A bare capture's metadata as header lines (it has no worktree or HEAD for
// [`Meta`]).
fn carried_heads(scratch: &Scratch, source: &str) -> Result<String> {
    let prefix = format!("refs/carry/v1/{source}/{}/", scratch.instance);
    let listed = text(git(&scratch.repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        &format!("--exclude={prefix}refs/"),
        &format!("--exclude={prefix}stashes/"),
        &prefix,
    ]))?;
    let mut heads = String::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let suffix = name
            .strip_prefix(prefix.as_str())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        heads.push_str(value);
        heads.push_str(" refs/carry-export/");
        heads.push_str(suffix);
        heads.push('\n');
    }
    Ok(heads)
}

// Every restorable ref of the destination or the capture, classed by
// ancestry in the scratch repository (which reads both).
fn ref_rows(
    repo: &Path,
    scratch: &Scratch,
    source: &str,
    ledger: &RefLedger,
    t: i128,
) -> Result<Vec<RefRow>> {
    let (native, symbolic) = native_refs::native_refs(repo)?;
    let carried = native_refs::carried(&scratch.repo, source, &scratch.instance)?;
    let checked = native_refs::checked_out(repo)?;
    let mut names: Vec<&String> = native.keys().chain(carried.keys()).collect();
    names.sort();
    names.dedup();
    let mut rows = Vec::new();
    for name in names {
        if !native_refs::restorable(name) || symbolic.contains(name) {
            continue;
        }
        let (here, there) = (native.get(name), carried.get(name));
        let Some(class) = ref_class(&scratch.repo, here, there)? else {
            continue;
        };
        let decision = native_refs::decide(Input {
            native: here.map(String::as_str),
            row: ledger.refs.get(name),
            carried: there.map(String::as_str),
            t,
            class: Class::of(name),
            checked_out: checked.contains(name),
        });
        rows.push(RefRow {
            name: name.clone(),
            class,
            row: decision.line.name().unwrap_or("untouched"),
        });
    }
    Ok(rows)
}

/// One ref's class, by ancestry in `repo` (which holds both values):
/// `same`, `source-only`, `dest-only`, `source-ahead` (the destination's
/// tip is an ancestor of the carried one), `dest-ahead` or `diverged`;
/// `None` when neither side has it. `estate-diff`'s ref rows and the
/// hand-off block (TIN-6569) share it.
pub(super) fn ref_class(
    repo: &Path,
    here: Option<&String>,
    there: Option<&String>,
) -> Result<Option<&'static str>> {
    Ok(Some(match (here, there) {
        (Some(here), Some(there)) if here == there => "same",
        (None, Some(_)) => "source-only",
        (Some(_), None) => "dest-only",
        (Some(here), Some(there)) => {
            if ancestor(repo, here, there)? {
                "source-ahead"
            } else if ancestor(repo, there, here)? {
                "dest-ahead"
            } else {
                "diverged"
            }
        }
        (None, None) => return Ok(None),
    }))
}

// A two-way hint from the HEADs alone.
fn hint(repo: &Path, here: &str, there: &str) -> Result<&'static str> {
    Ok(if here.is_empty() || here == there {
        "diverged"
    } else if ancestor(repo, here, there)? {
        "source-ahead"
    } else if ancestor(repo, there, here)? {
        "dest-ahead"
    } else {
        "diverged"
    })
}

// Destination work since `since` (ns since the epoch): HEAD's reflog
// entries, any ref's reflog entries and an index written after it
// (compare.py's SWAP evidence). Reads files only.
fn evidence(git_dir: &Path, common: &Path, since: Option<i128>) -> Result<Vec<String>> {
    let Some(since) = since else {
        return Ok(Vec::new());
    };
    let seconds = since / 1_000_000_000;
    let mut evidence = Vec::new();
    let head = reflog_after(&git_dir.join("logs/HEAD"), seconds)?;
    if head > 0 {
        evidence.push(format!("head-reflog-since={head}"));
    }
    let mut refs = 0u64;
    let mut pending = vec![common.join("logs/refs")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            } else {
                refs = refs.saturating_add(reflog_after(&path, seconds)?);
            }
        }
    }
    if refs > 0 {
        evidence.push(format!("ref-reflog-since={refs}"));
    }
    if let Some(meta) = lstat(&git_dir.join("index"))? {
        if i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()) > since {
            evidence.push("index-written-since".to_owned());
        }
    }
    Ok(evidence)
}

// The entries of one reflog file stamped after `seconds`: each line is
// `<old> <new> <identity> <seconds> <zone>\t<message>`.
fn reflog_after(path: &Path, seconds: i128) -> Result<u64> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(0)
        }
        Err(error) => {
            return Err(crate::refuse::io(
                &error,
                "git_carry::converge::diff::reflog",
            ))
        }
    };
    let mut after = 0u64;
    for line in bytes.split(|byte| *byte == b'\n') {
        let head = line.split(|byte| *byte == b'\t').next().unwrap_or_default();
        let fields: Vec<&[u8]> = head.rsplitn(3, |byte| *byte == b' ').collect();
        let stamp = fields
            .get(1)
            .and_then(|stamp| std::str::from_utf8(stamp).ok())
            .and_then(|stamp| stamp.parse::<i128>().ok());
        if stamp.is_some_and(|stamp| stamp > seconds) {
            after = after.saturating_add(1);
        }
    }
    Ok(after)
}

/// A path's bytes as `estate-diff` prints them.
#[must_use]
pub fn shown(path: &Path) -> String {
    OsStr::as_bytes(path.as_os_str()).escape_ascii().to_string()
}
