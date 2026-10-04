//! Native ordinary-file transport and offline provider composition on Unix.
//!
//! Argument parsing is hand-rolled on purpose. `clap` is not on the R34
//! dependency allowlist for the agent, and a binary whose whole point is a
//! closed dependency graph should not grow a parser crate to read one
//! subcommands.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use bulkload_agent::freshness::{Freshness, FreshnessCache as _, MemoryCache, StatIdentity};
use bulkload_agent::hash;
use bulkload_agent::walk::{self, HashPolicy, WalkOptions};
use bulkload_proto::{BulkloadRefusal, Control, FileKind, Frame, Result, RowSchema};

const USAGE: &str = "\
bulkload-agent -- ordinary-file transport and offline SQLite composition

USAGE:
    bulkload-agent [--durability=group|strict] [--min-free-percent=N]
                   [--priority=background|normal] <SUBCOMMAND>

SUBCOMMANDS:
    selftest    Hash a temporary file and round-trip a postcard frame
    walk PATH   Stat-walk PATH and print the row and refusal counts
    copy SOURCE DEST SOURCE_STATE DEST_STATE
                Native local copy with private resumable chunk stores
    pull HOST SOURCE DEST SOURCE_STATE DEST_STATE [REMOTE_EXECUTABLE [SSH_CONFIG]]
                Native SSH pull; remote bulkload-agent must be installed
    serve       Serve one framed request on stdin/stdout (for SSH)
    git-export REPO NEW_CAPTURE_DIR [--include-rebuildable]
                Archive refs/stashes and staged/worktree trees in a bundle
    git-carry-estimate [--state-dir DIR] SOURCE_REPO DEST [SOURCE_REPO DEST ...]
                Read-only: report what git carry v2 would move from SOURCE_REPO
                to DEST (a local path or HOST:PATH over ssh -T -oBatchMode=yes);
                missing_thin_pack_bytes is the gate metric (R-N74). Refuses a
                partial or differently-shallow DEST (R-N75); a refused pair
                prints refused=CODE and the verb exits nonzero. A child's
                stderr is never printed (R-N121): only stderr_class=, plus
                stderr_keyed_blake3= and stderr_file= when --state-dir keeps
                the raw bytes in DIR/stderr/<digest>.log (mode 0600). DIR
                must be yours, mode 0700 or tighter, no ACL, and outside
                every SOURCE_REPO and local DEST
    estate-add PLAN SOURCE_REPO DEST_REPO [ABSENT_WORKSPACE]
                Append an explicit reviewed item; no automatic worktree proliferation
    estate-show PLAN
                Print the exact selected sources, repositories and restore targets
    estate-add-batch PLAN SOURCE DEST WORKSPACE_OR_DASH [SOURCE DEST WORKSPACE_OR_DASH ...]
                Append selected items in one linear plan update
    estate-capture PLAN PRIVATE_STATE CORPUS JOBS [--include-rebuildable]
                Capture reviewed Git items with successful capture reuse (jobs 1 or 2)
    estate-apply PLAN CORPUS PRIVATE_STATE SOURCE JOBS
                Import refs and restore only explicitly selected absent workspaces
    closure-report [--attest LEDGER.json] PLAN CORPUS SOURCE PRIVATE_STATE [PRIVATE_STATE ...]
                Read-only: join PLAN with CORPUS's capture records and each
                PRIVATE_STATE's apply outcome records and journals (later
                directories override earlier outcome records), with SOURCE
                the label estate-apply was given. Prints a bulkload.closure.v1
                JSON ledger: every planned item is applied (workspace, exact
                current-capture journal), refused (typed code; a bare IO or
                FRAME_CODEC is not typed), or referenced-only (no workspace
                planned, exact refs journal), else unaccounted. Stale and
                foreign journals are listed. `verdict` is always the native
                one. Exits nonzero with CLOSURE_UNACCOUNTED when `gate` fails.
                --attest LEDGER.json (only first, before PLAN): join a
                bulkload.closure-ledger.v1 attestation ledger for items closed
                by audit, not by a verb. The ledger must name this PLAN and
                SOURCE (plan, source_label). Its rows are reported in a
                separate attested block and never override a native record
                or change `verdict`; `gate` then passes only if every item is
                native-accounted or attested (matching source and current
                capture digest, typed refusal, basis other than
                native-closure-report, evidence)
    git-import REPO BUNDLE SOURCE
                Preserve bundle refs in a content-addressed carry namespace
    git-restore BUNDLE ABSENT_DEST SOURCE
                Restore captured staged/unstaged work into a new repository
    git-restore-linked BUNDLE REPOSITORY ABSENT_DEST SOURCE
                Restore captured work into a new linked worktree without switching others
    git-repair-missing-index BUNDLE REPOSITORY SOURCE NEW_RECEIPT [PLAN CORPUS PRIVATE_STATE]
                Create a missing same-HEAD staged index only; never rewrite payload.
                With PLAN CORPUS PRIVATE_STATE, bind BUNDLE to the planned item
                whose CORPUS capture it is and record apply-style receipts in
                PRIVATE_STATE: journal refs-imported, outcome index-repaired
                (or refused with its code), so closure-report reads it natively.
                An item that plans a workspace never binds (estate-apply
                restores it)
    git-attach-matching-payload BUNDLE REPOSITORY DESTINATION SOURCE NEW_RECEIPT
                Attach exact matching payload using existing common Git administration
    git-attach-standalone-payload BUNDLE DESTINATION SOURCE NEW_RECEIPT ORIGIN_FROM ORIGIN_TO
                Attach exact payload with retained config and explicit local origin mapping
    git-restore-registered-payload BUNDLE REPOSITORY DESTINATION ADMIN SOURCE NEW_RECEIPT
                Restore absent payload only, preserving matching retained registration/index
    snapshot SOURCE OUTPUT [MAX_STEPS]
                Capture live SQLite through its online backup API
    compose BASE INCOMING OUTPUT SOURCE_ID [MAX_STEPS]
                Compose retained SQLite snapshots into a private candidate
    compose-state BASE INCOMING OUTPUT SOURCE_ID SOURCE_HOME DEST_HOME [MAX_STEPS]
                Compose Codex state with retained rollout path mapping
    hydrate-state SNAPSHOT SOURCE_HOME DEST_HOME MAX_BYTES JOBS [GZIP ZSTD]
                Add missing raw rollouts from retained compressed files; never replace
    apply-state-candidate LIVE BASE CANDIDATE MAX_ROWS
                Explicit external live-state import; requires operator authorization
    help        Print this message

BOUNDARIES:
    --durability=group (the default) seals each file with a barrier and makes
    each group of files durable with one SQLite commit; --durability=strict
    fully flushes every file (A/B comparison). pull passes strict to serve.
    --min-free-percent=N (0-100, default 25): DESTINATION_SPACE_INSUFFICIENT
    when planned bytes would leave the destination filesystem (statvfs) with
    less than N% free. copy/pull refuse each entry that does not fit, as a
    value, before requesting its content: the bytes of every entry decided
    and not yet Held count against a probe refreshed on each group commit,
    so the session continues and stays resumable. estate-apply refuses before
    any item, planning bundle sizes (a lower bound).
    --priority=background|normal (WP0(f)): serve, estate-capture, snapshot,
    git-carry-estimate, git-export and copy read a live source, so they enter
    background CPU and IO priority before anything else (Linux nice 19 and the
    idle IO class; Darwin IOPOL_THROTTLE, QOS_CLASS_BACKGROUND and nice 19),
    inherited by every thread and child; other verbs run at normal priority.
    --priority=normal is the explicit opt-out (gate (a)); every counters line
    records priority= and priority_from=default|flag.
    copy/pull require an existing destination directory.
    copy/pull preserve divergent destinations and refuse live SQLite files.
    They enumerate the source each run; completed content is resumable.
    File manifests allow 131072 chunks and frames at most 8 MiB; oversized files refuse.
    Git-native divergent union is not supplied by copy/pull.
    compose commands write offline candidates, never install live databases.
    Capture omits a fixed rebuildable set (target, node_modules, .venv, ...) at
    any depth when Git tracks nothing beneath it, records each omitted root and
    its size as custody, and carries every other untracked and ignored file.
    --include-rebuildable carries the rebuildable set too, at full fidelity.
    A foreign repository nested in a checkout, or a gitlink, is custody named
    per nest (path, admin, HEAD, unpushed commits, carried-ignored count) on
    git-export stderr and in every estate receipt; its tracked content and
    history are its own item and are not carried, its ignored files are
    (R-N89) unless the nest is planned as its own estate item, which then
    owns them and is named as carried-by=<item> (R-N114). A nest with any
    staged, unstaged or untracked change, hidden index flags, a conversion
    attribute, a stash, a detached-only commit, an operation in progress,
    filter commands, a populated submodule, or outer-tracked paths under it
    refuses (R-N73, R-N83, R-N115).
    estate-capture tolerates refs and worktree seats moving under a pass (R25):
    the item reports outcome=captured-with-drift with reason=drift=N, the N
    rows ride in the CORPUS {bundle}.drift sidecar (the bundle's
    refs/carry-export/capture-drift-v1 names the export's share), drifted
    seats are omitted from the bundle, and the next estate-capture re-reads
    exactly those seats plus any racy seat (stamped within 2 s of the pass
    start) and reuses every other blob (capture-extended-from-drift,
    source_bytes_read). A pass reusing nothing it was offered says why:
    reuse_unavailable=shallow|retained-unreadable|pass-start-unrecorded|
    future-stamp.
    A gc, repack or prune that rewrites the source's object store under a
    pass, so that a Git child reading through it fails, is drift custody too:
    outcome=deferred-with-drift with one ObjectStoreRewritten objects/pack
    row, no capture record, and the next estate-capture captures the
    rewritten store.
    A bundle that drifted under its export carries an in-band marker, and
    estate-apply and every git-restore/import/attach/repair verb refuse it
    with CAPTURE_DRIFTED; run estate-capture again first. HEAD, index,
    config, shallow,
    nested-worktree or nested-repository changes under a pass still refuse
    GIT_AUTHORITY_CHANGED;
    git-export never tolerates drift.

COUNTERS:
    Every verb ends with machine-readable key=value lines: `counters` (bytes
    read/written per stage, BLAKE3 bytes, flushes by kind, SQLite commits by
    kind, elapsed_ns). copy and pull also print `transfer_timing` and
    `chunk_timing`. copy and pull print them on stdout; serve prints its
    source-side lines on stderr (stdout is the wire; ssh relays stderr), and
    every other verb prints on stderr so its stdout contract is unchanged.
";

/// Verbs that read a live source on the host they run on. Each enters
/// background CPU and IO priority before anything else unless
/// `--priority=normal` opts out, and the opt-out is recorded (WP0(f),
/// OI-1003-Q17). `copy` is here because it reads the source in-process (it is
/// gate (a)'s verb); `pull` is not, since its source half is the remote
/// `serve`.
const SOURCE_SIDE_VERBS: &[&str] = &[
    "serve",
    "estate-capture",
    "snapshot",
    "git-carry-estimate",
    "git-export",
    "copy",
];

/// Where a verb's priority class came from, as its counters line records it.
#[derive(Clone, Copy)]
struct Priority {
    class: bulkload_agent::priority::PriorityClass,
    /// `true` when `--priority=` chose it, `false` for the verb's default.
    explicit: bool,
}

impl Priority {
    fn render(self) -> String {
        format!(
            "priority={} priority_from={}",
            self.class.label(),
            if self.explicit { "flag" } else { "default" }
        )
    }
}

fn main() -> ExitCode {
    use bulkload_agent::priority::PriorityClass;
    let started = std::time::Instant::now();
    // OI-1001-Q2: the binary keeps a 25% free-space floor unless told otherwise.
    bulkload_agent::space::set_min_free_percent(bulkload_agent::space::DEFAULT_MIN_FREE_PERCENT);
    let mut requested = None;
    let mut args = match global_flags(std::env::args_os().skip(1).collect(), &mut requested) {
        Ok(args) => args.into_iter(),
        Err(refusal) => {
            eprintln!("bulkload-agent: refused: {refusal}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let command = args.next();
    let verb = command
        .as_ref()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_owned();
    // WP0(f): before any thread or child exists, so all of them inherit it.
    let priority = Priority {
        class: requested.unwrap_or_else(|| {
            if SOURCE_SIDE_VERBS.contains(&verb.as_str()) {
                PriorityClass::Background
            } else {
                PriorityClass::Normal
            }
        }),
        explicit: requested.is_some(),
    };
    if priority.class == PriorityClass::Background {
        if let Err(refusal) = bulkload_agent::priority::enter_background() {
            eprintln!("bulkload-agent: refused: {refusal}");
            return ExitCode::FAILURE;
        }
    }
    // macOS starts at 256 open files; take the hard limit the host allows.
    let _ = bulkload_agent::limits::raise_descriptor_limit();
    let outcome = match command.as_ref().and_then(|value| value.to_str()) {
        Some("selftest") => selftest(),
        Some("walk") => {
            if let Some(path) = args.next() {
                walk_command(Path::new(&path))
            } else {
                eprintln!("bulkload-agent: walk requires a PATH\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        Some(
            "copy" | "pull" | "snapshot" | "compose" | "compose-state" | "git-export"
            | "git-import" | "git-restore" | "git-restore-linked",
        ) => native_command(
            command
                .as_ref()
                .and_then(|value| value.to_str())
                .unwrap_or(""),
            &args.collect::<Vec<_>>(),
        ),
        Some("git-carry-estimate") => estimate_command(&args.collect::<Vec<_>>()),
        Some("hydrate-state") => hydrate_command(&args.collect::<Vec<_>>()),
        Some("git-repair-missing-index") => repair_index_command(&args.collect::<Vec<_>>()),
        Some("git-restore-registered-payload") => registered_command(&args.collect::<Vec<_>>()),
        Some("git-attach-matching-payload") => attach_payload_command(&args.collect::<Vec<_>>()),
        Some("git-attach-standalone-payload") => {
            attach_standalone_command(&args.collect::<Vec<_>>())
        }
        Some(
            name @ ("estate-add" | "estate-add-batch" | "estate-show" | "estate-capture"
            | "estate-apply"),
        ) => estate_command(name, &args.collect::<Vec<_>>()),
        Some("apply-state-candidate") => apply_state_command(&args.collect::<Vec<_>>()),
        Some("closure-report") => closure_command(&args.collect::<Vec<_>>()),
        Some("serve") => {
            let (input, output) = (std::io::stdin(), std::io::stdout());
            bulkload_agent::transfer::tune_stream(&input);
            bulkload_agent::transfer::tune_stream(&output);
            // The reader thread owns stdin for the session; the process
            // exit closes the transport.
            bulkload_agent::transfer::serve(input, &mut output.lock())
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("bulkload-agent: unknown subcommand {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    report_counters(&verb, started, priority);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("bulkload-agent: refused: {refusal}");
            ExitCode::FAILURE
        }
    }
}

/// Remove `--durability=MODE` and `--min-free-percent=N` from the arguments
/// and apply them process-wide, and `--priority=CLASS` into `priority`.
fn global_flags(
    args: Vec<std::ffi::OsString>,
    priority: &mut Option<bulkload_agent::priority::PriorityClass>,
) -> Result<Vec<std::ffi::OsString>> {
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        if let Some(class) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--priority="))
        {
            *priority = Some(class.parse()?);
            continue;
        }
        match arg
            .to_str()
            .and_then(|value| value.strip_prefix("--durability="))
        {
            Some(mode) => bulkload_agent::durable::set_durability(mode.parse()?),
            None => match arg
                .to_str()
                .and_then(|value| value.strip_prefix("--min-free-percent="))
            {
                Some(percent) => bulkload_agent::space::set_min_free_percent(
                    bulkload_agent::space::parse_percent(percent)?,
                ),
                None => rest.push(arg),
            },
        }
    }
    Ok(rest)
}

// OI-1001-Q2: bulkload's own closure gate. The JSON ledger goes to stdout
// whether or not it passes; the verdict is the exit status.
fn closure_command(args: &[std::ffi::OsString]) -> Result<()> {
    // #95, #133: `--attest LEDGER.json` is an option only as the first
    // argument after the verb. Everywhere else every argument is positional,
    // so a PRIVATE_STATE literally named `--attest` is still a state.
    let (attest, positional) = match args {
        [flag, ledger, rest @ ..] if flag == "--attest" => (Some(PathBuf::from(ledger)), rest),
        [flag] if flag == "--attest" => return Err(BulkloadRefusal::RequiredFieldMissing),
        rest => (None, rest),
    };
    let [plan, corpus, source, states @ ..] = positional else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    if states.is_empty() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let source = source.to_str().ok_or(BulkloadRefusal::PathNotPortable)?;
    let states: Vec<PathBuf> = states.iter().map(PathBuf::from).collect();
    let ledger =
        bulkload_agent::estate::ledger(Path::new(plan), Path::new(corpus), source, &states)?;
    let mut report = bulkload_agent::closure::Report::from_ledger(&ledger);
    if let Some(attest) = attest {
        report.attest(&bulkload_agent::closure::AttestationLedger::read(
            &attest,
            Path::new(plan),
            source,
        )?)?;
    }
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", report.to_json())?;
    stdout.flush()?;
    report.gate()
}

// The only capture-policy word the agent accepts; anything else is a typo, and
// a typo must not silently change what a 40-minute capture carries.
fn capture_policy(
    argument: Option<&std::ffi::OsString>,
) -> Result<bulkload_agent::git_carry::CapturePolicy> {
    match argument.and_then(|value| value.to_str()) {
        None => Ok(bulkload_agent::git_carry::CapturePolicy::default()),
        Some("--include-rebuildable") => {
            Ok(bulkload_agent::git_carry::CapturePolicy::including_rebuildable())
        }
        Some(_) => Err(BulkloadRefusal::FieldDomainViolation),
    }
}

fn export_command(
    repo: &Path,
    capture: &Path,
    policy: bulkload_agent::git_carry::CapturePolicy,
) -> Result<()> {
    let export =
        bulkload_agent::git_carry::export_repository_with_policy(repo, capture, None, policy)?;
    // Custody goes to stderr so stdout stays exactly the bundle path.
    for omission in &export.omitted {
        eprintln!(
            "omitted-rebuildable path={} bytes={} entries={}",
            String::from_utf8_lossy(&omission.rel_path),
            omission.bytes,
            omission.entries
        );
    }
    // Escaped and quoted: a newline in a nest path cannot forge a line (F8).
    for nested in &export.nested_repositories {
        eprintln!("{}", nested.receipt_line());
    }
    println!("{}", export.bundle.display());
    Ok(())
}

fn native_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    if command == "git-restore-linked" && args.len() == 4 {
        let source = args
            .get(3)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::PathNotPortable)?;
        bulkload_agent::git_carry::restore_linked(path(0)?, path(1)?, path(2)?, source)?;
        println!("linked restoration complete");
        return Ok(());
    }
    match command {
        "git-restore" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            bulkload_agent::git_carry::restore_bundle(path(0)?, path(1)?, source)?;
            println!("{}", path(1)?.display());
            Ok(())
        }
        "git-export" if (2..=3).contains(&args.len()) => {
            export_command(path(0)?, path(1)?, capture_policy(args.get(2))?)
        }
        "git-import" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let count = bulkload_agent::git_carry::import_bundle(path(0)?, path(1)?, source)?;
            println!("{count}");
            Ok(())
        }
        "copy" if args.len() == 4 => {
            let stats = bulkload_agent::transfer::copy(path(0)?, path(1)?, path(2)?, path(3)?)?;
            report_transfer(&stats)
        }
        "pull" if (5..=7).contains(&args.len()) => pull_command(args),
        "snapshot" if (2..=3).contains(&args.len()) => {
            bulkload_agent::provider_sqlite::snapshot(path(0)?, path(1)?, steps(args.get(2))?)?;
            println!("snapshot complete");
            Ok(())
        }
        "compose" if (4..=5).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let stats = bulkload_agent::provider_sqlite::compose_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(4))?,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        "compose-state" if (6..=7).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let mapping = bulkload_agent::provider_sqlite::PathMapping {
                source_home: path(4)?,
                destination_home: path(5)?,
            };
            let stats = bulkload_agent::provider_sqlite::compose_state_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(6))?,
                &mapping,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

// Read-only on both sides: no fetch, no object or ref write (R-N60 baseline).
//
// Each pair prints one whole block, built before any of it is written. A
// refused pair prints `refused=CODE` in place of its measurements, with
// `refused_reason=`, `stderr_class=` and the store's fields when present; later
// pairs still run, and the verb exits nonzero with the first refusal (F13).
// A child's stderr is classified, never echoed (R-N121). With `--state-dir`,
// its raw bytes go to a private 0600 file whose path is printed as
// `stderr_file=`, with a digest keyed by the state dir's key; without it,
// nothing is written and no digest is printed.
// The estimate verb's `Refused` carries a `BulkloadRefusal`, whose
// path-carrying variants (#53) put it just over clippy's 128-byte
// large-error threshold; see git_carry::estimate.
#[allow(clippy::result_large_err)]
fn estimate_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::git_carry::estimate::{estimate_with, Destination, Refused, StderrStore};
    let (state_dir, args) = match args {
        [flag, dir, rest @ ..] if flag == "--state-dir" => (Some(Path::new(dir)), rest),
        _ => (None, args),
    };
    let store = state_dir.map(StderrStore::open).transpose()?;
    if args.is_empty() || !args.len().is_multiple_of(2) {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let mut stdout = std::io::stdout().lock();
    let mut first = None;
    for (index, pair) in args.chunks_exact(2).enumerate() {
        let [source, destination] = pair else {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        };
        let source = Path::new(source);
        let mut block = vec![
            format!("source={}", source.display()),
            format!("destination={}", Path::new(destination).display()),
        ];
        let outcome = Destination::parse(destination)
            .map_err(Refused::from)
            .and_then(|destination| estimate_with(source, &destination, store.as_ref()));
        match outcome {
            Ok(estimate) => block.extend(estimate.lines()),
            Err(refused) => {
                block.extend(refused.lines());
                first.get_or_insert(refused.refusal);
            }
        }
        emit(&mut stdout, index, &block)?;
    }
    first.map_or(Ok(()), Err)
}

fn emit(stdout: &mut impl std::io::Write, index: usize, block: &[String]) -> Result<()> {
    if index > 0 {
        writeln!(stdout)?;
    }
    for line in block {
        writeln!(stdout, "{line}")?;
    }
    stdout.flush()?;
    Ok(())
}

fn repair_index_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 && args.len() != 7 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    if args.len() == 7 {
        // #95: record the repair in an apply state directory, bound to its
        // plan item through CORPUS's capture record.
        bulkload_agent::estate::repair_missing_index(
            path(0)?,
            path(1)?,
            source,
            path(3)?,
            &bulkload_agent::estate::RepairLedger {
                plan: path(4)?,
                corpus: path(5)?,
                state: path(6)?,
            },
        )?;
        println!(
            "missing index repaired; outcome index-repaired recorded; payload parity not asserted"
        );
    } else {
        bulkload_agent::git_carry::repair_missing_index(path(0)?, path(1)?, source, path(3)?)?;
        println!("missing index repaired; payload parity not asserted");
    }
    Ok(())
}

fn registered_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(4)
        .and_then(|arg| arg.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::registered::restore(
        path(0)?,
        path(1)?,
        path(2)?,
        path(3)?,
        source,
        path(5)?,
    )?;
    println!("registered payload restored; original administration and staging preserved");
    Ok(())
}
fn attach_standalone_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::attach_standalone_payload(
        path(0)?,
        path(1)?,
        source,
        path(3)?,
        path(4)?,
        path(5)?,
    )?;
    println!("standalone payload attached; config retained, selected origin mapping activated");
    Ok(())
}

fn attach_payload_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 5 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::attach_matching_payload(
        path(0)?,
        path(1)?,
        path(2)?,
        source,
        path(4)?,
    )?;
    println!("matching payload attached; existing common Git administration preserved");
    Ok(())
}

// Escaped paths are intentional: receipts must not permit embedded newlines.
#[allow(clippy::unnecessary_debug_formatting)]
fn estate_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::estate;
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let jobs = || {
        args.last()
            .and_then(|arg| arg.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse::<usize>()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let receipt = |row: &estate::Receipt| {
        let mut output = std::io::stdout().lock();
        write!(
            output,
            "item={} source={:?} outcome={} reason={:?} source_bytes_read={}",
            row.item, row.source, row.outcome, row.reason, row.bytes_read
        )?;
        if let Some(why) = row.reuse_unavailable {
            write!(output, " reuse_unavailable={why}")?;
        }
        writeln!(output)?;
        // One line per drifted ref or seat, after the item line, so a clean
        // item stays one line.
        for line in &row.drift {
            writeln!(output, "item={} drift={line}", row.item)?;
        }
        // One line per nest after the item line, so a nest-free item stays
        // one line (R-N73). Paths inside are byte-escaped.
        for line in &row.nested {
            writeln!(output, "item={} {line}", row.item)?;
        }
        output.flush()?;
        Ok(())
    };
    match command {
        "estate-show" if args.len() == 1 => {
            let mut output = std::io::stdout().lock();
            for item in estate::inspect(path(0)?)? {
                writeln!(output, "{item:?}")?;
            }
            output.flush()?;
            Ok(())
        }
        "estate-add" if (3..=4).contains(&args.len()) => {
            estate::add(path(0)?, path(1)?, path(2)?, args.get(3).map(Path::new))
        }
        "estate-add-batch" if args.len() >= 4 && (args.len() - 1).is_multiple_of(3) => {
            let mut items = Vec::new();
            for group in args
                .get(1..)
                .ok_or(BulkloadRefusal::RequiredFieldMissing)?
                .chunks_exact(3)
            {
                let source = group
                    .first()
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let repository = group
                    .get(1)
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let workspace = group.get(2).filter(|p| *p != "-").map(PathBuf::from);
                items.push(estate::Item {
                    source,
                    repository,
                    workspace,
                });
            }
            estate::add_batch(path(0)?, &items)
        }
        "estate-capture" if (4..=5).contains(&args.len()) => {
            let policy = capture_policy(args.get(4))?;
            let jobs = args
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?
                .parse::<usize>()
                .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
            estate::capture_with_policy(path(0)?, path(1)?, path(2)?, jobs, policy, &receipt)
        }
        "estate-apply" if args.len() == 5 => {
            let source = args
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            estate::apply(path(0)?, path(1)?, path(2)?, source, jobs()?, &receipt)
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

fn apply_state_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let max_rows = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::FieldDomainViolation)?
        .parse()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    bulkload_agent::provider_sqlite::online::apply_state_candidate(
        path(0)?,
        path(1)?,
        path(2)?,
        max_rows,
        &|receipt| {
            let mut output = std::io::stdout().lock();
            writeln!(
                output,
                "table={} inserted={} corrected={} conflicts={}",
                receipt.table, receipt.inserted, receipt.corrected, receipt.conflicts
            )?;
            output.flush()?;
            Ok(())
        },
    )
}

fn hydrate_command(args: &[std::ffi::OsString]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    if args.len() != 5 && args.len() != 7 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let number = |index: usize| -> Result<u64> {
        args.get(index)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let mapping = bulkload_agent::provider_sqlite::PathMapping {
        source_home: path(1)?,
        destination_home: path(2)?,
    };
    let gzip = args
        .get(5)
        .map_or_else(|| Path::new("/usr/bin/gzip"), Path::new);
    let zstd = args
        .get(6)
        .map_or_else(|| Path::new("/usr/bin/zstd"), Path::new);
    let receipt = |report: &bulkload_agent::provider_sqlite::hydrate::Hydrated| -> Result<()> {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "published={} bytes={} blake3={} source_identity={:?} path={} source={}",
            report.published,
            report.bytes,
            report.blake3,
            report.source_identity,
            report.path.as_os_str().as_bytes().escape_ascii(),
            report.source.as_os_str().as_bytes().escape_ascii()
        )?;
        output.flush()?;
        Ok(())
    };
    let reports = bulkload_agent::provider_sqlite::hydrate::hydrate_state(
        path(0)?,
        &mapping,
        number(3)?,
        usize::try_from(number(4)?).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        gzip,
        zstd,
        &receipt,
    )?;
    println!("hydrated_files={}", reports.len());
    Ok(())
}

fn pull_command(args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let host = args.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    let remote = match args.get(5) {
        None => "bulkload-agent",
        Some(value) => {
            let value = value.to_str().ok_or(BulkloadRefusal::PathNotPortable)?;
            if !Path::new(value).is_absolute()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            {
                return Err(BulkloadRefusal::PathNotPortable);
            }
            value
        }
    };
    let mut ssh = Command::new("ssh");
    ssh.args(["-T", "-oBatchMode=yes", "-oConnectTimeout=15"]);
    if let Some(config) = args.get(6) {
        if !Path::new(config).is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        ssh.arg("-F").arg(config);
    }
    ssh.arg("--").arg(host).arg(remote);
    if bulkload_agent::durable::durability() == bulkload_agent::durable::Durability::Strict {
        ssh.arg("--durability=strict");
    }
    let mut child = ssh
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let result = {
        let mut output = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
        let mut input = child.stdout.take().ok_or(BulkloadRefusal::Io(None))?;
        bulkload_agent::transfer::tune_stream(&output);
        bulkload_agent::transfer::tune_stream(&input);
        bulkload_agent::transfer::receive(
            &mut input,
            &mut output,
            path(1)?,
            path(3)?,
            path(2)?,
            path(4)?,
        )
    };
    let exit_status = child.wait()?;
    let stats = result?;
    if !exit_status.success() {
        return Err(BulkloadRefusal::Io(None));
    }
    report_transfer(&stats)
}

fn steps(value: Option<&std::ffi::OsString>) -> Result<u32> {
    value.map_or(Ok(1_000_000), |value| {
        value
            .to_str()
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    })
}

/// Print the verb's process-scope counters (M2 W2, TIN-4541).
///
/// copy and pull print on stdout beside their transfer line; serve's stdout is
/// the wire, so it and every other verb print on stderr.
fn report_counters(verb: &str, started: std::time::Instant, priority: Priority) {
    use bulkload_agent::counters::{elapsed_ns, Counters};
    use bulkload_agent::transfer::TransferTiming;
    use bulkload_agent::transfer_store::ChunkTiming;
    let side = match verb {
        "copy" => "both",
        "pull" => "destination",
        "serve" => "source",
        _ => "local",
    };
    let prefix = format!(
        "verb={verb} side={side} scope=process {}",
        priority.render()
    );
    let mut lines = Vec::new();
    if matches!(verb, "copy" | "pull" | "serve") {
        lines.push(format!(
            "transfer_timing {prefix} {}",
            TransferTiming::snapshot().render()
        ));
        lines.push(format!(
            "chunk_timing {prefix} {}",
            ChunkTiming::snapshot().render()
        ));
    }
    lines.push(format!(
        "counters {prefix} elapsed_ns={} {}",
        elapsed_ns(started),
        Counters::snapshot().render()
    ));
    if matches!(verb, "copy" | "pull") {
        let mut output = std::io::stdout().lock();
        for line in &lines {
            let _ = writeln!(output, "{line}");
        }
        let _ = output.flush();
    } else {
        let mut output = std::io::stderr().lock();
        for line in &lines {
            let _ = writeln!(output, "{line}");
        }
    }
}

fn report_transfer(stats: &bulkload_agent::transfer::TransferStats) -> Result<()> {
    println!(
        "completed={} reused={} bytes_received={} source_bytes_read={} refusals={} \
         source_engine_temporaries={} capped_subtrees={}",
        stats.completed,
        stats.reused,
        stats.bytes_received,
        stats.source_bytes_read,
        stats.refusals.len(),
        stats.source_engine_temporaries.len(),
        stats.capped_subtrees()
    );
    for (path, code) in &stats.refusals {
        eprintln!("refused {}: {code}", path.escape_ascii());
    }
    if stats.temporaries_removed > 0 {
        eprintln!("temporaries-removed {}", stats.temporaries_removed);
    }
    for path in &stats.temporaries_left {
        eprintln!("temporary-left {}", path.escape_ascii());
    }
    for path in &stats.source_engine_temporaries {
        eprintln!("source-engine-temporary {}", path.escape_ascii());
    }
    for path in &stats.directories_fallback {
        eprintln!(
            "directory-created-by-mkdir-fallback {}",
            path.escape_ascii()
        );
    }
    if stats.refusals.is_empty() {
        Ok(())
    } else {
        Err(BulkloadRefusal::ContractSelfInconsistent)
    }
}

/// Exercise the pieces M1 actually ships: hash a real file off disk, put its
/// row in a frame, encode it with postcard, decode it back, and prove the
/// round trip is exact.
fn selftest() -> Result<()> {
    println!("bulkload-agent selftest");

    let path = scratch_path("selftest");
    let payload = b"tcfs bulkload M1 selftest payload";
    write_scratch(&path, payload)?;

    let digest = hash::hash_file(&path);
    let meta = std::fs::metadata(&path);
    let cleanup = std::fs::remove_file(&path);

    let digest = digest?;
    let meta = meta?;
    cleanup?;

    if digest != hash::hash_bytes(payload) {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    println!("  hashed        {} bytes", payload.len());
    println!("  blake3        {}", hex(&digest));
    println!("  crc32c        {:08x}", hash::checksum(payload));
    println!("  cdc chunks    {}", hash::chunk_boundaries(payload).len());

    let row = row_for(payload.len(), &meta, digest);
    let identity = StatIdentity::from_row(&row);
    let mut cache = MemoryCache::new();
    if cache.lookup(&identity)? != Freshness::Stale {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    cache.record(&identity)?;
    if cache.lookup(&identity)? != Freshness::Fresh {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    println!("  freshness     stale -> record -> fresh (ok)");

    let frame = Frame::Control(Control::Entry { entry: 0, row });
    let encoded = frame.encode()?;
    let (decoded, consumed) = Frame::decode(&encoded)?;
    if decoded != frame || consumed != encoded.len() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    println!("  frame bytes   {}", encoded.len());
    println!("  frame decoded {decoded:?}");
    println!("  round trip    exact (ok)");
    println!("selftest: ok");
    Ok(())
}

fn walk_command(root: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root)?;
    let mut cache = MemoryCache::new();
    let options = WalkOptions {
        hash_policy: HashPolicy::Never,
        ..WalkOptions::new(root)
    };
    let outcome = walk::walk(&options, &mut cache)?;
    println!("rows                     {}", outcome.rows.len());
    println!("refusals                 {}", outcome.refusals.len());
    println!(
        "capped_subtrees          {}",
        outcome
            .refusals
            .iter()
            .filter(|seat| walk::is_cap_refusal(seat.refusal.code()))
            .count()
    );
    println!(
        "engine_temporaries       {}",
        outcome.engine_temporaries.len()
    );
    println!("seats_seen               {}", outcome.stats.seats_seen);
    println!("bytes_seen               {}", outcome.stats.bytes_seen);
    println!("fresh_skipped            {}", outcome.stats.fresh_skipped);
    println!(
        "bytes_reread_on_resume   {}",
        outcome.stats.bytes_reread_on_resume
    );
    println!(
        "files_statted_twice      {}",
        outcome.stats.files_statted_twice
    );
    // Every declined seat by path and code, so a capped subtree (#129) is
    // attributable, never only a count.
    for seat in &outcome.refusals {
        eprintln!(
            "refused {}: {}",
            seat.rel_path.escape_ascii(),
            seat.refusal.code()
        );
    }
    Ok(())
}

fn row_for(len: usize, meta: &std::fs::Metadata, digest: [u8; 32]) -> RowSchema {
    use std::os::unix::fs::MetadataExt as _;
    RowSchema {
        rel_path: b"selftest".to_vec(),
        kind: FileKind::Regular,
        dev: meta.dev(),
        ino: meta.ino(),
        size: u64::try_from(len).unwrap_or(u64::MAX),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
        mode: meta.mode(),
        nlink: meta.nlink(),
        link_target: None,
        blake3: Some(digest),
    }
}

fn scratch_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("bulkload-agent-{name}-{}", std::process::id()));
    path
}

fn write_scratch(path: &std::path::Path, payload: &[u8]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(payload)?;
    bulkload_agent::counters::sync_full(&file)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}
