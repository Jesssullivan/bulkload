//! M2 round 2, item 1 (2026-10-17): the free-space rule held every pass of
//! the real in-place move back. The plan counted every landed file without
//! a ledger row whole, though the destination already held most of them
//! (adopted when equal, extended by their tail), and every symlinked
//! transcript's target, before any of it could run. A pass now charges what
//! it writes, and a regularization is judged per link as it copies.

use std::fs;
use std::os::unix::fs::MetadataExt as _;

use super::{run, transcript, World, SERIAL, SID, SID2};
use crate::reseat::{ledger, place, regularize, Summary};
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
        place::planned(None, dest, landed, &[]).unwrap()
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
    // A symlink into a `--replace-archived-dest` root is replaced by the
    // whole landed file (A-106), never regularized: the whole content.
    let archive = base.join("archive");
    std::fs::create_dir(&archive).unwrap();
    std::fs::write(archive.join("t.jsonl"), &bytes[..30]).unwrap();
    let archived_link = base.join("archived.jsonl");
    std::os::unix::fs::symlink(archive.join("t.jsonl"), &archived_link).unwrap();
    let roots = [std::fs::canonicalize(&archive).unwrap()];
    assert_eq!(
        place::planned(None, &archived_link, &landed, &roots).unwrap(),
        size
    );
    assert_eq!(planned(&archived_link, &landed), size - 30);
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

/// Review B2, probe A: a `Diverged` row keeps its file only while it is
/// there. The destination's file deleted, the placement writes the whole
/// landed file, so the plan charges it.
#[test]
fn a_kept_row_whose_destination_is_gone_charges_the_whole_file() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "gone", 5);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"{\"type\":\"someone else's\"}\n").unwrap();
    let mut options = world.options();
    options.codex = false;
    let (_, lines, _) = run(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("RESEAT_DEST_DIVERGED")),
        "{lines:?}"
    );
    let open = || ledger::Ledger::open_read_only(&options.state).unwrap();
    assert_eq!(
        place::planned(open().as_ref(), &dest, &landed, &[]).unwrap(),
        0
    );
    fs::remove_file(&dest).unwrap();
    assert_eq!(
        place::planned(open().as_ref(), &dest, &landed, &[]).unwrap(),
        bytes.len() as u64
    );
    // And the pass that writes it is held to the floor.
    options.min_free_percent = 100;
    let outcome = crate::reseat::reseat(&options, &mut |_| Ok(()));
    assert_eq!(
        outcome.err(),
        Some(BulkloadRefusal::DestinationSpaceInsufficient)
    );
    assert!(!dest.exists(), "nothing written past the floor");
}

/// Review B2, probe B: a symlinked transcript kept `Occupied` (no
/// regularization) that becomes a regular copy of a prefix is extended by
/// the placement, so the plan charges the tail.
#[test]
fn an_occupied_link_that_became_a_regular_prefix_charges_its_tail() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "occupied", 6);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let target = world.base.join("archive.jsonl");
    let cut = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    fs::write(&target, &bytes[..cut]).unwrap();
    std::os::unix::fs::symlink(&target, &dest).unwrap();
    let mut options = world.options();
    options.codex = false;
    options.regularize = false;
    let (_, lines, _) = run(&options);
    assert!(
        fs::symlink_metadata(&dest).unwrap().is_symlink(),
        "{lines:?}"
    );
    let open = || ledger::Ledger::open_read_only(&options.state).unwrap();
    let tail = (bytes.len() - cut) as u64;
    // Still the link: a regularization earlier in a pass would copy it.
    assert_eq!(
        place::planned(open().as_ref(), &dest, &landed, &[]).unwrap(),
        tail
    );
    fs::remove_file(&dest).unwrap();
    fs::write(&dest, &bytes[..cut]).unwrap();
    assert_eq!(
        place::planned(open().as_ref(), &dest, &landed, &[]).unwrap(),
        tail
    );
    let (_, lines, _) = run(&options);
    assert_eq!(fs::read(&dest).unwrap(), bytes, "{lines:?}");
}

/// Review B1 and B7: a regularization is judged with the bytes the pass's
/// placements were admitted for still reserved. Reserved so that the link
/// does not fit, it is `space-refused` and stays a link; a later pass with
/// room regularizes it and extends the copy to the landed session.
#[test]
fn a_regularization_keeps_the_placements_bytes_reserved_then_retries() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let line = format!(
        "{{\"type\":\"user\",\"sessionId\":\"{SID}\",\"cwd\":\"/Users/jess/git/lab\",\"pad\":\"{}\"}}\n",
        "x".repeat(1024)
    );
    let mut bytes = transcript(SID, "/Users/jess/git/lab", "big", 2);
    let head = bytes.len();
    for _ in 0..(64 * 1024) {
        bytes.extend_from_slice(line.as_bytes());
    }
    world.land(SID, "/Users/jess/git/lab", &bytes);
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let target = world.base.join("archive.jsonl");
    let archived = (bytes.len() - line.len()) as u64;
    fs::write(&target, &bytes[..bytes.len() - line.len()]).unwrap();
    std::os::unix::fs::symlink(&target, &dest).unwrap();
    assert!(bytes.len() - head > 32 * 1024 * 1024);
    let options = world.options();
    let root = fs::canonicalize(&options.dest_claude).unwrap();
    // Reserve all but half the link's size: the reservation alone fits, the
    // link beside it does not. The free space is probed here and again by
    // the regularization, and on a shared file system it moves between the
    // two: with the link at its real ~74 MB, a gain of half that (~37 MB)
    // made it fit (a loaded build host's moved by 1.5 GB in 10 s). So for
    // this pass the target is extended with a hole to an apparent 64 GiB
    // (no blocks, and nothing reads it before the space check refuses),
    // bounded by the free space so the reservation still decides on a small
    // disk: the margin either way is half of that, not ~37 MB. It is cut
    // back to its bytes before the pass with room.
    let limits = crate::space::probe_limits(&root).unwrap();
    let free = limits.quota.map_or(limits.filesystem.available, |quota| {
        quota.available.min(limits.filesystem.available)
    });
    let apparent = (64_u64 << 30).min(free).max(archived);
    fs::OpenOptions::new()
        .write(true)
        .open(&target)
        .unwrap()
        .set_len(apparent)
        .unwrap();
    let reserved_bytes = free.saturating_sub(apparent / 2);
    let reserved = [(root.as_path(), reserved_bytes)];
    {
        let ledger = ledger::Ledger::open(&options.state, options.now_ns, "test").unwrap();
        let mut summary = Summary::default();
        let mut lines = Vec::new();
        regularize::regularize(
            &root,
            &[],
            &ledger,
            regularize::Space {
                min_free_percent: 0,
                reserved: &reserved,
            },
            &mut summary,
            &mut |line| {
                lines.push(line.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            summary.rows.get("link-space-refused"),
            Some(&1),
            "{lines:?}"
        );
    }
    assert!(fs::symlink_metadata(&dest).unwrap().is_symlink());
    fs::OpenOptions::new()
        .write(true)
        .open(&target)
        .unwrap()
        .set_len(archived)
        .unwrap();
    // A pass with room: regularized, then extended by the last line.
    let (summary, lines, _) = run(&options);
    assert!(fs::symlink_metadata(&dest).unwrap().is_file(), "{lines:?}");
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{lines:?}");
    assert_eq!(fs::read(&target).unwrap().len(), bytes.len() - line.len());
}
