//! `estate-reseat`: every agent lane continues on the destination (R0,
//! TIN-6587).
//!
//! After a run, the source's `~/.claude` and `~/.codex` sit in the run's
//! landing on the destination host, byte for byte, where neither agent
//! looks. This verb re-seats them into the destination's **live** agent
//! state, so `claude --resume` / `--continue` and `codex resume` started in a
//! lane's destination working directory list and resume its sessions, with
//! transcripts byte-identical to the landed ones.
//!
//! The facts it rests on were measured against the real binaries (Claude
//! Code 2.1.299, codex-cli 0.163.0-alpha.11) in throwaway homes, recorded in
//! `docs/agent-notes/2026-10-16-reseat-investigation.md`:
//!
//! - Claude Code lists `projects/<key>/*.jsonl` **regular files** of the key
//!   of the canonical working directory ([`key::project_key`]); it keeps no
//!   index, ignores the recorded `cwd`, and never lists a symlinked
//!   transcript. `--continue` takes the newest by modification time.
//! - Codex lists rows of `state_5.sqlite`'s `threads` whose `cwd` equals the
//!   canonical working directory; the table is filled from rollouts once,
//!   so a rollout added later is listed only once its row is there.
//! - Opening a session appends to it on either agent, so a destination
//!   resume ends sync for that session.
//!
//! Inputs: the source home (to map lanes), the landed homes (the run's
//! copy; the source is never read, R25), the destination home, and the
//! run's private state (the ledger). A lane maps a source working directory
//! to a destination one: `--map SRC=DST` (longest prefix first), the plan's
//! items with `--lanes-from-plan`, else the same path under the home's
//! `git` directory, resolved to its real path on the destination.
//!
//! Never overwrite: see [`place`]. Every placement is ledgered
//! ([`ledger`]), so an unchanged rerun reads 0 landed bytes (S3) and a
//! later pass extends what grew. `--check` is the read-only R0 acceptance
//! check ([`check`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::closure::json_string;
use crate::{BulkloadRefusal, Result};

pub mod check;
mod claude;
mod codex;
pub mod json;
pub mod key;
pub mod ledger;
pub mod place;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]
mod tests;

pub use check::{check, CheckSummary};

/// The schema of every `estate-reseat` line.
pub const SCHEMA: &str = "bulkload.estate-reseat.v1";

/// What kind of file a placement row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Claude session transcript, `projects/<key>/<sid>.jsonl`.
    Transcript,
    /// A file beside a transcript, under `projects/<key>/<sid>/`.
    Sidecar,
    /// Per-session global state: `file-history`, `tasks`, `session-env`,
    /// `debug`, `todos`.
    Global,
    /// A plan, `plans/<slug>.md`.
    Plan,
    /// A project memory note, `projects/<key>/memory/`.
    Memory,
    /// A Codex rollout.
    Rollout,
}

impl Kind {
    /// The ledger word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Sidecar => "sidecar",
            Self::Global => "global",
            Self::Plan => "plan",
            Self::Memory => "memory",
            Self::Rollout => "rollout",
        }
    }

    /// The kind a ledger word names.
    ///
    /// # Errors
    /// `SCHEMA_MISMATCH` for a word this build does not write.
    pub fn parse(word: &str) -> Result<Self> {
        Ok(match word {
            "transcript" => Self::Transcript,
            "sidecar" => Self::Sidecar,
            "global" => Self::Global,
            "plan" => Self::Plan,
            "memory" => Self::Memory,
            "rollout" => Self::Rollout,
            _ => return Err(BulkloadRefusal::SchemaMismatch),
        })
    }
}

/// What happens to a session whose working directory has no lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unmapped {
    /// Reported; it stays in the landing (the default).
    #[default]
    Report,
    /// Placed under its source key (Claude) or its source `cwd` (Codex):
    /// resumable by ID only (`claude --resume <id>`, `codex resume <id>`),
    /// listed by no destination directory's picker.
    ById,
}

/// The Git half of the R0 check: `estate-diff`'s inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCheck {
    pub plan: PathBuf,
    pub corpus: PathBuf,
    pub source: String,
    pub states: Vec<PathBuf>,
}

/// One `estate-reseat` run's inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The source host's home (`/Users/jess`): lanes map from paths under it.
    pub source_home: PathBuf,
    /// The landed `.claude` (the run's copy of the source's).
    pub landing_claude: PathBuf,
    /// The landed `.codex`, holding the landed `state_5.sqlite` snapshot.
    pub landing_codex: PathBuf,
    /// The destination home: its `git` directory is the default lane root.
    pub dest_home: PathBuf,
    /// The destination's live `.claude`.
    pub dest_claude: PathBuf,
    /// The destination's live `.codex`.
    pub dest_codex: PathBuf,
    /// The run's private state: the ledger lives here.
    pub state: PathBuf,
    /// Source roots a Codex `rollout_path` is recorded under; default the
    /// source home's `.codex`.
    pub codex_source_roots: Vec<PathBuf>,
    /// Explicit lanes, `SRC=DST`.
    pub maps: Vec<(PathBuf, PathBuf)>,
    pub unmapped: Unmapped,
    pub claude: bool,
    pub codex: bool,
    /// `--check` only: the Git half.
    pub git: Option<GitCheck>,
    /// `--check` only: the hand-off note marker (TIN-6569) a lane's
    /// `AGENTS.md` or `CLAUDE.md` must hold.
    pub handoff_marker: Option<String>,
    /// `--check` only: Git and the hand-off are out of this check's scope.
    pub sessions_only: bool,
    /// Now, nanoseconds since the epoch (retention).
    pub now_ns: i64,
}

impl Options {
    /// The defaults for the three homes: landed and destination `.claude`
    /// and `.codex` under them, the source's `.codex` as its rollout root.
    #[must_use]
    pub fn new(source_home: &Path, landing_home: &Path, dest_home: &Path, state: &Path) -> Self {
        Self {
            source_home: source_home.to_path_buf(),
            landing_claude: landing_home.join(".claude"),
            landing_codex: landing_home.join(".codex"),
            dest_home: dest_home.to_path_buf(),
            dest_claude: dest_home.join(".claude"),
            dest_codex: dest_home.join(".codex"),
            state: state.to_path_buf(),
            codex_source_roots: vec![source_home.join(".codex")],
            maps: Vec::new(),
            unmapped: Unmapped::Report,
            claude: true,
            codex: true,
            git: None,
            handoff_marker: None,
            sessions_only: false,
            now_ns: now_ns(),
        }
    }
}

/// Now, nanoseconds since the epoch.
#[must_use]
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// A lane: where a source working directory continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    /// The destination working directory, resolved to its real path.
    pub dest_cwd: PathBuf,
    /// Whether it exists now.
    pub exists: bool,
    /// Claude Code's project key there.
    pub key: String,
}

/// The lane map of one run.
pub struct Lanes {
    maps: Vec<(PathBuf, PathBuf)>,
    source_git: PathBuf,
    dest_git: PathBuf,
    resolved: std::cell::RefCell<BTreeMap<PathBuf, Option<Lane>>>,
}

impl Lanes {
    /// The lanes `options` names, longest explicit source prefix first.
    #[must_use]
    pub fn new(options: &Options) -> Self {
        let mut maps = options.maps.clone();
        maps.sort_by_key(|(source, _)| std::cmp::Reverse(source.components().count()));
        Self {
            maps,
            source_git: options.source_home.join("git"),
            dest_git: options.dest_home.join("git"),
            resolved: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    /// The destination working directory `source_cwd` maps to, before
    /// resolution.
    #[must_use]
    pub fn target(&self, source_cwd: &Path) -> Option<PathBuf> {
        for (source, dest) in &self.maps {
            if let Ok(rest) = source_cwd.strip_prefix(source) {
                return Some(join(dest, rest));
            }
        }
        source_cwd
            .strip_prefix(&self.source_git)
            .ok()
            .map(|rest| join(&self.dest_git, rest))
    }

    /// The lane of `source_cwd`, or `None` when it has none.
    ///
    /// # Errors
    /// The refusal of resolving the destination path.
    pub fn lane(&self, source_cwd: &Path) -> Result<Option<Lane>> {
        if let Some(known) = self.resolved.borrow().get(source_cwd) {
            return Ok(known.clone());
        }
        let lane = match self.target(source_cwd) {
            None => None,
            Some(target) => {
                let (dest_cwd, exists) = key::resolve_dest(&target)?;
                let key = key::project_key(key::utf8(&dest_cwd)?);
                Some(Lane {
                    dest_cwd,
                    exists,
                    key,
                })
            }
        };
        self.resolved
            .borrow_mut()
            .insert(source_cwd.to_path_buf(), lane.clone());
        Ok(lane)
    }
}

fn join(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

/// The counts of one run, by tool and outcome word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// `(tool, word)` → files.
    pub files: BTreeMap<(String, String), u64>,
    /// `(tool, word)` → sessions or threads, by their transcript's or
    /// rollout's outcome, or a lane refusal.
    pub sessions: BTreeMap<(String, String), u64>,
    /// Codex thread rows: `registered`, `adopted`, `unchanged`,
    /// `conflict`, `index-absent`.
    pub rows: BTreeMap<String, u64>,
}

impl Summary {
    fn file(&mut self, tool: &str, outcome: place::Outcome) {
        *self
            .files
            .entry((tool.to_owned(), outcome.word().to_owned()))
            .or_default() += 1;
    }

    fn session(&mut self, tool: &str, word: &str) {
        *self
            .sessions
            .entry((tool.to_owned(), word.to_owned()))
            .or_default() += 1;
    }

    /// Files of `tool` with outcome `word`.
    #[must_use]
    pub fn files_of(&self, tool: &str, word: &str) -> u64 {
        self.files
            .get(&(tool.to_owned(), word.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    /// Sessions or threads of `tool` with outcome `word`.
    #[must_use]
    pub fn sessions_of(&self, tool: &str, word: &str) -> u64 {
        self.sessions
            .get(&(tool.to_owned(), word.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    fn line(&self) -> String {
        let pairs = |map: &BTreeMap<(String, String), u64>| {
            map.iter()
                .map(|((tool, word), count)| {
                    format!("{}:{count}", json_string(&format!("{tool}/{word}")))
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"summary\",\"sessions\":{{{}}},\"files\":{{{}}},\"rows\":{{{}}}}}",
            pairs(&self.sessions),
            pairs(&self.files),
            self.rows
                .iter()
                .map(|(word, count)| format!("{}:{count}", json_string(word)))
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

/// A path for a report line (lossy only for display).
fn shown(path: &Path) -> String {
    json_string(&path.to_string_lossy())
}

/// The canonical destination root `path`.
fn root(path: &Path) -> Result<PathBuf> {
    use crate::refuse::RefuseAt as _;
    std::fs::canonicalize(path).refuse_at("reseat::root")
}

/// Re-seat the landed sessions into the destination's live agent state:
/// one JSON line per session or thread, handed to `line`, then the summary
/// line.
///
/// # Errors
/// `RESEAT_RETENTION_UNSAFE` before anything is written when the
/// destination's Claude Code would delete a re-seated transcript; ledger,
/// I/O and schema refusals. A session's own refusal is its line's
/// `refused` code, never the run's.
pub fn reseat(options: &Options, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<Summary> {
    let lanes = Lanes::new(options);
    let mut summary = Summary::default();
    let claude = if options.claude && options.landing_claude.join("projects").is_dir() {
        Some(claude::Sessions::scan(options)?)
    } else {
        None
    };
    if let Some(sessions) = &claude {
        claude::retention(options, sessions)?;
    }
    let mut ledger = ledger::Ledger::open(&options.state, options.now_ns, "reseat")?;
    if let Some(sessions) = &claude {
        claude::reseat(options, &lanes, &ledger, sessions, &mut summary, emit)?;
    }
    if options.codex && options.landing_codex.join(codex::STATE_DB).is_file() {
        codex::reseat(options, &lanes, &mut ledger, &mut summary, emit)?;
    }
    emit(&summary.line())?;
    Ok(summary)
}
