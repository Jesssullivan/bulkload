//! Claude Code's half of the re-seat (R0, TIN-6587).
//!
//! Per landed session `projects/<srckey>/<sid>.jsonl`:
//!
//! 1. Its lane comes from the transcript's first top-level `cwd` (never from
//!    the key, which is lossy), checked against `project_key(cwd) ==
//!    srckey`; a session row caches it, so an unchanged session reads 0
//!    bytes.
//! 2. The transcript goes to `projects/<dest key>/<sid>.jsonl` in the live
//!    destination state, with its sidecar directory `<sid>/` beside it
//!    (subagent transcripts, workflows, tool results), its global
//!    per-session state (`file-history/<sid>/`, `tasks/<sid>/`,
//!    `session-env/<sid>/`, `debug/<sid>.txt`, `todos/<sid>-*`) and its
//!    plans (`plans/<slug>.md`, `plans/<slug>-agent-*.md`, by the
//!    transcript's `slug`), and the source key's `memory/` merges into the
//!    destination key's.
//! 3. Never carried: `sessions/` (the host's live-process registry) and
//!    `shell-snapshots/`, which are host-local. `history.jsonl` (prompt
//!    history) and `~/.claude.json` are not read by the picker and stay in
//!    the landing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use super::json::FieldScanner;
use super::ledger::{Identity, Ledger, Session};
use super::place::Placer;
use super::{key, shown, Kind, Lane, Lanes, Options, Summary, Unmapped, SCHEMA};
use crate::closure::json_string;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The tool name in ledger rows and report lines.
pub const TOOL: &str = "claude";
const CWD: &[&str] = &["cwd"];
const SLUG: &[&str] = &["slug"];
/// The deepest sidecar tree walked.
const DEPTH_LIMIT: usize = 64;
/// Claude Code's default `cleanupPeriodDays`.
const DEFAULT_RETENTION_DAYS: i64 = 30;
const DAY_NS: i64 = 86_400 * 1_000_000_000;

/// One landed transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The key directory it was landed under (the source's key).
    pub key: String,
    pub sid: String,
    pub path: PathBuf,
    pub mtime: i64,
}

/// The landed sessions, found by `lstat` alone.
pub struct Sessions {
    pub list: Vec<Candidate>,
    todos: Vec<String>,
    plans: Vec<String>,
}

fn names(directory: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).refuse_at("reseat::claude::names"),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.refuse_at("reseat::claude::names")?;
        if let Some(name) = entry.file_name().to_str() {
            out.push(name.to_owned());
        }
    }
    out.sort();
    Ok(out)
}

impl Sessions {
    /// Every `projects/*/*.jsonl` of the landed `.claude`.
    ///
    /// # Errors
    /// The I/O refusal of a listing.
    pub fn scan(options: &Options) -> Result<Self> {
        let projects = options.landing_claude.join("projects");
        let mut list = Vec::new();
        for key in names(&projects)? {
            let directory = projects.join(&key);
            // A key directory the landing holds as a symlink is listed
            // through it; each transcript is still checked with `lstat`.
            if !std::fs::metadata(&directory).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            for name in names(&directory)? {
                let Some(sid) = name.strip_suffix(".jsonl") else {
                    continue;
                };
                if sid.is_empty() || sid.starts_with('.') {
                    continue;
                }
                let path = directory.join(&name);
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                list.push(Candidate {
                    key: key.clone(),
                    sid: sid.to_owned(),
                    path,
                    mtime: Identity::of(&meta).mtime,
                });
            }
        }
        Ok(Self {
            list,
            todos: names(&options.landing_claude.join("todos"))?,
            plans: names(&options.landing_claude.join("plans"))?,
        })
    }
}

/// The destination's `cleanupPeriodDays` (`settings.json`, top level;
/// Claude Code's default 30 when absent).
fn retention_days(dest_claude: &Path) -> Result<i64> {
    let path = dest_claude.join("settings.json");
    let text = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_RETENTION_DAYS)
        }
        Err(error) => return Err(error).refuse_at("reseat::claude::retention"),
    };
    let text = String::from_utf8_lossy(&text);
    let Some(at) = text.find("\"cleanupPeriodDays\"") else {
        return Ok(DEFAULT_RETENTION_DAYS);
    };
    let rest = text
        .get(at + "\"cleanupPeriodDays\"".len()..)
        .unwrap_or_default()
        .trim_start()
        .strip_prefix(':')
        .unwrap_or_default()
        .trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Ok(digits.parse().unwrap_or(DEFAULT_RETENTION_DAYS))
}

/// Whether a transcript with modification time `mtime` survives the
/// destination's retention at `now`.
const fn retained(days: i64, mtime: i64, now: i64) -> bool {
    days > 0 && now.saturating_sub(mtime) < days.saturating_mul(DAY_NS)
}

/// Refuse before anything is written when the destination's Claude Code
/// would delete a re-seated transcript at its next start: they keep their
/// source modification times.
///
/// # Errors
/// `RESEAT_RETENTION_UNSAFE`; the I/O refusal of reading the settings.
pub fn retention(options: &Options, sessions: &Sessions) -> Result<()> {
    let days = retention_days(&options.dest_claude)?;
    if sessions
        .list
        .iter()
        .all(|candidate| retained(days, candidate.mtime, options.now_ns))
    {
        Ok(())
    } else {
        Err(BulkloadRefusal::ReseatRetentionUnsafe)
    }
}

/// How a session's lane resolved.
pub enum Resolved {
    /// To a lane.
    Lane(String, Lane),
    /// To no lane, kept under its source key (`--unmapped=by-id`).
    ById(String),
    /// To a refusal.
    Refused(String, BulkloadRefusal),
}

/// The transcript's first top-level `cwd`, reading only until it is found.
fn first_cwd(path: &Path) -> Result<Option<String>> {
    let mut file = crate::hash::open_nofollow(path)?;
    let mut scanner = FieldScanner::new(CWD);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .refuse_at("reseat::claude::first_cwd")?;
        if read == 0 {
            return Ok(None);
        }
        counters::add_len(Counter::ReseatLandingRead, read);
        scanner.feed(buffer.get(..read).unwrap_or_default());
        if let Some(cwd) = scanner.get(0) {
            return Ok(Some(cwd.to_owned()));
        }
    }
}

/// Resolve `candidate`'s lane, from its session row when it has one.
///
/// # Errors
/// I/O refusals of the scan; a destination path that cannot resolve.
pub fn resolve(
    candidate: &Candidate,
    cached: Option<&Session>,
    lanes: &Lanes,
    unmapped: Unmapped,
) -> Result<Resolved> {
    let cwd = match cached.and_then(|row| row.source_cwd.clone()) {
        Some(cwd) => Some(cwd),
        None => first_cwd(&candidate.path)?,
    };
    let Some(cwd) = cwd else {
        return Ok(Resolved::Refused(
            String::new(),
            BulkloadRefusal::ReseatLaneUnresolved,
        ));
    };
    if key::project_key(&cwd) != candidate.key {
        return Ok(Resolved::Refused(
            cwd,
            BulkloadRefusal::ReseatLaneUnresolved,
        ));
    }
    match lanes.lane(Path::new(&cwd))? {
        Some(lane) => Ok(Resolved::Lane(cwd, lane)),
        None if unmapped == Unmapped::ById => Ok(Resolved::ById(cwd)),
        None => Ok(Resolved::Refused(cwd, BulkloadRefusal::ReseatLaneUnmapped)),
    }
}

/// Every regular file (and every other node, to report) under `root`, as
/// paths relative to it, sorted.
pub fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    while let Some((rel, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(root.join(&rel)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).refuse_at("reseat::claude::walk"),
        };
        for entry in entries {
            let entry = entry.refuse_at("reseat::claude::walk")?;
            let child = rel.join(entry.file_name());
            let kind = entry.file_type().refuse_at("reseat::claude::walk")?;
            if kind.is_dir() {
                if depth >= DEPTH_LIMIT {
                    return Err(BulkloadRefusal::PathDepthExceeded);
                }
                pending.push((child, depth + 1));
            } else {
                out.push(child);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The landed files that belong to a session beside its transcript, as
/// `(landed path, destination path relative to the destination .claude,
/// kind)`.
pub fn companions(
    options: &Options,
    sessions: &Sessions,
    candidate: &Candidate,
    dest_key: &str,
    slug: Option<&str>,
) -> Result<Vec<(PathBuf, PathBuf, Kind)>> {
    let landing = &options.landing_claude;
    let sid = &candidate.sid;
    let mut out = Vec::new();
    let sidecar = landing.join("projects").join(&candidate.key).join(sid);
    for rel in walk(&sidecar)? {
        out.push((
            sidecar.join(&rel),
            Path::new("projects").join(dest_key).join(sid).join(rel),
            Kind::Sidecar,
        ));
    }
    for tree in ["file-history", "tasks", "session-env"] {
        let base = landing.join(tree).join(sid);
        for rel in walk(&base)? {
            out.push((
                base.join(&rel),
                Path::new(tree).join(sid).join(rel),
                Kind::Global,
            ));
        }
    }
    let debug = format!("{sid}.txt");
    if landing.join("debug").join(&debug).exists() {
        out.push((
            landing.join("debug").join(&debug),
            Path::new("debug").join(&debug),
            Kind::Global,
        ));
    }
    let prefix = format!("{sid}-");
    for name in sessions
        .todos
        .iter()
        .filter(|name| name.starts_with(&prefix))
    {
        out.push((
            landing.join("todos").join(name),
            Path::new("todos").join(name),
            Kind::Global,
        ));
    }
    if let Some(slug) = slug.filter(|slug| !slug.is_empty() && !slug.contains('/')) {
        let exact = format!("{slug}.md");
        let agent = format!("{slug}-agent-");
        for name in sessions.plans.iter().filter(|name| {
            **name == exact
                || (name.starts_with(&agent)
                    && Path::new(name.as_str())
                        .extension()
                        .is_some_and(|extension| extension == "md"))
        }) {
            out.push((
                landing.join("plans").join(name),
                Path::new("plans").join(name),
                Kind::Plan,
            ));
        }
    }
    Ok(out)
}

/// Re-seat every landed Claude session.
///
/// # Errors
/// Ledger and I/O refusals; a session's own refusal is reported.
pub fn reseat(
    options: &Options,
    lanes: &Lanes,
    ledger: &Ledger,
    sessions: &Sessions,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let root = super::root(&options.dest_claude)?;
    let placer = Placer {
        ledger,
        root: &root,
    };
    let mut memories: BTreeSet<(String, String)> = BTreeSet::new();
    for candidate in &sessions.list {
        let cached = ledger
            .session(TOOL, &candidate.sid)?
            .filter(|row| row.landing == candidate.path);
        let (cwd, lane, dest_key) =
            match resolve(candidate, cached.as_ref(), lanes, options.unmapped)? {
                Resolved::Lane(cwd, lane) => {
                    let key = lane.key.clone();
                    (cwd, Some(lane), key)
                }
                Resolved::ById(cwd) => (cwd, None, candidate.key.clone()),
                Resolved::Refused(cwd, refusal) => {
                    summary.session(TOOL, refusal.code());
                    emit(&session_line(
                        candidate,
                        &cwd,
                        (None, None, None),
                        "refused",
                        Some(&refusal),
                        &BTreeMap::new(),
                    ))?;
                    continue;
                }
            };
        let rel = Path::new("projects")
            .join(&dest_key)
            .join(format!("{}.jsonl", candidate.sid));
        let slug_known = cached.as_ref().and_then(|row| row.slug.clone());
        // A destination checkout that moved, or became a symlink, changes
        // the key: the earlier placement stays where it is and is named.
        let previous_key = cached
            .as_ref()
            .and_then(|row| row.dest_key.clone())
            .filter(|previous| *previous != dest_key);
        let transcript = placer.place(
            &candidate.path,
            &rel,
            Kind::Transcript,
            &candidate.sid,
            slug_known.is_none().then_some(SLUG),
        )?;
        let slug = slug_known.or_else(|| {
            transcript
                .scanner
                .as_ref()
                .and_then(|scanner| scanner.get(0).map(str::to_owned))
        });
        summary.file(TOOL, transcript.outcome);
        let mut files: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut place = |landed: &Path, dest: &Path, kind: Kind, session: &str| -> Result<()> {
            let outcome = placer.place(landed, dest, kind, session, None)?.outcome;
            summary.file(TOOL, outcome);
            *files.entry(outcome.word()).or_default() += 1;
            Ok(())
        };
        for (landed, dest, kind) in
            companions(options, sessions, candidate, &dest_key, slug.as_deref())?
        {
            place(&landed, &dest, kind, &candidate.sid)?;
        }
        if memories.insert((candidate.key.clone(), dest_key.clone())) {
            let base = options
                .landing_claude
                .join("projects")
                .join(&candidate.key)
                .join("memory");
            for rel in walk(&base)? {
                let dest = Path::new("projects")
                    .join(&dest_key)
                    .join("memory")
                    .join(&rel);
                place(&base.join(&rel), &dest, Kind::Memory, "")?;
            }
        }
        let word = transcript.outcome.word();
        summary.session(TOOL, word);
        ledger.put_session(&Session {
            tool: TOOL.to_owned(),
            id: candidate.sid.clone(),
            landing: candidate.path.clone(),
            source_cwd: Some(cwd.clone()),
            dest_cwd: lane.as_ref().map(|lane| lane.dest_cwd.clone()),
            dest_key: Some(dest_key.clone()),
            slug,
            outcome: word.to_owned(),
        })?;
        emit(&session_line(
            candidate,
            &cwd,
            (lane.as_ref(), Some(&dest_key), previous_key.as_deref()),
            word,
            transcript.outcome.refusal().as_ref(),
            &files,
        ))?;
    }
    Ok(())
}

fn session_line(
    candidate: &Candidate,
    cwd: &str,
    (lane, dest_key, previous_key): (Option<&Lane>, Option<&str>, Option<&str>),
    outcome: &str,
    refusal: Option<&BulkloadRefusal>,
    files: &BTreeMap<&'static str, u64>,
) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"session\",\"tool\":\"{TOOL}\",\"id\":{},\"source_cwd\":{},\"dest_cwd\":{},\"dest_cwd_exists\":{},\"dest_key\":{},\"previous_key\":{},\"outcome\":\"{outcome}\",\"refused\":{},\"files\":{{{}}}}}",
        json_string(&candidate.sid),
        json_string(cwd),
        lane.map_or_else(|| "null".to_owned(), |lane| shown(&lane.dest_cwd)),
        lane.is_some_and(|lane| lane.exists),
        dest_key.map_or_else(|| "null".to_owned(), json_string),
        previous_key.map_or_else(|| "null".to_owned(), json_string),
        refusal.map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
        files
            .iter()
            .map(|(word, count)| format!("\"{word}\":{count}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}
