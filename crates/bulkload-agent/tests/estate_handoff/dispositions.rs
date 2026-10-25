//! SPEC Done 2: accepted refusals close only their reviewed instance.

use super::*;
use bulkload_agent::disposition::{self, Decision, Row, Scope};
use bulkload_agent::outcome::OutcomeRecord;
use bulkload_agent::BulkloadRefusal;

const CODE: &str = "GIT_DESTINATION_MODIFIED";

fn refuse(world: &World, site: &'static str, reason: &str) -> String {
    let ledger =
        bulkload_agent::estate::ledger(&world.plan(), &world.corpus(), SOURCE, &[world.state()])
            .unwrap();
    let item = ledger
        .entries
        .iter()
        .find(|entry| entry.source == world.src())
        .unwrap()
        .item
        .clone();
    let record = OutcomeRecord::of_receipt(
        world.src(),
        "refused",
        Some(&BulkloadRefusal::GitDestinationModified),
        Some(reason.to_owned()),
        site,
    )
    .unwrap();
    fs::write(
        world.state().join(format!("{item}.outcome")),
        record.encode().unwrap(),
    )
    .unwrap();
    item
}

fn dispose(world: &World, item: &str, decision: &str) -> PathBuf {
    let ledger = world.root.join("reviews");
    let ran = agent(&os(&[
        &"closure-dispose",
        &ledger,
        &world.plan(),
        &world.corpus(),
        &SOURCE,
        &item,
        &CODE,
        &decision,
        &"jess",
        &"2026-10-24",
        &world.state(),
    ]));
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    ledger
}

fn check(world: &World, ledger: &Path) -> Ran {
    world.handoff(&["--check", "--dispositions", ledger.to_str().unwrap()])
}

fn refused_rows(ran: &Ran) -> usize {
    ran.stdout
        .lines()
        .filter(|line| line.contains("\"class\":\"refused\""))
        .count()
}

#[test]
fn handoff_dispositions_exact_accept_closes_and_changed_record_reopens() {
    let world = World::new();
    let item = refuse(&world, "test::original", "original reason");
    assert!(world.handoff(&[]).ok);
    let before = world.handoff(&["--check"]);
    assert!(refused_rows(&before) > 0, "{}", before.stdout);
    let ledger = dispose(&world, &item, "accept");
    let frozen = freeze_all(&world.root);
    let accepted = check(&world, &ledger);
    assert!(accepted.ok, "{}\n{}", accepted.stdout, accepted.stderr);
    assert_eq!(refused_rows(&accepted), 0, "{}", accepted.stdout);
    assert!(accepted.stdout.contains("\"kind\":\"reviewed-refusal\""));
    assert!(accepted.stdout.contains("\"decision\":\"accept\""));
    assert!(accepted.stdout.contains(CODE));
    assert_eq!(freeze_all(&world.root), frozen, "--check wrote files");

    // The same code and item at another site is a new refusal instance.
    refuse(&world, "test::later", "original reason");
    let changed = check(&world, &ledger);
    assert!(changed.ok, "{}", changed.stderr);
    assert!(refused_rows(&changed) > 0, "{}", changed.stdout);
    assert!(!changed.stdout.contains("\"kind\":\"reviewed-refusal\""));

    dispose(&world, &item, "accept");
    refuse(&world, "test::later", "different reason");
    assert!(refused_rows(&check(&world, &ledger)) > 0);

    dispose(&world, &item, "accept");
    world.source_change(b"a changed capture\n");
    assert!(refused_rows(&check(&world, &ledger)) > 0);
}

#[test]
fn handoff_dispositions_work_decisions_and_policies_remain_open() {
    let world = World::new();
    let item = refuse(&world, "test::original", "held");
    assert!(world.handoff(&[]).ok);
    for decision in ["re-carry", "abandon"] {
        let ledger = dispose(&world, &item, decision);
        let ran = check(&world, &ledger);
        assert!(ran.ok, "{}", ran.stderr);
        assert!(refused_rows(&ran) > 0, "{decision}: {}", ran.stdout);
    }
    let policy = world.root.join("policy");
    disposition::record(
        &policy,
        &world.plan(),
        SOURCE,
        Row::new(Scope::Policy, CODE, Decision::Accept, "jess", "2026-10-24").unwrap(),
    )
    .unwrap();
    let ran = check(&world, &policy);
    assert!(ran.ok, "{}", ran.stderr);
    assert!(refused_rows(&ran) > 0, "{}", ran.stdout);
}

#[test]
fn handoff_dispositions_placed_worktrees_keep_original_item_identity() {
    let world = World::new();
    let item = refuse(&world, "test::mapped", "held at the placed path");
    let ledger = dispose(&world, &item, "accept");
    let original = world.root.join("dst");
    let placed = world.root.join("placed");
    fs::rename(&original, &placed).unwrap();
    let main = placed.join("main");
    let linked = placed.join("wt");
    git(
        &main,
        &[
            "worktree",
            "repair",
            main.to_str().unwrap(),
            linked.to_str().unwrap(),
        ],
    );
    let mapping = format!("{}={}", original.display(), placed.display());
    let before = world.handoff(&["--check", "--place", &mapping]);
    assert!(before.ok, "{}\n{}", before.stdout, before.stderr);
    assert!(refused_rows(&before) > 0, "{}", before.stdout);
    assert!(
        !before.stdout.contains("\"class\":\"dest-only\""),
        "{}",
        before.stdout
    );

    let frozen = freeze_all(&world.root);
    let accepted = world.handoff(&[
        "--check",
        "--place",
        &mapping,
        "--dispositions",
        ledger.to_str().unwrap(),
    ]);
    assert!(accepted.ok, "{}\n{}", accepted.stdout, accepted.stderr);
    assert_eq!(refused_rows(&accepted), 0, "{}", accepted.stdout);
    assert!(
        !accepted.stdout.contains("\"class\":\"dest-only\""),
        "{}",
        accepted.stdout
    );
    assert!(accepted.stdout.contains(&format!("\"item\":\"{item}\"")));
    assert!(accepted.stdout.contains("\"kind\":\"reviewed-refusal\""));
    assert_eq!(
        freeze_all(&world.root),
        frozen,
        "placed --check wrote files"
    );
    assert!(row(&accepted.stdout, &main).contains(&format!("\"item\":\"{item}\"")));
    assert!(row(&accepted.stdout, &linked).contains("\"outcome\":\"checked\""));
}

#[test]
fn handoff_dispositions_rejects_other_plan_or_source_and_keeps_other_rows() {
    let world = World::new();
    let item = refuse(&world, "test::original", "held");
    assert!(world.handoff(&[]).ok);
    let ledger = dispose(&world, &item, "accept");
    let extra = world.root.join("destination-only");
    git(
        &world.dst(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "independent",
            extra.to_str().unwrap(),
        ],
    );
    fs::write(extra.join("unique.txt"), b"destination-only work\n").unwrap();
    let ran = check(&world, &ledger);
    assert!(ran.ok, "{}", ran.stderr);
    assert_eq!(refused_rows(&ran), 0, "{}", ran.stdout);
    assert!(
        ran.stdout.contains("\"class\":\"dest-only\""),
        "{}",
        ran.stdout
    );
    assert!(!ready(&ran.stdout, &world.dst()), "{}", ran.stdout);

    let reviews = disposition::Ledger::read(&ledger, &world.plan(), SOURCE).unwrap();
    let mut foreign = reviews.clone();
    foreign.source_label = "another-host".to_owned();
    let path = world.root.join("foreign");
    fs::write(&path, foreign.encode().unwrap()).unwrap();
    let refused = check(&world, &path);
    assert!(!refused.ok);
    assert!(refused.stderr.contains("RECEIPT_BINDING_INVALID"));

    let mut foreign = reviews.clone();
    foreign.plan = world.root.join("another-plan");
    fs::write(&path, foreign.encode().unwrap()).unwrap();
    let refused = check(&world, &path);
    assert!(!refused.ok);
    assert!(refused.stderr.contains("RECEIPT_BINDING_INVALID"));

    let mut foreign = reviews;
    foreign.plan_digest = "f".repeat(64);
    fs::write(&path, foreign.encode().unwrap()).unwrap();
    let refused = check(&world, &path);
    assert!(!refused.ok);
    assert!(refused.stderr.contains("RECEIPT_BINDING_INVALID"));
}
