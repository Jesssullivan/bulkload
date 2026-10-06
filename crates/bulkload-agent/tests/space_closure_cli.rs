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

    let dispose = |target: &str, code: &str, label: &str| {
        agent(&[
            "closure-dispose".as_ref(),
            reviews.as_os_str(),
            plan.as_os_str(),
            label.as_ref(),
            target.as_ref(),
            code.as_ref(),
            "accept".as_ref(),
            "jess".as_ref(),
            "2026-10-06".as_ref(),
        ])
    };
    // A bare IO names no cause: no review can record it.
    let io = dispose(&item, "IO", "neo");
    assert!(!io.status.success());
    assert!(String::from_utf8_lossy(&io.stderr).contains("FIELD_DOMAIN_VIOLATION"));
    assert!(!reviews.exists());
    // An item the plan does not hold binds nothing.
    let foreign = dispose(&"f".repeat(64), "GIT_NEST_STASHED", "neo");
    assert!(String::from_utf8_lossy(&foreign.stderr).contains("RECEIPT_BINDING_INVALID"));

    let recorded = dispose(&item, "GIT_NEST_STASHED", "neo");
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    assert!(String::from_utf8_lossy(&recorded.stdout).contains(&format!(
        "disposition recorded scope=item item={item} refusal=GIT_NEST_STASHED decision=accept"
    )));
    // The ledger is bound to its SOURCE label.
    let elsewhere = dispose("--policy", "GIT_NEST_STASHED", "sting");
    assert!(String::from_utf8_lossy(&elsewhere.stderr).contains("RECEIPT_BINDING_INVALID"));

    let closed = report(&["--dispositions".as_ref(), reviews.as_os_str()]);
    let out = String::from_utf8_lossy(&closed.stdout);
    assert!(closed.status.success(), "{out}");
    assert!(
        out.contains("\"verdict\":\"pass\",\"gate\":\"pass\"")
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
}
