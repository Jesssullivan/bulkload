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

/// What phase 6 leaves for the closure (C11): the two session transfer
/// states, and the re-seat's summary when it ran.
struct Sessions {
    pulls: Vec<PathBuf>,
    reseat: Option<bulkload_agent::reseat::Summary>,
}

/// What one `estate-move` run does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One pass of the move.
    Move,
    /// `--plan-only`: the decisions shown, never recorded.
    PlanOnly,
    /// `--key-map-proof` (#280 finding 9): report only, from the plan an
    /// earlier pass pulled; nothing is pulled, locked or written.
    KeyMapProof,
}

/// The real `.claude` and `.codex` this pass's pull ran under, resolved
/// once and owner-checked ([`Move::live_root`]): the re-seat runs under
/// exactly these (independent review of #280, N1).
struct Homes {
    claude: PathBuf,
    codex: PathBuf,
}

/// The real directory `path` resolves to, when this user owns it.
fn owned_real(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt as _;
    let real = std::fs::canonicalize(path).refuse_at("estate_move::owned_real")?;
    let resolved = std::fs::symlink_metadata(&real).refuse_at("estate_move::owned_real")?;
    // SAFETY: geteuid has no preconditions or side effects.
    let me = unsafe { libc::geteuid() };
    if !resolved.is_dir() || resolved.uid() != me {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(real)
}

/// Whether the live home `live` still resolves to `real`, the directory
/// this pass's pull ran under: a home relinked since is refused
/// `PATH_ESCAPES_ROOT`, so no re-seat runs over a tree the pull never
/// wrote (independent review of #280, N1).
fn same_home(live: &Path, real: &Path) -> Result<()> {
    if owned_real(live)? == real {
        Ok(())
    } else {
        Err(BulkloadRefusal::PathEscapesRoot)
    }
}

/// Write a session's source listing (NUL-ended relative paths, sorted).
///
/// Provenance is sticky (verification of #280, N4 across passes): the
/// listing is cumulative, every path an earlier pass listed and this pass's,
/// so a session the source pruned since stays the source's (its earlier
/// typed outcome stands until dispositioned). A pass that listed nothing
/// (its session did not walk) keeps the earlier listing as it is.
fn write_listing(path: &Path, listed: Option<Vec<Vec<u8>>>) -> Result<()> {
    let Some(listed) = listed else {
        return Ok(());
    };
    let mut all = std::collections::BTreeSet::new();
    match std::fs::read(path) {
        Ok(bytes) => all.extend(
            bytes
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(<[u8]>::to_vec),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).refuse_at("estate_move::write_listing"),
    }
    all.extend(listed);
    let mut bytes = Vec::new();
    for entry in all {
        bytes.extend_from_slice(&entry);
        bytes.push(0);
    }
    moving::write_report(path, &bytes)
}

/// The destination half's arguments.
struct Move {
    mode: Mode,
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
    /// `--replace-archived-dest ROOT` (A-106, second review of #280 N2):
    /// passed to `estate-reseat`, so a destination transcript or rollout
    /// that links into ROOT is replaced by the source's copy of that session
    /// (the archived fork stays in ROOT).
    archived: Vec<PathBuf>,
    /// `--live-link-root ROOT` (operator interview 2026-10-21, "Union into
    /// link target"): a home directory that is a link into ROOT is the live
    /// directory itself, merged at its real path; the link stays.
    live_link_roots: Vec<PathBuf>,
    /// `--regularize-link-root ROOT` (A-055, 2026-10-21): a link in a home
    /// into ROOT (a repair stage) is regularized before the pull: its live
    /// name becomes a real file or directory holding the target's bytes,
    /// which the pull then unions with the source's; ROOT is never touched.
    regularize_roots: Vec<PathBuf>,
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
            mode: Mode::Move,
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
            archived: Vec::new(),
            live_link_roots: Vec::new(),
            regularize_roots: Vec::new(),
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
                "--plan-only" if parsed.mode == Mode::Move => {
                    parsed.mode = Mode::PlanOnly;
                    step = 1;
                }
                "--key-map-proof" if parsed.mode == Mode::Move => {
                    parsed.mode = Mode::KeyMapProof;
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
                "--replace-archived-dest" => parsed.archived.push(absolute(args.get(at + 1))?),
                "--live-link-root" => parsed.live_link_roots.push(absolute(args.get(at + 1))?),
                "--regularize-link-root" => {
                    parsed.regularize_roots.push(absolute(args.get(at + 1))?);
                }
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
        // The proof reads the session homes.
        if parsed.mode == Mode::KeyMapProof && parsed.sessions.is_none() {
            return Err(BulkloadRefusal::RequiredFieldMissing);
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
                || !parsed.reseat_args.is_empty()
                || !parsed.archived.is_empty()
                || !parsed.live_link_roots.is_empty()
                || !parsed.regularize_roots.is_empty())
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

    // [`Self::transfer`] of the source half's `carry`, its corpus excluded:
    // only the plan and the pass marks land (SPEC rule 2).
    fn transfer_ledgers(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        moving::private_dir(to)?;
        let root = std::fs::canonicalize(to).refuse_at("estate_move::transfer_ledgers")?;
        bulkload_agent::transfer::set_excluded(&root, Some(vec![b"corpus".to_vec()]));
        let moved = self.transfer(from, to, name);
        bulkload_agent::transfer::set_excluded(&root, None);
        moved
    }

    // One recorded transfer of the source's `from` into `to`: `copy` on
    // this host, else `pull` from HOST; `serve` keeps its source state in
    // the source half's `MOVE_STATE/serve/NAME`, this side in
    // `MOVE_STATE/pull/NAME`. `to` is made private (0700) first.
    fn transfer(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        moving::private_dir(to)?;
        self.transfer_into(from, to, name)
    }

    // The real root a transfer into a live destination tree runs under (the
    // agent's own home
    // directory, plan of record 2026-10-19 C9): created 0700 when absent,
    // else taken as it is, whatever its mode (its mode is the agent's, not
    // bulkload's to tighten). A symlink there (sting's `~/.claude` and
    // `~/.codex` link to `/srv/fast-local/jess/state/{claude,codex}`; second
    // review of #280, N1) is resolved once: its real path must be a
    // directory this user owns, and the transfer then runs under that real
    // path, following no further link; the line below and the transfer's
    // record (its destination is the real path) name it. Anything else
    // refuses PATH_ESCAPES_ROOT.
    fn live_root(to: &Path, name: &str) -> Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt as _;
        match std::fs::DirBuilder::new().mode(0o700).create(to) {
            Ok(()) => Ok(to.to_path_buf()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let found = std::fs::symlink_metadata(to).refuse_at("estate_move::live_root")?;
                if found.is_dir() {
                    return Ok(to.to_path_buf());
                }
                if !found.file_type().is_symlink() {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
                let real = owned_real(to)?;
                out(&format!(
                    "estate-move sessions-home name={name} live={to:?} real={real:?}"
                ))?;
                Ok(real)
            }
            Err(error) => Err(bulkload_agent::refuse::io(&error, "estate_move::live_root")),
        }
    }

    fn transfer_into(&self, from: &Path, to: &Path, name: &str) -> Result<()> {
        self.transfer_kind(bulkload_agent::transfer::RootKind::Tree, from, to, name)
    }

    /// [`Self::transfer_into`] of one file (`--file`, #232), landed as `to`.
    fn transfer_kind(
        &self,
        kind: bulkload_agent::transfer::RootKind,
        from: &Path,
        to: &Path,
        name: &str,
    ) -> Result<()> {
        let pulls = self.state.join("pull");
        moving::private_dir(&pulls)?;
        let serve = self.source_state.join("serve").join(name);
        if self.local() {
            moving::private_dir(&self.source_state.join("serve"))?;
            return super::copy_recorded_kind(kind, from, to, &serve, &pulls.join(name));
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
        super::pull_recorded_kind(kind, &args)
    }

    /// The consolidation of the homes (operator ruling 2026-10-20): the
    /// `--map` set and the source home as the destination's, the ledger in
    /// `MOVE_STATE/consolidate`.
    fn consolidation(
        &self,
        home: &Path,
        plan_only: bool,
    ) -> Result<bulkload_agent::transfer::Consolidation> {
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let mut pairs: Vec<(String, String)> = self
            .maps
            .iter()
            .map(|(from, to)| (text(from), text(to)))
            .collect();
        pairs.push((text(home), text(&dest_home(self)?)));
        Ok(bulkload_agent::transfer::Consolidation {
            maps: bulkload_agent::consolidate::Maps::new(pairs),
            ledger: self.state.join("consolidate"),
            plan_only,
            archived: self.archived.clone(),
        })
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
/// startup, each one named (`STATE_FORMAT_RETIRED`); the pull lands only
/// the source half's plan and marks (a corpus is excluded), a plan with Git
/// items refuses `STATE_PAYLOAD_LEFT` at the pull until the streamed
/// capture replaces the corpus apply, and a plan with none (the homes pass,
/// `--sessions`) goes on; every pass ends with its state line, and the
/// closure line carries `state_payload_bytes=`.
fn move_command(args: &[OsString]) -> Result<()> {
    let run = Move::parse(args)?;
    bulkload_agent::state_payload::refuse_retired(&run.states(), &mut |path| {
        out(&format!("estate-move state=retired path={path:?}"))
    })?;
    let passed = move_pass(&run);
    // `--key-map-proof` is report-only (#294): it writes nothing and prints
    // no phase line, the state line included.
    if run.mode == Mode::KeyMapProof {
        return passed;
    }
    finish("estate-move", &run.states(), passed)
}

/// One destination pass (see [`move_command`]).
#[allow(clippy::too_many_lines)]
fn move_pass(run: &Move) -> Result<()> {
    if run.mode == Mode::KeyMapProof {
        return key_map_proof(run);
    }
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

    // 1. The source half's plan and pass marks, from a finished pass only.
    // SPEC rule 2: only its ledgers are pulled; a corpus (a source half
    // from before the reset) is excluded, never landed or read. A plan with
    // Git items needs the corpus apply, refused until the streamed capture
    // replaces it; a plan with none (the homes pass: `--sessions` only)
    // goes on.
    let pulled = run
        .transfer_ledgers(&run.source_state.join("carry"), &carry, "plan")
        .and_then(|()| {
            if estate::inspect(&plan)?.is_empty() {
                Ok(())
            } else {
                bulkload_agent::state_payload::refuse_staging(&run.state, &carry.join("corpus"))
            }
        })
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
    if run.mode == Mode::PlanOnly {
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

    // 6. Sessions (plan of record 2026-10-19, C9/C10): the agent state
    // trees into the live home, then the re-seat (TIN-6587), whenever both
    // session transfers completed, path refusals included: those stay
    // typed closure rows, and a refused settings file never holds the
    // transcripts' re-seat.
    let mut key_maps = None;
    let sessions: Option<Sessions> = if let Some(home) = &run.sessions {
        // #280 finding 9: the source's project keys land at their lanes'
        // destination keys, once; ambiguous keys stay, typed.
        let (pulled, homes, derived) = sessions_pull(run, home, &plan);
        key_maps = derived;
        note(&pulled);
        reached(&mut ledger, &ledger_path, "sessions-pull", word(&pulled))?;
        let mut outcome = Sessions {
            // Every session pull of the pass, the live links' included; a
            // plan's are never this pass's records.
            pulls: session_pulls(&run.state.join("pull")),
            reseat: None,
        };
        if let (true, Some(homes)) = (session_completed(&pulled), &homes) {
            let reseated = reseat(run, home, homes, (&plan, &corpus, &private));
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
    // consolidated entries accounted, their held paths pending
    // review, a transfer that never ran unaccounted), so the gate cannot
    // pass with agent state unaccounted.
    if let Some(sessions) = &sessions {
        report.join_transfers(&sessions.pulls)?;
    }
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
    let not_moved: u64 = notes.values().sum();
    let measured = bulkload_agent::state_payload::measure_all(&run.states())?;
    let held = reseat_held
        .iter()
        .map(|(word, count)| format!("{word}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    out(&format!(
        "estate-move closure planned={} applied={} applied_beside={} carried={} referenced_only={} refused={} refused_pending_review={} unaccounted={} not_moved={not_moved} refusals={} sessions={} transfer_rows={} reseat_unaccounted={reseat_unaccounted} reseat_held={} key_maps={} key_maps_held={} key_maps_unused={} diverged={} move_state_bytes={} pull_payload_bytes={} {} report={report_path:?}",
        rows.len(),
        report.applied,
        report.applied_beside,
        report.carried,
        report.referenced_only,
        report.refused,
        report.refused_pending_review,
        report.unaccounted,
        if refusals.is_empty() { "none" } else { &refusals },
        if sessions.is_some() { "claude,codex" } else { "none" },
        report.transfer_rows(),
        if held.is_empty() { "none" } else { &held },
        key_maps.as_ref().map_or(0, |maps| maps.maps.len()),
        key_maps.as_ref().map_or(0, |maps| maps.held.len()),
        key_maps.as_ref().map_or(0, |maps| maps.unused.len()),
        reseat_held
            .iter()
            .filter(|(word, _)| word.ends_with("/beside-diverged"))
            .map(|(_, count)| count)
            .sum::<u64>(),
        tree_bytes(&run.state, &|_| true),
        pull_payload_bytes(&run.state),
        measured.words(),
    ))?;
    let gate = report
        .gate()
        .and(if not_moved == 0 && reseat_unaccounted == 0 {
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
    if let Some(home) = &run.sessions {
        homes_plan(run, home, plan)?;
    }
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

/// Claude Code's user config beside `~/.claude`, and its pull's name.
const CLAUDE_JSON: &str = ".claude.json";
const CLAUDE_JSON_PULL: &str = "claude-json";

// `--plan-only` with `--sessions` (operator ruling 2026-10-20, item 3): every
// file of both homes and `~/.claude.json`, with its class, its action
// (create, adopt, extend, covered, consolidate or hold, and why) and the
// sizes on both sides, printed as `planned` lines. Nothing is staged,
// written or recorded; a live home that does not exist yet is named, not
// made.
fn homes_plan(run: &Move, home: &Path, plan: &Path) -> Result<()> {
    let dest = dest_home(run)?;
    bulkload_agent::transfer::set_consolidate(Some(run.consolidation(home, true)?));
    let codex_from = run
        .codex_root
        .clone()
        .unwrap_or_else(|| home.join(".codex"));
    let mut planned = Ok(());
    for (from, name, kind) in [
        (
            home.join(".claude"),
            ".claude",
            bulkload_agent::transfer::RootKind::Tree,
        ),
        (
            home.join(CLAUDE_JSON),
            CLAUDE_JSON,
            bulkload_agent::transfer::RootKind::File,
        ),
        (
            codex_from,
            ".codex",
            bulkload_agent::transfer::RootKind::Tree,
        ),
    ] {
        let mut to = dest.join(name);
        out(&format!("estate-move homes-plan name={name} live={to:?}"))?;
        if kind == bulkload_agent::transfer::RootKind::Tree {
            // The real pass's live root (sting's are links into
            // /srv/fast-local/jess/state), resolved and owner-checked as
            // `sessions_pull` resolves it, but never created: report only.
            match std::fs::symlink_metadata(&to) {
                Ok(found) if found.is_dir() => {}
                Ok(found) if found.file_type().is_symlink() => {
                    let real = owned_real(&to)?;
                    out(&format!(
                        "estate-move sessions-home name={} live={to:?} real={real:?}",
                        name.trim_start_matches('.')
                    ))?;
                    to = real;
                }
                Ok(_) => {
                    out(&format!(
                        "estate-move homes-plan name={name} refused={}",
                        BulkloadRefusal::PathEscapesRoot.code()
                    ))?;
                    planned = planned.and(Err(BulkloadRefusal::PathEscapesRoot));
                    continue;
                }
                Err(_) => {
                    out(&format!(
                        "estate-move homes-plan name={name} live=absent (every file would be created)"
                    ))?;
                    continue;
                }
            }
        }
        // #280 finding 9: the Claude tree is planned with the key map the
        // real pass derives, so its rows (and its source-key folds) are the
        // real pass's.
        let claude = name == ".claude";
        if claude {
            let maps = derive_key_maps(run, home, plan, &to)?;
            print_key_maps(&maps)?;
            bulkload_agent::transfer::set_map_prefixes(maps.prefixes())?;
        }
        let tree = kind == bulkload_agent::transfer::RootKind::Tree;
        let short = name.trim_start_matches('.');
        let links = if tree {
            // What the real pass regularizes first (counted, nothing
            // written), and the live links it merges at their real
            // directories.
            regularize_home(run, short, &to, true)?;
            bulkload_agent::reseat::links::live_links(&to, &run.live_link_roots)
        } else {
            Vec::new()
        };
        let pull = format!("plan-{short}");
        let pulled = if tree {
            pull_home(run, &from, &to, &pull, &links)
        } else {
            run.transfer_kind(kind, &from, &to, &pull)
        };
        if claude {
            bulkload_agent::transfer::set_map_prefixes(Vec::new())?;
        }
        let linked = pull_links(run, &from, &pull, &links);
        let pulled = pulled.and(linked);
        // Report-only: a path refusal (an absent `~/.claude.json` among
        // them) is printed with the plan; only a session that failed fails
        // it, and every refusal is named.
        if let Err(refusal) = pulled {
            out(&format!(
                "estate-move homes-plan name={name} refused={}",
                refusal.code()
            ))?;
            if !matches!(
                refusal,
                BulkloadRefusal::SourceRootMissing | BulkloadRefusal::ItemsRefused
            ) && planned.is_ok()
            {
                planned = Err(refusal);
            }
        }
    }
    bulkload_agent::transfer::set_consolidate(None);
    planned
}

/// The session pulls' states under `pulls`: every directory but a plan's.
fn session_pulls(pulls: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(pulls)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with("plan-")
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

// A-055 (2026-10-21): the home's links into a declared regularize root
// become real files or directories before the pull (`plan`: counted only),
// with a summary line; the root is never written.
fn regularize_home(run: &Move, name: &str, real: &Path, plan: bool) -> Result<()> {
    if run.regularize_roots.is_empty() {
        return Ok(());
    }
    let done = bulkload_agent::reseat::links::regularize(
        real,
        &run.regularize_roots,
        plan,
        &mut |line| out(line),
    )?;
    out(&format!(
        "estate-move regularized-links home={name} links={} files={} bytes={} remaining={}{}",
        done.links,
        done.files,
        done.bytes,
        done.remaining,
        if plan { " (plan only)" } else { "" }
    ))
}

// The home's own pull, its live links skipped (each is merged at its real
// directory by `pull_links`).
fn pull_home(
    run: &Move,
    from: &Path,
    real: &Path,
    name: &str,
    links: &[(std::ffi::OsString, PathBuf)],
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    bulkload_agent::transfer::set_skip(
        links
            .iter()
            .map(|(entry, _)| entry.as_bytes().to_vec())
            .collect(),
    );
    let pulled = run.transfer_into(from, real, name);
    bulkload_agent::transfer::set_skip(Vec::new());
    pulled
}

// Operator interview 2026-10-21 ("Union into link target"): each live link
// of the home (a directory linking into a --live-link-root) is the live
// directory itself: the source's same-named directory is merged there by
// the consolidate rules, its own transfer record; the link stays. A source
// with no such directory has nothing to merge.
fn pull_links(
    run: &Move,
    from: &Path,
    name: &str,
    links: &[(std::ffi::OsString, PathBuf)],
) -> Result<()> {
    bulkload_agent::transfer::set_listing(Vec::new());
    let mut result = Ok(());
    for (entry, target) in links {
        out(&format!(
            "estate-move live-link home={name} name={entry:?} target={target:?}"
        ))?;
        let pull = format!(
            "{name}-link-{}",
            entry
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
        );
        match run.transfer_into(&from.join(entry), target, &pull) {
            Ok(()) | Err(BulkloadRefusal::SourceRootMissing) => {}
            Err(refusal) => {
                if result.is_ok() {
                    result = Err(refusal);
                }
            }
        }
    }
    let _ = super::take_listed();
    result
}

// Phase 6a (plan of record 2026-10-19, C9; SPEC R4 "each tree lands once,
// at its live path"): SOURCE_HOME/.claude and the Codex root into the LIVE
// DEST_HOME/.claude and DEST_HOME/.codex, with `--beside .LABEL`: equal
// files adopted, grown append-only files extended, differing files (and the
// live Codex index's snapshot) landed beside as `<path>.LABEL`, nothing
// overwritten and nothing staged. Both trees are pulled whatever the first
// returned; the first refusal is the phase's. `ITEMS_REFUSED` means the
// session completed with typed path refusals (closure rows), which does
// not stop the re-seat (C10).
#[allow(clippy::too_many_lines)] // Three pulls, the key map and its notes.
fn sessions_pull(
    run: &Move,
    home: &Path,
    plan: &Path,
) -> (
    Result<()>,
    Option<Homes>,
    Option<bulkload_agent::reseat::keymap::KeyMaps>,
) {
    let dest = match dest_home(run) {
        Ok(dest) => dest,
        Err(refusal) => return (Err(refusal), None, None),
    };
    let codex_from = run
        .codex_root
        .clone()
        .unwrap_or_else(|| home.join(".codex"));
    let pulls = run.state.join("pull");
    // Consolidated, never beside (operator ruling 2026-10-20): every
    // differing live file is merged by its class's union, or held typed.
    let suffix = run
        .consolidation(home, false)
        .map(|settings| bulkload_agent::transfer::set_consolidate(Some(settings)));
    // The source's transcripts and rollouts, listed by each session: the
    // provenance the re-seat tells them from the destination's own by
    // (independent review of #280, N4). Listed as they land: under their
    // mapped keys.
    bulkload_agent::transfer::set_listing(vec![b"projects/".to_vec()]);
    let _ = super::take_listed();
    let claude_root = Move::live_root(&dest.join(".claude"), "claude");
    let claude_links = claude_root
        .as_ref()
        .map(|real| bulkload_agent::reseat::links::live_links(real, &run.live_link_roots))
        .unwrap_or_default();
    // #280 finding 9: the Claude tree with the key map of this pass's lanes,
    // derived for the real, owner-checked home the pull runs under; the
    // Codex tree without (its rollouts are kept by date, not by working
    // directory). A map that cannot be derived or set holds the Claude pull:
    // pulled unmapped, every re-keyed transcript would land twice.
    let key_maps = claude_root
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|real| derive_key_maps(run, home, plan, real));
    let claude = key_maps
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|maps| {
            print_key_maps(maps)?;
            bulkload_agent::transfer::set_map_prefixes(maps.prefixes())
        })
        .and_then(|()| {
            claude_root.as_ref().map_err(Clone::clone).and_then(|real| {
                regularize_home(run, "claude", real, false)?;
                pull_home(run, &home.join(".claude"), real, "claude", &claude_links)
            })
        });
    let unmapped = bulkload_agent::transfer::set_map_prefixes(Vec::new());
    let claude_listing = super::take_listed();
    let claude_linked = pull_links(run, &home.join(".claude"), "claude", &claude_links);
    // #294 review 4: a derived map the landed sessions of its lane did not
    // use (the source keyed them otherwise) leaves them to be re-seated as
    // before, a second copy: named, typed.
    let mut key_maps = key_maps;
    let noted = match (&mut key_maps, &claude_root, &claude_listing) {
        (Ok(maps), Ok(real), Some(listed)) => key_options(run, home, plan, real)
            .and_then(|options| bulkload_agent::reseat::keymap::unused(&options, maps, listed))
            .and_then(|unused| {
                for note in &unused {
                    out(&format!(
                        "estate-move key-map-unused from={} to={} seen={} cwd={:?} reason=source-keyed-otherwise",
                        note.from, note.to, note.seen, note.cwd
                    ))?;
                }
                maps.unused = unused;
                Ok(())
            }),
        _ => Ok(()),
    };
    let claude_listed = write_listing(&pulls.join("claude.listing"), claude_listing);
    // `~/.claude.json` (structured config), merged into the live one; a
    // source with none has nothing to merge, and leaves no record.
    let claude_json = run
        .transfer_kind(
            bulkload_agent::transfer::RootKind::File,
            &home.join(CLAUDE_JSON),
            &dest.join(CLAUDE_JSON),
            CLAUDE_JSON_PULL,
        )
        .or_else(|refusal| {
            // The source has none: nothing to merge, and no record left to
            // count (its one row would be the absent file's).
            if refusal == BulkloadRefusal::SourceRootMissing
                || source_had_none(&pulls.join(CLAUDE_JSON_PULL))
            {
                let _ = std::fs::remove_dir_all(pulls.join(CLAUDE_JSON_PULL));
                Ok(())
            } else {
                Err(refusal)
            }
        });
    // The Codex stores are the row union's (#278), never this file path:
    // held typed until it lands, and the live index is never written.
    bulkload_agent::transfer::set_listing(vec![
        b"sessions/".to_vec(),
        b"archived_sessions/".to_vec(),
    ]);
    let codex_root = Move::live_root(&dest.join(".codex"), "codex");
    let codex_links = codex_root
        .as_ref()
        .map(|real| bulkload_agent::reseat::links::live_links(real, &run.live_link_roots))
        .unwrap_or_default();
    let codex = codex_root.as_ref().map_err(Clone::clone).and_then(|real| {
        regularize_home(run, "codex", real, false)?;
        pull_home(run, &codex_from, real, "codex", &codex_links)
    });
    let codex_listed = write_listing(&pulls.join("codex.listing"), super::take_listed());
    let codex_linked = pull_links(run, &codex_from, "codex", &codex_links);
    bulkload_agent::transfer::set_listing(Vec::new());
    bulkload_agent::transfer::set_consolidate(None);
    let cleared = Ok(());
    // A session that did not complete outranks one that completed with
    // refused paths: the phase's word says whether the re-seat may run.
    let mut outcome = Ok(());
    for result in [
        suffix,
        claude,
        unmapped,
        noted,
        claude_listed,
        claude_linked,
        claude_json,
        codex,
        codex_listed,
        codex_linked,
        cleared,
    ] {
        if let Err(refusal) = result {
            if session_completed(&outcome) {
                outcome = Err(refusal);
            }
        }
    }
    let homes = match (claude_root, codex_root) {
        (Ok(claude), Ok(codex)) => Some(Homes { claude, codex }),
        _ => None,
    };
    (outcome, homes, key_maps.ok())
}

// Whether the one-file session recorded in `state` found no file at its
// source: it completed nothing, and its only rows are the source's absence.
fn source_had_none(state: &Path) -> bool {
    use bulkload_agent::transfer_outcome::{self, EntryOutcome, Session};
    let records: Vec<_> = transfer_outcome::records(state)
        .iter()
        .filter_map(|(_, path)| transfer_outcome::read_file(path)?.ok())
        .collect();
    !records.is_empty()
        && records.iter().all(|record| {
            record.session == Session::Completed
                && record.completed == 0
                && record.entries.iter().all(|entry| {
                    matches!(&entry.outcome, EntryOutcome::Refused { refusal, .. }
                        if matches!(refusal.code(), "SOURCE_VANISHED" | "SOURCE_ROOT_MISSING"))
                })
        })
}

// The lanes the re-seat resolves sessions by (`--lanes-from-plan`: the
// run's `--map` pairs, then the plan's items; `SOURCE_HOME/git` by
// default), as `estate-reseat` would build them for this pass.
fn lane_options(run: &Move, home: &Path, plan: &Path) -> Result<bulkload_agent::reseat::Options> {
    let dest = dest_home(run)?;
    let mut options =
        bulkload_agent::reseat::Options::new(home, &dest, &dest, &run.state.join("reseat"));
    let mut maps = run.maps.clone();
    for item in estate::inspect(plan)? {
        if let Some(workspace) = item.workspace {
            maps.push((item.source, workspace));
        }
    }
    options.maps = maps;
    Ok(options)
}

// #280 finding 9: the Claude project-key map of this pass's lanes
// (`reseat::keymap::derive`), for the real, owner-checked `.claude` the
// pull runs under (`claude`, independent review of #280, N1).
fn derive_key_maps(
    run: &Move,
    home: &Path,
    plan: &Path,
    claude: &Path,
) -> Result<bulkload_agent::reseat::keymap::KeyMaps> {
    bulkload_agent::reseat::keymap::derive(&key_options(run, home, plan, claude)?)
}

// The lanes ([`lane_options`]) over the real `.claude` (`claude`).
fn key_options(
    run: &Move,
    home: &Path,
    plan: &Path,
    claude: &Path,
) -> Result<bulkload_agent::reseat::Options> {
    let mut options = lane_options(run, home, plan)?;
    claude.clone_into(&mut options.dest_claude);
    claude.clone_into(&mut options.landing_claude);
    Ok(options)
}

// One line per mapped key and per key held unmapped (typed), then the
// counts.
fn print_key_maps(maps: &bulkload_agent::reseat::keymap::KeyMaps) -> Result<()> {
    for map in &maps.maps {
        out(&format!(
            "estate-move key-map from={} to={} cwd={:?}",
            map.from, map.to, map.cwd
        ))?;
    }
    for held in &maps.held {
        out(&format!(
            "estate-move key-map-held key={} reason={} to={} cwds={:?}",
            held.key,
            held.reason.word(),
            held.to.as_deref().unwrap_or("none"),
            held.cwds,
        ))?;
    }
    out(&format!(
        "estate-move key-maps mapped={} held={} same={}",
        maps.maps.len(),
        maps.held.len(),
        maps.same
    ))
}

// `--key-map-proof` (#280 finding 9): report only. From the plan an earlier
// pass pulled into MOVE_STATE/carry, the key map this pass would derive,
// then, for every file under each source-keyed directory of the live
// DEST_HOME/.claude/projects, whether its destination key holds it
// byte-identical or byte-prefix-extended (`reseat::keymap::proof`).
// Nothing is pulled, locked or written; the neo-key directories stay.
fn key_map_proof(run: &Move) -> Result<()> {
    let home = run
        .sessions
        .as_ref()
        .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    let plan = run.state.join("carry").join("plan.postcard");
    // The live `.claude` resolved and owner-checked as the pull resolves
    // it (N1), never created: report only.
    let claude = owned_real(&dest_home(run)?.join(".claude"))?;
    let options = key_options(run, home, &plan, &claude)?;
    let maps = bulkload_agent::reseat::keymap::derive(&options)?;
    print_key_maps(&maps)?;
    let summary = bulkload_agent::reseat::keymap::proof(&options, &maps, &mut |line| out(line))?;
    let counts = summary
        .files
        .iter()
        .map(|(word, count)| format!("{word}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    out(&format!(
        "estate-move key-map-proof dirs={} files={} statuses={}",
        summary.dirs,
        summary.files.values().sum::<u64>(),
        if counts.is_empty() { "none" } else { &counts },
    ))
}

// Bytes of every regular file under `root` that `keep` takes (by path),
// never following a link.
fn tree_bytes(root: &Path, keep: &dyn Fn(&Path) -> bool) -> u64 {
    let mut total = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() && keep(&entry.path()) {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

// Operator ruling 2026-10-20 ("no stage dupes"): bytes under
// MOVE_STATE/pull/{claude,codex} that are not the pulls' resume state (the
// transfer store, its lock and its closure records): a staged payload left
// there. Reported on the closure line.
fn pull_payload_bytes(state: &Path) -> u64 {
    ["claude", "codex"]
        .iter()
        .map(|name| {
            tree_bytes(&state.join("pull").join(name), &|path: &Path| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                let record = path
                    .components()
                    .any(|part| part.as_os_str() == bulkload_agent::transfer_outcome::RECORD_DIR);
                let lock = path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"));
                // The store and the consolidation ledger are state, not
                // payload.
                !(record
                    || name.starts_with("transfer.sqlite")
                    || name.starts_with("consolidate.sqlite")
                    || lock)
            })
        })
        .sum()
}

// Whether every session transfer completed: every path answered, refused
// ones as typed closure rows (`ITEMS_REFUSED`), or none.
const fn session_completed(pulled: &Result<()>) -> bool {
    matches!(pulled, Ok(()) | Err(BulkloadRefusal::ItemsRefused))
}

// Phase 6b: `estate-reseat --lanes-from-plan` from the live home (the
// landing) into DEST_HOME. No Codex index lands as a file (the stores are
// the row union's), so the re-seat reads none.
fn reseat(
    run: &Move,
    home: &Path,
    homes: &Homes,
    (plan, corpus, private): (&Path, &Path, &Path),
) -> Result<Option<bulkload_agent::reseat::Summary>> {
    let dest_home = dest_home(run)?;
    // The homes must still be the directories the pull ran under (N1).
    same_home(&dest_home.join(".claude"), &homes.claude)?;
    same_home(&dest_home.join(".codex"), &homes.codex)?;
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
    // Consolidated homes (operator ruling 2026-10-20): nothing lands
    // beside, so no beside suffix. A-106 is the pull's union now: a
    // transcript linked into a --replace-archived-dest root was unioned with
    // the archived file and regularized there (the archive copy stays), so
    // the re-seat is never asked to replace a link.
    if !run.archived.is_empty() {
        out("estate-move a106=union-at-pull")?;
    }
    // Review of #280, finding 6: the landed index is the snapshot this
    // pass took, named by this pass's Codex transfer record, or none: never
    // the live index, never an earlier pass's snapshot.
    let pulls = run.state.join("pull");
    args.extend([
        "--dest-claude".into(),
        homes.claude.clone().into_os_string(),
        "--landing-claude".into(),
        homes.claude.clone().into_os_string(),
        "--dest-codex".into(),
        homes.codex.clone().into_os_string(),
        "--landing-codex".into(),
        homes.codex.clone().into_os_string(),
    ]);
    for (flag, name) in [
        ("--claude-listing", "claude.listing"),
        ("--codex-listing", "codex.listing"),
    ] {
        let listing = pulls.join(name);
        if listing.is_file() {
            args.extend([flag.into(), listing.into_os_string()]);
        }
    }
    // No Codex index lands as a file: the stores are the row union's.
    args.push("--no-landing-codex-index".into());
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
