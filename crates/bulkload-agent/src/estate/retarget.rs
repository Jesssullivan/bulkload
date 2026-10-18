//! `estate-retarget` (M2 item 5, 2026-10-16; design in `docs/design.md`,
//! "Re-pointable captures"): change where a plan's items land without
//! capturing them again.
//!
//! An item's identity is the blake3 of its `(source, repository,
//! workspace)`, so changing a destination makes a new item, and its capture
//! record (`{item}.capture` in CORPUS) is not found: the next capture
//! exports the whole repository again. But the capture itself does not
//! depend on the destination: its key is the source's, its bundle name is
//! an opaque file name the record carries, and every sidecar (`.base`,
//! `.prior`, `.drift`, `.nested`, `.parts`, `.reuse`) hangs off that bundle
//! name. So re-pointing is a rewrite of the plan and one new record per
//! moved item, naming the same bundle:
//!
//! 1. Under the plan lock, each item's `repository` and `workspace` are
//!    mapped by the longest `--map OLD=NEW` prefix that matches them
//!    component-wise; the source never changes.
//! 2. The new plan is checked as `estate-add-batch` checks one (no two
//!    workspace targets overlap, R-N114; two items that become one are
//!    refused), before anything is written.
//! 3. For each moved item with a record, `{new}.capture` is written with the
//!    old record's bytes (temporary, fsync, rename, directory fsync). A
//!    `{new}.capture` already there with the same bytes is kept (a rerun);
//!    one with other bytes refuses the whole retarget
//!    (`RECEIPT_BINDING_INVALID`): the new identity already has a capture
//!    of its own, typically an older or newer capture of the same source
//!    made under that destination before (a plan re-pointed back after a
//!    re-capture). The operator chooses: capture again under the current
//!    plan first, or set the other record aside by hand.
//!
//! Refused before anything is read: a CORPUS that is not a directory, two
//! maps with the same `OLD`, and any `NEW` at or under any `OLD` (a rerun
//! would move the items again). The plan lock serialises plan writers only:
//! do not run it beside an `estate-capture` or `estate-apply` of the plan.
//!
//! On a plan already applied: the moved items are new items, so closure
//! counts them unaccounted until they are applied at their new paths, and
//! an apply lands a new checkout there (or, for a tree moved there by
//! hand, re-keys it under `--adopt`); the old landing and its records stay
//! as they were. Like any plan edit, it changes the plan's digest, so a
//! dispositions ledger or transfer manifest bound to it refuses
//! `RECEIPT_BINDING_INVALID`.
//! 4. The plan as it was is kept beside it
//!    (`{plan}.before-retarget-{digest}`), then the new plan replaces it.
//!
//! A crash before step 4 leaves the old plan and some new records, which
//! nothing reads; the rerun finishes. Nothing is deleted: the old records
//! stay, so the kept plan still resolves. No source byte is read.
//!
//! What it does not re-key, and why: apply STATE (journals and outcome
//! records name the old item, whose destination is not the new one: the
//! new item's first apply is a first landing or, for a tree moved there by
//! hand, an `--adopt` re-key of its workspace ledger, A11b), and the
//! capture's private STATE (attempt directories are per export; the next
//! capture reuses the record and writes the new item's outcome).

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use super::{
    exclusive, id, overlapping, read, target_key, write, write_bytes, Capture, Item, Plan,
};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// One item `retarget` moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The item before, and its identity.
    pub was: Item,
    pub was_id: String,
    /// The item now, and its identity.
    pub now: Item,
    pub now_id: String,
    /// What happened to its capture record: `rekeyed` (written),
    /// `present` (already there, the same bytes), `absent` (never
    /// captured).
    pub capture: &'static str,
}

/// Map `path` by the longest `(old, new)` prefix in `maps` that matches it
/// component-wise; `None` when none does.
#[must_use]
pub fn mapped(path: &Path, maps: &[(PathBuf, PathBuf)]) -> Option<PathBuf> {
    maps.iter()
        .filter_map(|(old, new)| {
            let rest = path.strip_prefix(old).ok()?;
            Some((old.components().count(), new.join(rest)))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| {
            // `join("")` adds a trailing separator; keep the spelling exact.
            path.components().collect::<PathBuf>()
        })
}

/// Refuse maps a rerun would apply again, or that are ambiguous: two maps
/// with the same `OLD` (component-wise), and any `NEW` at or under any
/// `OLD` (`--map /d=/d/v2`, or a chain `/a=/b`, `/b=/c`), which would move
/// the items once more on every rerun.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for either.
pub fn check_maps(maps: &[(PathBuf, PathBuf)]) -> Result<()> {
    let normal = |path: &Path| path.components().collect::<PathBuf>();
    for (index, (old, _)) in maps.iter().enumerate() {
        if maps
            .iter()
            .skip(index + 1)
            .any(|(other, _)| normal(other) == normal(old))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        if maps
            .iter()
            .any(|(_, new)| normal(new).starts_with(normal(old)))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
    }
    Ok(())
}

// The capture record of `identity`, raw, or `None`.
fn record_bytes(corpus: &Path, identity: &str) -> Result<Option<Vec<u8>>> {
    const SITE: &str = "estate::retarget::record";
    let path = corpus.join(format!("{identity}.capture"));
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .refuse_at(SITE)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    // It must be a capture record: only a well-formed one is re-keyed.
    postcard::from_bytes::<Capture>(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
    Ok(Some(bytes))
}

/// Re-point `plan`'s destinations by `maps` and re-key their capture
/// records in `corpus` (module docs). Returns the items it moved, in plan
/// order; an item no map matches is left as it is.
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a relative plan or map; `GIT_DESTINATION_OCCUPIED`
/// when two workspace targets of the new plan overlap or two items become
/// one; `RECEIPT_BINDING_INVALID` when a moved item's new identity already
/// has a different capture record; IO. Nothing is written on a refusal.
pub fn retarget(plan: &Path, corpus: &Path, maps: &[(PathBuf, PathBuf)]) -> Result<Vec<Moved>> {
    if !plan.is_absolute()
        || maps
            .iter()
            .any(|(old, new)| !old.is_absolute() || !new.is_absolute())
    {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    check_maps(maps)?;
    // A CORPUS that is not there would re-key nothing and still rewrite the
    // plan, after which no rerun could find the old records.
    if !fs::metadata(corpus)
        .refuse_at("estate::retarget::corpus")?
        .is_dir()
    {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let _lock = exclusive(&plan.with_extension("lock"))?;
    let before: Plan = read(plan)?;
    let mut after = Plan::default();
    let mut moved = Vec::new();
    let mut identities = std::collections::HashSet::new();
    let mut targets: Vec<(super::TargetKey, PathBuf)> = Vec::new();
    for item in &before.items {
        let now = Item {
            source: item.source.clone(),
            repository: mapped(&item.repository, maps).unwrap_or_else(|| item.repository.clone()),
            workspace: item
                .workspace
                .as_ref()
                .map(|workspace| mapped(workspace, maps).unwrap_or_else(|| workspace.clone())),
        };
        let now_id = id(&now)?;
        if !identities.insert(now_id.clone()) {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        if let Some(target) = &now.workspace {
            let key = target_key(target)?;
            if targets
                .iter()
                .any(|(other, source): &(super::TargetKey, PathBuf)| {
                    overlapping(&key, &now.source, other, source)
                })
            {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            targets.push((key, now.source.clone()));
        }
        if now != *item {
            moved.push(Moved {
                was: item.clone(),
                was_id: id(item)?,
                now: now.clone(),
                now_id,
                capture: "absent",
            });
        }
        after.items.push(now);
    }
    // Decide every record before writing any.
    let mut writes = Vec::new();
    for row in &mut moved {
        let Some(bytes) = record_bytes(corpus, &row.was_id)? else {
            continue;
        };
        match record_bytes(corpus, &row.now_id)? {
            Some(existing) if existing == bytes => row.capture = "present",
            Some(_) => return Err(BulkloadRefusal::ReceiptBindingInvalid),
            None => {
                row.capture = "rekeyed";
                writes.push((corpus.join(format!("{}.capture", row.now_id)), bytes));
            }
        }
    }
    if moved.is_empty() {
        return Ok(moved);
    }
    for (path, bytes) in &writes {
        write_bytes(path, bytes)?;
    }
    let old = fs::read(plan).refuse_at("estate::retarget")?;
    let kept = PathBuf::from(format!(
        "{}.before-retarget-{}",
        plan.display(),
        blake3::hash(&old).to_hex().get(..16).unwrap_or_default()
    ));
    if !kept.try_exists().refuse_at("estate::retarget")? {
        write_bytes(&kept, &old)?;
    }
    write(plan, &after)?;
    Ok(moved)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn maps(pairs: &[(&str, &str)]) -> Vec<(PathBuf, PathBuf)> {
        pairs
            .iter()
            .map(|(old, new)| (PathBuf::from(old), PathBuf::from(new)))
            .collect()
    }

    #[test]
    fn maps_a_rerun_would_apply_again_are_refused() {
        assert_eq!(
            check_maps(&maps(&[("/d", "/d/v2")])),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        assert_eq!(
            check_maps(&maps(&[("/a", "/b"), ("/b", "/c")])),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        assert_eq!(
            check_maps(&maps(&[("/d", "/x"), ("/d/", "/y")])),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        assert_eq!(check_maps(&maps(&[("/a", "/b"), ("/c", "/d")])), Ok(()));
        assert_eq!(check_maps(&maps(&[("/d/v2", "/d")])), Ok(()));
    }

    #[test]
    fn the_longest_component_prefix_wins() {
        let maps = maps(&[("/a", "/x"), ("/a/b", "/y")]);
        assert_eq!(
            mapped(Path::new("/a/c"), &maps),
            Some(PathBuf::from("/x/c"))
        );
        assert_eq!(
            mapped(Path::new("/a/b/c"), &maps),
            Some(PathBuf::from("/y/c"))
        );
        assert_eq!(mapped(Path::new("/a/b"), &maps), Some(PathBuf::from("/y")));
        assert_eq!(
            mapped(Path::new("/ab/c"), &maps),
            None,
            "components, not bytes"
        );
        assert_eq!(mapped(Path::new("/z"), &maps), None);
    }
}
