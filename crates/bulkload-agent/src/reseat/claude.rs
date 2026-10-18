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
//! 3. Everything else under a project key that is no session's (neo's
//!    `drafts/` and `vercel-plugin/`, orphaned sidecar directories) goes to
//!    the destination key with its memory (S4, Q185: nothing is dropped);
//!    a key none of whose sessions has a lane stays in the landing, named
//!    by a `key` line with its file count.
//! 4. A session refused (no lane, unresolved) is ledgered with its recorded
//!    `cwd` (or none) and its landed identity, so a rerun does not read it
//!    again (S3).
//! 5. Never carried: `sessions/` (the host's live-process registry) and
//!    `shell-snapshots/`, which are host-local. `history.jsonl` (prompt
//!    history) and `~/.claude.json` are not read by the picker and stay in
//!    the landing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use super::json::FieldScanner;
use super::ledger::{Identity, Ledger, Session};
use super::place::{self, Placer};
use super::{key, shown, Kind, Lane, Lanes, Options, Summary, SCHEMA};
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
/// How far into a transcript the retention check looks for its `slug`.
const HEAD_BYTES: u64 = 4 * 1024 * 1024;
const DAY_NS: i64 = 86_400 * 1_000_000_000;

/// One landed transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The key directory it was landed under (the source's key).
    pub key: String,
    pub sid: String,
    pub path: PathBuf,
    pub mtime: i64,
    pub size: u64,
}

/// The landed sessions, found by `lstat` alone.
pub struct Sessions {
    pub list: Vec<Candidate>,
    /// Every landed key directory.
    pub keys: Vec<String>,
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
    /// `RESEAT_LANDING_ABSENT` when the landed `.claude` is not a directory
    /// (absent, or a symlink to a source-only volume); the I/O refusal of a
    /// listing.
    pub fn scan(options: &Options) -> Result<Self> {
        if !std::fs::metadata(&options.landing_claude).is_ok_and(|meta| meta.is_dir()) {
            return Err(BulkloadRefusal::ReseatLandingAbsent);
        }
        let projects = options.landing_claude.join("projects");
        let mut list = Vec::new();
        let mut keys = Vec::new();
        for key in names(&projects)? {
            let directory = projects.join(&key);
            // A key directory the landing holds as a symlink is listed
            // through it; each transcript is still checked with `lstat`.
            if !std::fs::metadata(&directory).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            keys.push(key.clone());
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
                    size: meta.len(),
                });
            }
        }
        Ok(Self {
            list,
            keys,
            todos: names(&options.landing_claude.join("todos"))?,
            plans: names(&options.landing_claude.join("plans"))?,
        })
    }
}

/// The `cleanupPeriodDays` values `path` sets (every occurrence; a
/// settings file is small), none when it does not exist. A value that is
/// not a positive whole number counts as 0 (unsafe).
fn retention_values(path: &Path) -> Result<Vec<i64>> {
    let text = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new())
        }
        Err(error) => return Err(error).refuse_at("reseat::claude::retention"),
    };
    let text = String::from_utf8_lossy(&text);
    let key = "\"cleanupPeriodDays\"";
    let mut values = Vec::new();
    let mut rest = text.as_ref();
    while let Some(at) = rest.find(key) {
        rest = rest.get(at + key.len()..).unwrap_or_default();
        let value = rest
            .trim_start()
            .strip_prefix(':')
            .unwrap_or_default()
            .trim_start();
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        // Anything but a positive whole number is no retention Claude Code
        // can be trusted to keep.
        values.push(digits.parse().unwrap_or(0));
    }
    Ok(values)
}

/// The settings files Claude Code merges `cleanupPeriodDays` from, as seen
/// from the destination: managed (and its drop-ins), the user's, and each
/// lane's project and local settings.
fn settings_files(options: &Options, lanes: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for managed in &options.managed_settings {
        files.push(managed.clone());
        let drop_ins = managed.with_extension("d");
        for name in names(&drop_ins)? {
            if Path::new(&name)
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(drop_ins.join(name));
            }
        }
    }
    files.push(options.dest_claude.join("settings.json"));
    files.push(options.dest_claude.join("settings.local.json"));
    for lane in lanes {
        files.push(lane.join(".claude/settings.json"));
        files.push(lane.join(".claude/settings.local.json"));
    }
    Ok(files)
}

/// The smallest `cleanupPeriodDays` any of those files sets (Claude Code's
/// effective value is one of them; the smallest is the one that is safe to
/// assume), Claude Code's default 30 when none does.
fn retention_days(options: &Options, lanes: &[PathBuf]) -> Result<i64> {
    let mut days = None;
    for file in settings_files(options, lanes)? {
        for value in retention_values(&file)? {
            days = Some(days.map_or(value, |days: i64| days.min(value)));
        }
    }
    Ok(days.unwrap_or(DEFAULT_RETENTION_DAYS))
}

/// Whether a file with modification time `mtime` is still kept `margin`
/// days after `now` under a retention of `days`.
const fn retained(days: i64, margin: i64, mtime: i64, now: i64) -> bool {
    days > 0
        && now
            .saturating_sub(mtime)
            .saturating_add(margin.saturating_mul(DAY_NS))
            < days.saturating_mul(DAY_NS)
}

/// The modification times of everything a re-seat would place for
/// `candidate` that Claude Code's cleanup ages out: the transcript, its
/// sidecar tree and its per-session state (and its plans when its slug is
/// known).
fn session_mtimes(
    options: &Options,
    sessions: &Sessions,
    candidate: &Candidate,
    slug: Option<&str>,
) -> Result<Vec<i64>> {
    let mut out = vec![candidate.mtime];
    for (landed, _, _) in companions(options, sessions, candidate, &candidate.key, slug)? {
        if let Ok(meta) = std::fs::symlink_metadata(&landed) {
            out.push(Identity::of(&meta).mtime);
        }
    }
    Ok(out)
}

/// Refuse before anything is written when the destination's Claude Code
/// would delete a re-seated file within the margin
/// (`--retention-margin-days`, default 365): they keep their source
/// modification times. The retention is the smallest any settings source
/// sets (managed, user, each lane's project and local), and every file of
/// a session counts, not only its transcript. Returns the bytes the pass
/// would write (an upper bound, for the free-space check).
///
/// # Errors
/// `RESEAT_RETENTION_UNSAFE`; the I/O refusal of reading the settings or
/// a transcript's head (its lane and slug, once, then from the ledger).
pub fn retention(options: &Options, lanes: &Lanes, sessions: &Sessions) -> Result<u64> {
    let ledger = Ledger::open_read_only(&options.state)?;
    let root = std::fs::canonicalize(&options.dest_claude).ok();
    let mut lane_dirs = Vec::new();
    let mut mtimes = Vec::new();
    let mut planned = 0_u64;
    let mut keys_done: BTreeSet<(String, String)> = BTreeSet::new();
    for candidate in &sessions.list {
        let cached = match &ledger {
            Some(ledger) => ledger
                .session(TOOL, &candidate.sid)?
                .filter(|row| row.landing == candidate.path),
            None => None,
        };
        let dest_key = match resolve(candidate, cached.as_ref(), lanes, options)? {
            Resolved::Lane(_, lane) => {
                if !lane_dirs.contains(&lane.dest_cwd) {
                    lane_dirs.push(lane.dest_cwd.clone());
                }
                Some(lane.key)
            }
            Resolved::ById(_) => Some(candidate.key.clone()),
            Resolved::Refused(..) => None,
        };
        // A session a pass already decided is not read again (S3): its
        // slug is the one it found, or none.
        let slug = match &cached {
            Some(row) => row.slug.clone(),
            None if dest_key.is_some() => head_field(&candidate.path, SLUG)?,
            None => None,
        };
        if dest_key.is_some() {
            // Only what a pass places is aged by the destination's cleanup.
            mtimes.extend(session_mtimes(
                options,
                sessions,
                candidate,
                slug.as_deref(),
            )?);
        }
        if let (Some(dest_key), Some(root)) = (dest_key, root.as_ref()) {
            let rel = Path::new("projects")
                .join(&dest_key)
                .join(format!("{}.jsonl", candidate.sid));
            let mut files = vec![(candidate.path.clone(), rel)];
            files.extend(
                companions(options, sessions, candidate, &dest_key, slug.as_deref())?
                    .into_iter()
                    .map(|(landed, rel, _)| (landed, rel)),
            );
            if keys_done.insert((candidate.key.clone(), dest_key.clone())) {
                files.extend(
                    key_files(options, sessions, &candidate.key, &dest_key)?
                        .into_iter()
                        .map(|(landed, rel, _)| (landed, rel)),
                );
            }
            for (landed, rel) in files {
                planned = planned.saturating_add(place::planned(
                    ledger.as_ref(),
                    &root.join(rel),
                    &landed,
                    &options.replace_archived,
                )?);
            }
        }
    }
    let days = retention_days(options, &lane_dirs)?;
    if mtimes
        .iter()
        .all(|mtime| retained(days, options.retention_margin_days, *mtime, options.now_ns))
    {
        Ok(planned)
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
    field(path, CWD, u64::MAX)
}

/// The first top-level `field` in the transcript's first [`HEAD_BYTES`].
fn head_field(path: &Path, wanted: &'static [&'static str]) -> Result<Option<String>> {
    field(path, wanted, HEAD_BYTES)
}

fn field(path: &Path, wanted: &'static [&'static str], limit: u64) -> Result<Option<String>> {
    let file = crate::hash::open_nofollow(path)?;
    let mut file = file.take(limit);
    let mut scanner = FieldScanner::new(wanted);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).refuse_at("reseat::claude::field")?;
        if read == 0 {
            return Ok(None);
        }
        counters::add_len(Counter::ReseatLandingRead, read);
        scanner.feed(buffer.get(..read).unwrap_or_default());
        if let Some(value) = scanner.get(0) {
            return Ok(Some(value.to_owned()));
        }
    }
}

/// Resolve `candidate`'s lane, from its session row when it has one (a
/// found `cwd` always; "none recorded" while the landed transcript is
/// unchanged).
///
/// # Errors
/// I/O refusals of the scan; a destination path that cannot resolve.
pub fn resolve(
    candidate: &Candidate,
    cached: Option<&Session>,
    lanes: &Lanes,
    options: &Options,
) -> Result<Resolved> {
    let cwd = match cached.map(|row| (row.source_cwd.clone(), row)) {
        Some((Some(cwd), _)) if !cwd.is_empty() => Some(cwd),
        Some((Some(_), row))
            if row.landed_size == Some(candidate.size)
                && row.landed_mtime == Some(candidate.mtime) =>
        {
            None
        }
        _ => first_cwd(&candidate.path)?,
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
        None if options.by_id(&cwd) => Ok(Resolved::ById(cwd)),
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

/// The landed files under project key `key` that belong to no session
/// (its `memory/`, and anything else: `drafts/`, `vercel-plugin/`, an
/// orphaned sidecar directory), as `(landed path, destination path
/// relative to the destination .claude, kind)` under `dest_key`.
///
/// # Errors
/// The I/O refusal of a listing.
pub fn key_files(
    options: &Options,
    sessions: &Sessions,
    key: &str,
    dest_key: &str,
) -> Result<Vec<(PathBuf, PathBuf, Kind)>> {
    let base = options.landing_claude.join("projects").join(key);
    let sids: BTreeSet<&str> = sessions
        .list
        .iter()
        .filter(|candidate| candidate.key == key)
        .map(|candidate| candidate.sid.as_str())
        .collect();
    let mut out = Vec::new();
    for rel in walk(&base)? {
        let mut parts = rel.components();
        let first = parts
            .next()
            .and_then(|part| part.as_os_str().to_str())
            .unwrap_or_default()
            .to_owned();
        let top_level = parts.next().is_none();
        let transcript = top_level
            && first
                .strip_suffix(".jsonl")
                .is_some_and(|sid| sids.contains(sid));
        let sidecar = !top_level && sids.contains(first.as_str());
        if transcript || sidecar {
            continue;
        }
        let kind = if first == "memory" && !top_level {
            Kind::Memory
        } else {
            Kind::Extra
        };
        out.push((
            base.join(&rel),
            Path::new("projects").join(dest_key).join(&rel),
            kind,
        ));
    }
    Ok(out)
}

/// Re-seat every landed Claude session.
///
/// # Errors
/// Ledger and I/O refusals; a session's own refusal is reported.
#[allow(clippy::too_many_lines)]
pub fn reseat(
    options: &Options,
    lanes: &Lanes,
    ledger: &Ledger,
    sessions: &Sessions,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let root = super::root(&options.dest_claude)?;
    let placer = Placer::new(ledger, &root)
        .with_archived(&options.replace_archived)
        .with_rebind(&options.rebind);
    let mut keys_done: BTreeSet<(String, String)> = BTreeSet::new();
    let mut refused_keys: BTreeMap<String, BulkloadRefusal> = BTreeMap::new();
    for candidate in &sessions.list {
        let cached = ledger
            .session(TOOL, &candidate.sid)?
            .filter(|row| row.landing == candidate.path);
        let (cwd, lane, dest_key) = match resolve(candidate, cached.as_ref(), lanes, options)? {
            Resolved::Lane(cwd, lane) => {
                let key = lane.key.clone();
                (cwd, Some(lane), key)
            }
            Resolved::ById(cwd) => (cwd, None, candidate.key.clone()),
            Resolved::Refused(cwd, refusal) => {
                // Cached, so a rerun does not scan it again (S3).
                ledger.put_session(&Session {
                    tool: TOOL.to_owned(),
                    id: candidate.sid.clone(),
                    landing: candidate.path.clone(),
                    source_cwd: Some(cwd.clone()),
                    outcome: refusal.code().to_owned(),
                    landed_size: Some(candidate.size),
                    landed_mtime: Some(candidate.mtime),
                    ..Session::default()
                })?;
                refused_keys
                    .entry(candidate.key.clone())
                    .or_insert_with(|| refusal.clone());
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
        // A transcript kept out (occupied, diverged, handed over) takes
        // none of its companions: they stay in the landing with it.
        let companions_too = transcript.outcome.landed();
        let mut place = |landed: &Path, dest: &Path, kind: Kind, session: &str| -> Result<()> {
            let outcome = placer.place(landed, dest, kind, session, None)?.outcome;
            summary.file(TOOL, outcome);
            *files.entry(outcome.word()).or_default() += 1;
            if let Some(refusal) = outcome.refusal() {
                // A kept companion or memory note is named, with its code.
                emit(&format!(
                    "{{\"schema\":\"{SCHEMA}\",\"kind\":\"file\",\"tool\":\"{TOOL}\",\"session\":{},\"kind_of\":\"{}\",\"landed\":{},\"dest\":{},\"outcome\":\"{}\",\"refused\":{}}}",
                    json_string(session),
                    kind.word(),
                    shown(landed),
                    shown(dest),
                    outcome.word(),
                    json_string(refusal.code()),
                ))?;
            }
            Ok(())
        };
        if companions_too {
            for (landed, dest, kind) in
                companions(options, sessions, candidate, &dest_key, slug.as_deref())?
            {
                place(&landed, &dest, kind, &candidate.sid)?;
            }
        }
        if keys_done.insert((candidate.key.clone(), dest_key.clone())) {
            // The key's memory and everything else under it that is no
            // session's (S4, Q185).
            for (landed, dest, kind) in key_files(options, sessions, &candidate.key, &dest_key)? {
                place(&landed, &dest, kind, "")?;
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
            landed_size: Some(candidate.size),
            landed_mtime: Some(candidate.mtime),
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
    // A key none of whose sessions has a lane keeps what is under it that
    // is no session's in the landing, named (S4).
    let carried: BTreeSet<&str> = keys_done.iter().map(|(key, _)| key.as_str()).collect();
    for key in &sessions.keys {
        if carried.contains(key.as_str()) {
            continue;
        }
        let kept = key_files(options, sessions, key, key)?;
        if kept.is_empty() {
            continue;
        }
        let refusal = refused_keys
            .get(key)
            .cloned()
            .unwrap_or(BulkloadRefusal::ReseatLaneUnresolved);
        *summary
            .files
            .entry((TOOL.to_owned(), "kept".to_owned()))
            .or_default() += kept.len() as u64;
        let first = kept
            .first()
            .map(|(landed, _, _)| landed.to_string_lossy().into_owned())
            .unwrap_or_default();
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"key\",\"tool\":\"{TOOL}\",\"key\":{},\"files\":{},\"first\":{},\"outcome\":\"kept\",\"refused\":{}}}",
            json_string(key),
            kept.len(),
            json_string(&first),
            json_string(refusal.code()),
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
