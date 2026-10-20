//! `estate-beside-retire`: redundant beside trees, retired by proof.
//!
//! Operator ruling 2026-10-16 21:24Z: "refs in always; a tree beside ONLY
//! where neo has uncommitted work". The beside trees earlier passes landed
//! whatever the source held are proved a subset of the live repository and
//! retired ([`crate::git_carry::beside::subset`]).
//!
//! Report only by default: one line per tree (`verdict=retirable` with its
//! proof, or `verdict=refused reason=...`) and a summary. Nothing is
//! written. With `--delete`, each retirable tree is reconciled into the
//! live checkout, its refs created, ledgered in STATE and removed, one
//! receipt line per tree (`outcome=retired` or `outcome=kept reason=...`).

// Receipt paths print escaped and quoted, as every estate receipt's do.
#![allow(clippy::unnecessary_debug_formatting)]

use std::path::PathBuf;

use crate::git_carry::beside::retire::Retired;
use crate::git_carry::beside::{self, subset};
use crate::{BulkloadRefusal, Result};

/// The default depth `--scan` looks to below a root.
pub const SCAN_DEPTH: usize = 6;

/// What one `estate-beside-retire` run is asked to do.
#[derive(Debug, Clone, Default)]
pub struct RetireOptions {
    /// Retire (reconcile, ledger, remove); report only when false.
    pub delete: bool,
    /// The beside suffix (`.neo-20261016`).
    pub suffix: String,
    /// The SOURCE label the destination lock is taken under.
    pub source: String,
    /// The private state directory the records go in.
    pub state: PathBuf,
    /// Beside trees named on the command line.
    pub paths: Vec<PathBuf>,
    /// Roots to scan for `*SUFFIX` directories.
    pub scan: Vec<PathBuf>,
    /// How deep `--scan` looks.
    pub depth: usize,
    /// `--drop-rebuildable`: entries under a rebuildable root go with the
    /// tree, counted, as a capture omits them.
    pub drop_rebuildable: bool,
}

/// What one run found and did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub candidates: u64,
    pub retirable: u64,
    pub retirable_bytes: u64,
    pub empty: u64,
    pub refused: u64,
    pub retired: u64,
    pub retired_bytes: u64,
    pub kept: u64,
    pub settled: u64,
    pub pending: u64,
    /// Trees whose proof or retirement failed with a refusal (named on
    /// their line); the run goes on.
    pub errors: u64,
}

impl Summary {
    /// The summary line.
    #[must_use]
    pub fn line(&self, delete: bool) -> String {
        format!(
            "estate-beside-retire summary mode={} candidates={} retirable={} retirable_bytes={} empty={} refused={} retired={} retired_bytes={} kept={} settled={} pending_removals={} errors={}",
            if delete { "delete" } else { "report" },
            self.candidates,
            self.retirable,
            self.retirable_bytes,
            self.empty,
            self.refused,
            self.retired,
            self.retired_bytes,
            self.kept,
            self.settled,
            self.pending,
            self.errors,
        )
    }
}

fn proof_words(proof: &subset::Proof) -> String {
    let tally = &proof.tally;
    format!(
        "shape={} head={} tracked={} present={} to-place={} to-beside={} beside-present={} dirs-present={} dirs-to-place={} dirs-beside={} rebuildable-dropped={} handoff={} refs-to-import={} prunable-worktrees={} bytes={}",
        proof.shape.word(),
        if proof.head.is_empty() { "-" } else { &proof.head },
        proof.tracked,
        tally.adopted,
        tally.placed,
        tally.beside,
        tally.beside_present,
        tally.dirs_adopted,
        tally.dirs_placed,
        tally.dirs_beside,
        proof.rebuildable,
        proof.handoff,
        proof.refs.len(),
        proof.prunable,
        proof.bytes,
    )
}

/// Run `estate-beside-retire` (module docs); each line goes to `out`.
#[allow(clippy::too_many_lines)] // The settle, then one loop over the trees.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for a suffix that is no ref component or a
/// path that is not absolute; whatever reading or retiring refuses.
pub fn run(options: &RetireOptions, out: &mut dyn FnMut(&str) -> Result<()>) -> Result<Summary> {
    let ns = beside::namespace(&options.suffix)?;
    let mut summary = Summary::default();
    if options.delete {
        crate::estate::private_directory(&options.state)?;
        for (path, done) in subset::settle_all(&options.state)? {
            summary.settled += 1;
            out(&format!(
                "estate-beside-retire beside={path:?} settled={}{}",
                done.word(),
                if done.reason().is_empty() {
                    String::new()
                } else {
                    format!(" reason={}", done.reason())
                }
            ))?;
        }
    } else {
        summary.pending = subset::pending(&options.state)?;
    }
    let mut trees: Vec<PathBuf> = Vec::new();
    for path in &options.paths {
        if !path.is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        trees.push(path.clone());
    }
    for root in &options.scan {
        if !root.is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        trees.extend(subset::scan(root, &options.suffix, options.depth)?);
    }
    trees.sort();
    trees.dedup();
    for tree in trees {
        summary.candidates += 1;
        let Some(live) = subset::live_of(&tree, &options.suffix) else {
            summary.refused += 1;
            out(&format!(
                "estate-beside-retire beside={tree:?} verdict=refused reason=not-suffixed"
            ))?;
            continue;
        };
        let rules = subset::Rules {
            suffix: &options.suffix,
            ns: &ns,
            source: &options.source,
            drop_rebuildable: options.drop_rebuildable,
        };
        // A refusal proving one tree is that tree's typed answer; the run
        // goes on to the next.
        let verdict = match subset::prove(&tree, &live, rules) {
            Ok(verdict) => verdict,
            Err(refusal) => {
                summary.errors += 1;
                out(&format!(
                    "estate-beside-retire beside={tree:?} live={live:?} verdict=refused reason=error:{}",
                    refusal.code()
                ))?;
                continue;
            }
        };
        let proof = match verdict {
            subset::Verdict::Refused(why) => {
                summary.refused += 1;
                out(&format!(
                    "estate-beside-retire beside={tree:?} live={live:?} verdict=refused reason={why}"
                ))?;
                continue;
            }
            subset::Verdict::Retirable(proof) => proof,
        };
        summary.retirable += 1;
        summary.retirable_bytes += proof.bytes;
        if proof.shape == subset::Shape::Empty {
            summary.empty += 1;
        }
        let words = proof_words(&proof);
        if !options.delete {
            out(&format!(
                "estate-beside-retire beside={tree:?} live={live:?} verdict=retirable {words}"
            ))?;
            continue;
        }
        let done = match subset::retire(
            &options.state,
            &options.source,
            (&tree, &live),
            rules,
            &proof,
        ) {
            Ok(done) => done,
            Err(refusal) => {
                summary.errors += 1;
                Retired::Kept(format!("error:{}", refusal.code()))
            }
        };
        match &done {
            Retired::Retired => {
                summary.retired += 1;
                summary.retired_bytes += proof.bytes;
            }
            Retired::Kept(_) => summary.kept += 1,
        }
        out(&format!(
            "estate-beside-retire beside={tree:?} live={live:?} verdict=retirable {words} outcome={}{}",
            done.word(),
            if done.reason().is_empty() {
                String::new()
            } else {
                format!(" reason={}", done.reason())
            },
        ))?;
    }
    out(&summary.line(options.delete))?;
    Ok(summary)
}
