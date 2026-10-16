//! `estate-reseat --check`: the R0 acceptance check (TIN-6587, SPEC R0),
//! read-only everywhere.
//!
//! For every lane (a destination working directory some landed session or
//! thread maps to), from the landing, not from the ledger (a session no
//! pass placed fails):
//!
//! - **Claude, as the picker reads it** (facts 1.1, 1.2):
//!   `projects/<project_key(realpath(dest_cwd))>/<sid>.jsonl` is a regular
//!   file that holds the landed transcript's whole lines: exactly
//!   (`pass`), or as a byte prefix of more the destination wrote after it
//!   (`continued`: a session resumed there, which also passes). One that
//!   ends inside a line fails (`ends-mid-line`: a resume would append into
//!   that record). Every landed sidecar, per-session global and plan file
//!   is a regular file at its place (of the same size unless the session
//!   continued).
//! - **Codex, as the picker reads it** (fact 2.2): the destination's
//!   `threads` row has `cwd == realpath(dest_cwd)` and a `rollout_path`
//!   (the plain `.jsonl` re-seat placed, or the `.jsonl.zst` Codex
//!   compressed it into there) whose decompressed stream holds the landed
//!   one the same way. A landed rollout with no snapshot row fails
//!   (`row-absent`) unless a row is there; a row Codex wrote with the
//!   source's `cwd` (a by-ID resume that chose the session directory) is
//!   `row-cwd-is-source`. Another landed form of a thread's rollout that
//!   the destination does not hold (a longer `.jsonl.zst` beside the plain
//!   form the snapshot names) fails it (`extra-form-unplaced`). A thread
//!   whose rollout is recorded outside every `--codex-source-root` is
//!   unlaned `RESEAT_LANE_UNMAPPED:rollout-root`, the flag that fixes it.
//! - **Git**: `estate-diff` classes the lane's planned workspace
//!   `identical` (`--plan`), or Git is out of this check's scope
//!   (`--sessions-only`); with neither the lane is `incomplete`.
//! - **Hand-off** (TIN-6569): with `--note-marker`, the lane's
//!   `AGENTS.md` or `CLAUDE.md` holds the marker; without it the note is
//!   not required (TIN-6569 is not merged yet).
//! - **Retention**: the destination's `cleanupPeriodDays` keeps every
//!   landed file the margin past now.
//!
//! R0 holds only when every lane passes, retention holds, something was
//! checked, and no session or thread is left without a lane (unmapped,
//! unresolved) or failed by ID, unless `--waive` names it. Each such one
//! is a line of its own.
//!
//! The check never starts an agent in the live destination state: opening
//! a session appends to it (fact 1.3). The real-binary canaries run in
//! throwaway homes (`tests/estate_reseat.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use super::claude::{self, Resolved, Sessions};
use super::codex;
use super::json::FieldScanner;
use super::ledger::Ledger;
use super::place::{self, Landing};
use super::{key, shown, Lanes, Options, SCHEMA};
use crate::closure::json_string;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The most failures a lane line names.
const NAMED_FAILURES: usize = 16;
/// How much of `AGENTS.md` / `CLAUDE.md` is searched for the marker.
const HANDOFF_BYTES: u64 = 256 * 1024;
/// Bytes per comparison read.
const CHUNK: usize = 1024 * 1024;

/// One lane's tally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneCheck {
    pub exists: bool,
    pub sessions: u64,
    pub sessions_ok: u64,
    pub threads: u64,
    pub threads_ok: u64,
    /// Sessions and threads that hold because the destination continued
    /// them (counted in the `_ok` totals too).
    pub continued: u64,
    /// `identical`, another `estate-diff` class, `unplanned`, `waived` or
    /// `unchecked`.
    pub git: String,
    /// `present`, `absent` or `not-required`.
    pub handoff: String,
    /// `<tool>:<id>:<reason>`, the first [`NAMED_FAILURES`].
    pub failures: Vec<String>,
    /// `pass`, `fail` or `incomplete`.
    pub verdict: &'static str,
}

/// A session or thread with no lane, or kept by ID and not holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlaned {
    pub tool: &'static str,
    pub id: String,
    /// A refusal code or a check failure reason.
    pub reason: String,
    pub waived: bool,
}

/// The check's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckSummary {
    pub lanes: BTreeMap<PathBuf, LaneCheck>,
    /// Sessions and threads with no lane (unmapped or unresolved).
    pub unmapped: u64,
    /// Sessions and threads kept by ID only, and how many of them hold.
    pub by_id: u64,
    pub by_id_ok: u64,
    /// Every session or thread that holds no lane's place, by name.
    pub unlaned: Vec<Unlaned>,
    /// Sessions and threads the check looked at, in lanes or not.
    pub checked: u64,
    /// Whether the destination's retention keeps every landed file.
    pub retention_ok: bool,
}

impl CheckSummary {
    /// Lanes with `verdict`.
    #[must_use]
    pub fn count(&self, verdict: &str) -> u64 {
        self.lanes
            .values()
            .filter(|lane| lane.verdict == verdict)
            .count() as u64
    }

    /// R0 holds: something was checked, every lane passes, retention keeps
    /// every file, and nothing is left without a lane unless waived.
    ///
    /// # Errors
    /// `RESEAT_R0_FAILED` otherwise.
    pub fn verdict(&self) -> Result<()> {
        if self.retention_ok
            && self.checked > 0
            && self.lanes.values().all(|lane| lane.verdict == "pass")
            && self.unlaned.iter().all(|item| item.waived)
        {
            Ok(())
        } else {
            Err(BulkloadRefusal::ReseatR0Failed)
        }
    }
}

fn regular(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
}

/// How a destination file holds a landed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// Exactly its whole lines (or bytes).
    Exactly,
    /// Its whole lines, then more the destination wrote.
    Continued,
    /// Not: the reason.
    Not(&'static str),
}

/// Compare a destination file (decompressed when `.zst`) with a landed
/// one (likewise): the destination must start with every whole line of
/// the landed content (every byte when not JSON Lines), and, for JSON
/// Lines, end at a line's end.
fn holds(landed: &Path, dest: &Path, lines: bool) -> Result<Holds> {
    let Some(meta) = regular(dest) else {
        return Ok(Holds::Not(if std::fs::symlink_metadata(dest).is_ok() {
            "not-regular"
        } else {
            "absent"
        }));
    };
    if regular(landed).is_none() {
        return Ok(Holds::Not("landing-not-regular"));
    }
    if !place::compressed(landed) && !place::compressed(dest) && !lines {
        // Plain bytes: sizes first, then hashes.
        return Ok(
            if meta.len() != std::fs::metadata(landed).map_or(0, |meta| meta.len()) {
                Holds::Not("size-differs")
            } else if crate::hash::hash_file(dest)? != crate::hash::hash_file(landed)? {
                Holds::Not("hash-differs")
            } else {
                Holds::Exactly
            },
        );
    }
    let (landed, dest) = (Landing::open(landed)?, Landing::open(dest)?);
    match stream_holds(&landed, &dest, lines) {
        Err(BulkloadRefusal::ReseatUnreadableForm) => Ok(Holds::Not("unreadable-form")),
        other => other,
    }
}

/// Read up to `buffer.len()` bytes: fewer only at the end.
fn fill_full(reader: &mut dyn std::io::Read, buffer: &mut [u8]) -> Result<usize> {
    let mut at = 0;
    while at < buffer.len() {
        match reader.read(buffer.get_mut(at..).unwrap_or_default()) {
            Ok(0) => break,
            Ok(read) => at += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(BulkloadRefusal::ReseatUnreadableForm),
        }
    }
    counters::add_len(Counter::HashFileRead, at);
    Ok(at)
}

/// [`holds`] over two content streams read in step.
fn stream_holds(landed: &Landing, dest: &Landing, lines: bool) -> Result<Holds> {
    let mut a = landed.reader()?;
    let mut d = dest.reader()?;
    let (mut left, mut right) = (vec![0_u8; CHUNK], vec![0_u8; CHUNK]);
    let (mut len_a, mut len_d) = (0_u64, 0_u64);
    // The landed content's whole lines end here.
    let mut whole_a = 0_u64;
    let mut last_d = None;
    // Leading bytes the two hold equal.
    let mut equal = 0_u64;
    let mut diverged = false;
    let (mut a_done, mut d_done) = (false, false);
    while !(a_done && d_done) {
        let got_a = if a_done {
            0
        } else {
            fill_full(&mut a, &mut left)?
        };
        let got_d = if d_done {
            0
        } else {
            fill_full(&mut d, &mut right)?
        };
        a_done |= got_a < CHUNK;
        d_done |= got_d < CHUNK;
        let (chunk_a, chunk_d) = (
            left.get(..got_a).unwrap_or_default(),
            right.get(..got_d).unwrap_or_default(),
        );
        if let Some(at) = chunk_a.iter().rposition(|byte| *byte == b'\n') {
            whole_a = len_a + at as u64 + 1;
        }
        if let Some(byte) = chunk_d.last() {
            last_d = Some(*byte);
        }
        if !diverged && len_a == len_d {
            let common = got_a.min(got_d);
            match chunk_a
                .iter()
                .zip(chunk_d)
                .take(common)
                .position(|(x, y)| x != y)
            {
                Some(at) => {
                    diverged = true;
                    equal = len_a + at as u64;
                }
                None => equal = len_a + common as u64,
            }
        }
        len_a += got_a as u64;
        len_d += got_d as u64;
    }
    let whole = if lines { whole_a } else { len_a };
    if lines && len_d > 0 && last_d != Some(b'\n') {
        return Ok(Holds::Not("ends-mid-line"));
    }
    if equal >= whole && len_d >= whole {
        return Ok(if len_d == whole {
            Holds::Exactly
        } else {
            Holds::Continued
        });
    }
    Ok(Holds::Not(if len_d == whole {
        "hash-differs"
    } else {
        "size-differs"
    }))
}

/// Run the R0 check: one line per tool, per lane and per session or
/// thread without a lane, then the summary line.
///
/// # Errors
/// `RESEAT_LANDING_ABSENT` when a landed tree the check covers is absent;
/// I/O, ledger and snapshot refusals. A lane's failure is its line's
/// `verdict`; [`CheckSummary::verdict`] turns any into
/// `RESEAT_R0_FAILED`.
pub fn check(options: &Options, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<CheckSummary> {
    let lanes = Lanes::new(options);
    let mut summary = CheckSummary {
        retention_ok: true,
        ..CheckSummary::default()
    };
    let mut table = BTreeMap::new();
    let sessions = if options.claude {
        Some(Sessions::scan(options)?)
    } else {
        None
    };
    let codex_landed = if options.codex {
        Some(codex::landed_anything(options)?)
    } else {
        None
    };
    let ledger = Ledger::open_read_only(&options.state)?;
    if let Some(sessions) = &sessions {
        check_claude(
            options,
            &lanes,
            ledger.as_ref(),
            sessions,
            &mut summary,
            &mut table,
        )?;
    }
    if codex_landed == Some(true) {
        check_codex(options, &lanes, ledger.as_ref(), &mut summary, &mut table)?;
    }
    for (tool, state) in [
        (
            claude::TOOL,
            sessions.as_ref().map_or("skipped", |sessions| {
                if sessions.list.is_empty() {
                    "empty"
                } else {
                    "checked"
                }
            }),
        ),
        (
            codex::TOOL,
            match codex_landed {
                None => "skipped",
                Some(false) => "empty",
                Some(true) => "checked",
            },
        ),
    ] {
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"tool\",\"tool\":\"{tool}\",\"state\":\"{state}\"}}"
        ))?;
    }
    let workspaces = git_classes(options)?;
    for (dest_cwd, lane) in &mut table {
        judge(options, workspaces.as_ref(), dest_cwd, lane)?;
        emit(&lane_line(dest_cwd, lane))?;
    }
    for item in &summary.unlaned {
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"unlaned\",\"tool\":\"{}\",\"id\":{},\"reason\":{},\"waived\":{}}}",
            item.tool,
            json_string(&item.id),
            json_string(&item.reason),
            item.waived,
        ))?;
    }
    summary.lanes = table;
    emit(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"check-summary\",\"checked\":{},\"lanes\":{},\"pass\":{},\"fail\":{},\"incomplete\":{},\"unmapped\":{},\"by_id\":{},\"by_id_ok\":{},\"unlaned\":{},\"waived\":{},\"retention\":\"{}\",\"r0\":\"{}\"}}",
        summary.checked,
        summary.lanes.len(),
        summary.count("pass"),
        summary.count("fail"),
        summary.count("incomplete"),
        summary.unmapped,
        summary.by_id,
        summary.by_id_ok,
        summary.unlaned.len(),
        summary.unlaned.iter().filter(|item| item.waived).count(),
        if summary.retention_ok { "ok" } else { "unsafe" },
        if summary.verdict().is_ok() { "pass" } else { "fail" },
    ))?;
    Ok(summary)
}

/// A session or thread with no lane.
fn unlaned(
    summary: &mut CheckSummary,
    options: &Options,
    tool: &'static str,
    id: &str,
    reason: &str,
) {
    summary.checked += 1;
    summary.unmapped += 1;
    summary.unlaned.push(Unlaned {
        tool,
        id: id.to_owned(),
        reason: reason.to_owned(),
        waived: options.waived.contains(id),
    });
}

/// Record one session or thread: in its lane, or as kept by ID.
fn tally(
    summary: &mut CheckSummary,
    options: &Options,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
    lane: Option<&super::Lane>,
    (tool, id): (&'static str, &str),
    held: Holds,
) {
    summary.checked += 1;
    let failure = match held {
        Holds::Not(reason) => Some(reason),
        Holds::Exactly | Holds::Continued => None,
    };
    let Some(lane) = lane else {
        summary.by_id += 1;
        summary.by_id_ok += u64::from(failure.is_none());
        if let Some(reason) = failure {
            summary.unlaned.push(Unlaned {
                tool,
                id: id.to_owned(),
                reason: format!("by-id:{reason}"),
                waived: options.waived.contains(id),
            });
        }
        return;
    };
    let entry = table.entry(lane.dest_cwd.clone()).or_default();
    entry.exists = lane.exists;
    let ok = failure.is_none();
    if tool == claude::TOOL {
        entry.sessions += 1;
        entry.sessions_ok += u64::from(ok);
    } else {
        entry.threads += 1;
        entry.threads_ok += u64::from(ok);
    }
    entry.continued += u64::from(held == Holds::Continued);
    if let Some(reason) = failure {
        if entry.failures.len() < NAMED_FAILURES {
            entry.failures.push(format!("{tool}:{id}:{reason}"));
        }
    }
}

fn check_claude(
    options: &Options,
    lanes: &Lanes,
    ledger: Option<&Ledger>,
    sessions: &Sessions,
    summary: &mut CheckSummary,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
) -> Result<()> {
    summary.retention_ok = claude::retention(options, lanes, sessions).is_ok();
    let root = super::root(&options.dest_claude)?;
    for candidate in &sessions.list {
        let cached = match ledger {
            Some(ledger) => ledger
                .session(claude::TOOL, &candidate.sid)?
                .filter(|row| row.landing == candidate.path),
            None => None,
        };
        let (lane, key) = match claude::resolve(candidate, cached.as_ref(), lanes, options)? {
            Resolved::Lane(_, lane) => {
                let key = lane.key.clone();
                (Some(lane), key)
            }
            Resolved::ById(_) => (None, candidate.key.clone()),
            Resolved::Refused(_, refusal) => {
                unlaned(
                    summary,
                    options,
                    claude::TOOL,
                    &candidate.sid,
                    refusal.code(),
                );
                continue;
            }
        };
        let dest = root
            .join("projects")
            .join(&key)
            .join(format!("{}.jsonl", candidate.sid));
        let mut held = holds(&candidate.path, &dest, true)?;
        if !matches!(held, Holds::Not(_)) {
            let slug = match cached.as_ref().and_then(|row| row.slug.clone()) {
                Some(slug) => Some(slug),
                None => slug_of(&candidate.path)?,
            };
            for (landed, rel, _) in
                claude::companions(options, sessions, candidate, &key, slug.as_deref())?
            {
                let Some(meta) = regular(&landed) else {
                    continue;
                };
                let at = regular(&root.join(&rel));
                let present = match (held, at) {
                    (_, None) => false,
                    (Holds::Continued, Some(_)) => true,
                    (_, Some(dest)) => {
                        dest.len() == meta.len()
                            || (place::line_ruled(&rel) && dest.len() < meta.len() && {
                                holds(&landed, &root.join(&rel), true)? == Holds::Exactly
                            })
                    }
                };
                if !present {
                    held = Holds::Not("companion-missing");
                    break;
                }
            }
        }
        tally(
            summary,
            options,
            table,
            lane.as_ref(),
            (claude::TOOL, &candidate.sid),
            held,
        );
    }
    Ok(())
}

/// The first top-level `slug` in a transcript.
fn slug_of(path: &Path) -> Result<Option<String>> {
    let mut file = crate::hash::open_nofollow(path)?;
    let mut scanner = FieldScanner::new(&["slug"]);
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = file.read(&mut buffer).refuse_at("reseat::check::slug")?;
        if read == 0 {
            return Ok(None);
        }
        counters::add_len(Counter::HashFileRead, read);
        scanner.feed(buffer.get(..read).unwrap_or_default());
        if let Some(slug) = scanner.get(0) {
            return Ok(Some(slug.to_owned()));
        }
    }
}

#[allow(clippy::too_many_lines)]
fn check_codex(
    options: &Options,
    lanes: &Lanes,
    ledger: Option<&Ledger>,
    summary: &mut CheckSummary,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
) -> Result<()> {
    let root = super::root(&options.dest_codex)?;
    let dest_db = root.join(codex::STATE_DB);
    let dest = if dest_db.is_file() {
        Some(
            Connection::open_with_flags(
                &dest_db,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| BulkloadRefusal::SqliteStateChanged)?,
        )
    } else {
        None
    };
    let threads = codex::landed_threads(&options.landing_codex.join(codex::STATE_DB))?;
    let ids: BTreeSet<String> = threads.iter().map(|thread| thread.id.clone()).collect();
    let mut named = BTreeSet::new();
    // Each tallied thread: its lane (or none, by ID), expected cwd, source
    // cwd and whether it held, for the extra-form rule below.
    let mut tallied: BTreeMap<String, (Option<super::Lane>, String, String, bool)> =
        BTreeMap::new();
    for thread in &threads {
        let Some(recorded) = codex::rollout_rel(options, &thread.rollout) else {
            unlaned(
                summary,
                options,
                codex::TOOL,
                &thread.id,
                &format!(
                    "{}:rollout-root",
                    BulkloadRefusal::ReseatLaneUnmapped.code()
                ),
            );
            continue;
        };
        let rel = codex::landed_form(&options.landing_codex, &recorded);
        if let Some(rel) = &rel {
            named.insert(rel.clone());
        }
        let lane = lanes.lane(Path::new(&thread.cwd))?;
        let expected_cwd = match &lane {
            Some(lane) => key::utf8(&lane.dest_cwd)?.to_owned(),
            None if options.by_id(&thread.cwd) => thread.cwd.clone(),
            None => {
                unlaned(
                    summary,
                    options,
                    codex::TOOL,
                    &thread.id,
                    BulkloadRefusal::ReseatLaneUnmapped.code(),
                );
                continue;
            }
        };
        let held = match rel {
            None => Holds::Not("landing-absent"),
            Some(rel) => thread_holds(
                options,
                (&root, dest.as_ref()),
                &thread.id,
                &rel,
                (&expected_cwd, &thread.cwd),
            )?,
        };
        tallied.insert(
            thread.id.clone(),
            (
                lane.clone(),
                expected_cwd.clone(),
                thread.cwd.clone(),
                !matches!(held, Holds::Not(_)),
            ),
        );
        tally(
            summary,
            options,
            table,
            lane.as_ref(),
            (codex::TOOL, &thread.id),
            held,
        );
    }
    for file in codex::unindexed(options, &named)? {
        let Some(id) = file.id.clone() else {
            unlaned(
                summary,
                options,
                codex::TOOL,
                &file.rel.to_string_lossy(),
                BulkloadRefusal::ReseatUnreadableForm.code(),
            );
            continue;
        };
        if ids.contains(&id) {
            // Another landed form of a snapshot thread: the destination must
            // hold it too (a longer form never placed fails the thread).
            if let Some((lane, expected_cwd, source_cwd, true)) = tallied.get(&id).cloned() {
                if let Holds::Not(_) = thread_holds(
                    options,
                    (&root, dest.as_ref()),
                    &id,
                    &file.rel,
                    (&expected_cwd, &source_cwd),
                )? {
                    demote(
                        summary,
                        options,
                        table,
                        lane.as_ref(),
                        &id,
                        "extra-form-unplaced",
                    );
                    if let Some(entry) = tallied.get_mut(&id) {
                        entry.3 = false;
                    }
                }
            }
            continue;
        }
        let landed = options.landing_codex.join(&file.rel);
        if file
            .rel
            .extension()
            .is_some_and(|extension| extension == "gz")
        {
            unlaned(
                summary,
                options,
                codex::TOOL,
                &id,
                BulkloadRefusal::ReseatUnreadableForm.code(),
            );
            continue;
        }
        let cached = codex::cached_cwds(ledger, &[(id.clone(), landed.clone())]);
        let cwd = match cached.get(&id) {
            Some(cwd) => Some(cwd.clone()),
            None => match codex::meta_cwd(&landed) {
                Ok(cwd) => cwd,
                Err(BulkloadRefusal::ReseatUnreadableForm) => None,
                Err(refusal) => return Err(refusal),
            },
        };
        let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
            unlaned(
                summary,
                options,
                codex::TOOL,
                &id,
                BulkloadRefusal::ReseatLaneUnresolved.code(),
            );
            continue;
        };
        let lane = lanes.lane(Path::new(&cwd))?;
        let expected_cwd = match &lane {
            Some(lane) => key::utf8(&lane.dest_cwd)?.to_owned(),
            None if options.by_id(&cwd) => cwd.clone(),
            None => {
                unlaned(
                    summary,
                    options,
                    codex::TOOL,
                    &id,
                    BulkloadRefusal::ReseatLaneUnmapped.code(),
                );
                continue;
            }
        };
        let held = thread_holds(
            options,
            (&root, dest.as_ref()),
            &id,
            &file.rel,
            (&expected_cwd, &cwd),
        )?;
        tally(
            summary,
            options,
            table,
            lane.as_ref(),
            (codex::TOOL, &id),
            held,
        );
    }
    Ok(())
}

/// Turn a thread already tallied as holding into a failure (`reason`).
fn demote(
    summary: &mut CheckSummary,
    options: &Options,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
    lane: Option<&super::Lane>,
    id: &str,
    reason: &str,
) {
    let Some(lane) = lane else {
        summary.by_id_ok = summary.by_id_ok.saturating_sub(1);
        summary.unlaned.push(Unlaned {
            tool: codex::TOOL,
            id: id.to_owned(),
            reason: format!("by-id:{reason}"),
            waived: options.waived.contains(id),
        });
        return;
    };
    let entry = table.entry(lane.dest_cwd.clone()).or_default();
    entry.threads_ok = entry.threads_ok.saturating_sub(1);
    if entry.failures.len() < NAMED_FAILURES {
        entry
            .failures
            .push(format!("{}:{id}:{reason}", codex::TOOL));
    }
}

/// A lane's Git, hand-off and verdict.
pub(crate) fn judge(
    options: &Options,
    workspaces: Option<&BTreeMap<PathBuf, String>>,
    dest_cwd: &Path,
    lane: &mut LaneCheck,
) -> Result<()> {
    lane.git = if options.sessions_only {
        "waived".to_owned()
    } else {
        workspaces.map_or_else(
            || "unchecked".to_owned(),
            |classes| {
                classes
                    .iter()
                    .filter(|(workspace, _)| dest_cwd.starts_with(workspace))
                    .max_by_key(|(workspace, _)| workspace.components().count())
                    .map_or_else(|| "unplanned".to_owned(), |(_, class)| class.clone())
            },
        )
    };
    lane.handoff = match (&options.handoff_marker, options.sessions_only) {
        (Some(marker), false) => {
            if handoff_present(dest_cwd, marker)? {
                "present".to_owned()
            } else {
                "absent".to_owned()
            }
        }
        _ => "not-required".to_owned(),
    };
    if !lane.exists
        && !lane
            .failures
            .iter()
            .any(|failure| failure == "dest-cwd-absent")
    {
        // The pickers run in the directory: it must exist.
        lane.failures.insert(0, "dest-cwd-absent".to_owned());
    }
    let sessions_hold =
        lane.exists && lane.sessions_ok == lane.sessions && lane.threads_ok == lane.threads;
    lane.verdict = match lane.git.as_str() {
        _ if !sessions_hold || lane.handoff == "absent" => "fail",
        "identical" | "waived" => "pass",
        "unchecked" => "incomplete",
        _ => "fail",
    };
    Ok(())
}

/// How a thread holds at the destination.
fn thread_holds(
    options: &Options,
    (root, dest): (&Path, Option<&Connection>),
    id: &str,
    rel: &Path,
    (expected_cwd, source_cwd): (&str, &str),
) -> Result<Holds> {
    if rel.extension().is_some_and(|extension| extension == "gz") {
        return Ok(Holds::Not("unreadable-form"));
    }
    let Some(dest) = dest else {
        return Ok(Holds::Not("index-absent"));
    };
    let row: Option<(String, String)> = dest
        .query_row(
            "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    let Some((cwd, rollout)) = row else {
        return Ok(Holds::Not("row-absent"));
    };
    if cwd != expected_cwd {
        // A resume by ID that kept the session directory writes the
        // source's own cwd: say so, it is the fix's name.
        return Ok(Holds::Not(if cwd == source_cwd {
            "row-cwd-is-source"
        } else {
            "row-cwd-differs"
        }));
    }
    let placed = root.join(codex::dest_form(rel));
    let rollout = PathBuf::from(rollout);
    // The plain file re-seat placed, or the `.zst` Codex compressed it
    // into at the destination.
    let compressed_there = {
        let mut name = placed.clone().into_os_string();
        name.push(".zst");
        PathBuf::from(name)
    };
    if rollout != placed && rollout != compressed_there {
        return Ok(Holds::Not("row-rollout-differs"));
    }
    holds(&options.landing_codex.join(rel), &rollout, true)
}

/// Each planned workspace's `estate-diff` class, when `--plan` was given.
fn git_classes(options: &Options) -> Result<Option<BTreeMap<PathBuf, String>>> {
    let Some(git) = &options.git else {
        return Ok(None);
    };
    let summary = crate::estate::diff(
        &git.plan,
        &git.corpus,
        &git.source,
        &git.states,
        &crate::estate::DiffOptions::default(),
        &mut |_| Ok(()),
    )?;
    Ok(Some(summary.workspaces))
}

fn handoff_present(dest_cwd: &Path, marker: &str) -> Result<bool> {
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = dest_cwd.join(name);
        if regular(&path).is_none() {
            continue;
        }
        let mut bytes = Vec::new();
        crate::hash::open_nofollow(&path)?
            .take(HANDOFF_BYTES)
            .read_to_end(&mut bytes)
            .refuse_at("reseat::check::handoff")?;
        if String::from_utf8_lossy(&bytes).contains(marker) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lane_line(dest_cwd: &Path, lane: &LaneCheck) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"lane\",\"dest_cwd\":{},\"dest_cwd_exists\":{},\"sessions\":{},\"sessions_ok\":{},\"threads\":{},\"threads_ok\":{},\"continued\":{},\"git\":{},\"handoff\":{},\"verdict\":\"{}\",\"failures\":[{}]}}",
        shown(dest_cwd),
        lane.exists,
        lane.sessions,
        lane.sessions_ok,
        lane.threads,
        lane.threads_ok,
        lane.continued,
        json_string(&lane.git),
        json_string(&lane.handoff),
        lane.verdict,
        lane.failures
            .iter()
            .map(|failure| json_string(failure))
            .collect::<Vec<_>>()
            .join(","),
    )
}
