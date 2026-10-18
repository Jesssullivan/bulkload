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
pub fn command(verb: &str, args: &[OsString]) -> Result<()> {
    if verb == "estate-move-source" {
        source_command(args)
    } else {
        move_command(args)
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
/// `MOVE_STATE/private`). A capture refusal is an item's (its line says
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
    let existing = if plan.try_exists().refuse_at("estate_move::source")? {
        estate::inspect(&plan)?
    } else {
        Vec::new()
    };
    let new: Vec<estate::Item> = found
        .items
        .iter()
        .map(|planned| planned.item.clone())
        .filter(|item| !existing.contains(item))
        .collect();
    if !new.is_empty() {
        estate::add_batch(&plan, &new)?;
    }
    let noted = |what: &str| -> u64 {
        found
            .notes
            .iter()
            .filter(|note| note.what == what)
            .map(|note| note.count)
            .sum()
    };
    out(&format!(
        "estate-move-source phase=inventory outcome=ok items={} new={} symlinks={} not_git_files={} prunable={} unreadable={}",
        found.items.len(),
        new.len(),
        noted("symlink"),
        noted("not-git"),
        noted("prunable"),
        noted("unreadable"),
    ))?;
    let (captured, refused) = (AtomicU64::new(0), AtomicU64::new(0));
    let policy = if rebuildable {
        bulkload_agent::git_carry::CapturePolicy::including_rebuildable()
    } else {
        bulkload_agent::git_carry::CapturePolicy::default()
    };
    let capture = estate::capture_with_policy(
        &plan,
        &state.join("private"),
        &carry.join("corpus"),
        count,
        policy,
        &|row| {
            if row.outcome == "refused" {
                refused.fetch_add(1, Ordering::Relaxed);
            } else {
                captured.fetch_add(1, Ordering::Relaxed);
            }
            super::print_receipt(row)
        },
    );
    let (captured, refused) = (captured.into_inner(), refused.into_inner());
    out(&format!(
        "estate-move-source phase=capture outcome={} captured={captured} refused={refused}",
        match &capture {
            Ok(()) => "ok",
            Err(_) if refused > 0 => "items-refused",
            Err(refusal) => refusal.code(),
        },
    ))?;
    match capture {
        Err(refusal) if refused == 0 => Err(refusal),
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
                "--no-handoff" if parsed.handoff => {
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
        // The label is a path suffix and one ref component; the source
        // label a provenance namespace. Both checked before anything runs.
        if !bulkload_agent::git_carry::source_label_allowed(&parsed.label)
            || !bulkload_agent::git_carry::source_label_allowed(&parsed.source)
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
        if parsed.codex_root.is_some() && parsed.sessions.is_none() {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        }
        parsed.host.clone_from(host);
        parsed.source_state = absolute(Some(source_state))?;
        parsed.state = absolute(Some(state))?;
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
/// end is recorded in `MOVE_STATE/move.ledger`; a rerun is the next
/// converge pass. Exits with the closure gate.
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

    // 1. The source half's plan and corpus.
    let pulled = run.transfer(&run.source_state.join("carry"), &carry, "corpus");
    reached(&mut ledger, &ledger_path, "pull", word(&pulled))?;
    pulled?;

    // 2. Decide every item against the destination now.
    let items = estate::inspect(&plan)?;
    let rows = moving::decide(
        &items,
        &corpus,
        &mut ledger,
        !run.side_roots.is_empty(),
        now_ns(),
    )?;
    let suffix = run.suffix();
    for row in &rows {
        let place = row.item.workspace.as_ref().unwrap_or(&row.item.repository);
        let beside = if row.decision.action == moving::Action::CarryBeside {
            format!(
                " beside={:?}",
                bulkload_agent::git_carry::beside::path(place, &suffix)
            )
        } else {
            String::new()
        };
        out(&format!(
            "estate-move item={} action={} reason={} presence={} captured={} kept={} source={:?} live={place:?}{beside}",
            row.id,
            row.decision.action.word(),
            row.decision.reason,
            row.decision.presence.word(),
            row.decision.captured,
            row.kept,
            row.item.source,
        ))?;
    }
    let counts = moving::histogram(&rows);
    out(&format!(
        "estate-move plan items={} {}",
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
        .join(" ")
    ))?;
    reached(&mut ledger, &ledger_path, "plan", "ok".to_owned())?;
    if run.plan_only {
        // estate-diff's preview of what the apply will do (read-only at the
        // destination), one row per line in MOVE_STATE/diff.jsonl.
        let rows_path = run.state.join("diff.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&rows_path)
            .refuse_at("estate_move::diff")?;
        let summary = estate::diff(
            &plan,
            &corpus,
            &run.source,
            std::slice::from_ref(&private),
            &estate::DiffOptions {
                since: None,
                scan: Vec::new(),
                activate_origin: run.origins.clone(),
                beside: Some(suffix),
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
            "estate-move diff classes={} adopt={} rows={rows_path:?}",
            show(&summary.classes),
            show(&summary.adopt),
        ))?;
        return Ok(());
    }

    // 3. Land absent, adopt equal, beside what differs, refs in.
    let refused = AtomicU64::new(0);
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
                beside: Some(suffix.clone()),
                refs_in: true,
            },
        ),
        &|row| {
            if row.outcome == "refused" {
                refused.fetch_add(1, Ordering::Relaxed);
            }
            super::print_receipt(row)
        },
        &mut |line| out(line),
    );
    let refused = refused.into_inner();
    // Item refusals are each item's, recorded for closure; the move goes on
    // to carry, hand off and account for them. Any other refusal stops it.
    let outcome = match &applied {
        Err(_) if refused > 0 => format!("items-refused:{refused}"),
        other => word(other),
    };
    reached(&mut ledger, &ledger_path, "apply", outcome)?;
    if refused == 0 {
        applied?;
    }

    // 4. What the capture refused, carried as files: at the live path when
    // the plan found it absent or empty, else beside it.
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
            args.extend(["--beside".into(), suffix.clone().into()]);
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
            plan.clone().into_os_string(),
            corpus.clone().into_os_string(),
            private.clone().into_os_string(),
            run.source.clone().into(),
            carry_state.clone().into_os_string(),
        ]);
        let result = super::carry_files_command(&args);
        if carried.is_ok() {
            carried = result;
        }
    }
    reached(&mut ledger, &ledger_path, "carry", word(&carried))?;

    // 5. The hand-off block in each landed workspace (OI-1003-Q183).
    if run.handoff {
        let handed = super::estate_command(
            "estate-handoff",
            &[
                plan.clone().into_os_string(),
                corpus.clone().into_os_string(),
                private.clone().into_os_string(),
                run.source.clone().into(),
            ],
        );
        reached(&mut ledger, &ledger_path, "handoff", word(&handed))?;
    }

    // 6. Sessions: the agent state trees, then the re-seat (TIN-6587).
    if let Some(home) = &run.sessions {
        let landing = run.state.join("landing");
        moving::private_dir(&landing)?;
        let codex_root = run
            .codex_root
            .clone()
            .unwrap_or_else(|| home.join(".codex"));
        let claude = run.transfer(&home.join(".claude"), &landing.join(".claude"), "claude");
        let codex = run.transfer(&codex_root, &landing.join(".codex"), "codex");
        let pulled = claude.and(codex);
        reached(&mut ledger, &ledger_path, "sessions-pull", word(&pulled))?;
        let dest_home = match &run.dest_home {
            Some(home) => home.clone(),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .ok_or(BulkloadRefusal::RequiredFieldMissing)?,
        };
        let mut args: Vec<OsString> = vec![
            "--lanes-from-plan".into(),
            "--plan".into(),
            plan.clone().into_os_string(),
            "--corpus".into(),
            corpus.clone().into_os_string(),
            "--source".into(),
            run.source.clone().into(),
            "--private-state".into(),
            private.clone().into_os_string(),
        ];
        for (from, to) in &run.maps {
            let mut pair = from.as_os_str().to_owned();
            pair.push("=");
            pair.push(to);
            args.extend(["--map".into(), pair]);
        }
        args.extend([
            "--codex-source-root".into(),
            codex_root.into_os_string(),
            home.clone().into_os_string(),
            landing.into_os_string(),
            dest_home.into_os_string(),
            run.state.join("reseat").into_os_string(),
        ]);
        let reseated = super::reseat_command(&args);
        reached(&mut ledger, &ledger_path, "reseat", word(&reseated))?;
    }

    // 7. Closure over the move: every planned item accounted.
    let report = bulkload_agent::closure::Report::from_ledger(&estate::ledger_with(
        &plan,
        &corpus,
        &run.source,
        std::slice::from_ref(&private),
        false,
    )?);
    let report_path = run.state.join("closure.json");
    moving::write_report(&report_path, report.to_json().as_bytes())?;
    let refusals = report
        .refusals
        .iter()
        .map(|(code, count)| format!("{code}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    out(&format!(
        "estate-move closure planned={} applied={} applied_beside={} carried={} referenced_only={} refused={} refused_pending_review={} unaccounted={} refusals={} report={report_path:?}",
        rows.len(),
        report.applied,
        report.applied_beside,
        report.carried,
        report.referenced_only,
        report.refused,
        report.refused_pending_review,
        report.unaccounted,
        if refusals.is_empty() { "none" } else { &refusals },
    ))?;
    let gate = report.gate();
    reached(&mut ledger, &ledger_path, "closure", word(&gate))?;
    out("estate-move pass done: a rerun is the next converge pass (deltas only); the resume test is the operator's")?;
    gate
}
