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
        out.contains("\"schema\":\"bulkload.closure.v2\",\"verdict\":\"pass\",\"gate\":\"pass\"")
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
