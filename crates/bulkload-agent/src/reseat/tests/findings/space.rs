//! M2 round 2, item 1 (2026-10-17): the free-space rule held every pass of
//! the real in-place move back. The plan counted every landed file without
//! a ledger row whole, though the destination already held most of them
//! (adopted when equal, extended by their tail), and every symlinked
//! transcript's target, before any of it could run. A pass now charges what
//! it writes, and a regularization is judged per link as it copies.

use std::fs;
use std::os::unix::fs::MetadataExt as _;

use super::{run, transcript, World, SERIAL, SID, SID2};
use crate::reseat::place;
use crate::BulkloadRefusal;

/// What is at the destination decides the bytes of a placement with no
/// row: nothing there is the whole file; an equal or diverged
/// append-only copy at most what is past it; a symlinked transcript the
/// tail past its target; another file, a hard-linked file or a symlink
/// that is not a transcript nothing.
#[test]
fn a_placement_without_a_row_charges_only_what_it_writes() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let bytes = transcript(SID, "/Users/jess/git/lab", "space", 6);
    let landed = base.join("landed.jsonl");
    fs::write(&landed, &bytes).unwrap();
    let size = bytes.len() as u64;
    let planned = |dest: &std::path::Path, landed: &std::path::Path| {
        place::planned(None, dest, landed).unwrap()
    };
    // Absent, under an absent parent too.
    assert_eq!(planned(&base.join("absent.jsonl"), &landed), size);
    assert_eq!(planned(&base.join("no/such/dir.jsonl"), &landed), size);
    // Equal: adopted, nothing written.
    let equal = base.join("equal.jsonl");
    fs::write(&equal, &bytes).unwrap();
    assert_eq!(planned(&equal, &landed), 0);
    // A prefix: its tail.
    let prefix = base.join("prefix.jsonl");
    fs::write(&prefix, &bytes[..40]).unwrap();
    assert_eq!(planned(&prefix, &landed), size - 40);
    // A symlinked transcript: regularized to its target, then the tail.
    let target = base.join("archive.jsonl");
    fs::write(&target, &bytes[..30]).unwrap();
    let link = base.join("link.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(planned(&link, &landed), size - 30);
    let dangling = base.join("dangling.jsonl");
    std::os::unix::fs::symlink(base.join("gone"), &dangling).unwrap();
    assert_eq!(planned(&dangling, &landed), 0);
    // A hard-linked copy is never written.
    let linked = base.join("linked.jsonl");
    fs::hard_link(&prefix, &linked).unwrap();
    assert_eq!(fs::metadata(&linked).unwrap().nlink(), 2);
    assert_eq!(planned(&linked, &landed), 0);
    // A file that is not append-only is adopted or kept, never written.
    let plan_landed = base.join("plan-landed.md");
    fs::write(&plan_landed, b"a plan, longer than the copy").unwrap();
    let plan = base.join("plan.md");
    fs::write(&plan, b"a plan").unwrap();
    assert_eq!(planned(&plan, &plan_landed), 0);
    assert_eq!(planned(&base.join("plan-absent.md"), &plan_landed), 28);
    // A directory or other node: never written.
    let node = base.join("node.jsonl");
    fs::create_dir(&node).unwrap();
    assert_eq!(planned(&node, &landed), 0);
}

/// The live failure: a second pass over an unchanged landing writes
/// nothing, so no floor refuses it, and the destination already holding
/// a session (equal) plans nothing on the first pass either.
#[test]
fn a_pass_over_what_the_destination_holds_plans_nothing() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "held", 5);
    world.land(SID, "/Users/jess/git/lab", &bytes);
    let other = transcript(SID2, "/Users/jess/git/lab", "held2", 5);
    world.land(SID2, "/Users/jess/git/lab", &other);
    // The destination already holds SID2, equal (pulled there before).
    let held = world.dest_transcript(SID2);
    fs::create_dir_all(held.parent().unwrap()).unwrap();
    fs::write(&held, &other).unwrap();
    let mut options = world.options();
    options.codex = false;
    // A floor no write can meet: only a pass that writes nothing passes.
    options.min_free_percent = 100;
    let outcome = crate::reseat::reseat(&options, &mut |_| Ok(()));
    assert_eq!(
        outcome.err(),
        Some(BulkloadRefusal::DestinationSpaceInsufficient),
        "SID is new: its bytes are charged"
    );
    options.min_free_percent = 0;
    let (_, lines, _) = run(&options);
    assert_eq!(
        fs::read(world.dest_transcript(SID)).unwrap(),
        bytes,
        "{lines:?}"
    );
    // Now nothing is left to write: the 100% floor passes.
    options.min_free_percent = 100;
    let (summary, lines, _) = run(&options);
    assert!(
        !lines.iter().any(|line| line.contains("DESTINATION_SPACE")),
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("claude", "placed"), 0, "{lines:?}");
}

/// A symlinked transcript the floor does not admit is that link's own
/// `space-refused` line, kept a link; the pass's other work runs, and a
/// later pass with room regularizes it.
#[test]
fn a_regularization_is_judged_per_link_and_never_holds_the_pass() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let key_dir = world.dest(&format!("projects/{}", world.lab_key()));
    fs::create_dir_all(&key_dir).unwrap();
    let target = world.base.join("archive.jsonl");
    let own = "0e0e0e0e-1111-4222-8333-444444444444";
    fs::write(&target, transcript(own, &world.real_lab(), "own", 3)).unwrap();
    let link = key_dir.join(format!("{own}.jsonl"));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut options = world.options();
    options.codex = false;
    options.min_free_percent = 100;
    let (summary, lines, _) = run(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"outcome\":\"space-refused\"")
                && line.contains("DESTINATION_SPACE_INSUFFICIENT")),
        "{lines:?}"
    );
    assert_eq!(
        summary.rows.get("link-space-refused"),
        Some(&1),
        "{lines:?}"
    );
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    let refused = crate::space::last_refusal().expect("recorded");
    assert_eq!(refused.min_free_percent, 100);
    options.min_free_percent = 0;
    let (_, lines, _) = run(&options);
    assert!(fs::symlink_metadata(&link).unwrap().is_file(), "{lines:?}");
}
