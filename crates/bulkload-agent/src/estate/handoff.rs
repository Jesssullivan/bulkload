//! `estate-handoff` (Linear TIN-6569, operator ruling OI-1003-Q183,
//! 2026-10-16; design `docs/agent-notes/2026-10-16-estate-handoff-design.md`):
//! the estate side of the hand-off block. It reads the plan, each item's
//! capture record in CORPUS and outcome record in STATE, hands each landed
//! workspace to `git_carry::converge::handoff::run`, and prints one
//! [`SCHEMA`] JSON line per workspace and a summary line.
//!
//! A render or a clear takes `PRIVATE_STATE/estate.lock` exclusively, as
//! `estate-apply` does, so it never runs beside an apply; `--check` and
//! `--dry-run` write nothing, at the destination or in STATE. The source
//! is never read.

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::{
    exclusive, filename, hash_file, id, private_directory, read, retained_omissions, Capture, Plan,
};
use crate::closure::json_string;
use crate::git_carry::converge::handoff::{
    self, render::shown, Estate, Mode, Omitted, Planned, Request, Row, Session, Workspace,
};
use crate::{BulkloadRefusal, Result};

pub use handoff::SCHEMA;

/// Which of `estate-handoff`'s modes a run is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HandoffMode {
    /// Render, re-render or keep each workspace's block.
    #[default]
    Render,
    /// `--check`: read-only; the state, open rows and commit detection.
    Check,
    /// `--clear`: remove every intact block and the record (with
    /// `--clear-carried`, carried blocks too).
    Clear,
    /// `--dry-run`: print the rows and the blocks, write nothing.
    DryRun,
}

/// What one `estate-handoff` run does (design 3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffOptions {
    pub mode: HandoffMode,
    /// `--renew`: put back a block a person removed by hand.
    pub renew: bool,
    /// `--readme`: also an existing `README.md` (never created).
    pub readme: bool,
    /// `--item ID` (repeatable): only these items.
    pub items: Vec<String>,
    /// `--workspace PATH` (repeatable): only these workspaces.
    pub workspaces: Vec<PathBuf>,
    /// `--sibling LANDED_PREFIX=LIVE_PREFIX` (repeatable).
    pub siblings: Vec<(PathBuf, PathBuf)>,
    /// `--clear-carried` (with `--clear` only): also remove the carried
    /// blocks under this landing's, as a recorded edit (OI-1003-Q190).
    pub clear_carried: bool,
    /// `--as-of NS`: the block's instant, for a reproducible render.
    pub as_of: Option<i128>,
    /// `--place SOURCE_PREFIX=LIVE_PREFIX` (repeatable): where a planned
    /// workspace under `SOURCE_PREFIX` was placed (estate-move's default home
    /// map, 2026-10-21): the block goes to the placed path, never the plan's
    /// raw source path.
    pub places: Vec<(PathBuf, PathBuf)>,
}

/// The counts of one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffSummary {
    pub workspaces: u64,
    pub written: u64,
    pub unchanged: u64,
    pub cleared: u64,
    pub planned: u64,
    pub checked: u64,
    pub refused: u64,
}

/// `path` canonical when it exists, else absolute: a block names every
/// path absolutely, so its commands run from any directory.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn validate(options: &HandoffOptions, source: &str) -> Result<()> {
    if (options.renew && matches!(options.mode, HandoffMode::Check | HandoffMode::Clear))
        || (options.clear_carried && options.mode != HandoffMode::Clear)
        || !filename(source)
    {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    for (landed, live) in &options.siblings {
        if !landed.is_absolute() || !live.is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
    }
    if options.workspaces.iter().any(|path| !path.is_absolute()) {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    Ok(())
}

/// The digest of what a block says of one item's records: its current
/// capture, its omissions and its last refusal, never the records' bytes
/// (an apply rewrites an outcome record without changing what it says).
fn facts_digest(
    current: Option<&[u8; 32]>,
    omitted: Option<&Vec<Omitted>>,
    refused: Option<&str>,
) -> Result<[u8; 32]> {
    let bytes = postcard::to_allocvec(&(current, omitted, refused))
        .map_err(|_| BulkloadRefusal::FrameCodec)?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

/// Each plan item as the hand-off needs it: its canonical workspace, its
/// current capture, its omissions, its last refusal, and the digest of its
/// records (metadata for the inputs digest).
fn planned(plan: &Plan, corpus: &Path, state: &Path) -> Result<Vec<Planned>> {
    let mut items: Vec<Planned> = Vec::new();
    for item in &plan.items {
        let identity = id(item)?;
        if items.iter().any(|seen| seen.id == identity) {
            continue;
        }
        let capture_path = corpus.join(format!("{identity}.capture"));
        let outcome_path = state.join(format!("{identity}.outcome"));
        let captured = read::<Capture>(&capture_path).ok();
        let omitted = captured
            .as_ref()
            .filter(|captured| filename(&captured.bundle))
            .and_then(|captured| retained_omissions(corpus, &captured.bundle))
            .map(|roots| {
                roots
                    .into_iter()
                    .map(|root| Omitted {
                        root: root.rel_path,
                        entries: root.entries,
                    })
                    .collect()
            });
        let refused =
            crate::outcome::read_record(&outcome_path)
                .ok()
                .and_then(|record| match record.outcome {
                    crate::outcome::Outcome::Refused(refusal) => Some(refusal.code().to_owned()),
                    _ => None,
                });
        // The streamed capture (SPEC rule 2 reset, B1): its item snapshot,
        // beside the corpus, is the item's current capture.
        let current = captured.map(|captured| captured.digest).or_else(|| {
            corpus
                .parent()
                .and_then(|carry| super::streamed::recorded(carry, &identity).ok().flatten())
                .map(|snapshot| snapshot.digest())
        });
        items.push(Planned {
            records: facts_digest(current.as_ref(), omitted.as_ref(), refused.as_deref())?,
            id: identity,
            workspace: item.workspace.as_deref().map(canonical),
            current,
            omitted,
            refused,
        });
    }
    Ok(items)
}

fn line_of(item: &str, workspace: &Path, mode: &str, result: &Result<Row>) -> String {
    use std::fmt::Write as _;
    let mut text = format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"workspace\",\"item\":{},\"workspace\":{},\"mode\":\"{mode}\"",
        json_string(item),
        json_string(&shown(workspace.as_os_str().as_bytes())),
    );
    let null = || "null".to_owned();
    match result {
        Err(refusal) => {
            let _ = write!(
                text,
                ",\"outcome\":\"refused\",\"refusal\":{}",
                json_string(refusal.code())
            );
        }
        Ok(row) => {
            let _ = write!(
                text,
                ",\"outcome\":\"{}\",\"refusal\":{},\"state\":{},\"source_history\":{},\"inputs\":{},\"open\":{},\"ready\":{}",
                row.outcome,
                row.refusal
                    .as_ref()
                    .map_or_else(null, |refusal| json_string(refusal.code())),
                row.state.map_or_else(null, json_string),
                row.source_history.map_or_else(null, |seen| seen.to_string()),
                row.inputs.map_or_else(null, |inputs| json_string(
                    &hex(inputs.get(..8).unwrap_or_default())
                )),
                row.open.map_or_else(null, |open| open.to_string()),
                row.open.is_some_and(|open| open == 0),
            );
            text.push_str(",\"files\":[");
            for (index, file) in row.files.iter().enumerate() {
                let _ = write!(
                    text,
                    "{}{{\"path\":{},\"kind\":\"{}\",\"action\":\"{}\"}}",
                    if index == 0 { "" } else { "," },
                    json_string(&shown(&file.path)),
                    file.kind,
                    file.action
                );
            }
            text.push(']');
            if let Some(block) = &row.block {
                let _ = write!(text, ",\"block\":{}", json_string(&shown(block)));
            }
        }
    }
    text.push('}');
    text
}

/// Hand off PLAN's landed workspaces (design 3): one line per workspace,
/// handed to `line`, then the summary line.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` or `PATH_NOT_ABSOLUTE` for a malformed option;
/// a malformed plan; the lock held by an apply. After every line: the
/// first workspace's refusal, or `HANDOFF_MARKER_COMMITTED` when `--check`
/// found the token staged or committed.
#[allow(clippy::too_many_lines)]
pub fn handoff(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source: &str,
    options: &HandoffOptions,
    line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<HandoffSummary> {
    validate(options, source)?;
    let mode = match options.mode {
        HandoffMode::Check => Mode::Check,
        HandoffMode::Clear => Mode::Clear,
        HandoffMode::DryRun => Mode::DryRun,
        HandoffMode::Render => Mode::Render,
    };
    let mode_name = match mode {
        Mode::Check => "check",
        Mode::Clear => "clear",
        Mode::DryRun => "dry-run",
        Mode::Render => "render",
    };
    let writes = matches!(mode, Mode::Render | Mode::Clear);
    let _lock = if writes {
        private_directory(state)?;
        Some(exclusive(&state.join("estate.lock"))?)
    } else {
        None
    };
    let (plan, corpus, state) = (&canonical(plan), &canonical(corpus), &canonical(state));
    let contents: Plan = read(plan)?;
    let plan_digest = hash_file(plan)?;
    let items = planned(&contents, corpus, state)?;
    let siblings: Vec<(PathBuf, PathBuf)> = options
        .siblings
        .iter()
        .map(|(landed, live)| (canonical(landed), live.clone()))
        .collect();
    let wanted: Vec<PathBuf> = options
        .workspaces
        .iter()
        .map(|path| canonical(path))
        .collect();
    let estate = Estate {
        plan,
        plan_digest,
        corpus,
        state,
        source,
        items: &items,
    };
    let request = Request {
        mode,
        readme: options.readme,
        renew: options.renew,
        clear_carried: options.clear_carried,
        siblings: &siblings,
        as_of: options.as_of,
        h_step: false,
    };
    let mut session = Session::default();
    let mut summary = HandoffSummary::default();
    let mut first: Option<BulkloadRefusal> = None;
    let mut open_total = 0u64;
    let mut seen = Vec::new();
    for item in &contents.items {
        let Some(planned_at) = item.workspace.as_deref() else {
            continue;
        };
        let placed = options
            .places
            .iter()
            .find_map(|(from, to)| planned_at.strip_prefix(from).ok().map(|rest| to.join(rest)));
        let workspace = placed.as_deref().unwrap_or(planned_at);
        let identity = id(item)?;
        if seen.contains(&identity) {
            continue;
        }
        seen.push(identity.clone());
        if !options.items.is_empty() && !options.items.contains(&identity) {
            continue;
        }
        if !wanted.is_empty() && !wanted.contains(&canonical(workspace)) {
            continue;
        }
        let result = handoff::run(
            &Workspace {
                item: &identity,
                worktree: workspace,
                source_path: &item.source,
            },
            &request,
            &estate,
            &mut session,
        );
        summary.workspaces += 1;
        match &result {
            Ok(row) => {
                match row.outcome {
                    "written" => summary.written += 1,
                    "cleared" => summary.cleared += 1,
                    "planned" => summary.planned += 1,
                    "checked" => summary.checked += 1,
                    _ => summary.unchanged += 1,
                }
                open_total = open_total.saturating_add(row.open.unwrap_or(0));
                if let Some(refusal) = &row.refusal {
                    first.get_or_insert_with(|| refusal.clone());
                }
            }
            Err(refusal) => {
                summary.refused += 1;
                first.get_or_insert_with(|| refusal.clone());
            }
        }
        line(&line_of(&identity, workspace, mode_name, &result))?;
    }
    line(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"summary\",\"mode\":\"{mode_name}\",\"workspaces\":{},\"written\":{},\"unchanged\":{},\"cleared\":{},\"planned\":{},\"checked\":{},\"refused\":{},\"open\":{open_total}}}",
        summary.workspaces,
        summary.written,
        summary.unchanged,
        summary.cleared,
        summary.planned,
        summary.checked,
        summary.refused,
    ))?;
    first.map_or(Ok(summary), Err)
}

/// The H step (design 6.4): after `estate-apply` changed a repository, each
/// of its workspaces that holds a hand-off record has its block re-rendered
/// under the record's options and this apply's PLAN, CORPUS and STATE, so
/// a converge that replaced a target puts the block back in the same run.
/// Its result is a receipt word (`handoff=written`, `handoff=unchanged` or
/// `handoff=<CODE>`), never an item's outcome: the landing is applied and
/// the block is an annotation on it. `None` without a record.
#[must_use]
pub(super) fn after_apply(
    contents: &Plan,
    (plan, corpus, state): (&Path, &Path, &Path),
    source: &str,
    item: &super::Item,
) -> Option<String> {
    let workspace = item.workspace.as_deref()?;
    let stored = match handoff::recorded_options(workspace, source) {
        Ok(Some(stored)) => stored,
        Ok(None) => return None,
        Err(refusal) => return Some(format!("handoff={}", refusal.code())),
    };
    let run = || -> Result<&'static str> {
        let identity = id(item)?;
        let (plan, corpus, state) = (&canonical(plan), &canonical(corpus), &canonical(state));
        let items = planned(contents, corpus, state)?;
        let path = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        let siblings: Vec<(PathBuf, PathBuf)> = stored
            .siblings
            .iter()
            .map(|(landed, live)| (path(landed), path(live)))
            .collect();
        let row = handoff::run(
            &Workspace {
                item: &identity,
                worktree: workspace,
                source_path: &item.source,
            },
            &Request {
                mode: Mode::Render,
                readme: stored.readme,
                renew: false,
                clear_carried: false,
                siblings: &siblings,
                as_of: None,
                h_step: true,
            },
            &Estate {
                plan,
                plan_digest: hash_file(plan)?,
                corpus,
                state,
                source,
                items: &items,
            },
            &mut Session::default(),
        )?;
        Ok(row.outcome)
    };
    Some(match run() {
        Ok(outcome) => format!("handoff={outcome}"),
        Err(refusal) => format!("handoff={}", refusal.code()),
    })
}

/// The H step after an apply's last level: each planned workspace that
/// `changed` names (a repository the run changed, the item not refused),
/// or that holds a pending hand-off intent (one `lstat`, so a crashed H
/// step is settled even by a journal no-op apply), and that holds a record
/// gets [`after_apply`]; each result is handed to `line` as
/// `item=<id> handoff=<word>`. A no-op apply touches none (S3).
///
/// # Errors
/// A malformed plan item; a refusal of `line`.
pub(super) fn after_levels(
    contents: &Plan,
    paths: (&Path, &Path, &Path),
    source: &str,
    changed: &dyn Fn(&super::Item) -> Result<bool>,
    line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for item in &contents.items {
        let pending = item
            .workspace
            .as_deref()
            .is_some_and(|workspace| handoff::intent_pending(workspace, source));
        if !(pending || changed(item)?) {
            continue;
        }
        if let Some(word) = after_apply(contents, paths, source, item) {
            line(&format!("item={} {word}", id(item)?))?;
        }
    }
    Ok(())
}
