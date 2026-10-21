//! Git's three-way merge as a union (B2 part 2): a file of a Git worktree
//! both sides hold differently is merged against its version at the merge
//! base of the two sides' HEADs, with `git merge-file`'s semantics. The
//! result replaces the live file only when no hunk conflicts and the live
//! file is still the one read (no live holder changed it); a conflicting
//! hunk holds the file typed, nothing written.

use std::path::PathBuf;

use super::{Action, Live, Outcome};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The merge base a worktree's consolidation merges against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    /// The live repository (a worktree of it), whose store holds both
    /// sides' history.
    pub repo: PathBuf,
    /// The merge base commit of the two sides' HEADs.
    pub commit: String,
    /// Where the merge's inputs are written apart: the move's swept
    /// scratch directory.
    pub scratch: PathBuf,
}

/// Merge `theirs` (the source's bytes) into the live file against `base`.
///
/// # Errors
/// `HOMES_CONSOLIDATE_RACED` when a writer changed the live file; a Git
/// child's refusal; IO.
pub fn union(live: &Live<'_>, theirs: &[u8], base: &Base) -> Result<Outcome> {
    use super::config::{read_live, swap_in, Read};
    let name = crate::io::c_name(live.name).refuse_at("consolidate::merge3")?;
    let (stat, ours) = match read_live(live.dir, &name)? {
        Read::Live(stat, bytes) => (stat, bytes),
        Read::Changed | Read::Absent => return Err(BulkloadRefusal::HomesConsolidateRaced),
    };
    let held = |why: &'static str| Outcome {
        action: Action::Held(why),
        written: 0,
        recorded: 0,
    };
    if ours == theirs {
        return Ok(Outcome {
            action: Action::Adopted,
            written: 0,
            recorded: 0,
        });
    }
    let Some(original) = crate::git_carry::union::blob_at(&base.repo, &base.commit, live.rel)?
    else {
        return Ok(held("git: the merge base has no such file"));
    };
    let Some(merged) =
        crate::git_carry::union::merge_file(&base.scratch, &original, &ours, theirs)?
    else {
        return Ok(held("git: a conflicting hunk"));
    };
    if merged == ours {
        return Ok(Outcome {
            action: Action::Covered,
            written: 0,
            recorded: 0,
        });
    }
    if !swap_in(live.dir, &name, &stat, &merged, 1)? {
        return Err(BulkloadRefusal::HomesConsolidateRaced);
    }
    Ok(Outcome {
        action: Action::Consolidated,
        written: merged.len() as u64,
        recorded: 0,
    })
}
