//! Companion waivers (A-20261018-111, "Keep sting's, waive it"; Linear
//! TIN-6587).
//!
//! A companion the destination already held with different bytes is kept
//! by the never-overwrite rule (`diverged`, or `dest-continued` /
//! `dest-ahead`), and `--check` then fails its session
//! (`companion-missing`: the destination file is not the landed one). The
//! operator may accept the destination's copy in place of the landed one,
//! per path: `--waive-companion DEST` on a pass records a `waiver` row
//! binding exactly that pair, the destination file's identity and BLAKE3
//! and the landed file's identity, after the placement decided it.
//!
//! - Only a kept companion is waived: its placement row is this STATE's,
//!   of a companion kind (sidecar, per-session global state, plan, memory
//!   note or other key file; never a transcript or rollout, whose session
//!   `--waive` names), in a kept state (`diverged` or `continued`), with
//!   the destination file and the landed file both as the row recorded
//!   them. Anything else is a `waive-refused` line naming why; nothing is
//!   recorded.
//! - `--check` accepts a waived companion only while the destination file
//!   and the landed file are both still the waived ones: it names each
//!   (`waived-companion` line, `companions_waived` on the summary), and a
//!   waiver whose files changed since fails the session
//!   (`companion-waiver-stale`), never silently.
//! - `--undo` marks every active waiver `undone` (a `waiver` undo line);
//!   the check then fails the session again.
//!
//! The path is judged by its real parent directory, so `~/.claude/...`
//! spelled through sting's `.claude` symlink names the same row.
//!
//! **Lanes** (A-20261018-112). A lane whose working directory is absent on
//! the destination and on the source (a worktree removed on neo after its
//! work) fails `--check` (`dest-cwd-absent`) though its transcripts are
//! carried and listed. `--waive-lane PATH --reason source-absent` on a pass
//! records a `lane_waiver` row for the lane's directory (resolved as a
//! lane is), only while it is absent on the destination (the source is
//! never read, R25: its absence is the operator's finding, named by the
//! reason). `--check` then waives that lane's directory-bound rules (the
//! directory, Git, the hand-off note) while the directory is still absent,
//! naming the waiver on the lane line; its sessions and threads are still
//! checked like any other. `--undo` revokes it.

use std::path::{Path, PathBuf};

use super::ledger::{Identity, Ledger, State, Waiver};
use super::place::lstat;
use super::{shown, Summary, SCHEMA};
use crate::closure::json_string;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The waiver state of an active row.
pub const ACTIVE: &str = "active";
/// The waiver state `--undo` leaves.
pub const UNDONE: &str = "undone";

/// The placement kinds a waiver may name.
const COMPANION_KINDS: [&str; 5] = ["sidecar", "global", "plan", "memory", "extra"];

/// Each `--waive-companion` path as the ledger spells destinations: its
/// parent's real path joined with its name, under the real destination
/// `.claude` root `root`.
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a relative path; `FIELD_DOMAIN_VIOLATION` for a
/// path whose parent does not resolve, or that is not under `root`.
pub fn normalize(paths: &[PathBuf], root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(BulkloadRefusal::FieldDomainViolation);
        };
        let parent =
            std::fs::canonicalize(parent).map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
        let dest = parent.join(name);
        if !dest.starts_with(root) || dest == root {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        if !out.contains(&dest) {
            out.push(dest);
        }
    }
    Ok(out)
}

/// One waiver's report line.
fn line(
    dest: &Path,
    row: Option<&super::ledger::Placement>,
    outcome: &str,
    why: Option<(&str, &BulkloadRefusal)>,
) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"waiver\",\"tool\":\"claude\",\"dest\":{},\"landing\":{},\"session\":{},\"kind_of\":{},\"dest_size\":{},\"landed_size\":{},\"outcome\":\"{outcome}\",\"why\":{},\"refused\":{}}}",
        shown(dest),
        row.map_or_else(|| "null".to_owned(), |row| shown(&row.landing)),
        row.map_or_else(|| "null".to_owned(), |row| json_string(&row.session)),
        row.map_or_else(|| "null".to_owned(), |row| json_string(row.kind)),
        row.map_or_else(|| "null".to_owned(), |row| row.placed_identity.size.to_string()),
        row.map_or_else(|| "null".to_owned(), |row| row.landed.size.to_string()),
        why.map_or_else(|| "null".to_owned(), |(why, _)| json_string(why)),
        why.map_or_else(
            || "null".to_owned(),
            |(_, refusal)| json_string(refusal.code())
        ),
    )
}

/// Record a waiver for each of `dests` (normalized) after the placement:
/// one `waiver` line each (`waived`, `rewaived`, `already-waived` or
/// `waive-refused`).
///
/// # Errors
/// Ledger and I/O refusals; a path's own refusal is its line.
pub fn apply(
    dests: &[PathBuf],
    ledger: &Ledger,
    now_ns: i64,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for dest in dests {
        let row = ledger.placement(dest)?;
        let refused = |why: &str, refusal: BulkloadRefusal| (why.to_owned(), refusal);
        let verdict: std::result::Result<Waiver, (String, BulkloadRefusal)> = match &row {
            None => Err(refused(
                "no-placement-row",
                BulkloadRefusal::FieldDomainViolation,
            )),
            Some(row) if !COMPANION_KINDS.contains(&row.kind) => Err(refused(
                "not-a-companion",
                BulkloadRefusal::FieldDomainViolation,
            )),
            Some(row)
                if !matches!(row.state, State::Diverged | State::Continued)
                    || row.intent.is_some() =>
            {
                Err(refused("not-kept", BulkloadRefusal::FieldDomainViolation))
            }
            Some(row) => {
                let at_dest = lstat(dest)?.filter(std::fs::Metadata::is_file);
                let landed_now = lstat(&row.landing)?.map(|meta| Identity::of(&meta));
                if at_dest.as_ref().map(Identity::of) != Some(row.placed_identity) {
                    Err(refused(
                        "dest-changed",
                        BulkloadRefusal::ReseatDestContinued,
                    ))
                } else if landed_now != Some(row.landed) {
                    Err(refused(
                        "landing-changed",
                        BulkloadRefusal::SourceChangedAfterSnapshot,
                    ))
                } else {
                    match super::regularize::read_digest(dest)? {
                        Some((identity, digest)) if identity == row.placed_identity => Ok(Waiver {
                            dest: dest.clone(),
                            landing: row.landing.clone(),
                            kind: row.kind.to_owned(),
                            session: row.session.clone(),
                            landed: row.landed,
                            dest_identity: identity,
                            digest,
                            state: ACTIVE.to_owned(),
                            at_ns: now_ns,
                        }),
                        _ => Err(refused(
                            "dest-changed",
                            BulkloadRefusal::ReseatDestContinued,
                        )),
                    }
                }
            }
        };
        let outcome = match verdict {
            Err((why, refusal)) => {
                emit(&line(
                    dest,
                    row.as_ref(),
                    "waive-refused",
                    Some((&why, &refusal)),
                ))?;
                "waive-refused"
            }
            Ok(waiver) => {
                let earlier = ledger.waiver(dest)?;
                let same = earlier.as_ref().is_some_and(|earlier| {
                    earlier.state == ACTIVE
                        && earlier.landing == waiver.landing
                        && earlier.landed == waiver.landed
                        && earlier.dest_identity == waiver.dest_identity
                        && earlier.digest == waiver.digest
                });
                let outcome = if same {
                    "already-waived"
                } else {
                    ledger.put_waiver(&waiver)?;
                    if earlier.is_some() {
                        "rewaived"
                    } else {
                        "waived"
                    }
                };
                emit(&line(dest, row.as_ref(), outcome, None))?;
                outcome
            }
        };
        *summary.rows.entry(format!("waiver-{outcome}")).or_default() += 1;
    }
    Ok(())
}

/// How a missing companion stands against the waivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Waived, and both files are still the waived ones.
    Waived,
    /// Waived, but a file changed since: the waiver does not hold.
    Stale,
    /// Not waived.
    No,
}

/// Whether the companion at `dest` (landed at `landed`) is waived now.
///
/// # Errors
/// Ledger and I/O refusals.
pub fn standing(ledger: Option<&Ledger>, dest: &Path, landed: &Path) -> Result<Standing> {
    let Some(waiver) = ledger
        .map(|ledger| ledger.waiver(dest))
        .transpose()?
        .flatten()
    else {
        return Ok(Standing::No);
    };
    if waiver.state != ACTIVE {
        return Ok(Standing::No);
    }
    let at_dest = lstat(dest)?
        .filter(std::fs::Metadata::is_file)
        .map(|meta| Identity::of(&meta));
    let landed_now = lstat(landed)?.map(|meta| Identity::of(&meta));
    if waiver.landing == landed
        && landed_now == Some(waiver.landed)
        && at_dest == Some(waiver.dest_identity)
    {
        Ok(Standing::Waived)
    } else {
        Ok(Standing::Stale)
    }
}

/// Mark every active waiver `undone` (`--undo`), one undo line each;
/// returns how many.
///
/// # Errors
/// Ledger refusals.
pub fn undo(ledger: &Ledger, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<u64> {
    let mut undone = 0;
    for mut waiver in ledger.waivers()? {
        if waiver.state != ACTIVE {
            continue;
        }
        UNDONE.clone_into(&mut waiver.state);
        ledger.put_waiver(&waiver)?;
        undone += 1;
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"waiver\",\"dest\":{},\"outcome\":\"undone\"}}",
            shown(&waiver.dest),
        ))?;
    }
    Ok(undone)
}

/// The reasons a lane may be waived for.
pub const LANE_REASONS: [&str; 1] = ["source-absent"];

/// Record a lane waiver for each of `paths` with `reason`: one
/// `lane-waiver` line each (`waived`, `already-waived`, or `waive-refused`
/// when the directory exists on the destination).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for a reason not in [`LANE_REASONS`]; path,
/// ledger and I/O refusals.
pub fn apply_lanes(
    paths: &[PathBuf],
    reason: &str,
    ledger: &Ledger,
    now_ns: i64,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !LANE_REASONS.contains(&reason) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let earlier = ledger.lane_waivers()?;
    for given in paths {
        let (dest_cwd, exists) = super::key::resolve_dest(given)?;
        let outcome = if exists {
            "waive-refused"
        } else if earlier
            .iter()
            .any(|row| row.dest_cwd == dest_cwd && row.state == ACTIVE && row.reason == reason)
        {
            "already-waived"
        } else {
            ledger.put_lane_waiver(&super::ledger::LaneWaiver {
                dest_cwd: dest_cwd.clone(),
                given: given.clone(),
                reason: reason.to_owned(),
                state: ACTIVE.to_owned(),
                at_ns: now_ns,
            })?;
            "waived"
        };
        *summary
            .rows
            .entry(format!("lane-waiver-{outcome}"))
            .or_default() += 1;
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"lane-waiver\",\"dest_cwd\":{},\"given\":{},\"reason\":{},\"outcome\":\"{outcome}\",\"why\":{},\"refused\":{}}}",
            shown(&dest_cwd),
            shown(given),
            json_string(reason),
            if exists { "\"dest-cwd-exists\"" } else { "null" },
            if exists {
                json_string(BulkloadRefusal::FieldDomainViolation.code())
            } else {
                "null".to_owned()
            },
        ))?;
    }
    Ok(())
}

/// The reason an active lane waiver gives for `dest_cwd`, while the
/// directory is still absent; `None` otherwise.
///
/// # Errors
/// Ledger refusals.
pub fn lane_standing(ledger: Option<&Ledger>, dest_cwd: &Path) -> Result<Option<String>> {
    let Some(ledger) = ledger else {
        return Ok(None);
    };
    if std::fs::symlink_metadata(dest_cwd).is_ok() {
        return Ok(None);
    }
    Ok(ledger
        .lane_waivers()?
        .into_iter()
        .find(|row| row.dest_cwd == dest_cwd && row.state == ACTIVE)
        .map(|row| row.reason))
}

/// Mark every active lane waiver `undone` (`--undo`), one undo line each;
/// returns how many.
///
/// # Errors
/// Ledger refusals.
pub fn undo_lanes(ledger: &Ledger, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<u64> {
    let mut undone = 0;
    for mut waiver in ledger.lane_waivers()? {
        if waiver.state != ACTIVE {
            continue;
        }
        UNDONE.clone_into(&mut waiver.state);
        ledger.put_lane_waiver(&waiver)?;
        undone += 1;
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"lane-waiver\",\"dest_cwd\":{},\"outcome\":\"undone\"}}",
            shown(&waiver.dest_cwd),
        ))?;
    }
    Ok(undone)
}

/// The real destination `.claude` root, for [`normalize`].
///
/// # Errors
/// The I/O refusal of resolving it.
pub fn claude_root(dest_claude: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(dest_claude).refuse_at("reseat::waiver::root")
}
