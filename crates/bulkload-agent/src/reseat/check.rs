//! `estate-reseat --check`: the R0 acceptance check (TIN-6587, SPEC R0),
//! read-only everywhere.
//!
//! For every lane (a destination working directory some landed session or
//! thread maps to), from the landing, not from the ledger (a session no
//! pass placed fails):
//!
//! - **Claude, as the picker reads it** (facts 1.1, 1.2):
//!   `projects/<project_key(realpath(dest_cwd))>/<sid>.jsonl` is a regular
//!   file whose BLAKE3 equals the landed transcript's (the source's last
//!   pass), and every landed sidecar, per-session global and plan file is a
//!   regular file of the same size at its place.
//! - **Codex, as the picker reads it** (fact 2.2): the destination's
//!   `threads` row has `cwd == realpath(dest_cwd)` and a `rollout_path`
//!   that is a regular file whose BLAKE3 equals the landed rollout's.
//! - **Git**: `estate-diff` classes the lane's planned workspace
//!   `identical` (`--plan`), or Git is out of this check's scope
//!   (`--sessions-only`); with neither the lane is `incomplete`.
//! - **Hand-off** (TIN-6569): with `--note-marker`, the lane's
//!   `AGENTS.md` or `CLAUDE.md` holds the marker; without it the note is
//!   not required (TIN-6569 is not merged yet).
//! - **Retention**: the destination's `cleanupPeriodDays` keeps every
//!   landed transcript.
//!
//! The check never starts an agent in the live destination state: opening
//! a session appends to it (fact 1.3). The real-binary canary runs in a
//! throwaway home (`tests/estate_reseat.rs`).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use super::claude::{self, Resolved, Sessions};
use super::codex;
use super::json::FieldScanner;
use super::ledger::Ledger;
use super::{key, shown, Lanes, Options, Unmapped, SCHEMA};
use crate::closure::json_string;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The most failures a lane line names.
const NAMED_FAILURES: usize = 16;
/// How much of `AGENTS.md` / `CLAUDE.md` is searched for the marker.
const HANDOFF_BYTES: u64 = 256 * 1024;

/// One lane's tally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneCheck {
    pub exists: bool,
    pub sessions: u64,
    pub sessions_ok: u64,
    pub threads: u64,
    pub threads_ok: u64,
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

/// The check's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckSummary {
    pub lanes: BTreeMap<PathBuf, LaneCheck>,
    /// Sessions and threads with no lane (unmapped or unresolved).
    pub unmapped: u64,
    /// Sessions and threads kept by ID only, and how many of them hold.
    pub by_id: u64,
    pub by_id_ok: u64,
    /// Whether the destination's retention keeps every landed transcript.
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

    /// R0 holds: every lane passes and retention keeps every transcript.
    ///
    /// # Errors
    /// `RESEAT_R0_FAILED` otherwise.
    pub fn verdict(&self) -> Result<()> {
        if self.retention_ok && self.lanes.values().all(|lane| lane.verdict == "pass") {
            Ok(())
        } else {
            Err(BulkloadRefusal::ReseatR0Failed)
        }
    }
}

/// BLAKE3 of a file's bytes and the first value of each `want` field seen
/// in them.
fn hash_scan(path: &Path, want: &'static [&'static str]) -> Result<([u8; 32], FieldScanner)> {
    let mut file = crate::hash::open_nofollow(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut scanner = FieldScanner::new(want);
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).refuse_at("reseat::check::hash")?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).unwrap_or_default();
        counters::add_len(Counter::HashFileRead, read);
        counters::update(&mut hasher, Counter::HashFile, chunk);
        if !scanner.complete() {
            scanner.feed(chunk);
        }
    }
    Ok((*hasher.finalize().as_bytes(), scanner))
}

fn regular(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
}

/// Why the destination copy of a landed file does not hold, or `None`.
fn same_file(landed: &Path, dest: &Path) -> Result<Option<&'static str>> {
    let Some(meta) = regular(dest) else {
        return Ok(Some(if std::fs::symlink_metadata(dest).is_ok() {
            "not-regular"
        } else {
            "absent"
        }));
    };
    let Some(landed_meta) = regular(landed) else {
        return Ok(Some("landing-not-regular"));
    };
    if meta.len() != landed_meta.len() {
        return Ok(Some("size-differs"));
    }
    if crate::hash::hash_file(dest)? != crate::hash::hash_file(landed)? {
        return Ok(Some("hash-differs"));
    }
    Ok(None)
}

/// Run the R0 check: one line per lane, then the summary line.
///
/// # Errors
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
    if options.claude && options.landing_claude.join("projects").is_dir() {
        check_claude(options, &lanes, &mut summary, &mut table)?;
    }
    if options.codex && options.landing_codex.join(codex::STATE_DB).is_file() {
        check_codex(options, &lanes, &mut summary, &mut table)?;
    }
    let workspaces = git_classes(options)?;
    for (dest_cwd, lane) in &mut table {
        judge(options, workspaces.as_ref(), dest_cwd, lane)?;
        emit(&lane_line(dest_cwd, lane))?;
    }
    summary.lanes = table;
    emit(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"check-summary\",\"lanes\":{},\"pass\":{},\"fail\":{},\"incomplete\":{},\"unmapped\":{},\"by_id\":{},\"by_id_ok\":{},\"retention\":\"{}\",\"r0\":\"{}\"}}",
        summary.lanes.len(),
        summary.count("pass"),
        summary.count("fail"),
        summary.count("incomplete"),
        summary.unmapped,
        summary.by_id,
        summary.by_id_ok,
        if summary.retention_ok { "ok" } else { "unsafe" },
        if summary.verdict().is_ok() { "pass" } else { "fail" },
    ))?;
    Ok(summary)
}

/// Record one session or thread: in its lane, or as kept by ID.
fn tally(
    summary: &mut CheckSummary,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
    lane: Option<&super::Lane>,
    (tool, id): (&str, &str),
    failure: Option<&str>,
) {
    let Some(lane) = lane else {
        summary.by_id += 1;
        summary.by_id_ok += u64::from(failure.is_none());
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
    if let Some(reason) = failure {
        if entry.failures.len() < NAMED_FAILURES {
            entry.failures.push(format!("{tool}:{id}:{reason}"));
        }
    }
}

fn check_claude(
    options: &Options,
    lanes: &Lanes,
    summary: &mut CheckSummary,
    table: &mut BTreeMap<PathBuf, LaneCheck>,
) -> Result<()> {
    let ledger = Ledger::open_read_only(&options.state)?;
    let sessions = Sessions::scan(options)?;
    summary.retention_ok = claude::retention(options, &sessions).is_ok();
    let root = super::root(&options.dest_claude)?;
    for candidate in &sessions.list {
        let cached = match &ledger {
            Some(ledger) => ledger
                .session(claude::TOOL, &candidate.sid)?
                .filter(|row| row.landing == candidate.path),
            None => None,
        };
        let (lane, key) =
            match claude::resolve(candidate, cached.as_ref(), lanes, options.unmapped)? {
                Resolved::Lane(_, lane) => {
                    let key = lane.key.clone();
                    (Some(lane), key)
                }
                Resolved::ById(_) => (None, candidate.key.clone()),
                Resolved::Refused(..) => {
                    summary.unmapped += 1;
                    continue;
                }
            };
        let dest = root
            .join("projects")
            .join(&key)
            .join(format!("{}.jsonl", candidate.sid));
        let mut failure = same_file(&candidate.path, &dest)?;
        if failure.is_none() {
            let slug = match cached.as_ref().and_then(|row| row.slug.clone()) {
                Some(slug) => Some(slug),
                None => hash_scan(&candidate.path, &["slug"])?
                    .1
                    .get(0)
                    .map(str::to_owned),
            };
            for (landed, rel, _) in
                claude::companions(options, &sessions, candidate, &key, slug.as_deref())?
            {
                let Some(meta) = regular(&landed) else {
                    continue;
                };
                if regular(&root.join(&rel)).map(|dest| dest.len()) != Some(meta.len()) {
                    failure = Some("companion-missing");
                    break;
                }
            }
        }
        tally(
            summary,
            table,
            lane.as_ref(),
            (claude::TOOL, &candidate.sid),
            failure,
        );
    }
    Ok(())
}

fn check_codex(
    options: &Options,
    lanes: &Lanes,
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
    for thread in codex::landed_threads(&options.landing_codex.join(codex::STATE_DB))? {
        let lane = lanes.lane(Path::new(&thread.cwd))?;
        let expected_cwd = match &lane {
            Some(lane) => key::utf8(&lane.dest_cwd)?.to_owned(),
            None if options.unmapped == Unmapped::ById => thread.cwd.clone(),
            None => {
                summary.unmapped += 1;
                continue;
            }
        };
        let failure = thread_failure(options, &root, dest.as_ref(), &thread, &expected_cwd)?;
        tally(
            summary,
            table,
            lane.as_ref(),
            (codex::TOOL, &thread.id),
            failure,
        );
    }
    Ok(())
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

/// Why a thread does not hold at the destination, or `None`.
fn thread_failure(
    options: &Options,
    root: &Path,
    dest: Option<&Connection>,
    thread: &codex::Thread,
    expected_cwd: &str,
) -> Result<Option<&'static str>> {
    let Some(rel) = codex::rollout_rel(options, &thread.rollout) else {
        return Ok(Some("rollout-unmapped"));
    };
    let Some(rel) = codex::landed_form(&options.landing_codex, &rel) else {
        return Ok(Some("landing-absent"));
    };
    let Some(dest) = dest else {
        return Ok(Some("index-absent"));
    };
    let row: Option<(String, String)> = dest
        .query_row(
            "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
            params![thread.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    let Some((cwd, rollout)) = row else {
        return Ok(Some("row-absent"));
    };
    if cwd != expected_cwd {
        return Ok(Some("row-cwd-differs"));
    }
    if Path::new(&rollout) != root.join(&rel) {
        return Ok(Some("row-rollout-differs"));
    }
    same_file(&options.landing_codex.join(&rel), Path::new(&rollout))
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
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"lane\",\"dest_cwd\":{},\"dest_cwd_exists\":{},\"sessions\":{},\"sessions_ok\":{},\"threads\":{},\"threads_ok\":{},\"git\":{},\"handoff\":{},\"verdict\":\"{}\",\"failures\":[{}]}}",
        shown(dest_cwd),
        lane.exists,
        lane.sessions,
        lane.sessions_ok,
        lane.threads,
        lane.threads_ok,
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
