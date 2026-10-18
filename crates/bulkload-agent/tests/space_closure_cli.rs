//! OI-1001-Q2 at the binary: the `--min-free-percent` preflight on `copy`
//! and the `closure-report` verb's exit status and JSON.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::process::{Command, Output};

fn agent(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .output()
        .expect("run bulkload-agent")
}

fn copy_with_floor(root: &Path, floor: &str) -> Output {
    let flag = format!("--min-free-percent={floor}");
    agent(&[
        flag.as_ref(),
        "copy".as_ref(),
        root.join("source").as_os_str(),
        root.join("destination").as_os_str(),
        root.join("source-state").as_os_str(),
        root.join("destination-state").as_os_str(),
    ])
}

#[test]
fn copy_refuses_before_writing_when_the_floor_cannot_hold() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("source")).unwrap();
    std::fs::create_dir(root.path().join("destination")).unwrap();
    std::fs::write(root.path().join("source").join("payload"), vec![7u8; 4096]).unwrap();

    // 100% free afterwards is impossible: the entry is refused as a value
    // (wire v5 Decide), typed, and nothing is written.
    let refused = copy_with_floor(root.path(), "100");
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("refused payload: DESTINATION_SPACE_INSUFFICIENT"),
        "{stderr}"
    );
    assert!(!root.path().join("destination").join("payload").exists());

    // With no floor the same session resumes and completes, so the refusal
    // was the floor and left the cohort resumable.
    let copied = copy_with_floor(root.path(), "0");
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    assert_eq!(
        std::fs::read(root.path().join("destination").join("payload")).unwrap(),
        vec![7u8; 4096]
    );
}

#[test]
fn an_out_of_domain_floor_is_refused_as_a_value() {
    let root = tempfile::tempdir().unwrap();
    let output = copy_with_floor(root.path(), "101");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("FIELD_DOMAIN_VIOLATION"));
}

#[test]
fn closure_report_fails_when_a_planned_item_is_unaccounted() {
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan");
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    // estate-add resolves the source, so it must exist; nothing is captured.
    std::fs::create_dir(root.path().join("source")).unwrap();
    let added = agent(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        root.path().join("source").as_os_str(),
        root.path().join("repository").as_os_str(),
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let report = agent(&[
        "closure-report".as_ref(),
        plan.as_os_str(),
        root.path().join("corpus").as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    assert!(!report.status.success());
    let stdout = String::from_utf8_lossy(&report.stdout);
    assert!(
        stdout.contains("\"verdict\":\"fail\"")
            && stdout.contains("\"planned\":1")
            && stdout.contains("\"unaccounted\":1")
            && stdout.contains("\"unaccounted_reason\":\"no-outcome-record\""),
        "{stdout}"
    );
    assert!(String::from_utf8_lossy(&report.stderr).contains("refused: CLOSURE_UNACCOUNTED"));

    // No state directory is a usage refusal, not a pass.
    let missing = agent(&[
        "closure-report".as_ref(),
        plan.as_os_str(),
        root.path().join("corpus").as_os_str(),
        "neo".as_ref(),
    ]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
}

// #133: a bound attestation ledger, given first, closes the item. The exit
// status follows `gate`; `verdict` stays the native one.
#[test]
fn closure_report_attestation_is_bound_and_never_changes_the_verdict() {
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan");
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(root.path().join("source")).unwrap();
    assert!(agent(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        root.path().join("source").as_os_str(),
        root.path().join("repository").as_os_str(),
    ])
    .status
    .success());
    let native = agent(&[
        "closure-report".as_ref(),
        plan.as_os_str(),
        root.path().join("corpus").as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    let stdout = String::from_utf8_lossy(&native.stdout);
    let item = stdout
        .split("\"item\":\"")
        .nth(1)
        .and_then(|rest| rest.get(..64))
        .unwrap()
        .to_owned();
    let source = std::fs::canonicalize(root.path().join("source")).unwrap();
    let ledger = root.path().join("ledger.json");
    let write_ledger = |label: &str| {
        std::fs::write(
            &ledger,
            format!(
                r#"{{"schema":"bulkload.closure-ledger.v1","plan":"{}","source_label":"{label}","items":[{{"item":"{item}","source":"{}","capture":null,"disposition":"source-absent","basis":"audit","evidence":"test"}}]}}"#,
                plan.display(),
                source.display()
            ),
        )
        .unwrap();
    };
    write_ledger("neo");
    let attested = agent(&[
        "closure-report".as_ref(),
        "--attest".as_ref(),
        ledger.as_os_str(),
        plan.as_os_str(),
        root.path().join("corpus").as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    let out = String::from_utf8_lossy(&attested.stdout);
    assert!(
        attested.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&attested.stderr)
    );
    assert!(
        out.contains("\"verdict\":\"fail\",\"gate\":\"pass\"") && out.contains("\"attested\":1"),
        "{out}"
    );
    // A ledger for another SOURCE label refuses before any report.
    write_ledger("sting");
    let other = agent(&[
        "closure-report".as_ref(),
        "--attest".as_ref(),
        ledger.as_os_str(),
        plan.as_os_str(),
        root.path().join("corpus").as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    assert!(!other.status.success());
    assert!(other.stdout.is_empty());
    assert!(String::from_utf8_lossy(&other.stderr).contains("RECEIPT_BINDING_INVALID"));

    // After PLAN, `--attest` is positional: a state directory may carry
    // that name.
    let named = root.path().join("--attest");
    std::fs::create_dir(&named).unwrap();
    let positional = std::process::Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .current_dir(root.path())
        .args([
            "closure-report".as_ref(),
            plan.as_os_str(),
            root.path().join("corpus").as_os_str(),
            "neo".as_ref(),
            state.as_os_str(),
            "--attest".as_ref(),
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&positional.stdout);
    assert!(
        out.contains("\"verdict\":\"fail\",\"gate\":\"fail\"") && !out.contains("\"attested\""),
        "{out}{}",
        String::from_utf8_lossy(&positional.stderr)
    );
}

// S4 (WP3 PR 3): a typed refusal is pending review until closure-dispose
// records a review bound to the plan and SOURCE; a bare IO can never be
// dispositioned.
#[test]
#[allow(clippy::too_many_lines)]
fn closure_dispose_records_a_review_that_closes_a_typed_refusal() {
    use bulkload_agent::outcome::{Outcome, OutcomeRecord, Refusal};
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan");
    let state = root.path().join("state");
    let corpus = root.path().join("corpus");
    let reviews = root.path().join("reviews");
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(root.path().join("source")).unwrap();
    assert!(agent(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        root.path().join("source").as_os_str(),
        root.path().join("repository").as_os_str(),
    ])
    .status
    .success());
    let report = |extra: &[&std::ffi::OsStr]| {
        let mut args: Vec<&std::ffi::OsStr> = vec!["closure-report".as_ref()];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            plan.as_os_str(),
            corpus.as_os_str(),
            "neo".as_ref(),
            state.as_os_str(),
        ]);
        agent(&args)
    };
    // The planned item's identity and source, as the native report names them.
    let native = String::from_utf8_lossy(&report(&[]).stdout).into_owned();
    let field = |name: &str| {
        let start = native.find(&format!("\"{name}\":\"")).unwrap() + name.len() + 4;
        native[start..start + native[start..].find('"').unwrap()].to_owned()
    };
    let (item, source) = (field("item"), field("source"));
    // The verb's typed record: a refusal with a code, recorded at a site.
    let record = OutcomeRecord {
        source: source.into(),
        outcome: Outcome::Refused(
            Refusal::new("GIT_NEST_STASHED", "estate::capture", None).unwrap(),
        ),
        reason: Some("GIT_NEST_STASHED".into()),
    };
    std::fs::write(
        state.join(format!("{item}.outcome")),
        record.encode().unwrap(),
    )
    .unwrap();

    let pending = report(&[]);
    let out = String::from_utf8_lossy(&pending.stdout);
    assert!(!pending.status.success());
    assert!(
        out.contains("\"verdict\":\"pass\",\"gate\":\"fail\"")
            && out.contains("\"disposition\":\"refused-pending-review\",\"refusal\":\"GIT_NEST_STASHED\",\"site\":\"estate::capture\""),
        "{out}"
    );

    let dispose = |target: &str, code: &str, label: &str, states: &[&std::ffi::OsStr]| {
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "closure-dispose".as_ref(),
            reviews.as_os_str(),
            plan.as_os_str(),
            corpus.as_os_str(),
            label.as_ref(),
            target.as_ref(),
            code.as_ref(),
            "accept".as_ref(),
            "jess".as_ref(),
            "2026-10-06".as_ref(),
        ];
        args.extend_from_slice(states);
        agent(&args)
    };
    let here: &[&std::ffi::OsStr] = &[state.as_os_str()];
    let refuses = |output: Output, code: &str| {
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(code), "{stderr}");
    };
    // A bare IO names no cause: no review can record it.
    refuses(dispose(&item, "IO", "neo", here), "FIELD_DOMAIN_VIOLATION");
    // An item the plan does not hold binds nothing.
    refuses(
        dispose(&"f".repeat(64), "GIT_NEST_STASHED", "neo", here),
        "RECEIPT_BINDING_INVALID",
    );
    // A review is not written ahead of its refusal: the item holds no
    // refusal with this code, and an item review must read the state.
    refuses(
        dispose(&item, "CAPTURE_DRIFTED", "neo", here),
        "RECEIPT_BINDING_INVALID",
    );
    refuses(
        dispose(&item, "GIT_NEST_STASHED", "neo", &[]),
        "REQUIRED_FIELD_MISSING",
    );
    assert!(!reviews.exists());

    let recorded = dispose(&item, "GIT_NEST_STASHED", "neo", here);
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let instance = {
        let start = out.find("\"instance\":\"").unwrap() + 12;
        out[start..start + 64].to_owned()
    };
    assert!(String::from_utf8_lossy(&recorded.stdout).contains(&format!(
        "disposition recorded scope=item item={item} instance={instance} refusal=GIT_NEST_STASHED decision=accept"
    )));
    // The ledger is bound to its SOURCE label.
    refuses(
        dispose("--policy", "GIT_NEST_STASHED", "sting", &[]),
        "RECEIPT_BINDING_INVALID",
    );

    let closed = report(&["--dispositions".as_ref(), reviews.as_os_str()]);
    let out = String::from_utf8_lossy(&closed.stdout);
    assert!(closed.status.success(), "{out}");
    assert!(
        out.contains("\"schema\":\"bulkload.closure.v5\",\"verdict\":\"pass\",\"gate\":\"pass\"")
            && out.contains("\"review\":{\"decision\":\"accept\",\"reviewer\":\"jess\",\"date\":\"2026-10-06\",\"basis\":\"item\"}"),
        "{out}"
    );
    // Each option at most once.
    let twice = report(&[
        "--dispositions".as_ref(),
        reviews.as_os_str(),
        "--dispositions".as_ref(),
        reviews.as_os_str(),
    ]);
    assert!(String::from_utf8_lossy(&twice.stderr).contains("FIELD_DOMAIN_VIOLATION"));

    // The same item refuses the same code again, with another record (run 2
    // of the verb): the run-1 review does not dispose it, the gate is red
    // again, and the stale row is listed.
    let again = OutcomeRecord {
        reason: Some("GIT_NEST_STASHED path=\"later\"".into()),
        ..record
    };
    std::fs::write(
        state.join(format!("{item}.outcome")),
        again.encode().unwrap(),
    )
    .unwrap();
    let rerun = report(&["--dispositions".as_ref(), reviews.as_os_str()]);
    let out = String::from_utf8_lossy(&rerun.stdout);
    assert!(!rerun.status.success(), "{out}");
    assert!(
        out.contains("\"verdict\":\"pass\",\"gate\":\"fail\"")
            && out.contains("\"disposition\":\"refused-pending-review\"")
            && out.contains("\"reason\":\"refusal-instance-stale\""),
        "{out}"
    );
    assert!(String::from_utf8_lossy(&rerun.stderr).contains("refused: CLOSURE_UNACCOUNTED"));
}

// The disposition ledger is bound to the plan's bytes, and closure-dispose
// reads the plan for a standing policy too.
#[test]
fn closure_dispose_binds_the_plan_content_not_only_its_path() {
    use bulkload_agent::outcome::{Outcome, OutcomeRecord, Refusal};
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan");
    let state = root.path().join("state");
    let corpus = root.path().join("corpus");
    let reviews = root.path().join("reviews");
    std::fs::create_dir(&state).unwrap();
    let add = |plan: &Path, source: &str| {
        std::fs::create_dir(root.path().join(source)).unwrap();
        assert!(agent(&[
            "estate-add".as_ref(),
            plan.as_os_str(),
            root.path().join(source).as_os_str(),
            root.path().join("repository").as_os_str(),
        ])
        .status
        .success());
    };
    let policy = |ledger: &Path, plan: &Path| {
        agent(&[
            "closure-dispose".as_ref(),
            ledger.as_os_str(),
            plan.as_os_str(),
            corpus.as_os_str(),
            "neo".as_ref(),
            "--policy".as_ref(),
            "GIT_NEST_STASHED".as_ref(),
            "accept".as_ref(),
            "jess".as_ref(),
            "2026-10-06".as_ref(),
        ])
    };
    // A plan path that holds no plan: no ledger is created.
    let none = policy(&root.path().join("l2"), &root.path().join("no-such-plan"));
    assert!(!none.status.success());
    assert!(!root.path().join("l2").exists());

    add(&plan, "s1");
    let recorded = policy(&reviews, &plan);
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );

    // Another plan placed at the same path, whose item refuses the policy's
    // code: the ledger written for the first plan binds nothing.
    let other = root.path().join("other-plan");
    add(&other, "s2");
    std::fs::rename(&other, &plan).unwrap();
    let report = |extra: &[&std::ffi::OsStr]| {
        let mut args: Vec<&std::ffi::OsStr> = vec!["closure-report".as_ref()];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            plan.as_os_str(),
            corpus.as_os_str(),
            "neo".as_ref(),
            state.as_os_str(),
        ]);
        agent(&args)
    };
    let native = String::from_utf8_lossy(&report(&[]).stdout).into_owned();
    let field = |name: &str| {
        let start = native.find(&format!("\"{name}\":\"")).unwrap() + name.len() + 4;
        native[start..start + native[start..].find('"').unwrap()].to_owned()
    };
    let record = OutcomeRecord {
        source: field("source").into(),
        outcome: Outcome::Refused(Refusal::new("GIT_NEST_STASHED", "estate::apply", None).unwrap()),
        reason: Some("GIT_NEST_STASHED".into()),
    };
    std::fs::write(
        state.join(format!("{}.outcome", field("item"))),
        record.encode().unwrap(),
    )
    .unwrap();
    let swapped = report(&["--dispositions".as_ref(), reviews.as_os_str()]);
    assert!(!swapped.status.success());
    assert!(swapped.stdout.is_empty());
    assert!(String::from_utf8_lossy(&swapped.stderr).contains("RECEIPT_BINDING_INVALID"));
    let append = policy(&reviews, &plan);
    assert!(String::from_utf8_lossy(&append.stderr).contains("RECEIPT_BINDING_INVALID"));
    // Without the ledger the refusal is pending review, as it should be.
    let pending = report(&[]);
    assert!(
        String::from_utf8_lossy(&pending.stdout).contains("\"verdict\":\"pass\",\"gate\":\"fail\"")
    );
}

// WP3 PR 4 (S4): a copy session's outcomes are rows of the same closure
// report, reviewed through the same disposition ledger, and gate the run
// with its planned items. Before WP3 PR 4 the copy's refusal reached only
// stderr and the report passed over it.
#[test]
#[allow(clippy::too_many_lines)]
fn a_copy_session_is_rows_of_the_closure_report_and_its_gate() {
    use bulkload_agent::outcome::{Outcome, OutcomeRecord, Refusal};
    let root = tempfile::tempdir().unwrap();
    let (plan, state, corpus, reviews) = (
        root.path().join("plan"),
        root.path().join("state"),
        root.path().join("corpus"),
        root.path().join("reviews"),
    );
    let (source, destination, dest_state) = (
        root.path().join("tree"),
        root.path().join("destination"),
        root.path().join("destination-state"),
    );
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(root.path().join("source")).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(source.join("kept"), b"the source's bytes").unwrap();
    std::fs::write(source.join("carried"), b"carried").unwrap();
    // The destination already holds other bytes at `kept`: no-clobber.
    std::fs::write(destination.join("kept"), b"someone else's").unwrap();
    assert!(agent(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        root.path().join("source").as_os_str(),
        root.path().join("repository").as_os_str(),
    ])
    .status
    .success());
    let report = |extra: &[&std::ffi::OsStr]| {
        let mut args: Vec<&std::ffi::OsStr> = vec!["closure-report".as_ref()];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            plan.as_os_str(),
            corpus.as_os_str(),
            "neo".as_ref(),
            state.as_os_str(),
        ]);
        agent(&args)
    };
    // The planned item holds a typed refusal; a policy closes it below.
    let native = String::from_utf8_lossy(&report(&[]).stdout).into_owned();
    let field = |text: &str, name: &str| {
        let start = text.find(&format!("\"{name}\":\"")).unwrap() + name.len() + 4;
        text[start..start + text[start..].find('"').unwrap()].to_owned()
    };
    let (item, item_source) = (field(&native, "item"), field(&native, "source"));
    std::fs::write(
        state.join(format!("{item}.outcome")),
        OutcomeRecord {
            source: item_source.into(),
            outcome: Outcome::Refused(
                Refusal::new("GIT_NEST_STASHED", "estate::capture", None).unwrap(),
            ),
            reason: None,
        }
        .encode()
        .unwrap(),
    )
    .unwrap();

    let copied = agent(&[
        "copy".as_ref(),
        source.as_os_str(),
        destination.as_os_str(),
        root.path().join("source-state").as_os_str(),
        dest_state.as_os_str(),
    ]);
    assert!(!copied.status.success());
    assert!(String::from_utf8_lossy(&copied.stderr).contains("refused kept: DESTINATION_OCCUPIED"));
    // #231 (S4): the run-level exit names the refused paths, never a
    // contract fault.
    assert_eq!(
        String::from_utf8_lossy(&copied.stderr).lines().last(),
        Some("bulkload-agent: refused: ITEMS_REFUSED"),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    // One record per transfer, under transfer.closure.d (review finding 2).
    let records = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dest_state.join("transfer.closure.d"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "closure"))
            .collect()
    };
    assert_eq!(records().len(), 1);

    let transfer: &[&std::ffi::OsStr] = &["--transfer".as_ref(), dest_state.as_os_str()];
    let joined = report(transfer);
    let out = String::from_utf8_lossy(&joined.stdout).into_owned();
    assert!(!joined.status.success());
    assert!(
        out.contains("\"schema\":\"bulkload.closure.v5\",\"verdict\":\"pass\",\"gate\":\"fail\"")
            && out.contains("\"planned\":1,\"transfer_rows\":2,\"applied\":1")
            && out.contains("\"refused_pending_review\":2")
            && out.contains("\"disposition\":\"applied\",\"kind\":\"transfer-session\"")
            && out.contains("\"disposition\":\"refused-pending-review\",\"kind\":\"transfer-entry\"")
            && out.contains("\"path\":\"kept\",\"transfer_outcome\":\"refused\",\"refusal\":\"DESTINATION_OCCUPIED\",\"site\":\"transfer::copy\""),
        "{out}"
    );
    // The entry row's identity, as the report names it.
    let entry = {
        let at = out.find("\"path\":\"kept\"").unwrap();
        let start = out[..at].rfind("{\"item\":\"").unwrap();
        field(&out[start..], "item")
    };
    let dispose = |transfers: &[&std::ffi::OsStr], target: &str, code: &str| {
        let mut args: Vec<&std::ffi::OsStr> = vec!["closure-dispose".as_ref()];
        args.extend_from_slice(transfers);
        args.extend_from_slice(&[
            reviews.as_os_str(),
            plan.as_os_str(),
            corpus.as_os_str(),
            "neo".as_ref(),
            target.as_ref(),
            code.as_ref(),
            "accept".as_ref(),
            "jess".as_ref(),
            "2026-10-15".as_ref(),
            state.as_os_str(),
        ]);
        agent(&args)
    };
    // Without --transfer the entry is no row of the report: no review of it
    // is written ahead of a refusal the report holds.
    let unbound = dispose(&[], &entry, "DESTINATION_OCCUPIED");
    assert!(String::from_utf8_lossy(&unbound.stderr).contains("RECEIPT_BINDING_INVALID"));
    let recorded = dispose(transfer, &entry, "DESTINATION_OCCUPIED");
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    assert!(
        String::from_utf8_lossy(&recorded.stdout).contains(&format!("scope=item item={entry} "))
    );
    assert!(dispose(&[], "--policy", "GIT_NEST_STASHED")
        .status
        .success());

    let closed = report(&[
        "--dispositions".as_ref(),
        reviews.as_os_str(),
        "--transfer".as_ref(),
        dest_state.as_os_str(),
    ]);
    let out = String::from_utf8_lossy(&closed.stdout);
    assert!(closed.status.success(), "{out}");
    assert!(
        out.contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{out}"
    );

    // A session whose end is not on record never closes: the record gone,
    // the run's report fails whatever the reviews say.
    for record in records() {
        std::fs::remove_file(record).unwrap();
    }
    let gone = report(&[
        "--dispositions".as_ref(),
        reviews.as_os_str(),
        "--transfer".as_ref(),
        dest_state.as_os_str(),
    ]);
    let out = String::from_utf8_lossy(&gone.stdout);
    assert!(!gone.status.success());
    assert!(
        out.contains("\"verdict\":\"fail\"")
            && out.contains("\"unaccounted_reason\":\"no-transfer-record\""),
        "{out}"
    );
}

// Every entry below `root`: its relative path, kind, mode and bytes.
fn tree_of(root: &Path) -> Vec<(std::path::PathBuf, &'static str, u32, Vec<u8>)> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let mode = meta.permissions().mode();
            if meta.is_dir() {
                out.push((rel, "dir", mode, Vec::new()));
                stack.push(path);
            } else {
                out.push((rel, "file", mode, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

// A plan with one item (unaccounted: no capture), so closure-report prints
// its JSON; transfer rows are asserted by their content.
fn bare_plan(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let (plan, state, corpus) = (root.join("plan"), root.join("state"), root.join("corpus"));
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(root.join("planned")).unwrap();
    assert!(agent(&[
        "estate-add".as_ref(),
        plan.as_os_str(),
        root.join("planned").as_os_str(),
        root.join("repository").as_os_str(),
    ])
    .status
    .success());
    (plan, state, corpus)
}

// Review finding 1 (S2): copy's overlap refusal comes before any write. A
// DEST_STATE that already exists inside SOURCE (or inside DEST) is refused
// SNAPSHOT_ROOTS_OVERLAP and the tree holding it is byte for byte as it
// was: no transfer record, no lock, no temporary.
#[test]
fn a_copy_refused_for_overlapping_roots_writes_nothing_inside_them() {
    for inside in ["source", "destination"] {
        let root = tempfile::tempdir().unwrap();
        let (source, destination) = (root.path().join("source"), root.path().join("destination"));
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("payload"), b"live bytes").unwrap();
        let held = root.path().join(inside);
        let dest_state = held.join(".dstate");
        std::fs::create_dir(&dest_state).unwrap();
        let before = tree_of(&held);
        let refused = agent(&[
            "copy".as_ref(),
            source.as_os_str(),
            destination.as_os_str(),
            root.path().join("source-state").as_os_str(),
            dest_state.as_os_str(),
        ]);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("SNAPSHOT_ROOTS_OVERLAP"),
            "{inside}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(tree_of(&held), before, "{inside}: a write inside the root");
    }
}

// Review finding 2 (S4): two transfers that share one DEST_STATE keep their
// own rows. The first transfer's refused path is still a row of the report
// after the second transfer completes into the same DEST_STATE.
#[test]
fn transfers_sharing_a_dest_state_keep_each_others_rows() {
    let root = tempfile::tempdir().unwrap();
    let (plan, state, corpus) = bare_plan(root.path());
    let shared = root.path().join("shared-state");
    let copy = |n: &str| {
        agent(&[
            "copy".as_ref(),
            root.path().join(format!("src{n}")).as_os_str(),
            root.path().join(format!("dst{n}")).as_os_str(),
            root.path().join(format!("ss{n}")).as_os_str(),
            shared.as_os_str(),
        ])
    };
    for n in ["1", "2"] {
        std::fs::create_dir(root.path().join(format!("src{n}"))).unwrap();
        std::fs::create_dir(root.path().join(format!("dst{n}"))).unwrap();
        std::fs::write(root.path().join(format!("src{n}/x")), b"first").unwrap();
    }
    assert!(copy("1").status.success());
    // The landed x is replaced by an occupied directory and the source
    // moves on: the rerun refuses x.
    std::fs::remove_file(root.path().join("dst1/x")).unwrap();
    std::fs::create_dir(root.path().join("dst1/x")).unwrap();
    std::fs::write(root.path().join("dst1/x/someone"), b"else").unwrap();
    std::fs::write(root.path().join("src1/x"), b"second, longer").unwrap();
    let rerun = copy("1");
    assert!(!rerun.status.success());
    assert!(String::from_utf8_lossy(&rerun.stderr).contains("refused x: DESTINATION_OCCUPIED"));
    // Another transfer completes into the same DEST_STATE.
    assert!(copy("2").status.success());
    let report = agent(&[
        "closure-report".as_ref(),
        "--transfer".as_ref(),
        shared.as_os_str(),
        plan.as_os_str(),
        corpus.as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(!report.status.success());
    assert!(
        out.contains(
            "\"path\":\"x\",\"transfer_outcome\":\"refused\",\"refusal\":\"DESTINATION_OCCUPIED\""
        ),
        "{out}"
    );
    // Both sessions are rows: two session rows, both applied.
    assert_eq!(
        out.matches("\"disposition\":\"applied\",\"kind\":\"transfer-session\"")
            .count(),
        2,
        "{out}"
    );
}

// Review finding 3 (S4): one DEST_STATE named twice under two spellings is
// one DEST_STATE: refused, never joined twice (rows double-counted).
#[test]
fn a_dest_state_named_twice_through_an_alias_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (plan, state, corpus) = bare_plan(root.path());
    let (source, destination, dest_state) = (
        root.path().join("src"),
        root.path().join("dst"),
        root.path().join("ds"),
    );
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(source.join("x"), b"x").unwrap();
    assert!(agent(&[
        "copy".as_ref(),
        source.as_os_str(),
        destination.as_os_str(),
        root.path().join("ss").as_os_str(),
        dest_state.as_os_str(),
    ])
    .status
    .success());
    let alias = root.path().join("dst").join("..").join("ds");
    let report = agent(&[
        "closure-report".as_ref(),
        "--transfer".as_ref(),
        dest_state.as_os_str(),
        "--transfer".as_ref(),
        alias.as_os_str(),
        plan.as_os_str(),
        corpus.as_os_str(),
        "neo".as_ref(),
        state.as_os_str(),
    ]);
    assert!(!report.status.success());
    assert!(
        report.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&report.stdout)
    );
    assert!(String::from_utf8_lossy(&report.stderr).contains("FIELD_DOMAIN_VIOLATION"));
}
