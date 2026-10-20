//! P86 TRANSFER-CLOSURE (WP3 PR 4, S4, OI-1003-Q1, OI-1003-Q148).
//!
//! A `copy` or `pull` session's durable record joins the closure report as
//! rows: the session, each refused path and each covered sidecar. The gate
//! is green iff the session ended, every typed refusal (session or entry)
//! carries a review of its current instance or a standing policy (until
//! the 2026-10-19 plan of record, C4, an integrity failure had the ruled
//! default `abandon`, OI-1003-Q148; C14 carries corrupt stores as files
//! instead, so no default remains), and
//! nothing is a bare `IO`, a `FRAME_CODEC` or a retired code. Before WP3
//! PR 4 none of these rows existed: a transfer's refusals never reached the
//! gate, so a closure report could pass over a run whose transfers refused.

use super::*;
use crate::disposition::{Decision, Row as Review, Scope};
use crate::outcome::Refusal;
use crate::test_support::prop_config;
use crate::transfer::TransferStats;
use crate::transfer_outcome::{Entry, EntryOutcome, Session, TransferRecord};
use proptest::prelude::*;

fn reviews() -> crate::disposition::Ledger {
    crate::disposition::Ledger::bound(std::path::Path::new("/plan"), &"d".repeat(64), "neo")
}

// A DEST_STATE holding `record`.
fn state_with(record: &TransferRecord) -> tempfile::TempDir {
    let state = tempfile::tempdir().unwrap();
    assert!(crate::transfer_outcome::write(state.path(), record).unwrap());
    state
}

fn report_of(states: &[&std::path::Path]) -> Report {
    let mut report = Report::from_ledger(&Ledger::default());
    report
        .join_transfers(&states.iter().map(|s| s.to_path_buf()).collect::<Vec<_>>())
        .unwrap();
    report
}

fn ended(stats: TransferStats) -> TransferRecord {
    TransferRecord::ended(
        "pull",
        "neo:/Users/jess",
        std::path::Path::new("/srv/sting"),
        &Ok(stats),
        "transfer::pull",
    )
}

fn refused(stats: &mut TransferStats, path: &str, code: &str, sqlite: Option<i32>) {
    stats
        .refusals
        .push((path.as_bytes().to_vec(), code.to_owned()));
    if let Some(sqlite) = sqlite {
        stats
            .refusal_sqlite_codes
            .insert(path.as_bytes().to_vec(), sqlite);
    }
}

fn row<'r>(report: &'r Report, path: &str) -> &'r Row {
    report
        .rows
        .iter()
        .find(|row| {
            row.transfer
                .as_ref()
                .and_then(|detail| detail.path.as_deref())
                == Some(path.as_bytes())
        })
        .unwrap()
}

#[test]
#[allow(clippy::too_many_lines)]
fn a_transfer_session_joins_the_gate_row_by_row() {
    // One session's stats, with or without a bare IO among its refusals.
    let stats_of = |lost: bool| {
        let mut stats = TransferStats::default();
        refused(&mut stats, "kept", "DESTINATION_OCCUPIED", None);
        refused(
            &mut stats,
            "corrupt.db",
            "SQLITE_INTEGRITY_CHECK_FAILED",
            None,
        );
        // SQLITE_NOTADB, extended bits set: masked to its primary code.
        refused(&mut stats, "notadb.db", "SQLITE_BACKUP_FAILED", Some(0x11a));
        refused(&mut stats, "busy.db", "SQLITE_BACKUP_FAILED", Some(5));
        if lost {
            refused(&mut stats, "lost", "IO", None);
        }
        stats.sqlite_snapshots.push(b"state.db".to_vec());
        stats.sqlite_sidecars_covered.push(b"state.db-wal".to_vec());
        // C14: a corrupt store carried beside its path, and a sidecar
        // whose outcome is its database's one refusal.
        stats.sqlite_carried_corrupt.push((
            b"rescue.db".to_vec(),
            b"rescue.db.corrupt-20261001".to_vec(),
        ));
        stats
            .sqlite_sidecars_follow_refused
            .push((b"busy.db-wal".to_vec(), b"busy.db".to_vec()));
        stats
    };
    let dest = state_with(&ended(stats_of(true)));
    let report = report_of(&[dest.path()]);
    assert_eq!(report.planned(), 0);
    assert_eq!(report.transfer_rows(), 9);
    assert_eq!(report.applied, 1, "the session ran to its end");
    assert_eq!(
        report.covered, 2,
        "the covered sidecar and the following one"
    );
    assert_eq!(
        report.carried, 1,
        "the corrupt store, carried beside its path"
    );
    // No refusal has a ruled default since C14: every typed one waits for
    // its review.
    assert_eq!((report.refused, report.ruled_default), (0, 0));
    assert_eq!(report.refused_pending_review, 4);
    // A bare IO names no cause: unaccounted, so the native verdict fails.
    assert_eq!(report.unaccounted, 1);
    assert!(!report.native_passes() && !report.passes());
    assert_eq!(
        row(&report, "lost").disposition,
        Disposition::Unaccounted("refusal-untyped-io")
    );
    assert_eq!(
        row(&report, "corrupt.db").disposition,
        Disposition::RefusedPendingReview("SQLITE_INTEGRITY_CHECK_FAILED".into())
    );
    assert_eq!(
        row(&report, "busy.db").disposition,
        Disposition::RefusedPendingReview("SQLITE_BACKUP_FAILED".into())
    );
    assert_eq!(
        row(&report, "state.db-wal").disposition,
        Disposition::Covered
    );
    assert_eq!(row(&report, "rescue.db").disposition, Disposition::Carried);
    assert_eq!(
        row(&report, "busy.db-wal").disposition,
        Disposition::Covered
    );
    let json = report.to_json();
    assert!(
        json.contains("\"schema\":\"bulkload.closure.v5\"")
            && json.contains("\"transfer_rows\":9")
            && json.contains("\"ruled_default\":0"),
        "{json}"
    );
    assert!(
        json.contains("\"disposition\":\"refused-pending-review\",\"kind\":\"transfer-entry\""),
        "{json}"
    );
    assert!(json.contains(
        "\"path\":\"notadb.db\",\"transfer_outcome\":\"refused\",\"refusal\":\"SQLITE_BACKUP_FAILED\",\"site\":\"transfer::pull\",\"errno\":null,\"sqlite_code\":282}"
    ), "{json}");
    assert!(json.contains(
        "\"path\":\"rescue.db\",\"transfer_outcome\":\"carried-corrupt\",\"related\":\"rescue.db.corrupt-20261001\""
    ), "{json}");
    assert!(json.contains(
        "\"path\":\"busy.db-wal\",\"transfer_outcome\":\"sidecar-follows-refused-base\",\"related\":\"busy.db\""
    ), "{json}");
    assert!(
        json.contains(
            "\"path\":\"lost\",\"transfer_outcome\":\"refused\",\"recorded_refusal\":\"IO\""
        ),
        "{json}"
    );

    // The rerun has no bare IO; reviews close the rest: an item row for the
    // occupied path, a standing policy for the busy backup, an operator's
    // re-carry of the integrity failure.
    let dest = state_with(&ended(stats_of(false)));
    let mut report = report_of(&[dest.path()]);
    let mut ledger = reviews();
    for (path, code, decision) in [
        ("kept", "DESTINATION_OCCUPIED", Decision::Accept),
        (
            "corrupt.db",
            "SQLITE_INTEGRITY_CHECK_FAILED",
            Decision::ReCarry,
        ),
    ] {
        let found = row(&report, path);
        ledger.rows.push(
            Review::new(
                Scope::Item {
                    item: found.item.clone(),
                    instance: found.instance.clone(),
                },
                code,
                decision,
                "jess",
                "2026-10-15",
            )
            .unwrap(),
        );
    }
    ledger.rows.push(
        Review::new(
            Scope::Policy,
            "SQLITE_BACKUP_FAILED",
            Decision::Abandon,
            "jess",
            "2026-10-15",
        )
        .unwrap(),
    );
    report.dispose(&ledger);
    assert!(report.passes(), "{}", report.to_json());
    assert_eq!(report.unaccounted, 0);
    // The policy covers both backup failures; nothing has a default.
    assert_eq!(report.ruled_default, 0);
    let recarried = row(&report, "corrupt.db");
    assert_eq!(
        recarried.review.as_ref().unwrap().decision(),
        Decision::ReCarry
    );
    assert!(!recarried.by_default);
}

#[test]
fn a_transfer_that_never_ended_or_left_nothing_is_unaccounted() {
    // Each DEST_STATE records its own transfer (one destination each): one
    // transfer recorded in two states would count twice and refuses.
    let to = |n: &str| std::path::PathBuf::from(format!("/dst{n}"));
    let running = state_with(&TransferRecord::running("copy", "/src", &to("1")));
    let absent = tempfile::tempdir().unwrap();
    let garbage = tempfile::tempdir().unwrap();
    let item = TransferRecord::running("copy", "/src", &to("3")).session_item();
    std::fs::create_dir(crate::transfer_outcome::record_dir(garbage.path())).unwrap();
    std::fs::write(
        crate::transfer_outcome::record_path(garbage.path(), &item),
        b"no",
    )
    .unwrap();
    let bare = state_with(&TransferRecord::ended(
        "copy",
        "/src",
        &to("4"),
        &Err(BulkloadRefusal::Io(Some(5))),
        "transfer::copy",
    ));
    let typed = state_with(&TransferRecord::ended(
        "copy",
        "/src",
        &to("5"),
        &Err(BulkloadRefusal::SnapshotRootsOverlap),
        "transfer::copy",
    ));
    let report = report_of(&[
        running.path(),
        absent.path(),
        garbage.path(),
        bare.path(),
        typed.path(),
    ]);
    let reasons: Vec<&Disposition> = report.rows.iter().map(|row| &row.disposition).collect();
    assert_eq!(
        reasons,
        vec![
            &Disposition::Unaccounted("transfer-interrupted"),
            &Disposition::Unaccounted("no-transfer-record"),
            &Disposition::Unaccounted("transfer-record-unreadable"),
            &Disposition::Unaccounted("refusal-untyped-io"),
            &Disposition::RefusedPendingReview("SNAPSHOT_ROOTS_OVERLAP".into()),
        ]
    );
    assert!(report
        .rows
        .iter()
        .all(|row| row.kind == RowKind::TransferSession));
    // A DEST_STATE named twice is refused, as an item listed twice is,
    // under any spelling of it (review finding 3).
    let mut twice = Report::from_ledger(&Ledger::default());
    assert_eq!(
        twice.join_transfers(&[typed.path().to_path_buf(), typed.path().to_path_buf()]),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    let alias = typed
        .path()
        .join("..")
        .join(typed.path().file_name().unwrap());
    let mut aliased = Report::from_ledger(&Ledger::default());
    assert_eq!(
        aliased.join_transfers(&[typed.path().to_path_buf(), alias]),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    // An empty DEST_STATE under two spellings is one state too: its one
    // no-transfer-record row is keyed on the canonical path.
    let empty_alias = absent
        .path()
        .join("..")
        .join(absent.path().file_name().unwrap());
    let mut empty = Report::from_ledger(&Ledger::default());
    assert_eq!(
        empty.join_transfers(&[absent.path().to_path_buf(), empty_alias]),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    // One transfer recorded in two DEST_STATEs would count twice.
    let again = state_with(&TransferRecord::ended(
        "copy",
        "/src",
        &to("5"),
        &Ok(TransferStats::default()),
        "transfer::copy",
    ));
    let mut doubled = Report::from_ledger(&Ledger::default());
    assert_eq!(
        doubled.join_transfers(&[typed.path().to_path_buf(), again.path().to_path_buf()]),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    // A record filed under another transfer's name proves nothing.
    let misplaced = state_with(&TransferRecord::ended(
        "copy",
        "/src",
        &to("6"),
        &Ok(TransferStats::default()),
        "transfer::copy",
    ));
    let right = TransferRecord::running("copy", "/src", &to("6")).session_item();
    std::fs::rename(
        crate::transfer_outcome::record_path(misplaced.path(), &right),
        crate::transfer_outcome::record_path(misplaced.path(), &"f".repeat(64)),
    )
    .unwrap();
    assert_eq!(
        report_of(&[misplaced.path()]).rows[0].disposition,
        Disposition::Unaccounted("transfer-record-misplaced")
    );
}

// Review finding 2: transfers sharing one DEST_STATE are each their own
// rows; neither replaces the other's record.
#[test]
fn transfers_sharing_a_dest_state_are_each_their_rows() {
    let shared = tempfile::tempdir().unwrap();
    let mut refusing = TransferStats::default();
    refused(&mut refusing, "x", "DESTINATION_OCCUPIED", None);
    for (source, stats) in [("/src1", refusing), ("/src2", TransferStats::default())] {
        crate::transfer_outcome::record_session(
            shared.path(),
            "copy",
            source,
            std::path::Path::new(source),
            "transfer::copy",
            || Ok(()),
            || Ok(stats),
        )
        .unwrap();
    }
    let report = report_of(&[shared.path()]);
    assert_eq!(report.transfer_rows(), 3);
    assert_eq!(report.applied, 2);
    assert_eq!(
        row(&report, "x").disposition,
        Disposition::RefusedPendingReview("DESTINATION_OCCUPIED".into())
    );
}

// Review finding 3: a manifest bound to the PLAN names the transfers a run
// expects. A listed transfer with no record, or with a record older than
// the manifest's `since`, is unaccounted; a record the manifest does not
// list in the same DEST_STATE is not joined; the manifest's PLAN binds.
#[test]
#[allow(clippy::too_many_lines)]
fn a_manifest_binds_the_runs_transfers_to_its_plan() {
    use crate::transfer_outcome::{expect, Expected, Manifest};
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan");
    for dir in ["checkout", "elsewhere"] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    crate::estate::add(
        &plan,
        &root.path().join("checkout"),
        &root.path().join("repository"),
        None,
    )
    .unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let expected = |n: &str| Expected {
        verb: "copy".into(),
        source: format!("/src{n}"),
        destination: std::path::PathBuf::from(format!("/dst{n}")),
        state: state.clone(),
    };
    let record = |n: &str, started: u64| {
        let mut done = TransferRecord::ended(
            "copy",
            &format!("/src{n}"),
            std::path::Path::new(&format!("/dst{n}")),
            &Ok(TransferStats::default()),
            "transfer::copy",
        );
        done.started = started;
        assert!(crate::transfer_outcome::write(&state, &done).unwrap());
    };
    let path = root.path().join("manifest");
    for n in ["1", "2", "3"] {
        expect(&path, &plan, expected(n)).unwrap();
    }
    // Listing one again is a no-op; the same transfer in another state is not.
    assert_eq!(
        expect(&path, &plan, expected("1")).unwrap().transfers.len(),
        3
    );
    let mut elsewhere = expected("1");
    elsewhere.state = root.path().join("other");
    assert_eq!(
        expect(&path, &plan, elsewhere),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    let manifest = Manifest::read(&path, &plan).unwrap();
    // 1 ran in this run, 2 before it (stale), 3 never; 4 is not listed.
    record("1", manifest.since);
    record("2", manifest.since - 1);
    record("4", manifest.since);
    let mut report = Report::from_ledger(&Ledger::default());
    report.join_manifest(&manifest).unwrap();
    let reasons: Vec<&Disposition> = report.rows.iter().map(|row| &row.disposition).collect();
    assert_eq!(
        reasons,
        vec![
            &Disposition::Applied,
            &Disposition::Unaccounted("transfer-record-stale"),
            &Disposition::Unaccounted("no-transfer-record"),
        ]
    );
    assert_eq!(report.rows[2].item, expected("3").item());
    assert!(!report.passes());
    let json = report.to_json();
    assert!(
        json.contains(&format!(
            "\"transfer_manifest\":{{\"path\":{:?},\"plan_digest\":\"{}\",\"since\":{},\"expected\":3}}",
            path.to_string_lossy(),
            manifest.plan_digest,
            manifest.since
        )),
        "{json}"
    );
    assert!(
        json.contains(&format!("\"started\":{}", manifest.since)),
        "{json}"
    );
    // A second manifest, or the same DEST_STATE joined ad hoc too, refuses.
    assert_eq!(
        report.join_manifest(&manifest),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    let mut both = Report::from_ledger(&Ledger::default());
    both.join_manifest(&manifest).unwrap();
    assert_eq!(
        both.join_transfers(std::slice::from_ref(&state)),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    // Another plan's bytes: the manifest does not bind.
    let other = root.path().join("other-plan");
    crate::estate::add(
        &other,
        &root.path().join("elsewhere"),
        &root.path().join("repository"),
        None,
    )
    .unwrap();
    assert_eq!(
        Manifest::read(&path, &other),
        Err(BulkloadRefusal::ReceiptBindingInvalid)
    );
    // A verb that does not match the listed one is a mismatch.
    let mut pulled = TransferRecord::ended(
        "pull",
        "/src3",
        std::path::Path::new("/dst3"),
        &Ok(TransferStats::default()),
        "transfer::pull",
    );
    pulled.started = manifest.since;
    assert!(crate::transfer_outcome::write(&state, &pulled).unwrap());
    let mut report = Report::from_ledger(&Ledger::default());
    report.join_manifest(&manifest).unwrap();
    assert_eq!(
        report.rows[2].disposition,
        Disposition::Unaccounted("transfer-record-mismatch")
    );
}

#[test]
fn an_attestation_never_closes_a_transfer_row() {
    let running = state_with(&TransferRecord::running(
        "copy",
        "/src",
        std::path::Path::new("/dst"),
    ));
    let mut report = report_of(&[running.path()]);
    let item = report.rows[0].item.clone();
    let document = format!(
        r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{{"item":"{item}","source":"/src","capture":null,"disposition":"present","basis":"audit","evidence":"p86"}}]}}"#
    );
    report
        .attest(&AttestationLedger::parse(&document, std::path::Path::new("/plan"), "neo").unwrap())
        .unwrap();
    let attested = report.attested.as_ref().unwrap();
    assert_eq!(attested.foreign, vec![item]);
    assert!(attested.items.is_empty());
    assert_eq!(report.remaining_unaccounted(), 1);
    assert!(!report.passes());
}

#[test]
fn a_transfer_review_is_bound_to_its_refusal_instance() {
    let busy = |sqlite: i32| {
        let mut stats = TransferStats::default();
        refused(&mut stats, "busy.db", "SQLITE_BACKUP_FAILED", Some(sqlite));
        stats
    };
    let first = state_with(&ended(busy(5)));
    let report = report_of(&[first.path()]);
    let found = row(&report, "busy.db");
    assert_eq!(
        report.reviewable(&found.item, "SQLITE_BACKUP_FAILED"),
        Some(found.instance.as_str())
    );
    let mut ledger = reviews();
    ledger.rows.push(
        Review::new(
            Scope::Item {
                item: found.item.clone(),
                instance: found.instance.clone(),
            },
            "SQLITE_BACKUP_FAILED",
            Decision::Accept,
            "jess",
            "2026-10-15",
        )
        .unwrap(),
    );
    // A rerun refusing identically is the same instance: the review stands.
    let again = state_with(&ended(busy(5)));
    let mut report = report_of(&[again.path()]);
    report.dispose(&ledger);
    assert!(report.passes());
    // The same path refused with another SQLite code is another instance.
    let changed = state_with(&ended(busy(10)));
    let mut report = report_of(&[changed.path()]);
    report.dispose(&ledger);
    assert!(!report.passes());
    assert_eq!(report.pending_review(), 1);
    assert_eq!(
        report.reviewed.as_ref().unwrap().unmatched[0].2,
        "refusal-instance-stale"
    );
}

/// The codes P86 draws an entry's refusal from: every taxonomy code
/// (`IO` and `FRAME_CODEC` among them), and one retired code.
fn drawn_codes() -> Vec<&'static str> {
    let mut codes = BulkloadRefusal::CODES.to_vec();
    codes.push(RETIRED);
    codes
}

const RETIRED: &str = "RETIRED_FOR_P86";

/// A drawn entry: its code, its `SQLite` code and its review (0 none, 1 an
/// item row for its instance, 2 a standing policy, 3 an item row for
/// another instance); a covered sidecar; a corrupt store carried beside
/// its path (C14); or a sidecar following its base's refusal.
#[derive(Debug, Clone)]
enum Drawn {
    Refused(usize, Option<i32>, u8),
    Covered,
    CarriedCorrupt,
    Follows,
}

fn drawn() -> impl Strategy<Value = Drawn> {
    prop_oneof![
        6 => (
            0..drawn_codes().len(),
            prop_oneof![
                Just(None),
                Just(Some(5)),
                Just(Some(11)),
                Just(Some(26)),
                Just(Some(0x10b)),
                Just(Some(0x20a)),
            ],
            0..4_u8
        )
            .prop_map(|(code, sqlite, how)| Drawn::Refused(code, sqlite, how)),
        1 => Just(Drawn::Covered),
        1 => Just(Drawn::CarriedCorrupt),
        1 => Just(Drawn::Follows),
    ]
}

/// The session a record ends with: 0 completed, 1 running, 2 a typed
/// refusal (reviewed by its policy when `true`), 3 a bare `IO`.
fn session(kind: u8) -> Session {
    match kind {
        0 => Session::Completed,
        1 => Session::Running,
        2 => Session::Refused(
            Refusal::new("SNAPSHOT_ROOTS_OVERLAP", "transfer::pull", None).unwrap(),
        ),
        _ => Session::Refused(Refusal::new("IO", "transfer::pull", Some(5)).unwrap()),
    }
}

/// No refusal has a ruled default since C14 (RUL-14; plan of record
/// 2026-10-19, C4): the model's default clause is always false.
fn ruled(code: &str, sqlite: Option<i32>) -> bool {
    let _ = (code, sqlite);
    false
}

proptest! {
    #![proptest_config(prop_config(256))]

    /// Green iff the session ended (or refused typed, reviewed) and every
    /// entry is covered, or a typed refusal disposed by its own instance's
    /// review, a standing policy or its ruled default; never with a bare
    /// IO, a FRAME_CODEC, a retired code or an interrupted converge.
    #[test]
    fn p86_a_transfer_closes_iff_every_refusal_is_typed_and_disposed(
        entries in proptest::collection::vec(drawn(), 0..10),
        kind in 0..4_u8,
        policy_session in any::<bool>(),
    ) {
        let codes = drawn_codes();
        let mut record = TransferRecord::running("pull", "neo:/x", std::path::Path::new("/y"));
        record.session = session(kind);
        for (index, entry) in entries.iter().enumerate() {
            let rel_path = format!("p{index}").into_bytes();
            record.entries.push(Entry {
                rel_path,
                outcome: match entry {
                    Drawn::Refused(code, sqlite, _) => EntryOutcome::Refused {
                        refusal: Refusal::decoded(codes[*code], "transfer::pull", None).unwrap(),
                        sqlite_code: *sqlite,
                    },
                    Drawn::Covered => EntryOutcome::SidecarCovered,
                    Drawn::CarriedCorrupt => EntryOutcome::CarriedCorrupt {
                        beside: format!("p{index}.corrupt-20261019").into_bytes(),
                    },
                    Drawn::Follows => EntryOutcome::SidecarFollowsRefusedBase {
                        database: b"base.db".to_vec(),
                    },
                },
            });
        }
        let state = state_with(&record);
        let mut report = report_of(&[state.path()]);
        let mut ledger = reviews();
        let mut policies = std::collections::BTreeSet::new();
        for (entry, drawn) in record.entries.iter().zip(&entries) {
            let Drawn::Refused(code, _, how) = drawn else { continue };
            let code = codes[*code];
            let item = record.entry_item(entry);
            let found = report.rows.iter().find(|row| row.item == item).unwrap();
            let scope = match how {
                1 => Scope::Item { item: item.clone(), instance: found.instance.clone() },
                2 => Scope::Policy,
                3 => Scope::Item { item: item.clone(), instance: item.clone() },
                _ => continue,
            };
            // No review can name an untyped or retired code.
            if let Ok(review) = Review::new(scope, code, Decision::Accept, "p86", "2026-10-15") {
                if *how == 2 {
                    policies.insert(code);
                }
                ledger.rows.push(review);
            }
        }
        if policy_session {
            ledger.rows.push(Review::new(Scope::Policy, "SNAPSHOT_ROOTS_OVERLAP", Decision::Accept, "p86", "2026-10-15").unwrap());
            policies.insert("SNAPSHOT_ROOTS_OVERLAP");
        }
        report.dispose(&ledger);

        // The model, from the draws alone.
        let closes = |code: &str, sqlite: Option<i32>, how: u8| {
            crate::outcome::is_typed_code(code)
                && crate::closure::rerun_only(code).is_none()
                && (how == 1 || policies.contains(code) || ruled(code, sqlite))
        };
        let session_ok = match kind {
            0 => true,
            2 => policy_session,
            _ => false,
        };
        let green = session_ok
            && entries.iter().all(|drawn| match drawn {
                Drawn::Covered | Drawn::CarriedCorrupt | Drawn::Follows => true,
                Drawn::Refused(code, sqlite, how) => closes(codes[*code], *sqlite, *how),
            });
        prop_assert_eq!(report.passes(), green, "{:?} {:?}", kind, entries);
        prop_assert_eq!(report.gate().is_ok(), green);
        // Every row is counted exactly once, and a typed refusal is never
        // unaccounted nor an untyped one pending.
        prop_assert_eq!(report.transfer_rows(), 1 + entries.len() as u64);
        prop_assert_eq!(
            report.applied + report.refused + report.refused_pending_review
                + report.covered + report.carried + report.unaccounted,
            report.transfer_rows()
        );
        prop_assert_eq!(
            report.carried,
            entries.iter().filter(|drawn| matches!(drawn, Drawn::CarriedCorrupt)).count() as u64
        );
        let typed = entries.iter().filter(|drawn| matches!(drawn,
            Drawn::Refused(code, _, _) if crate::outcome::is_typed_code(codes[*code])
                && crate::closure::rerun_only(codes[*code]).is_none())).count() as u64
            + u64::from(kind == 2);
        prop_assert_eq!(report.refused + report.refused_pending_review, typed);
        // A ruled default disposes only what no review row disposes.
        let defaulted = entries.iter().filter(|drawn| matches!(drawn,
            Drawn::Refused(code, sqlite, how) if crate::outcome::is_typed_code(codes[*code])
                && ruled(codes[*code], *sqlite)
                && *how != 1 && !policies.contains(codes[*code]))).count() as u64;
        prop_assert_eq!(report.ruled_default, defaulted);
    }
}

/// Plan of record 2026-10-19, C1/C2: a seat `--beside SUFFIX` landed beside
/// a differing live file is an `applied-beside` transfer row, accounted and
/// counted in `applied_beside`, never pending review, and the row names the
/// beside path; a refused path in the same session still holds the gate,
/// so the gate reads the agent state exactly as it is on the destination.
#[test]
fn an_applied_beside_entry_is_accounted_and_gate_relevant() {
    let stats_of = || {
        let mut stats = TransferStats::default();
        stats.applied_beside.push((
            b"settings.json".to_vec(),
            b"settings.json.neo-20261019".to_vec(),
        ));
        stats.applied_beside.push((
            b"state_5.sqlite".to_vec(),
            b"state_5.sqlite.neo-20261019".to_vec(),
        ));
        stats.sqlite_snapshots.push(b"state_5.sqlite".to_vec());
        stats.completed = 2;
        stats
    };
    let dest = state_with(&ended(stats_of()));
    let report = report_of(&[dest.path()]);
    assert_eq!(report.transfer_rows(), 3, "the session and two entries");
    assert_eq!(report.applied, 1, "the session ran to its end");
    assert_eq!(report.applied_beside, 2);
    assert_eq!((report.unaccounted, report.refused_pending_review), (0, 0));
    assert!(report.passes(), "nothing pending: the gate passes");
    let placed = row(&report, "settings.json");
    assert_eq!(placed.disposition, Disposition::AppliedBeside);
    assert_eq!(placed.kind, RowKind::TransferEntry);
    assert_eq!(
        placed
            .transfer
            .as_ref()
            .and_then(|detail| detail.beside.as_deref()),
        Some(&b"settings.json.neo-20261019"[..])
    );
    let json = report.to_json();
    assert!(
        json.contains(
            "\"transfer_outcome\":\"applied-beside\",\"beside\":\"settings.json.neo-20261019\""
        ),
        "{json}"
    );
    assert!(json.contains("\"applied_beside\":2"), "{json}");

    // The same session with one path refused beside the two landed: the
    // beside rows stay accounted, the refusal holds the gate.
    let mut stats = stats_of();
    refused(&mut stats, "kept", "DESTINATION_OCCUPIED", None);
    let dest = state_with(&ended(stats));
    let report = report_of(&[dest.path()]);
    assert_eq!(report.applied_beside, 2);
    assert_eq!(report.refused_pending_review, 1);
    assert_eq!(report.unaccounted, 0);
    assert!(!report.passes());
    assert_eq!(
        row(&report, "kept").disposition,
        Disposition::RefusedPendingReview("DESTINATION_OCCUPIED".into())
    );
}
