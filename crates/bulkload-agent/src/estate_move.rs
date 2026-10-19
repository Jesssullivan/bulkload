//! `estate-move-source` and `estate-move` (M2 item 7; design
//! `docs/agent-notes/2026-10-16-estate-move-design.md`; operator ruling
//! A-20261018-110): the in-place lane's scripts folded into two commands,
//! each a composition of the verbs this binary already has. Nothing here
//! writes a repository, a seat or a session by itself: every write is the
//! composed verb's, with its own proofs, records and typed refusals.
//!
//! Two halves, not one command over SSH: the source half runs on the source
//! (as `estate-capture` did in the lane), and the destination half reaches
//! the source only through `pull`'s own `serve` session. A destination that
//! drove the source's capture over SSH would need a new child process, and
//! the S2 source-command registry (P76) freezes the set of child builders
//! (design question 4; reported to the operator).

// Paths print escaped and quoted, as every estate receipt's do.
#![allow(clippy::unnecessary_debug_formatting)]

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bulkload_agent::estate::{self, moving};
use bulkload_agent::refuse::RefuseAt as _;
use bulkload_proto::{BulkloadRefusal, Result};

/// `estate-move-source` or `estate-move` with `args`.
///
/// # Errors
/// The verb's refusal.
pub fn command(verb: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<OsString> = args.collect();
    if verb == "estate-move-source" {
        source_command(&args)
    } else {
        move_command(&args)
    }
}

// Ref namespaces a label may not take: Git's own, and bulkload's.
const RESERVED: &[&str] = &[
    "heads",
    "tags",
    "remotes",
    "notes",
    "stash",
    "HEAD",
    "bisect",
    "replace",
    "worktree",
    "rewritten",
    "prefetch",
    "bulkload",
];

// The source half's pass marks in `MOVE_STATE/carry` (see `source_command`).
const STARTED: &str = "move-source.started";
const DONE: &str = "move-source.done";
const REFUSALS: &str = "move-source.refusals";
const NOTES: &str = "move-source.notes";

// Whether the pulled corpus is a finished source pass's: its `done` mark
// names the pass its `started` mark names.
fn finished_pass(carry: &Path) -> Result<()> {
    let read = |name: &str| std::fs::read(carry.join(name)).ok();
    match (read(STARTED), read(DONE)) {
        (Some(started), Some(done)) if started == done => Ok(()),
        (None, _) => Err(BulkloadRefusal::RequiredFieldMissing),
        _ => Err(BulkloadRefusal::SourceChangedAfterSnapshot),
    }
}

fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
        })
}

fn out(line: &str) -> Result<()> {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{line}").refuse_at("estate_move::out")?;
    output.flush().refuse_at("estate_move::out")
}

// `--map SRC=DST`, both absolute.
fn map_pair(value: Option<&OsString>) -> Result<(PathBuf, PathBuf)> {
    let (from, to) = value
        .ok_or(BulkloadRefusal::RequiredFieldMissing)?
        .to_str()
        .ok_or(BulkloadRefusal::PathNotPortable)?
        .split_once('=')
        .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    let (from, to) = (PathBuf::from(from), PathBuf::from(to));
    if !from.is_absolute() || !to.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    Ok((from, to))
}

fn absolute(value: Option<&OsString>) -> Result<PathBuf> {
    let path = PathBuf::from(value.ok_or(BulkloadRefusal::RequiredFieldMissing)?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(BulkloadRefusal::PathNotAbsolute)
    }
}

fn jobs(value: Option<&OsString>) -> Result<usize> {
    value
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .ok_or(BulkloadRefusal::FieldDomainViolation)
}

/// `estate-move-source [--map SRC=LIVE]... [--jobs N] [--include-rebuildable]
/// MOVE_STATE SOURCE_ROOT...`, on the source: inventory every repository
/// under the roots ([`moving::inventory`]), append the new items to
/// `MOVE_STATE/carry/plan.postcard` (`estate-add-batch`), and capture the
/// plan into `MOVE_STATE/carry/corpus` (`estate-capture`, private state in
/// `MOVE_STATE/private`), and make `MOVE_STATE/serve` and
/// `MOVE_STATE/serve/carry`, where the destination half's `pull`s and its
/// carry phase keep their `serve` states (#276). A capture
/// refusal is an item's (its line says
/// why; the destination carries it as files); only a refusal of the whole
/// capture fails the verb.
#[allow(clippy::too_many_lines)] // Arguments, then the inventory and capture.
fn source_command(args: &[OsString]) -> Result<()> {
    let mut maps = Vec::new();
    let mut count = 2;
    let mut rebuildable = false;
    let mut at = 0;
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        match arg {
            "--map" => {
                maps.push(map_pair(args.get(at + 1))?);
                at += 2;
            }
            "--jobs" => {
                count = jobs(args.get(at + 1))?;
                at += 2;
            }
            "--include-rebuildable" if !rebuildable => {
                rebuildable = true;
                at += 1;
            }
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
    }
    let [state, roots @ ..] = args.get(at..).unwrap_or_default() else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    if roots.is_empty() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let state = absolute(Some(state))?;
    let roots = roots
        .iter()
        .map(|root| absolute(Some(root)))
        .collect::<Result<Vec<_>>>()?;
    moving::private_dir(&state)?;
    let _lock = moving::lock(&state)?;
    let carry = state.join("carry");
    moving::private_dir(&carry)?;
    // #276: the destination half's `pull`s name their source states under
    // `MOVE_STATE/serve/NAME` (the corpus, the sessions) and its carry
    // phase under `MOVE_STATE/serve/carry/ITEM` (`estate-carry-files
    // --source-state-root`), on this host, over the transport it chose. The
    // directories are this verb's, like `carry/` and `private/`, so they are
    // here before the first `serve` is asked to open a state in them
    // (`serve` makes every missing parent too, for a state handed to it by
    // anything else).
    moving::private_dir(&state.join("serve"))?;
    moving::private_dir(&state.join("serve/carry"))?;
    // The notes phase's `serve` states (2026-10-19), one per note.
    moving::private_dir(&state.join("serve/notes"))?;
    // The pass's mark: `estate-move` refuses a corpus pulled while a pass
    // was still writing it (its `done` mark is another pass's, or none).
    let pass = now_ns().to_string();
    moving::write_report(&carry.join(STARTED), pass.as_bytes())?;
    let plan = carry.join("plan.postcard");
    let found = moving::inventory(&roots, &maps)?;
    for planned in &found.items {
        out(&format!(
            "estate-move-source item={} kind={} source={:?} repository={:?} workspace={:?}",
            estate::item_id(&planned.item)?,
            planned.kind,
            planned.item.source,
            planned.item.repository,
            planned.item.workspace,
        ))?;
    }
    for note in &found.notes {
        out(&format!(
            "estate-move-source note={} path={:?} count={} code={}",
            note.what,
            note.path,
            note.count,
            note.code.unwrap_or("none"),
        ))?;
    }
    let plan_existed = plan.try_exists().refuse_at("estate_move::source")?;
    let existing = if plan_existed {
        estate::inspect(&plan)?
    } else {
        Vec::new()
    };
    // A source already planned elsewhere (a changed --map) would land
    // twice: refused before the plan changes.
    if found.items.iter().any(|planned| {
        existing
            .iter()
            .any(|item| item.source == planned.item.source && *item != planned.item)
    }) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let new: Vec<estate::Item> = found
        .items
        .iter()
        .map(|planned| planned.item.clone())
        .filter(|item| !existing.contains(item))
        .collect();
    // A first pass whose roots hold no repository still writes its (empty)
    // plan, so the pass completes and the destination half reads a plan,
    // never `IO (errno 2)` (#283 review 5).
    if !new.is_empty() || !plan_existed {
        estate::add_batch(&plan, &new)?;
    }
    let planned = estate::inspect(&plan)?.len();
    let noted = |what: &str| -> u64 {
        found
            .notes
            .iter()
            .filter(|note| note.what == what)
            .map(|note| note.count)
            .sum()
    };
    out(&format!(
        "estate-move-source phase=inventory outcome=ok items={} new={} planned={planned} symlinks={} not_git_files={} not_git_trees={} prunable={} unreadable={} source_vanished={} not_in_census={}",
        found.items.len(),
        new.len(),
        noted(moving::notes::LINK),
        noted(moving::notes::FILE) + noted(moving::notes::TREE),
        found
            .notes
            .iter()
            .filter(|note| note.what == moving::notes::TREE)
            .count(),
        noted("prunable"),
        noted("unreadable"),
        noted("source-vanished"),
        noted("not-in-census"),
    ))?;
    let policy = if rebuildable {
        bulkload_agent::git_carry::CapturePolicy::including_rebuildable()
    } else {
        bulkload_agent::git_carry::CapturePolicy::default()
    };
    // Each refused item's code, and each nest its capture holds only as
    // custody (its content is not carried: M2r2 item 5), for the
    // destination.
    let refusals = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let nests = std::sync::Mutex::new(Vec::new());
    let receipts = AtomicU64::new(0);
    let capture = estate::capture_with_policy(
        &plan,
        &state.join("private"),
        &carry.join("corpus"),
        count,
        policy,
        &|row| {
            receipts.fetch_add(1, Ordering::Relaxed);
            if row.outcome == "refused" {
                let code = row.refusal.as_ref().map_or("IO", BulkloadRefusal::code);
                refusals
                    .lock()
                    .map_err(|_| BulkloadRefusal::WorkerLost)?
                    .insert(row.item.clone(), code.to_owned());
            }
            for line in &row.nested {
                if line.contains("kind=Directory") && !line.contains("seats=own-item") {
                    nests
                        .lock()
                        .map_err(|_| BulkloadRefusal::WorkerLost)?
                        .push(moving::Note {
                            path: nested_path(&row.source, line),
                            what: "nested",
                            count: 1,
                            code: None,
                        });
                }
            }
            super::print_receipt(row)
        },
    );
    let refusals = refusals
        .into_inner()
        .map_err(|_| BulkloadRefusal::WorkerLost)?;
    let mut all_notes = found.notes.clone();
    all_notes.extend(
        nests
            .into_inner()
            .map_err(|_| BulkloadRefusal::WorkerLost)?,
    );
    let receipts = receipts.into_inner();
    // Item refusals only when every planned item answered: a refusal that
    // cut the pass short is the verb's, and the pass is not finished.
    let finished = capture.is_ok() || receipts == u64::try_from(planned).unwrap_or(u64::MAX);
    if finished {
        moving::write_report(
            &carry.join(REFUSALS),
            moving::render_refusals(&refusals).as_bytes(),
        )?;
        moving::write_report(
            &carry.join(NOTES),
            moving::render_notes(&all_notes, &maps).as_bytes(),
        )?;
        moving::write_report(&carry.join(DONE), pass.as_bytes())?;
    }
    out(&format!(
        "estate-move-source phase=capture outcome={} captured={} refused={} nested_custody_only={}",
        match &capture {
            Ok(()) => "ok".to_owned(),
            Err(refusal) if finished => format!("items-refused first={}", refusal.code()),
            Err(refusal) => format!("refused:{}", refusal.code()),
        },
        receipts.saturating_sub(u64::try_from(refusals.len()).unwrap_or(0)),
        refusals.len(),
        all_notes
            .iter()
            .filter(|note| note.what == "nested")
            .count(),
    ))?;
    match capture {
        Err(refusal) if !finished => Err(refusal),
        _ => Ok(()),
    }
}

// A nested repository's own path: the item's source joined with the nest's
// (its receipt line's escaped `path="..."`), so each nest is its own note;
// the item's source when the line names none it can read.
fn nested_path(source: &Path, line: &str) -> PathBuf {
    // The closing quote is the first one no backslash escapes.
    let quoted = |rest: &str| {
        let mut escaped = false;
        rest.char_indices()
            .find(|&(_, c)| {
                let end = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                end
            })
            .and_then(|(at, _)| rest.get(..at))
            .map(str::to_owned)
    };
    line.split_once("path=\"")
        .and_then(|(_, rest)| quoted(rest))
        .and_then(|escaped| moving::notes::unescape(&escaped))
        .filter(|rel| !rel.is_empty())
        .map_or_else(
            || source.to_path_buf(),
            |rel| source.join(std::ffi::OsStr::from_bytes(&rel)),
        )
}

/// What phase 6 leaves for the closure (C11): the two session transfer
/// states, and the re-seat's summary when it ran.
struct Sessions {
    pulls: Vec<PathBuf>,
    reseat: Option<bulkload_agent::reseat::Summary>,
}

/// The destination half's arguments.
struct Move {
    plan_only: bool,
    label: String,
    source: String,
    origins: Vec<String>,
    side_roots: Vec<PathBuf>,
    exclude_union: bool,
    handoff: bool,
    sessions: Option<PathBuf>,
    codex_root: Option<PathBuf>,
    dest_home: Option<PathBuf>,
    maps: Vec<(PathBuf, PathBuf)>,
    reseat_args: Vec<OsString>,
    dispositions: Option<PathBuf>,
    remote: Option<OsString>,
    config: Option<OsString>,
    jobs: usize,
    host: OsString,
    source_state: PathBuf,
    state: PathBuf,
}

impl Move {
    #[allow(clippy::too_many_lines)] // Argument parsing only.
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut parsed = Self {
            plan_only: false,
            label: String::new(),
            source: String::new(),
            origins: Vec::new(),
            side_roots: Vec::new(),
            exclude_union: false,
            handoff: true,
            sessions: None,
            codex_root: None,
            dest_home: None,
            maps: Vec::new(),
            reseat_args: Vec::new(),
            dispositions: None,
            remote: None,
            config: None,
            jobs: 2,
            host: OsString::new(),
            source_state: PathBuf::new(),
            state: PathBuf::new(),
        };
        let (mut label, mut source) = (None, None);
        let mut at = 0;
        let text = |at: usize| -> Result<String> {
            args.get(at)
                .ok_or(BulkloadRefusal::RequiredFieldMissing)?
                .to_str()
                .map(str::to_owned)
                .ok_or(BulkloadRefusal::FieldDomainViolation)
        };
        while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
            let mut step = 2;
            match arg {
                "--plan-only" if !parsed.plan_only => {
                    parsed.plan_only = true;
                    step = 1;
                }
                "--exclude-union" if !parsed.exclude_union => {
                    parsed.exclude_union = true;
                    step = 1;
                }
                "--no-hand-off" if parsed.handoff => {
                    parsed.handoff = false;
                    step = 1;
                }
                "--label" if label.is_none() => label = Some(text(at + 1)?),
                "--source" if source.is_none() => source = Some(text(at + 1)?),
                "--activate-origin" => {
                    let prefix = text(at + 1)?;
                    if !bulkload_agent::git_carry::origin_prefix_allowed(&prefix) {
                        return Err(BulkloadRefusal::FieldDomainViolation);
                    }
                    parsed.origins.push(prefix);
                }
                "--side-root" => parsed.side_roots.push(absolute(args.get(at + 1))?),
                "--sessions" if parsed.sessions.is_none() => {
                    parsed.sessions = Some(absolute(args.get(at + 1))?);
                }
                "--codex-source-root" if parsed.codex_root.is_none() => {
                    parsed.codex_root = Some(absolute(args.get(at + 1))?);
                }
                "--dest-home" if parsed.dest_home.is_none() => {
                    parsed.dest_home = Some(absolute(args.get(at + 1))?);
                }
                "--map" => parsed.maps.push(map_pair(args.get(at + 1))?),
                // One `estate-reseat` option, passed on verbatim before its
                // positional arguments (e.g. --no-regularize,
                // --unmapped=by-id, --rebind-landing OLD=NEW as two).
                "--reseat-arg" => parsed.reseat_args.push(
                    args.get(at + 1)
                        .cloned()
                        .ok_or(BulkloadRefusal::RequiredFieldMissing)?,
                ),
                "--dispositions" if parsed.dispositions.is_none() => {
                    parsed.dispositions = Some(absolute(args.get(at + 1))?);
                }
                "--remote-executable" if parsed.remote.is_none() => {
                    parsed.remote = Some(
                        args.get(at + 1)
                            .cloned()
                            .ok_or(BulkloadRefusal::RequiredFieldMissing)?,
                    );
                }
                "--ssh-config" if parsed.config.is_none() => {
                    parsed.config = Some(absolute(args.get(at + 1))?.into_os_string());
                }
                "--jobs" => parsed.jobs = jobs(args.get(at + 1))?,
                other if other.starts_with("--") => {
                    return Err(BulkloadRefusal::FieldDomainViolation)
                }
                _ => break,
            }
            at += step;
        }
        let [host, source_state, state] = args.get(at..).unwrap_or_default() else {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        };
        parsed.label = label.ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        parsed.source = source.ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        // The label is a path suffix and one ref component (`refs/LABEL/`,
        // never one of Git's or bulkload's own namespaces); the source label
        // a provenance namespace. Both checked before anything runs.
        if !bulkload_agent::git_carry::source_label_allowed(&parsed.label)
            || !bulkload_agent::git_carry::source_label_allowed(&parsed.source)
            || bulkload_agent::git_carry::beside::namespace(&parsed.label).is_err()
            || RESERVED.contains(&parsed.label.as_str())
            || parsed.label.starts_with("carry")
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        // `pull` takes SSH_CONFIG only after REMOTE_EXECUTABLE.
        if parsed.config.is_some() && parsed.remote.is_none() {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        }
        if host.as_os_str() == "-" && (parsed.remote.is_some() || parsed.config.is_some()) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        // The session options act only with --sessions.
        if parsed.sessions.is_none()
            && (parsed.codex_root.is_some()
                || parsed.dest_home.is_some()
                || !parsed.maps.is_empty()
                || !parsed.reseat_args.is_empty())
        {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        }
        parsed.host.clone_from(host);
        parsed.source_state = absolute(Some(source_state))?;
        parsed.state = absolute(Some(state))?;
        // On this host the two halves keep separate states.
        if parsed.local()
            && (parsed.state.starts_with(&parsed.source_state)
                || parsed.source_state.starts_with(&parsed.state))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        Ok(parsed)
    }

    fn local(&self) -> bool {
        self.host.as_os_str() == "-"
    }

    fn suffix(&self) -> String {
        format!(".{}", self.label)
    }

    // One recorded transfer of the source's `from` into `to`: `copy` on
    // this host, else `pull` from HOST; `serve` keeps its source state in
    // the source half's `MOVE_STATE/serve/NAME`, this side in
    // `MOVE_STATE/pull/NAME`. `to` is made private (0700) first.
    fn transfer(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        moving::private_dir(to)?;
        self.transfer_into(from, to, name)
    }

    // [`Self::transfer`] into a live destination tree (the agent's own home
    // directory, plan of record 2026-10-19 C9): created 0700 when absent,
    // else taken as it is, whatever its mode (its mode is the agent's, not
    // bulkload's to tighten); a non-directory there refuses
    // PATH_ESCAPES_ROOT.
    fn transfer_live(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        match std::fs::DirBuilder::new().mode(0o700).create(to) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !std::fs::symlink_metadata(to)
                    .refuse_at("estate_move::transfer_live")?
                    .is_dir()
                {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
            }
            Err(error) => {
                return Err(bulkload_agent::refuse::io(
                    &error,
                    "estate_move::transfer_live",
                ))
            }
        }
        self.transfer_into(from, to, name)
    }

    fn transfer_into(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        let pulls = self.state.join("pull");
        moving::private_dir(&pulls)?;
        let serve = self.source_state.join("serve").join(name);
        if self.local() {
            moving::private_dir(&self.source_state.join("serve"))?;
            return super::copy_recorded(from, to, &serve, &pulls.join(name));
        }
        let mut args: Vec<OsString> = vec![
            self.host.clone(),
            from.as_os_str().to_owned(),
            to.as_os_str().to_owned(),
            serve.into_os_string(),
            pulls.join(name).into_os_string(),
        ];
        args.extend(self.remote.iter().cloned());
        args.extend(self.config.iter().cloned());
        super::pull_recorded(&args)
    }
}

// Record a phase's end in the ledger (written durably) and print it.
fn reached(ledger: &mut moving::Ledger, path: &Path, phase: &str, outcome: String) -> Result<()> {
    out(&format!("estate-move phase={phase} outcome={outcome}"))?;
    ledger.reached(phase, outcome, now_ns());
    ledger.save(path)
}

fn word(result: &Result<()>) -> String {
    match result {
        Ok(()) => "ok".to_owned(),
        Err(refusal) => format!("refused:{}", refusal.code()),
    }
}

/// `estate-move` on the destination (see the usage text): pull the source
/// half's plan and corpus, decide every item ([`moving::decide`]), then
/// `estate-apply --adopt --beside .LABEL --refs-in`, `estate-carry-files`
/// (live, or beside), `estate-handoff`, the session pulls and
/// `estate-reseat` (with `--sessions`), and `closure-report`. Each phase's
/// end is recorded in `MOVE_STATE/move.ledger`. Exits with the first
/// phase's refusal, else the closure gate.
#[allow(clippy::too_many_lines)]
fn move_command(args: &[OsString]) -> Result<()> {
    let run = Move::parse(args)?;
    moving::private_dir(&run.state)?;
    let _lock = moving::lock(&run.state)?;
    let ledger_path = moving::ledger_path(&run.state);
    let mut ledger = moving::Ledger::open(
        &ledger_path,
        &run.label,
        &run.source,
        run.host.to_str().ok_or(BulkloadRefusal::PathNotPortable)?,
        &run.source_state,
    )?;
    // Files carried as files and agent state trees carry their SQLite
    // databases as backup-API snapshots, never their live bytes (#218).
    bulkload_agent::transfer::set_sqlite_mode(bulkload_agent::transfer::parse_sqlite_mode(
        "snapshot",
    )?);
    let carry = run.state.join("carry");
    let private = run.state.join("private");
    let plan = carry.join("plan.postcard");
    let corpus = carry.join("corpus");
    // The first phase that refused: the verb's exit when closure passes.
    let mut first: Option<BulkloadRefusal> = None;
    let mut note = |result: &Result<()>| {
        if let (None, Err(refusal)) = (&first, result) {
            first = Some(refusal.clone());
        }
    };

    // 1. The source half's plan and corpus, from a finished pass only.
    let pulled = run
        .transfer(&run.source_state.join("carry"), &carry, "corpus")
        .and_then(|()| finished_pass(&carry));
    reached(&mut ledger, &ledger_path, "pull", word(&pulled))?;
    pulled?;
    let text = |name: &str| std::fs::read_to_string(carry.join(name)).unwrap_or_default();
    let refusals = moving::parse_refusals(&text(REFUSALS));
    let notes = moving::note_counts(&text(NOTES));
    let note_lines = moving::notes::parse(&text(NOTES));

    // 2. Decide every item against the destination now. Under --plan-only
    // the decisions are shown, never recorded.
    let items = estate::inspect(&plan)?;
    let inputs = moving::Inputs {
        corpus: &corpus,
        refusals: &refusals,
        state: &private,
        source: &run.source,
        seats: !run.side_roots.is_empty(),
    };
    let mut decided = ledger.clone();
    let mut rows = moving::decide(&items, &inputs, &mut decided, now_ns())?;
    let suffix = run.suffix();
    print_plan(&rows, &suffix, &notes)?;
    // The source notes, decided against the plan's items (2026-10-19).
    let decided_notes = moving::notes::decide(&note_lines, &items);
    print_notes_plan(&decided_notes)?;
    if run.plan_only {
        return preview(&run, (&plan, &corpus, &private));
    }
    ledger.items = decided.items;
    reached(&mut ledger, &ledger_path, "plan", "ok".to_owned())?;

    // 3. Land absent, adopt equal, beside what differs, refs in.
    let receipts = AtomicU64::new(0);
    // The items whose planned workspace this pass landed, adopted or
    // converged: the hand-off's (a beside landing, a carry or a refusal
    // leaves the planned workspace without this landing).
    let landed = std::sync::Mutex::new(Vec::new());
    let applied = estate::apply_reporting(
        &plan,
        &corpus,
        &private,
        &run.source,
        (
            run.jobs,
            &estate::ApplyOptions {
                adopt: true,
                activate_origin: run.origins.clone(),
                side_roots: run.side_roots.clone(),
                exclude_union: run.exclude_union,
                beside: Some(suffix),
                refs_in: true,
            },
        ),
        &|row| {
            receipts.fetch_add(1, Ordering::Relaxed);
            if matches!(
                row.outcome,
                "workspace-restored" | "previous-workspace-restoration-not-revalidated"
            ) {
                landed
                    .lock()
                    .map_err(|_| BulkloadRefusal::WorkerLost)?
                    .push(row.item.clone());
            }
            super::print_receipt(row)
        },
        &mut |line| out(line),
    );
    let landed = landed
        .into_inner()
        .map_err(|_| BulkloadRefusal::WorkerLost)?;
    // Item refusals (every planned item answered) are each item's,
    // recorded for closure, and the pass goes on to carry, hand off and
    // account for them. A refusal that cut the apply short stops it.
    let answered = receipts.into_inner() == u64::try_from(items.len()).unwrap_or(u64::MAX);
    let outcome = match &applied {
        Err(refusal) if answered => format!("items-refused first={}", refusal.code()),
        other => word(other),
    };
    reached(&mut ledger, &ledger_path, "apply", outcome)?;
    if !answered {
        applied?;
    }

    // 4. What the capture refused for good, carried as files: at the live
    // path when it is still absent or empty, else beside it. A carry is
    // recorded as started before it runs, and stays where it went.
    for row in &mut rows {
        if row.decision.action == moving::Action::CarryLive && !row.kept {
            let place = row.item.workspace.as_ref().unwrap_or(&row.item.repository);
            if !matches!(
                moving::presence(place),
                moving::Presence::Absent | moving::Presence::Empty
            ) {
                row.decision.action = moving::Action::CarryBeside;
                "capture-refused-occupied".clone_into(&mut row.decision.reason);
            }
        }
        if row.decision.action.carries() {
            row.decision.attempted = true;
            ledger.items.insert(row.id.clone(), row.decision.clone());
        }
    }
    ledger.save(&ledger_path)?;
    let carried = carry_files(&run, &rows, (&plan, &corpus, &private));
    note(&carried);
    reached(&mut ledger, &ledger_path, "carry", word(&carried))?;

    // 4b. The source notes (2026-10-19; "NOTHING is EVER excluded"): every
    // note the source half could not plan as an item, carried through the
    // same transport to its live path, else covered or held typed. Each
    // note's refusal is that note's (a closure row); only a refusal of the
    // phase itself stops it.
    let note_pass = carry_notes(&run, &decided_notes);
    let note_word = match &note_pass {
        Ok(pass) => pass.word(),
        Err(refusal) => format!("refused:{}", refusal.code()),
    };
    note(&note_pass.as_ref().map(drop).map_err(Clone::clone));
    reached(&mut ledger, &ledger_path, "notes", note_word)?;
    // A phase that stopped leaves every note not moved (its first refusal
    // is the verb's).
    let note_pass = note_pass.unwrap_or_else(|_| NotesPass::not_run(&decided_notes));

    // 5. The hand-off block in each landed workspace (OI-1003-Q183).
    if run.handoff && !landed.is_empty() {
        let mut args: Vec<OsString> = Vec::new();
        for item in &landed {
            args.extend(["--item".into(), item.into()]);
        }
        args.extend([
            plan.clone().into_os_string(),
            corpus.clone().into_os_string(),
            private.clone().into_os_string(),
            run.source.clone().into(),
        ]);
        let handed = super::estate_command("estate-handoff", &args);
        note(&handed);
        reached(&mut ledger, &ledger_path, "hand-off", word(&handed))?;
    }

    // 6. Sessions (plan of record 2026-10-19, C9/C10): the agent state
    // trees into the live home, then the re-seat (TIN-6587), whenever both
    // session transfers completed, path refusals included: those stay
    // typed closure rows, and a refused settings file never holds the
    // transcripts' re-seat.
    let sessions: Option<Sessions> = if let Some(home) = &run.sessions {
        let pass_started = now_secs();
        let pulled = sessions_pull(&run, home);
        note(&pulled);
        reached(&mut ledger, &ledger_path, "sessions-pull", word(&pulled))?;
        let mut outcome = Sessions {
            pulls: ["claude", "codex"]
                .iter()
                .map(|name| run.state.join("pull").join(name))
                .collect(),
            reseat: None,
        };
        if session_completed(&pulled) {
            let reseated = reseat(&run, home, (&plan, &corpus, &private), pass_started);
            let ended: Result<()> = reseated.as_ref().map(drop).map_err(Clone::clone);
            note(&ended);
            reached(&mut ledger, &ledger_path, "reseat", word(&ended))?;
            outcome.reseat = reseated.ok().flatten();
        } else {
            reached(
                &mut ledger,
                &ledger_path,
                "reseat",
                "skipped:sessions-pull-refused".to_owned(),
            )?;
        }
        Some(outcome)
    } else {
        None
    };

    // 7. Closure over the move: every planned item accounted, with the
    // operator's reviews joined; anything the source half could not plan
    // (notes) is not moved, and holds the gate.
    let mut report = bulkload_agent::closure::Report::from_ledger(&estate::ledger_with(
        &plan,
        &corpus,
        &run.source,
        std::slice::from_ref(&private),
        false,
    )?);
    // C11: the session transfers are rows of the same report (their
    // `applied-beside` entries accounted, their refused paths pending
    // review, a transfer that never ran unaccounted), so the gate cannot
    // pass with agent state unaccounted.
    if let Some(sessions) = &sessions {
        report.join_transfers(&sessions.pulls)?;
    }
    // 2026-10-19: each carried note's transfer rows, and one row for each
    // note no transfer of this pass accounts for.
    report.join_transfers(&note_pass.states)?;
    report.join_notes(
        &note_pass
            .rows
            .iter()
            .map(moving::notes::RowRecord::row)
            .collect::<Vec<_>>(),
    )?;
    if let Some(dispositions) = &run.dispositions {
        report.dispose(&bulkload_agent::disposition::Ledger::read(
            dispositions,
            &plan,
            &run.source,
        )?);
    }
    // The re-seat's outcomes that leave a session not resumable (or a
    // re-seat that did not run) hold the gate too.
    let reseat_held: std::collections::BTreeMap<String, u64> = match &sessions {
        None => std::collections::BTreeMap::new(),
        Some(Sessions {
            reseat: Some(summary),
            ..
        }) => summary.unaccounted(),
        Some(Sessions { reseat: None, .. }) => {
            std::iter::once(("reseat/not-run".to_owned(), 1)).collect()
        }
    };
    let reseat_unaccounted: u64 = reseat_held.values().sum();
    let report_path = run.state.join("closure.json");
    moving::write_report(&report_path, report.to_json().as_bytes())?;
    let refusals = report
        .refusals
        .iter()
        .map(|(code, count)| format!("{code}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    // What is still not moved: each note neither carried nor covered,
    // weighted by the entries it stands for, and how much of it a review
    // or a standing policy disposes.
    let (not_moved, not_moved_dispositioned) = note_pass.not_moved(&report);
    let held = reseat_held
        .iter()
        .map(|(word, count)| format!("{word}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    out(&format!(
        "estate-move closure planned={} applied={} applied_beside={} carried={} referenced_only={} refused={} refused_pending_review={} unaccounted={} notes={} notes_carried={} not_moved={not_moved} not_moved_dispositioned={not_moved_dispositioned} refusals={} sessions={} transfer_rows={} note_rows={} reseat_unaccounted={reseat_unaccounted} reseat_held={} report={report_path:?}",
        rows.len(),
        report.applied,
        report.applied_beside,
        report.carried,
        report.referenced_only,
        report.refused,
        report.refused_pending_review,
        report.unaccounted,
        note_pass.settled.len(),
        note_pass.carried,
        if refusals.is_empty() { "none" } else { &refusals },
        if sessions.is_some() { "claude,codex" } else { "none" },
        report.transfer_rows(),
        report.note_rows(),
        if held.is_empty() { "none" } else { &held },
    ))?;
    let gate = report.gate().and(
        if not_moved == not_moved_dispositioned && reseat_unaccounted == 0 {
            Ok(())
        } else {
            Err(BulkloadRefusal::ClosureUnaccounted)
        },
    );
    reached(&mut ledger, &ledger_path, "closure", word(&gate))?;
    out(&format!(
        "estate-move gate={} next={}",
        if gate.is_ok() && first.is_none() { "pass" } else { "fail" },
        match (&first, &gate) {
            (Some(refusal), _) => format!(
                "a phase refused {}: read its estate-move phase= line, then rerun",
                refusal.code()
            ),
            (None, Err(_)) => format!(
                "review {report_path:?} (closure-dispose, then --dispositions) and the source notes; rerun estate-move-source, then estate-move, for the next pass"
            ),
            (None, Ok(())) => "the resume test (the operator's); rerun both halves to converge".to_owned(),
        }
    ))?;
    first.map_or(gate, Err)
}

// The plan's per-item lines, its histogram and the source notes' counts.
fn print_plan(
    rows: &[moving::Row],
    suffix: &str,
    notes: &std::collections::BTreeMap<String, u64>,
) -> Result<()> {
    for row in rows {
        let place = row.item.workspace.as_ref().unwrap_or(&row.item.repository);
        let beside = if row.decision.action == moving::Action::CarryBeside {
            format!(
                " beside={:?}",
                bulkload_agent::git_carry::beside::path(place, suffix)
            )
        } else {
            String::new()
        };
        out(&format!(
            "estate-move item={} action={} reason={} presence={} capture={:?} kept={} source={:?} live={place:?}{beside}",
            row.id,
            row.decision.action.word(),
            row.decision.reason,
            row.decision.presence.word(),
            row.decision.capture,
            row.kept,
            row.item.source,
        ))?;
    }
    let counts = moving::histogram(rows);
    out(&format!(
        "estate-move plan items={} {} notes={}",
        rows.len(),
        [
            moving::Action::Land,
            moving::Action::Adopt,
            moving::Action::RefsOnly,
            moving::Action::CarryLive,
            moving::Action::CarryBeside,
            moving::Action::Refuse,
        ]
        .iter()
        .map(|action| format!("{}={}", action.word(), counts.get(action).unwrap_or(&0)))
        .collect::<Vec<_>>()
        .join(" "),
        if notes.is_empty() {
            "none".to_owned()
        } else {
            notes
                .iter()
                .map(|(what, count)| format!("{what}:{count}"))
                .collect::<Vec<_>>()
                .join(",")
        },
    ))
}

// `--plan-only`: estate-diff's preview of what the apply will do
// (read-only at the destination), one row per line in MOVE_STATE/diff.jsonl.
fn preview(run: &Move, (plan, corpus, private): (&Path, &Path, &Path)) -> Result<()> {
    let rows_path = run.state.join("diff.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&rows_path)
        .refuse_at("estate_move::diff")?;
    let summary = estate::diff(
        plan,
        corpus,
        &run.source,
        &[private.to_path_buf()],
        &estate::DiffOptions {
            since: None,
            scan: Vec::new(),
            activate_origin: run.origins.clone(),
            beside: Some(run.suffix()),
            side_roots: run.side_roots.clone(),
        },
        &mut |line| writeln!(file, "{line}").refuse_at("estate_move::diff"),
    )?;
    let show = |map: &std::collections::BTreeMap<String, u64>| {
        map.iter()
            .map(|(key, count)| format!("{key}:{count}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    out(&format!(
        "estate-move diff classes={} adopt={} rows={rows_path:?} (plan only: nothing recorded or landed)",
        show(&summary.classes),
        show(&summary.adopt),
    ))
}

// Phase 4: `estate-carry-files` for the carry rows, live then beside.
fn carry_files(
    run: &Move,
    rows: &[moving::Row],
    (plan, corpus, private): (&Path, &Path, &Path),
) -> Result<()> {
    let carry_state = run.state.join("carry-files");
    let mut carried = Ok(());
    for beside in [false, true] {
        let wanted = if beside {
            moving::Action::CarryBeside
        } else {
            moving::Action::CarryLive
        };
        let ids: Vec<&String> = rows
            .iter()
            .filter(|row| row.decision.action == wanted)
            .map(|row| &row.id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        moving::private_dir(&carry_state)?;
        let mut args: Vec<OsString> = Vec::new();
        if beside {
            args.extend(["--beside".into(), run.suffix().into()]);
        }
        for id in ids {
            args.extend(["--item".into(), id.into()]);
        }
        if !run.local() {
            args.extend([
                "--host".into(),
                run.host.clone(),
                "--source-state-root".into(),
                run.source_state.join("serve/carry").into_os_string(),
            ]);
            if let Some(remote) = &run.remote {
                args.extend(["--remote-executable".into(), remote.clone()]);
            }
            if let Some(config) = &run.config {
                args.extend(["--ssh-config".into(), config.clone()]);
            }
        }
        args.extend([
            plan.as_os_str().to_owned(),
            corpus.as_os_str().to_owned(),
            private.as_os_str().to_owned(),
            run.source.clone().into(),
            carry_state.clone().into_os_string(),
        ]);
        let result = super::carry_files_command(&args);
        if carried.is_ok() {
            carried = result;
        }
    }
    carried
}

// The notes' plan: one count per plan word (`estate-move notes-plan`).
fn print_notes_plan(decided: &[moving::notes::Decided]) -> Result<()> {
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for note in decided {
        *counts.entry(note.plan.word().to_owned()).or_default() += 1;
    }
    out(&format!(
        "estate-move notes-plan notes={} {}",
        decided.len(),
        if counts.is_empty() {
            "none".to_owned()
        } else {
            counts
                .iter()
                .map(|(word, count)| format!("{word}={count}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    ))
}

/// How one note of the pass was settled.
#[derive(Debug, Clone)]
enum Settled {
    /// Its transfer ran: the record in this `DEST_STATE` says how it ended.
    Transfer(PathBuf),
    /// No transfer of the pass accounts for it: its row, by identity.
    Row(String),
}

/// What phase 4b leaves for the closure (2026-10-19).
#[derive(Debug, Default)]
struct NotesPass {
    /// Each note, the entries it stands for and how it was settled.
    settled: Vec<(u64, Settled)>,
    /// The transfer states of the notes whose transfer ran.
    states: Vec<PathBuf>,
    /// The notes no transfer of the pass accounts for.
    rows: Vec<moving::notes::RowRecord>,
    /// Notes whose transfer completed.
    carried: u64,
    /// Source bytes the notes' sessions read (0 on an unchanged rerun, R25).
    source_bytes_read: u64,
    /// Notes whose transfer ran and refused.
    refused: u64,
}

impl NotesPass {
    // Every note as a row, for a phase that stopped: a planned carry is
    // unaccounted (`note-not-carried`), the rest as planned.
    fn not_run(decided: &[moving::notes::Decided]) -> Self {
        let mut pass = Self::default();
        for note in decided {
            let record = moving::notes::RowRecord::of(note, None);
            pass.settled
                .push((note.line.count, Settled::Row(record.item.clone())));
            pass.rows.push(record);
        }
        pass
    }

    fn word(&self) -> String {
        if self.refused == 0 {
            "ok".to_owned()
        } else {
            format!("notes-refused count={}", self.refused)
        }
    }

    // The entries still not moved, and how many of them a review or a
    // standing policy disposes, read from the closure report the notes'
    // rows joined (after its reviews).
    fn not_moved(&self, report: &bulkload_agent::closure::Report) -> (u64, u64) {
        use bulkload_agent::closure::{Disposition, RowKind};
        let (mut not_moved, mut disposed) = (0_u64, 0_u64);
        for (count, settled) in &self.settled {
            let dispositions: Vec<&Disposition> = match settled {
                Settled::Transfer(state) => {
                    let state = bulkload_agent::transfer_outcome::stable(state);
                    report
                        .rows
                        .iter()
                        .filter(|row| {
                            row.kind == RowKind::TransferSession
                                && row
                                    .transfer
                                    .as_ref()
                                    .is_some_and(|detail| detail.state == state)
                        })
                        .map(|row| &row.disposition)
                        .collect()
                }
                Settled::Row(item) => report
                    .rows
                    .iter()
                    .filter(|row| row.kind == RowKind::SourceNote && row.item == *item)
                    .map(|row| &row.disposition)
                    .collect(),
            };
            if dispositions
                .iter()
                .any(|found| matches!(found, Disposition::Applied | Disposition::Covered))
            {
                continue;
            }
            not_moved = not_moved.saturating_add(*count);
            if dispositions
                .iter()
                .any(|found| matches!(found, Disposition::Refused(_)))
            {
                disposed = disposed.saturating_add(*count);
            }
        }
        (not_moved, disposed)
    }
}

// Phase 4b (2026-10-19): every carriable note through the transport with
// `--beside .LABEL`, each a recorded transfer of its own
// (`MOVE_STATE/pull/notes/ID`, the source's `serve` state
// `SOURCE_MOVE_STATE/serve/notes/ID`), and a row for every other note
// (`MOVE_STATE/notes.rows`, which `closure-dispose --source-notes` reads).
fn carry_notes(run: &Move, decided: &[moving::notes::Decided]) -> Result<NotesPass> {
    let pulled = run.state.join("pull/notes");
    moving::private_dir(&run.state.join("pull"))?;
    moving::private_dir(&pulled)?;
    let parents = run.state.join("notes");
    moving::private_dir(&parents)?;
    if run.local() {
        moving::private_dir(&run.source_state.join("serve"))?;
        moving::private_dir(&run.source_state.join("serve/notes"))?;
    }
    let mut pass = NotesPass::default();
    bulkload_agent::transfer::set_beside_suffix(Some(run.suffix().into_bytes()))?;
    let carried = (|| -> Result<()> {
        for note in decided {
            let moving::notes::Plan::Carry { shape, live } = &note.plan else {
                let record = moving::notes::RowRecord::of(note, None);
                out(&format!(
                    "estate-move note={} what={} plan={} count={} path={:?}",
                    note.id,
                    note.line.what,
                    note.plan.word(),
                    note.line.count,
                    note.line
                        .path
                        .as_deref()
                        .unwrap_or_else(|| Path::new(&note.line.text)),
                ))?;
                pass.settled
                    .push((note.line.count, Settled::Row(record.item.clone())));
                pass.rows.push(record);
                continue;
            };
            // On this host with no --map for it, the note is at its live
            // path already: nothing to move.
            if run.local() && note.line.path.as_deref() == Some(live.as_path()) {
                let record =
                    moving::notes::RowRecord::settled(note, "covered-in-place".to_owned(), None);
                pass.settled
                    .push((note.line.count, Settled::Row(record.item.clone())));
                pass.rows.push(record);
                continue;
            }
            let transfer_state = pulled.join(&note.id);
            match carry_note(run, note, (*shape, live), &transfer_state, &parents) {
                Ok(ended) => {
                    pass.states.push(transfer_state.clone());
                    pass.settled
                        .push((note.line.count, Settled::Transfer(transfer_state)));
                    match &ended.result {
                        Ok(stats) => {
                            pass.carried += 1;
                            pass.source_bytes_read = pass
                                .source_bytes_read
                                .saturating_add(stats.source_bytes_read);
                        }
                        Err(_) => pass.refused += 1,
                    }
                    out(&ended.line(note))?;
                }
                Err(refusal) => {
                    // Refused before its transfer ran: no record, so the
                    // note's own row holds the refusal.
                    let record = moving::notes::RowRecord::of(note, Some(&refusal));
                    out(&format!(
                        "estate-move note={} what={} plan={} outcome=refused:{} path={:?} live={live:?}",
                        note.id,
                        note.line.what,
                        note.plan.word(),
                        refusal.code(),
                        note.line.path.as_deref().unwrap_or_else(|| Path::new("")),
                    ))?;
                    pass.settled
                        .push((note.line.count, Settled::Row(record.item.clone())));
                    pass.rows.push(record);
                }
            }
        }
        Ok(())
    })();
    let cleared = bulkload_agent::transfer::set_beside_suffix(None);
    carried?;
    cleared?;
    moving::notes::write_rows(&run.state.join("notes.rows"), &pass.rows)?;
    out(&format!(
        "estate-move notes notes={} carried={} refused={} rows={} source_bytes_read={}",
        pass.settled.len(),
        pass.carried,
        pass.refused,
        pass.rows.len(),
        pass.source_bytes_read,
    ))?;
    Ok(pass)
}

/// One carried note's ended transfer.
struct Ended {
    at: PathBuf,
    beside: bool,
    result: Result<bulkload_agent::transfer::TransferStats>,
}

impl Ended {
    fn line(&self, note: &moving::notes::Decided) -> String {
        let counters = match &self.result {
            Ok(stats) => format!(
                "outcome=carried completed={} reused={} source_bytes_read={} refusals={} applied_beside={}",
                stats.completed,
                stats.reused,
                stats.source_bytes_read,
                stats.refusals.len(),
                stats.applied_beside.len(),
            ),
            Err(refusal) => format!("outcome=refused:{}", refusal.code()),
        };
        format!(
            "estate-move note={} what={} plan={} {counters} path={:?} at={:?} beside_root={}",
            note.id,
            note.line.what,
            note.plan.word(),
            note.line.path.as_deref().unwrap_or_else(|| Path::new("")),
            self.at,
            self.beside,
        )
    }
}

// One note's transfer: where it lands (live, or beside what the live path
// holds that cannot take it), the absent parents made (0755, recorded in
// `MOVE_STATE/notes/ID.parents`, as `estate-apply` makes an item's), the
// S2 checks, then the recorded session. `Err` is a refusal before the
// session (no record); a session that ran and refused is `Ok` with its
// refusal, its record in `state`.
fn carry_note(
    run: &Move,
    note: &moving::notes::Decided,
    (shape, live): (moving::notes::Shape, &Path),
    state: &Path,
    parents: &Path,
) -> Result<Ended> {
    use bulkload_agent::git_carry::parents::{absent_parents, FirstTarget};
    use bulkload_agent::transfer::RootKind;
    let from = note
        .line
        .path
        .as_deref()
        .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    let suffix = run.suffix();
    let (at, beside) = moving::notes::landing(shape, live, &suffix);
    if !absent_parents(&at)?.is_empty() {
        let record = parents.join(format!("{}.parents", note.id));
        FirstTarget::of(&at)?.place(Some((record.as_path(), note.id.as_str())))?;
    }
    let kind = match shape {
        moving::notes::Shape::File | moving::notes::Shape::Link => RootKind::File,
        moving::notes::Shape::Tree => RootKind::Tree,
    };
    moving::private_dir(state)?;
    let serve = run.source_state.join("serve/notes").join(&note.id);
    let result = if run.local() {
        bulkload_agent::transfer::copy_roots_apart_kind(from, &at, &serve, state, kind)?;
        super::copy_session_recorded(kind, from, &at, &serve, state)
    } else {
        bulkload_agent::transfer::receive_roots_apart_kind(&at, state, kind)?;
        let mut args: Vec<OsString> = vec![
            run.host.clone(),
            from.as_os_str().to_owned(),
            at.as_os_str().to_owned(),
            serve.into_os_string(),
            state.as_os_str().to_owned(),
        ];
        args.extend(run.remote.iter().cloned());
        args.extend(run.config.iter().cloned());
        super::pull_session_recorded(kind, &args)
    };
    Ok(Ended { at, beside, result })
}

// The destination home the sessions land in (`--dest-home`, default
// `$HOME`).
fn dest_home(run: &Move) -> Result<PathBuf> {
    run.dest_home.as_ref().map_or_else(
        || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .ok_or(BulkloadRefusal::RequiredFieldMissing)
        },
        |home| Ok(home.clone()),
    )
}

/// The Codex index name.
const STATE_DB: &str = "state_5.sqlite";

// Phase 6a (plan of record 2026-10-19, C9; SPEC R4 "each tree lands once,
// at its live path"): SOURCE_HOME/.claude and the Codex root into the LIVE
// DEST_HOME/.claude and DEST_HOME/.codex, with `--beside .LABEL`: equal
// files adopted, grown append-only files extended, differing files (and the
// live Codex index's snapshot) landed beside as `<path>.LABEL`, nothing
// overwritten and nothing staged. Both trees are pulled whatever the first
// returned; the first refusal is the phase's. `ITEMS_REFUSED` means the
// session completed with typed path refusals (closure rows), which does
// not stop the re-seat (C10).
fn sessions_pull(run: &Move, home: &Path) -> Result<()> {
    let dest = dest_home(run)?;
    let codex_root = run
        .codex_root
        .clone()
        .unwrap_or_else(|| home.join(".codex"));
    bulkload_agent::transfer::set_beside_suffix(Some(run.suffix().into_bytes()))?;
    let claude = run.transfer_live(&home.join(".claude"), &dest.join(".claude"), "claude");
    // The Codex index always lands beside, as this pass's snapshot: the
    // pull never writes the live index (review of #280, finding 6).
    bulkload_agent::transfer::set_beside_always(vec![STATE_DB.as_bytes().to_vec()]);
    let codex = run.transfer_live(&codex_root, &dest.join(".codex"), "codex");
    bulkload_agent::transfer::set_beside_always(Vec::new());
    let cleared = bulkload_agent::transfer::set_beside_suffix(None);
    // A session that did not complete outranks one that completed with
    // refused paths: the phase's word says whether the re-seat may run.
    let mut outcome = Ok(());
    for result in [claude, codex, cleared] {
        if let Err(refusal) = result {
            if session_completed(&outcome) {
                outcome = Err(refusal);
            }
        }
    }
    outcome
}

// The Codex index snapshot this pass landed beside the live index: the
// `applied-beside` row of `state_5.sqlite` in a completed Codex transfer
// record of this pass (started at or after `pass_started`, Unix seconds).
fn landed_index(run: &Move, dest_codex: &Path, pass_started: u64) -> Option<PathBuf> {
    use bulkload_agent::transfer_outcome::{self, EntryOutcome, Session};
    let state = run.state.join("pull").join("codex");
    let destination = transfer_outcome::stable(dest_codex);
    transfer_outcome::records(&state)
        .iter()
        .filter_map(|(_, path)| transfer_outcome::read_file(path)?.ok())
        .filter(|record| {
            record.session == Session::Completed
                && record.started >= pass_started
                && record.destination == destination
        })
        .find_map(|record| {
            record
                .entries
                .iter()
                .find_map(|entry| match &entry.outcome {
                    EntryOutcome::AppliedBeside { beside }
                        if entry.rel_path == STATE_DB.as_bytes() =>
                    {
                        Some(dest_codex.join(std::ffi::OsStr::from_bytes(beside)))
                    }
                    _ => None,
                })
        })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

// Whether every session transfer completed: every path answered, refused
// ones as typed closure rows (`ITEMS_REFUSED`), or none.
const fn session_completed(pulled: &Result<()>) -> bool {
    matches!(pulled, Ok(()) | Err(BulkloadRefusal::ItemsRefused))
}

// Phase 6b: `estate-reseat --lanes-from-plan` from the live home (the
// landing) into DEST_HOME. The landed Codex index is the beside snapshot
// `state_5.sqlite.LABEL` when the live index was kept (it had live
// sidecars, or differed), else the index the pull landed.
fn reseat(
    run: &Move,
    home: &Path,
    (plan, corpus, private): (&Path, &Path, &Path),
    pass_started: u64,
) -> Result<Option<bulkload_agent::reseat::Summary>> {
    let dest_home = dest_home(run)?;
    let codex_root = run
        .codex_root
        .clone()
        .unwrap_or_else(|| home.join(".codex"));
    let mut args: Vec<OsString> = vec![
        "--lanes-from-plan".into(),
        "--plan".into(),
        plan.as_os_str().to_owned(),
        "--corpus".into(),
        corpus.as_os_str().to_owned(),
        "--source".into(),
        run.source.clone().into(),
        "--private-state".into(),
        private.as_os_str().to_owned(),
    ];
    for (from, to) in &run.maps {
        let mut pair = from.as_os_str().to_owned();
        pair.push("=");
        pair.push(to);
        args.extend(["--map".into(), pair]);
    }
    args.extend(run.reseat_args.iter().cloned());
    // Review of #280, finding 6: the landed index is the snapshot this
    // pass took, named by this pass's Codex transfer record, or none: never
    // the live index, never an earlier pass's snapshot.
    match landed_index(run, &dest_home.join(".codex"), pass_started) {
        Some(index) => args.extend(["--landing-codex-index".into(), index.into_os_string()]),
        None => args.push("--no-landing-codex-index".into()),
    }
    args.extend([
        "--codex-source-root".into(),
        codex_root.into_os_string(),
        home.as_os_str().to_owned(),
        dest_home.clone().into_os_string(),
        dest_home.into_os_string(),
        run.state.join("reseat").into_os_string(),
    ]);
    super::reseat_run(&args)
}
