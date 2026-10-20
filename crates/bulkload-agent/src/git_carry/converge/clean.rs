//! A clean source lands no tree beside the live checkout.
//!
//! Operator ruling 2026-10-16 21:24Z: "refs in always; a tree beside ONLY
//! where neo has uncommitted work" (lane note
//! `docs/agent-notes/2026-10-19-clean-source-no-beside.md`).
//!
//! `estate-apply --adopt --beside SUFFIX --refs-in` (and so `estate-move`):
//! where the adopt proof of a live checkout bulkload did not land finds it
//! differs (`GIT_DESTINATION_DIVERGED`, or a `GIT_ADVANCE_*` refusal under
//! `--advance-ancestor`), the capture used to land beside it at
//! `<path>SUFFIX` whatever it held. Now, before that, [`assess`] reads the
//! capture (in a scratch repository borrowing the live repository's
//! objects; nothing written at the destination):
//!
//! 1. **Tracked-clean**: the captured index is the captured HEAD's tree
//!    (no staged change), it holds no intent-to-add entry, and every path
//!    the HEAD tracks is a carried seat of the same kind, blob and
//!    executable bit (no unstaged change; a missing tracked file is one).
//! 2. **Its HEAD reachable in the destination after refs-in**: the
//!    captured HEAD commit is reachable from a ref the live repository
//!    holds (provenance `refs/carry*` aside) or from a captured branch or
//!    tag, which `--refs-in` brings in under `refs/<ns>/` when the live
//!    repository lacks it at that value.
//!
//! When both hold, there is nothing of the source's a tree beside would
//! hold that the live repository does not get by ref: [`land`] writes no
//! tree beside. Its refs go in (`--refs-in`, the caller, stash entries
//! included) and the source's untracked and ignored entries are reconciled
//! file by file into the live checkout ([`super::files`]: absent placed,
//! equal adopted, different landed as `<file>SUFFIX`), recorded in STATE
//! (`{item}-{blake3(SOURCE)}.files`). The live checkout's HEAD, index,
//! tracked files, branches, stash, configuration and `info/exclude` are
//! never touched; the source's `info/exclude` and every captured byte stay
//! in the capture's provenance in the live repository. Outcome and journal
//! `workspace-refs-in`, which closure counts `applied`.
//!
//! When either does not hold (a tracked change on the source, or a HEAD no
//! ref brings in), the capture lands beside as before.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use super::super::batch_objects::BatchObjects;
use super::super::landed::{self, Kind};
use super::super::native_refs;
use super::super::{git, input, output, text};
use super::adopt::{objects_of, Scratch};
use super::files::{self, FilesRecord, Want};
use super::{sides, Applied, Landing, Meta, Side, Target};
use crate::{BulkloadRefusal, Result};

/// A capture proved clean and reachable ([`assess`]).
pub struct Clean {
    scratch: Scratch,
    head: String,
    tracked: u64,
    wants: Vec<Want>,
}

impl Clean {
    /// The captured HEAD commit.
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    /// How many entries the captured HEAD tracks.
    #[must_use]
    pub const fn tracked(&self) -> u64 {
        self.tracked
    }

    /// How many untracked and ignored entries the capture carries.
    #[must_use]
    pub const fn untracked(&self) -> usize {
        self.wants.len()
    }
}

// The paths the captured HEAD tracks (its trees and gitlinks too) and how
// many of them are files or links, when the captured index and every
// tracked seat are the HEAD's (module docs, 1); `None` when they are not.
fn tracked(
    scratch: &Scratch,
    meta: &Meta,
    sides: &BTreeMap<Vec<u8>, Side>,
) -> Result<Option<(BTreeSet<Vec<u8>>, u64)>> {
    let head = meta.get("head")?;
    let tree = |revision: &str| {
        text(git(&scratch.repo).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{revision}^{{tree}}"),
        ]))
        .ok()
    };
    let (Some(head_tree), Some(staged_tree)) = (tree(head), tree(meta.get("staged")?)) else {
        return Ok(None);
    };
    if head_tree != staged_tree {
        return Ok(None);
    }
    if !super::super::carried_intent_to_add(&scratch.repo, &meta.heads())?.is_empty() {
        return Ok(None);
    }
    let listed =
        output(git(&scratch.repo).args(["ls-tree", "-r", "-t", "-z", "--full-tree", head]))?;
    let mut paths = BTreeSet::new();
    let mut files = 0_u64;
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(entry.get(..tab).unwrap_or_default())
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let (Some(mode), Some(_), Some(value)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        let path = entry.get(tab + 1..).unwrap_or_default().to_vec();
        match mode {
            // A tree.
            "040000" => {}
            // A gitlink: a nested repository's place, as the capture's rows
            // leave it (a directory, or nothing); a file or a link there is
            // a type change.
            "160000" => {
                if sides
                    .get(&path)
                    .is_some_and(|side| side.kind != Kind::Directory)
                {
                    return Ok(None);
                }
            }
            "100644" | "100755" | "120000" => {
                let Some(side) = sides.get(&path) else {
                    return Ok(None);
                };
                let kind = if mode == "120000" {
                    Kind::Symlink
                } else {
                    Kind::Regular
                };
                if side.kind != kind || side.blob.as_deref() != Some(value) {
                    return Ok(None);
                }
                if kind == Kind::Regular && (side.mode & 0o111 != 0) != (mode == "100755") {
                    return Ok(None);
                }
                files += 1;
            }
            _ => return Ok(None),
        }
        paths.insert(path);
    }
    Ok(Some((paths, files)))
}

/// Whether `commit` is reachable, in the repository at `scratch` (which
/// borrows the live one's objects), from one of `tips`.
fn reachable_from(scratch: &Path, commit: &str, tips: &BTreeSet<String>) -> bool {
    if tips.contains(commit) {
        return true;
    }
    let mut request = format!("{commit}\n");
    for tip in tips {
        request.push('^');
        request.push_str(tip);
        request.push('\n');
    }
    // A Git child that fails (an object it cannot read) says no.
    input(
        git(scratch).args(["rev-list", "--max-count=1", "--stdin"]),
        request.as_bytes(),
    )
    .is_ok_and(|listed| listed.iter().all(u8::is_ascii_whitespace))
}

/// The values of every ref of `repository` that holds a commit durably:
/// all but bulkload's provenance (`refs/carry*`), the remote-tracking refs
/// (a fetch moves them) and the stash (its reflog is its entries).
///
/// # Errors
/// Whatever listing the refs refuses.
pub(in crate::git_carry) fn live_tips(repository: &Path) -> Result<BTreeSet<String>> {
    let listed = text(git(repository).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    let mut tips = BTreeSet::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !(name.starts_with("refs/carry")
            || name.starts_with("refs/remotes/")
            || name == "refs/stash")
        {
            tips.insert(value.to_owned());
        }
    }
    Ok(tips)
}

/// Whether `commit` is reachable from a ref of the live repository at
/// `repository` ([`live_tips`]), read in that repository.
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn reachable_in(repository: &Path, commit: &str) -> Result<bool> {
    Ok(reachable_from(repository, commit, &live_tips(repository)?))
}

/// Read the capture `staged` for `target`'s live checkout (module docs).
///
/// `Some` when it is tracked-clean and its HEAD is reachable after
/// refs-in, so no tree lands beside; `None` when it is not. Read-only at
/// the destination.
///
/// # Errors
/// Whatever building the scratch repository or reading the capture or the
/// live refs refuses.
pub fn assess(staged: &super::super::StagedBundle, target: &Target<'_>) -> Result<Option<Clean>> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let scratch = Scratch::new(
        staged,
        Some(&objects_of(workspace)?),
        target.base,
        target.source,
    )?;
    let meta = scratch.meta(target.source)?;
    let sides = sides(&scratch.repo, &meta)?;
    let Some((paths, tracked)) = tracked(&scratch, &meta, &sides)? else {
        return Ok(None);
    };
    // Every captured ref but the stash goes in under the namespace
    // (`--refs-in` of a clean source); the caller checks again once it has.
    let mut tips = live_tips(target.repository)?;
    for (name, value) in native_refs::carried(&scratch.repo, target.source, &scratch.instance)? {
        if name != "refs/stash" {
            tips.insert(value);
        }
    }
    let head = meta.get("head")?.to_owned();
    if !reachable_from(&scratch.repo, &head, &tips) {
        return Ok(None);
    }
    let wants: Vec<Want> = sides
        .into_iter()
        .filter(|(path, _)| !paths.contains(path))
        .map(|(path, side)| Want::seat(path, side))
        .collect();
    // A path no reconcile may name (a `.git` component): the beside
    // landing decides it, as before.
    if !files::all_plain(&wants) {
        return Ok(None);
    }
    Ok(Some(Clean {
        scratch,
        head,
        tracked,
        wants,
    }))
}

/// Reconcile `clean`'s untracked and ignored entries into `target`'s live
/// checkout (module docs), under the destination lock, recorded at
/// `record` (STATE) write-ahead. No tree lands beside.
///
/// # Errors
/// `GIT_BESIDE_FOREIGN` naming the first `<path>SUFFIX` that holds
/// something else (every other entry in; the record says which);
/// `GIT_DESTINATION_MODIFIED` when the live tree moved under the pass;
/// whatever a read, a write or the base import refuses.
pub fn land(
    clean: &Clean,
    target: &Target<'_>,
    landing: &Landing<'_>,
    (record, suffix): (&Path, &str),
) -> Result<Applied> {
    let workspace = target
        .workspace
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let git_dir =
        landed::worktree_git_dir(workspace)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    let common = landed::common_dir(&git_dir)?;
    // The bytes come from the scratch repository; the refs-in that follows
    // imports the capture (and a deferred plan base) into the live one.
    let _lock = landed::lock(&common, target.source)?;
    files::settle(record)?;
    let salt = record.as_os_str().as_bytes();
    let mut objects = BatchObjects::new(&clean.scratch.repo)?;
    let tracked = files::index_paths(workspace)?;
    let planned = files::plan(
        workspace,
        suffix,
        (&clean.wants, &tracked),
        Some(&mut objects),
    )?;
    let mut written = FilesRecord {
        item: landing.item.to_owned(),
        capture: landing.capture,
        live: workspace.as_os_str().as_bytes().to_vec(),
        suffix: suffix.to_owned(),
        tracked: clean.tracked,
        temps: files::temps(workspace, &planned, salt),
        dirs: files::dirs(workspace, &planned, &clean.wants)?,
        done: false,
        tally: files::Tally::default(),
        besides: planned.besides(&clean.wants),
        occupied: planned.occupied(),
    };
    landed::write(record, &written)?;
    let tally = files::execute(workspace, &planned, &clean.wants, salt, Some(&mut objects))?;
    objects.finish()?;
    written.tally = tally;
    written.done = true;
    landed::write(record, &written)?;
    if let Some(first) = written.occupied.first() {
        return Err(BulkloadRefusal::GitBesideForeign(
            workspace
                .join(OsStr::from_bytes(first))
                .as_os_str()
                .as_bytes()
                .to_vec(),
        ));
    }
    Ok(Applied {
        converged: false,
        custody: Some(format!(
            "beside-tree=none files-tracked={} {}",
            clean.tracked,
            tally.words()
        )),
        advanced: false,
    })
}
