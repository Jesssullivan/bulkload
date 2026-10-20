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
//!
//! One run at a time: a pass holds an exclusive `flock` on each
//! destination agent root it writes (the `.claude` and `.codex`
//! directories themselves, so nothing is added to them) and on
//! `STATE/reseat.lock`; another run refuses `RESEAT_LOCKED` before writing
//! (re-review of `8a89f9a`). Before anything is written it checks the
//! engine's free-space floor (`--min-free-percent`,
//! `DESTINATION_SPACE_INSUFFICIENT`) against the bytes it would write on
//! each destination file system (decompressed `.zst` sizes included).
//!
//! The operator's rulings of 2026-10-16 (OI-1003-Q198, Linear TIN-6587):
//! sessions resume at the lane's `~/git/<name>` (A, the default lane);
//! gftb and `GloriousFlywheel` lanes follow sting's existing `~/git` links to
//! their estate paths (B, `realpath`); home and `/private/tmp` sessions are
//! placed resumable by ID and waived from the per-lane check (C); rowless
//! rollouts get a row from their header (D, [`codex`]); `.zst` rollouts are
//! placed decompressed (E). Existing Codex rows spelled through `~/git` get
//! only their `cwd` re-spelled ([`codex`]), and sting's symlinked
//! transcripts are regularized into regular files ([`regularize`]); both
//! are ledgered and reversed by `--undo` ([`undo`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::closure::json_string;
use crate::{BulkloadRefusal, Result};

pub mod check;
mod claude;
mod codex;
pub mod json;
pub mod key;
pub mod keymap;
pub mod ledger;
pub mod place;
pub mod regularize;
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
    /// Anything else under a project key that is no session's (`drafts/`,
    /// `vercel-plugin/`, an orphaned sidecar directory).
    Extra,
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
            Self::Extra => "extra",
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
            "extra" => Self::Extra,
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
#[allow(clippy::struct_excessive_bools)] // one per CLI switch
pub struct Options {
    /// The source host's home (`/Users/jess`): lanes map from paths under it.
    pub source_home: PathBuf,
    /// The landed `.claude` (the run's copy of the source's).
    pub landing_claude: PathBuf,
    /// The landed `.codex`, holding the landed `state_5.sqlite` snapshot.
    pub landing_codex: PathBuf,
    /// Where the landed Codex index is ([`LandingIndex`]).
    pub landing_index: LandingIndex,
    /// `--beside-suffix SUFFIX` (second review of #280, N2): the suffix the
    /// run's pull landed differing seats beside their live paths with. On a
    /// live-home landing, `<path>SUFFIX` beside a destination symlink (an
    /// archive link) is the source's copy of that session or rollout, and is
    /// re-seated as the landed one; the link is never read.
    pub beside_suffix: Option<String>,
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
    /// `--check` only: session and thread IDs whose missing lane (or
    /// failed by-ID placement) is accepted by name (`--waive`).
    pub waived: BTreeSet<String>,
    /// Days a re-seated file must still be kept past now under the
    /// destination's `cleanupPeriodDays` (`--retention-margin-days`).
    pub retention_margin_days: i64,
    /// Claude Code's managed settings files on the destination (their
    /// `.d` drop-ins are read too).
    pub managed_settings: Vec<PathBuf>,
    /// Now, nanoseconds since the epoch (retention).
    pub now_ns: i64,
    /// Working directories whose sessions are placed resumable by ID and
    /// waived from the per-lane check (OI-1003-Q198 C): exactly these.
    pub by_id_exact: Vec<PathBuf>,
    /// … and anything under these.
    pub by_id_prefixes: Vec<PathBuf>,
    /// Re-spell existing destination Codex rows whose `cwd` is a
    /// non-canonical spelling of their lane (OI-1003-Q198 (1);
    /// `--no-respell` turns it off).
    pub respell_cwd: bool,
    /// Regularize the destination's symlinked transcripts (OI-1003-Q198
    /// (2); `--no-regularize` turns it off).
    pub regularize: bool,
    /// The free-space floor, percent of each destination file system
    /// (`--min-free-percent`; the engine's rule).
    pub min_free_percent: u8,
    /// `--rebind-landing OLD=NEW` (A-106 follow-up): placement rows naming
    /// a landed file under OLD are re-bound to the same file under NEW
    /// when the placed bytes are proved NEW's (never inferred from size).
    pub rebind: Vec<(PathBuf, PathBuf)>,
    /// `--replace-archived-dest ROOT`, canonical (A-106): a destination
    /// transcript, sidecar or rollout that is a symlink to a regular file
    /// under one of these is replaced by the differing landed file, the
    /// archive target kept and the link ledgered for `--undo`.
    pub replace_archived: Vec<PathBuf>,
}

/// The default `--retention-margin-days`.
pub const RETENTION_MARGIN_DAYS: i64 = 365;

/// Claude Code's managed settings files on this platform.
#[must_use]
pub fn managed_settings() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![PathBuf::from(
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        )]
    } else {
        vec![PathBuf::from("/etc/claude-code/managed-settings.json")]
    }
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
            landing_index: LandingIndex::InLanding,
            beside_suffix: None,
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
            waived: BTreeSet::new(),
            retention_margin_days: RETENTION_MARGIN_DAYS,
            managed_settings: managed_settings(),
            now_ns: now_ns(),
            by_id_exact: vec![source_home.to_path_buf()],
            by_id_prefixes: BY_ID_PREFIXES.iter().map(PathBuf::from).collect(),
            respell_cwd: true,
            regularize: true,
            min_free_percent: crate::space::min_free_percent(),
            rebind: Vec::new(),
            replace_archived: Vec::new(),
        }
    }

    /// The landed Codex index, when the run has one ([`LandingIndex`]).
    #[must_use]
    pub fn landing_index(&self) -> Option<PathBuf> {
        match &self.landing_index {
            LandingIndex::InLanding => Some(self.landing_codex.join(codex::STATE_DB)),
            LandingIndex::At(path) => Some(path.clone()),
            LandingIndex::Absent => None,
        }
    }

    /// Whether the landed Codex index is the destination's live index (one
    /// file under two names): never read as a snapshot (review of #280,
    /// finding 6: it is written while it is read, and `immutable=1` then
    /// gives wrong answers).
    #[must_use]
    pub fn landing_index_is_live(&self) -> bool {
        let Some(landed) = self.landing_index() else {
            return false;
        };
        match (
            std::fs::canonicalize(landed),
            std::fs::canonicalize(self.dest_codex.join(codex::STATE_DB)),
        ) {
            (Ok(landed), Ok(live)) => landed == live,
            _ => false,
        }
    }

    /// Whether the landed `.claude` is the destination's own live `.claude`
    /// (a live-home landing, plan of record 2026-10-19, C9): the two resolve
    /// to one directory. A landed session already under the key of its own
    /// working directory is then in place, with nothing to re-seat.
    #[must_use]
    pub fn live_claude_landing(&self) -> bool {
        match (
            std::fs::canonicalize(&self.landing_claude),
            std::fs::canonicalize(&self.dest_claude),
        ) {
            (Ok(landing), Ok(dest)) => landing == dest,
            _ => false,
        }
    }

    /// `path` with the beside suffix appended, when the run has one.
    #[must_use]
    pub fn beside_of(&self, path: &Path) -> Option<PathBuf> {
        let suffix = self.beside_suffix.as_ref()?;
        let mut beside = path.as_os_str().to_owned();
        beside.push(suffix);
        Some(PathBuf::from(beside))
    }

    /// The landed rollout at `rel` (relative to the landed `.codex`) as the
    /// re-seat reads it (second review of #280, N2): the file there, or, on
    /// a live-home landing where that is a symlink (the destination's own
    /// archive link, never read), the source's copy the pull landed beside
    /// it when there is one.
    #[must_use]
    pub fn landed_codex(&self, rel: &Path) -> PathBuf {
        let path = self.landing_codex.join(rel);
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            if let Some(beside) = self
                .beside_of(&path)
                .filter(|beside| std::fs::symlink_metadata(beside).is_ok_and(|meta| meta.is_file()))
            {
                return beside;
            }
        }
        path
    }

    /// [`Self::live_claude_landing`] for `.codex`: the landed `.codex` is the
    /// destination's live one (C9).
    #[must_use]
    pub fn live_codex_landing(&self) -> bool {
        match (
            std::fs::canonicalize(&self.landing_codex),
            std::fs::canonicalize(&self.dest_codex),
        ) {
            (Ok(landing), Ok(dest)) => landing == dest,
            _ => false,
        }
    }

    /// Whether a session recorded in `cwd` that has no lane is placed
    /// resumable by ID: `--unmapped=by-id`, the source home itself, or a
    /// path under a by-ID prefix (`/private/tmp`, `/tmp`; OI-1003-Q198 C).
    #[must_use]
    pub fn by_id(&self, cwd: &str) -> bool {
        let cwd = Path::new(cwd);
        self.unmapped == Unmapped::ById
            || self.by_id_exact.iter().any(|exact| exact == cwd)
            || self
                .by_id_prefixes
                .iter()
                .any(|prefix| cwd.starts_with(prefix))
    }

    /// Whether `cwd` is one the rulings place by ID and waive from the
    /// per-lane check (not `--unmapped=by-id` alone).
    #[must_use]
    pub fn waived_by_rule(&self, cwd: &str) -> bool {
        let cwd = Path::new(cwd);
        self.by_id_exact.iter().any(|exact| exact == cwd)
            || self
                .by_id_prefixes
                .iter()
                .any(|prefix| cwd.starts_with(prefix))
    }
}

/// The default by-ID prefixes (OI-1003-Q198 C): neo's scratch directories.
pub const BY_ID_PREFIXES: [&str; 2] = ["/private/tmp", "/tmp"];

/// Where a run's landed Codex index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandingIndex {
    /// `landing_codex/state_5.sqlite` (the default).
    InLanding,
    /// `--landing-codex-index FILE`: with a live-home landing (plan of
    /// record 2026-10-19, C9) the snapshot this pass took sits beside the
    /// live index as `state_5.sqlite<suffix>`.
    At(PathBuf),
    /// `--no-landing-codex-index`: this pass landed no snapshot (its index
    /// seat was refused, or the source has none). The landed rollouts are
    /// still re-seated, each by its header; no thread is read from an
    /// index (review of #280, finding 6: a stale snapshot of an earlier
    /// pass is never reused).
    Absent,
}

/// Whether a session or thread outcome word leaves the session accounted
/// for in an `estate-move` closure (C11).
///
/// Accounted: resumable at its lane (`placed`,
/// `adopted`, `extended`, `superseded`, `unchanged`, `rebound`,
/// `replaced-archived`, `in-place`, and the Codex row words `registered`,
/// `destination-adopted`, `destination-holds`, `destination-row-only`), or
/// kept with the destination's own copy beside it, as SPEC R2.2 rules
/// (`diverged`, `dest-ahead`, `dest-continued`, `both-continued`,
/// `dest-removed`, `extend-raced`). Every other word (a lane refusal code,
/// `occupied`, `collision`, `held`, `unreadable-form`, `exchange-unsupported`,
/// `index-conflict`, ...) leaves the landed bytes somewhere no resume finds
/// them, and holds the gate.
#[must_use]
pub fn word_accounted(word: &str) -> bool {
    matches!(
        word,
        "placed"
            | "adopted"
            | "extended"
            | "superseded"
            | "unchanged"
            | "rebound"
            | "replaced-archived"
            | "in-place"
            | "registered"
            | "destination-adopted"
            | "destination-holds"
            | "destination-row-only"
            | "diverged"
            | "dest-ahead"
            | "dest-continued"
            | "both-continued"
            | "dest-removed"
            | "extend-raced"
    )
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
    /// Codex thread rows: `registered`, `adopted` (a row naming this
    /// placement), `unchanged`, `conflict`, `synthesized`, `absent`,
    /// `respelled`, `changed-since` (a re-spelling whose row changed
    /// first), `respell-not-text` (rows skipped: `id` or `cwd` not UTF-8),
    /// `respell-link-moved` (rows not re-spelled: a link moved during the
    /// sweep), and the holding words `destination-adopted`,
    /// `destination-holds`, `destination-row-only` (also session words;
    /// distinct from the placement word `adopted`).
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

    /// The session and thread outcomes of this pass that leave a landed
    /// session not resumable at its lane and not kept beside
    /// ([`word_accounted`]), as `tool/word` → count. `estate-move` holds its
    /// gate while any remain (plan of record 2026-10-19, C11).
    #[must_use]
    pub fn unaccounted(&self) -> BTreeMap<String, u64> {
        self.sessions
            .iter()
            .filter(|((_, word), count)| **count > 0 && !word_accounted(word))
            .map(|((tool, word), count)| (format!("{tool}/{word}"), *count))
            .collect()
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

/// Exclusive `flock`s held for one run; released when dropped.
struct Held(Vec<std::fs::File>);

impl Drop for Held {
    fn drop(&mut self) {
        for file in &self.0 {
            let _ = crate::io::sys::flock_unlock(file);
        }
    }
}

fn locked(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        BulkloadRefusal::ReseatLocked
    } else {
        crate::refuse::io(error, site)
    }
}

/// Take an exclusive `flock` on the destination agent root `path` (the
/// directory itself, through its real path), when it exists.
fn lock_root(held: &mut Held, path: &Path) -> Result<()> {
    let Ok(real) = std::fs::canonicalize(path) else {
        return Ok(());
    };
    let directory = place::open_dir(&real)?;
    crate::io::sys::flock_exclusive(&directory)
        .map_err(|error| locked(&error, "reseat::lock_root"))?;
    held.0.push(directory);
    Ok(())
}

/// Take an exclusive `flock` on `STATE/reseat.lock`, creating STATE.
fn lock_state(held: &mut Held, state: &Path) -> Result<()> {
    use crate::refuse::RefuseAt as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    match std::fs::DirBuilder::new().mode(0o700).create(state) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).refuse_at("reseat::lock_state"),
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(state.join(LOCK))
        .refuse_at("reseat::lock_state")?;
    crate::io::sys::flock_exclusive(&file).map_err(|error| locked(&error, "reseat::lock_state"))?;
    held.0.push(file);
    Ok(())
}

/// The lock file in STATE.
pub const LOCK: &str = "reseat.lock";

/// Refuse `DESTINATION_SPACE_INSUFFICIENT` unless each destination file
/// system, and each project quota a destination is under, keeps the floor
/// after the bytes planned for it (the engine's rule, `--min-free-percent`;
/// [`crate::space::preflight_many`]).
fn space(options: &Options, wants: &[(&Path, u64)]) -> Result<()> {
    crate::space::preflight_many(wants, options.min_free_percent)
}

/// Refuse a `--replace-archived-dest` root that overlaps the live state.
///
/// `FIELD_DOMAIN_VIOLATION` before anything is read or written when a root
/// equals, contains or is under the real destination `.claude` or
/// `.codex`, or contains the destination home: a swap there would happen
/// inside the archive, or the root would take the live state itself for
/// an archive (review A1).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION`.
pub fn archive_roots_disjoint(options: &Options) -> Result<()> {
    let live: Vec<PathBuf> = [&options.dest_claude, &options.dest_codex]
        .into_iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    let home = std::fs::canonicalize(&options.dest_home).ok();
    for root in &options.replace_archived {
        if live
            .iter()
            .any(|dest| root.starts_with(dest) || dest.starts_with(root))
            || home.as_ref().is_some_and(|home| home.starts_with(root))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
    }
    Ok(())
}

/// Settle the regularizations and archive replacements a crash left in
/// flight, before any placement, whether or not this pass regularizes.
fn settle_intents(
    options: &Options,
    ledger: &ledger::Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for (on, dest, tool) in [
        (options.claude, &options.dest_claude, claude::TOOL),
        (options.codex, &options.dest_codex, codex::TOOL),
    ] {
        if let (true, Ok(real)) = (on, std::fs::canonicalize(dest)) {
            regularize::settle_intents(&real, tool, ledger, summary, emit)?;
        }
    }
    Ok(())
}

/// Re-seat the landed sessions into the destination's live agent state:
/// one JSON line per tool, per session and per thread, handed to `line`,
/// then the summary line.
///
/// # Errors
/// Before anything is written: `FIELD_DOMAIN_VIOLATION` for a
/// `--replace-archived-dest` root that overlaps the live destination
/// ([`archive_roots_disjoint`]), `RESEAT_LOCKED` when another run holds a
/// destination root or STATE, `RESEAT_LANDING_ABSENT` when a landed tree
/// the run covers is absent, `RESEAT_RETENTION_UNSAFE` when the
/// destination's Claude Code would delete a re-seated file within the
/// margin, `RESEAT_INDEX_ABSENT` when there is a Codex thread to register
/// and no destination index, `DESTINATION_SPACE_INSUFFICIENT` below the
/// free-space floor. Then ledger, I/O and schema refusals. A session's own
/// refusal is its line's `refused` code, never the run's.
#[allow(clippy::too_many_lines)] // Preflight every tool, then each tool's pass.
pub fn reseat(options: &Options, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<Summary> {
    archive_roots_disjoint(options)?;
    // Review of #280, finding 6: the live index is never the snapshot.
    if options.codex && options.landing_index_is_live() {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let lanes = Lanes::new(options);
    let mut summary = Summary::default();
    let mut held = Held(Vec::new());
    if options.claude {
        lock_root(&mut held, &options.dest_claude)?;
    }
    if options.codex {
        lock_root(&mut held, &options.dest_codex)?;
    }
    let claude = if options.claude {
        Some(claude::Sessions::scan(options)?)
    } else {
        None
    };
    let codex = if options.codex {
        Some(codex::landed_anything(options)?)
    } else {
        None
    };
    let planned_claude = match &claude {
        Some(sessions) => claude::retention(options, &lanes, sessions)?,
        None => 0,
    };
    if codex == Some(true) {
        codex::preflight(options)?;
    }
    let admitted;
    {
        let ledger = ledger::Ledger::open_read_only(&options.state)?;
        let planned_codex = if codex == Some(true) {
            codex::planned(options, &lanes, ledger.as_ref())?
        } else {
            0
        };
        // Regularization is checked per link as it copies
        // (`regularize::regularize`), never here: every symlinked
        // transcript's bytes at once held whole passes back (M2 round 2,
        // item 1).
        space(
            options,
            &[
                (&options.dest_claude, planned_claude),
                (&options.dest_codex, planned_codex),
            ],
        )?;
        admitted = (planned_claude, planned_codex);
    }
    lock_state(&mut held, &options.state)?;
    let mut ledger = ledger::Ledger::open(&options.state, options.now_ns, "reseat")?;
    for (tool, state) in [
        (
            claude::TOOL,
            claude.as_ref().map_or("skipped", |sessions| {
                if sessions.list.is_empty() {
                    "empty"
                } else {
                    "present"
                }
            }),
        ),
        (
            codex::TOOL,
            match codex {
                None => "skipped",
                Some(false) => "empty",
                Some(true) => "present",
            },
        ),
    ] {
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"tool\",\"tool\":\"{tool}\",\"state\":\"{state}\"}}"
        ))?;
    }
    settle_intents(options, &ledger, &mut summary, emit)?;
    if options.claude && options.regularize {
        // Before placement: a regularized copy that is a prefix of a landed
        // transcript is then extended like any other.
        let reserved = [
            (options.dest_claude.as_path(), admitted.0),
            (options.dest_codex.as_path(), admitted.1),
        ];
        regularize::regularize(
            &root(&options.dest_claude)?,
            &options.replace_archived,
            &ledger,
            regularize::Space {
                min_free_percent: options.min_free_percent,
                reserved: &reserved,
            },
            &mut summary,
            emit,
        )?;
    }
    if let Some(sessions) = &claude {
        claude::reseat(options, &lanes, &ledger, sessions, &mut summary, emit)?;
    }
    match codex {
        Some(true) => codex::reseat(options, &lanes, &mut ledger, &mut summary, emit)?,
        // Nothing landed: the destination's symlink-spelled rows are still
        // re-spelled (operator M2 round 2 review).
        Some(false) => codex::sweep_alone(options, &mut ledger, &mut summary, emit)?,
        None => {}
    }
    emit(&summary.line())?;
    drop(held);
    Ok(summary)
}

/// Reverse what this STATE's ledger changed in the destination's own
/// state under the 2026-10-16 rulings (OI-1003-Q198).
///
/// Its `--rebind-landing` rebinds are put back too (ledger only: each
/// placement row names its old landed file again).
///
/// It puts each regularized
/// transcript's symlink back and each re-spelled Codex row's old `cwd`,
/// only where the destination still holds exactly what re-seat wrote; one
/// changed since is kept (`undo-kept`, `RESEAT_DEST_CONTINUED`). Placed
/// sessions are not removed. Returns `(links, rows)` put back.
///
/// # Errors
/// `RESEAT_LOCKED`; ledger, `SQLite` and I/O refusals.
pub fn undo(options: &Options, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<(u64, u64)> {
    let mut held = Held(Vec::new());
    lock_root(&mut held, &options.dest_claude)?;
    lock_root(&mut held, &options.dest_codex)?;
    lock_state(&mut held, &options.state)?;
    let mut ledger = ledger::Ledger::open(&options.state, options.now_ns, "undo")?;
    let codex_root = std::fs::canonicalize(&options.dest_codex).ok();
    let links = regularize::undo(&ledger, codex_root.as_deref(), emit)?;
    let rows = codex::undo(&mut ledger, emit)?;
    let mut rebinds = 0_u64;
    // Newest first: a chained rebind is put back hop by hop.
    for rebind in ledger.rebinds()?.into_iter().rev() {
        if rebind.state != "done" {
            continue;
        }
        // Ledger only: the placement row names the old landed file again.
        let back = ledger.undo_rebind(&rebind)?;
        rebinds += u64::from(back);
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"rebind\",\"dest\":{},\"restored\":{},\"outcome\":\"{}\"}}",
            shown(&rebind.dest),
            shown(&rebind.old_landing),
            if back { "undone" } else { "undo-kept" },
        ))?;
    }
    emit(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo-summary\",\"links\":{links},\"rows\":{rows},\"rebinds\":{rebinds}}}"
    ))?;
    drop(held);
    Ok((links, rows))
}
