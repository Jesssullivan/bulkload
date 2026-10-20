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

// The end of every pass (SPEC rule 2): `states` measured, each payload
// file named, and one state line. Payload in a state outranks the pass's
// own result: the verb refuses `STATE_PAYLOAD_LEFT`.
fn finish(verb: &str, states: &[&Path], passed: Result<()>) -> Result<()> {
    let measured = match bulkload_agent::state_payload::measure_all(states) {
        Ok(measured) => measured,
        Err(refusal) => return passed.and(Err(refusal)),
    };
    for path in &measured.payload {
        out(&format!("{verb} state=payload path={path:?}"))?;
    }
    let refusal = measured.refusal();
    out(&format!(
        "{verb} phase=state outcome={} {}",
        refusal.as_ref().map_or_else(
            || "ok".to_owned(),
            |refusal| format!("refused:{}", refusal.code())
        ),
        measured.words()
    ))?;
    refusal.map_or(passed, Err)
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
/// `MOVE_STATE/carry/plan.postcard` (`estate-add-batch`), and make
/// `MOVE_STATE/serve` and `MOVE_STATE/serve/carry`, where the destination
/// half's `pull`s and its carry phase keep their `serve` states (#276).
///
/// SPEC rule 2 (reset 2026-10-20): the state holds ledgers and resume
/// marks only. A state holding a retired staging format is refused at
/// startup (`STATE_FORMAT_RETIRED`, each one named); the capture, which
/// would write bundles and repository copies into the state, is refused
/// before it writes (`STATE_PAYLOAD_LEFT`) for any non-empty plan until the
/// streamed capture replaces it; and every pass ends with its state line
/// (`state_payload_bytes=`), refusing `STATE_PAYLOAD_LEFT` when the state
/// measures any payload.
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
    // SPEC rule 2 (reset 2026-10-20): a retired staging format in the state
    // is named and refused before anything is read, written or reused.
    bulkload_agent::state_payload::refuse_retired(&[&state], &mut |path| {
        out(&format!("estate-move-source state=retired path={path:?}"))
    })?;
    let passed = source_pass(&state, &roots, &maps, count, rebuildable);
    finish("estate-move-source", &[&state], passed)
}

/// One source pass over `roots` into `state` (see [`source_command`]).
#[allow(clippy::too_many_lines)] // The inventory, then the capture.
fn source_pass(
    state: &Path,
    roots: &[PathBuf],
    maps: &[(PathBuf, PathBuf)],
    count: usize,
    rebuildable: bool,
) -> Result<()> {
    moving::private_dir(state)?;
    let _lock = moving::lock(state)?;
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
    // The pass's mark: `estate-move` refuses a corpus pulled while a pass
    // was still writing it (its `done` mark is another pass's, or none).
    let pass = now_ns().to_string();
    moving::write_report(&carry.join(STARTED), pass.as_bytes())?;
    let plan = carry.join("plan.postcard");
    let found = moving::inventory(roots, maps)?;
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
        "estate-move-source phase=inventory outcome=ok items={} new={} planned={planned} symlinks={} not_git_files={} prunable={} unreadable={} source_vanished={} not_in_census={}",
        found.items.len(),
        new.len(),
        noted("symlink"),
        noted("not-git"),
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
    // SPEC rule 2: a capture writes its bundles and repository copies into
    // the state (`private/`, `carry/corpus`), so it is refused before it
    // writes; an empty plan captures nothing. The streamed capture replaces
    // it.
    let staged = || {
        bulkload_agent::state_payload::refuse_staging(state, &carry.join("corpus"))?;
        bulkload_agent::state_payload::refuse_staging(state, &state.join("private"))
    };
    let capture = if planned == 0 {
        Ok(())
    } else {
        staged().and_then(|()| {
            estate::capture_with_policy(
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
                            nests.lock().map_err(|_| BulkloadRefusal::WorkerLost)?.push(
                                moving::Note {
                                    path: row.source.clone(),
                                    what: "nested",
                                    count: 1,
                                    code: None,
                                },
                            );
                        }
                    }
                    super::print_receipt(row)
                },
            )
        })
    };
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
            moving::render_notes(&all_notes).as_bytes(),
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

    // The state directories this run measures and checks: its own, and the
    // source half's when it is on this host.
    fn states(&self) -> Vec<&Path> {
        if self.local() {
            vec![self.state.as_path(), self.source_state.as_path()]
        } else {
            vec![self.state.as_path()]
        }
    }

    // One recorded transfer of the source's `from` into `to`: `copy` on
    // this host, else `pull` from HOST; `serve` keeps its source state in
    // the source half's `MOVE_STATE/serve/NAME`, this side in
    // `MOVE_STATE/pull/NAME`.
    fn transfer(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        moving::private_dir(to)?;
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
///
/// SPEC rule 2 (reset 2026-10-20): a state holding a retired staging
/// format (this one, or the source half's on this host) is refused at
/// startup, each one named (`STATE_FORMAT_RETIRED`); a pull of the corpus
/// or of the session trees into the state is refused before it writes
/// (`STATE_PAYLOAD_LEFT`), so a pass refuses at its pull until the streamed
/// capture replaces it; every pass ends with its state line, and the
/// closure line carries `state_payload_bytes=`.
fn move_command(args: &[OsString]) -> Result<()> {
    let run = Move::parse(args)?;
    bulkload_agent::state_payload::refuse_retired(&run.states(), &mut |path| {
        out(&format!("estate-move state=retired path={path:?}"))
    })?;
    let passed = move_pass(&run);
    finish("estate-move", &run.states(), passed)
}

/// One destination pass (see [`move_command`]).
#[allow(clippy::too_many_lines)]
fn move_pass(run: &Move) -> Result<()> {
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

    // 1. The source half's plan and corpus, from a finished pass only. A
    // corpus pulled into the state is staging (SPEC rule 2): refused before
    // the pull writes.
    let pulled = bulkload_agent::state_payload::refuse_staging(&run.state, &carry.join("corpus"))
        .and_then(|()| run.transfer(&run.source_state.join("carry"), &carry, "corpus"))
        .and_then(|()| finished_pass(&carry));
    reached(&mut ledger, &ledger_path, "pull", word(&pulled))?;
    pulled?;
    let text = |name: &str| std::fs::read_to_string(carry.join(name)).unwrap_or_default();
    let refusals = moving::parse_refusals(&text(REFUSALS));
    let notes = moving::note_counts(&text(NOTES));

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
    if run.plan_only {
        return preview(run, (&plan, &corpus, &private));
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
    let carried = carry_files(run, &rows, (&plan, &corpus, &private));
    note(&carried);
    reached(&mut ledger, &ledger_path, "carry", word(&carried))?;

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

    // 6. Sessions: the agent state trees, then the re-seat (TIN-6587),
    // only over a complete landing.
    if let Some(home) = &run.sessions {
        let pulled = sessions_pull(run, home);
        note(&pulled);
        reached(&mut ledger, &ledger_path, "sessions-pull", word(&pulled))?;
        if pulled.is_ok() {
            let reseated = reseat(run, home, (&plan, &corpus, &private));
            note(&reseated);
            reached(&mut ledger, &ledger_path, "reseat", word(&reseated))?;
        } else {
            reached(
                &mut ledger,
                &ledger_path,
                "reseat",
                "skipped:sessions-pull-refused".to_owned(),
            )?;
        }
    }

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
    if let Some(dispositions) = &run.dispositions {
        report.dispose(&bulkload_agent::disposition::Ledger::read(
            dispositions,
            &plan,
            &run.source,
        )?);
    }
    let report_path = run.state.join("closure.json");
    moving::write_report(&report_path, report.to_json().as_bytes())?;
    let refusals = report
        .refusals
        .iter()
        .map(|(code, count)| format!("{code}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    let not_moved: u64 = notes.values().sum();
    let measured = bulkload_agent::state_payload::measure_all(&run.states())?;
    out(&format!(
        "estate-move closure planned={} applied={} applied_beside={} carried={} referenced_only={} refused={} refused_pending_review={} unaccounted={} not_moved={not_moved} refusals={} {} report={report_path:?}",
        rows.len(),
        report.applied,
        report.applied_beside,
        report.carried,
        report.referenced_only,
        report.refused,
        report.refused_pending_review,
        report.unaccounted,
        if refusals.is_empty() { "none" } else { &refusals },
        measured.words(),
    ))?;
    let gate = report.gate().and(if not_moved == 0 {
        Ok(())
    } else {
        Err(BulkloadRefusal::ClosureUnaccounted)
    });
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

// Phase 6a: SOURCE_HOME/.claude and the Codex root into MOVE_STATE/landing.
// A landing area in the state is staging (SPEC rule 2): refused before it
// is made.
fn sessions_pull(run: &Move, home: &Path) -> Result<()> {
    let landing = run.state.join("landing");
    bulkload_agent::state_payload::refuse_staging(&run.state, &landing)?;
    moving::private_dir(&landing)?;
    let codex_root = run
        .codex_root
        .clone()
        .unwrap_or_else(|| home.join(".codex"));
    let claude = run.transfer(&home.join(".claude"), &landing.join(".claude"), "claude");
    let codex = run.transfer(&codex_root, &landing.join(".codex"), "codex");
    claude.and(codex)
}

// Phase 6b: `estate-reseat --lanes-from-plan` of the landing into DEST_HOME.
fn reseat(run: &Move, home: &Path, (plan, corpus, private): (&Path, &Path, &Path)) -> Result<()> {
    let dest_home = match &run.dest_home {
        Some(home) => home.clone(),
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?,
    };
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
    args.extend([
        "--codex-source-root".into(),
        codex_root.into_os_string(),
        home.as_os_str().to_owned(),
        run.state.join("landing").into_os_string(),
        dest_home.into_os_string(),
        run.state.join("reseat").into_os_string(),
    ]);
    super::reseat_command(&args)
}
