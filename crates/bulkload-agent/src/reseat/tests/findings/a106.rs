//! The operator's A-106 (2026-10-18): "neo's version wins; the fork stays
//! in the archive". With `--replace-archived-dest ROOT`, a destination
//! transcript, subagent sidecar or rollout that is a symlink to a regular
//! file under ROOT, and whose landed file differs, is replaced by the
//! landed file; the archive target is never written; the link is ledgered
//! and `--undo` puts it back exactly. Anything else is as before.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::{check_lines, rel, rollout_bytes, World, T1};
use crate::reseat::ledger::{Ledger, RegularState};
use crate::reseat::place::set_replace_hook;
use crate::reseat::tests::{run, set_mtime, transcript, SERIAL, SID, SID2};
use crate::reseat::{reseat, undo, Options};
use crate::BulkloadRefusal;

const SID3: &str = "33333333-4444-4555-8666-777777777777";
const SID4: &str = "44444444-5555-4666-8777-888888888888";

impl World {
    fn archive(&self) -> PathBuf {
        let archive = self.base.join("archive");
        fs::create_dir_all(&archive).unwrap();
        archive
    }

    /// `target` holding `bytes`, linked (absolute text) at `link`.
    fn archive_link(link: &Path, target: &Path, bytes: &[u8]) {
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    fn replacing(&self) -> Options {
        let mut options = self.options();
        options.regularize = false;
        options.replace_archived = vec![fs::canonicalize(self.archive()).unwrap()];
        options
    }
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn line_for<'a>(lines: &'a [String], needle: &str, word: &str) -> Option<&'a String> {
    lines
        .iter()
        .find(|line| line.contains(needle) && line.contains(word))
}

/// A transcript and its subagent sidecar linked into the archive with a
/// shorter fork are replaced by the landed files (bytes and mtime); the
/// archive targets are untouched; an equal landing and a link outside the
/// root stay links; `--check` counts the candidates first, the replaced
/// files after, and holds the lane; an unchanged rerun reads nothing;
/// `--undo` puts the exact links back and a later pass does not replace
/// them again.
#[test]
#[allow(clippy::too_many_lines)]
fn a106_archive_links_are_replaced_by_the_landed_files_and_undone() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    let neo = transcript(SID, cwd, "neo", 40);
    let landed = world.land(SID, cwd, &neo);
    set_mtime(&landed, 7_200);
    let fork = transcript(SID, cwd, "fork", 3);
    let target = archive.join("claude/fork.jsonl");
    World::archive_link(&world.dest_transcript(SID), &target, &fork);
    let sidecar_rel = format!("{SID}/subagents/agent-a1.jsonl");
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{sidecar_rel}")),
        b"{\"isSidechain\":true,\"n\":2}\n{\"n\":3}\n",
    )
    .unwrap();
    let sidecar_dest = world.dest(&format!("projects/{}/{sidecar_rel}", world.lab_key()));
    let sidecar_target = archive.join("claude/agent-a1.jsonl");
    World::archive_link(&sidecar_dest, &sidecar_target, b"{\"isSidechain\":true}\n");
    // Equal to its archive target: stays a link.
    let same = transcript(SID2, cwd, "same", 5);
    world.land(SID2, cwd, &same);
    World::archive_link(
        &world.dest_transcript(SID2),
        &archive.join("claude/same.jsonl"),
        &same,
    );
    // A link outside every root: kept as before.
    world.land(SID3, cwd, &transcript(SID3, cwd, "out", 5));
    World::archive_link(
        &world.dest_transcript(SID3),
        &world.base.join("elsewhere/out.jsonl"),
        &transcript(SID3, cwd, "out", 2),
    );
    let options = world.replacing();

    let (before, check) = check_lines(&options);
    assert_eq!(
        (
            before.replace_pending,
            before.replace_equal_size,
            before.replaced_archived
        ),
        (2, 1, 0),
        "{check:?}"
    );
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        2,
        "{lines:?}"
    );
    let dest = world.dest_transcript(SID);
    let meta = fs::symlink_metadata(&dest).unwrap();
    assert!(meta.is_file(), "{lines:?}");
    assert_eq!(fs::read(&dest).unwrap(), neo);
    assert_eq!(meta.mtime(), fs::metadata(&landed).unwrap().mtime());
    assert_eq!(fs::read(&target).unwrap(), fork, "the archive kept");
    assert!(fs::symlink_metadata(&sidecar_dest).unwrap().is_file());
    assert_eq!(
        fs::read(&sidecar_target).unwrap(),
        b"{\"isSidechain\":true}\n"
    );
    assert!(is_link(&world.dest_transcript(SID2)), "equal: kept");
    assert!(
        is_link(&world.dest_transcript(SID3)),
        "outside the root: kept"
    );
    assert!(line_for(&lines, SID3, "occupied").is_some(), "{lines:?}");
    {
        let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
        let rows: Vec<_> = ledger
            .regulars()
            .unwrap()
            .into_iter()
            .filter(|row| row.replace)
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.state == RegularState::Done
            && row.digest.is_some()
            && row.resolved.starts_with(&archive)));
    }
    let (after, check) = check_lines(&options);
    assert_eq!(
        (
            after.replace_pending,
            after.replace_equal_size,
            after.replace_proved,
            after.replaced_archived
        ),
        (0, 0, 1, 2),
        "{check:?}"
    );
    let lane = after.lanes.values().next().unwrap();
    assert!(
        lane.sessions_ok >= 1,
        "the replaced session holds: {lane:?}"
    );
    let (again, lines, read) = run(&options);
    assert_eq!(read, 0, "S3: {lines:?}");
    assert_eq!(again.files_of("claude", "replaced-archived"), 0);
    // --undo: the exact links back over unchanged copies.
    let mut undo_lines = Vec::new();
    let (restored, _) = undo(&options, &mut |line| {
        undo_lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(restored, 2, "{undo_lines:?}");
    assert_eq!(fs::read_link(&dest).unwrap(), target);
    assert_eq!(fs::read_link(&sidecar_dest).unwrap(), sidecar_target);
    assert_eq!(fs::read(&target).unwrap(), fork);
    let (later, lines, _) = run(&options);
    assert_eq!(
        later.files_of("claude", "replaced-archived"),
        0,
        "undone stays undone: {lines:?}"
    );
    assert!(is_link(&dest));
}

/// The live shape: an earlier pass from another landing (or without the
/// flag) kept the archive link `occupied`; with the flag a pass from the
/// new landing replaces it instead of `collision`. A destination file this
/// ledger placed from the old landing is still a collision.
#[test]
fn a106_an_occupied_archive_link_from_another_landing_is_replaced() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    world.land(SID, cwd, &transcript(SID, cwd, "neo", 20));
    World::archive_link(
        &world.dest_transcript(SID),
        &archive.join("fork.jsonl"),
        &transcript(SID, cwd, "fork", 2),
    );
    world.land(SID4, cwd, &transcript(SID4, cwd, "placed", 4));
    let mut options = world.options();
    options.regularize = false;
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.files_of("claude", "occupied"), 1, "{lines:?}");
    // A second landing of the same home.
    let second = world.base.join("landing2/.claude");
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    copy_tree(&world.base.join("landing/.claude"), &second);
    let mut options = world.replacing();
    options.landing_claude = second;
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        1,
        "{lines:?}"
    );
    assert!(line_for(&lines, SID4, "collision").is_some(), "{lines:?}");
    assert!(fs::symlink_metadata(world.dest_transcript(SID))
        .unwrap()
        .is_file());
}

/// With regularization on, a link into the root is left for the placement
/// (replaced when a landed file differs, a link when none names it); a
/// link outside the root is regularized as before.
#[test]
fn a106_regularize_leaves_links_into_the_root_to_the_placement() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    let neo = transcript(SID, cwd, "neo", 10);
    world.land(SID, cwd, &neo);
    World::archive_link(
        &world.dest_transcript(SID),
        &archive.join("fork.jsonl"),
        &transcript(SID, cwd, "fork", 2),
    );
    // Sting's own archived session, no landed file: stays a link.
    let own = world.dest_transcript(SID2);
    World::archive_link(
        &own,
        &archive.join("own.jsonl"),
        &transcript(SID2, cwd, "own", 3),
    );
    // A link outside the root: regularized.
    let outside = world.dest_transcript(SID3);
    World::archive_link(
        &outside,
        &world.base.join("elsewhere/x.jsonl"),
        &transcript(SID3, cwd, "x", 3),
    );
    let mut options = world.replacing();
    options.regularize = true;
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        1,
        "{lines:?}"
    );
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), neo);
    assert!(is_link(&own), "{lines:?}");
    assert!(
        fs::symlink_metadata(&outside).unwrap().is_file(),
        "{lines:?}"
    );
}

/// A Codex rollout whose plain destination is a link into the archive
/// with a different stream is replaced by the landed rollout and its row
/// registered; the archive kept.
#[test]
fn a106_a_codex_rollout_link_is_replaced() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let bytes = rollout_bytes(T1, "legacy", 6);
    world.land_thread(T1, "", &bytes, 1);
    drop(world.dest_index());
    let target = archive.join("codex/old.jsonl");
    World::archive_link(
        &world.codex_dest(&rel(T1)),
        &target,
        &rollout_bytes(T1, "legacy", 2),
    );
    let options = world.replacing();
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("codex", "replaced-archived"),
        1,
        "{lines:?}"
    );
    let placed = world.codex_dest(&rel(T1));
    assert_eq!(fs::read(&placed).unwrap(), bytes);
    assert_eq!(fs::read(&target).unwrap(), rollout_bytes(T1, "legacy", 2));
    assert_eq!(
        world.dest_row(T1),
        Some((world.real_lab(), placed.to_str().unwrap().to_owned())),
        "{lines:?}"
    );
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&path, &dest);
        } else {
            fs::copy(&path, &dest).unwrap();
        }
    }
}

/// Review A1: a root that equals, contains or is under the live `.claude`
/// or `.codex`, or contains the destination home, is refused before
/// anything is read or written; a `.claude` that is itself a symlink into
/// the root (so the swap would happen inside the archive) is refused the
/// same way.
#[test]
fn a106_a_root_overlapping_the_live_state_is_refused() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let refused = |roots: Vec<PathBuf>, options: &mut Options| {
        options.replace_archived = roots
            .into_iter()
            .map(|root| fs::canonicalize(root).unwrap())
            .collect();
        reseat(options, &mut |_| Ok(()))
            .err()
            .map(|refusal| refusal.code())
    };
    for root in [
        world.base.clone(),
        world.base.join("dest"),
        world.base.join("dest/.claude"),
        world.base.join("dest/.codex"),
    ] {
        let mut options = world.options();
        assert_eq!(
            refused(vec![root.clone()], &mut options),
            Some(BulkloadRefusal::FieldDomainViolation.code()),
            "{root:?}"
        );
    }
    let mut options = world.options();
    fs::create_dir_all(world.base.join("dest/.claude/projects")).unwrap();
    assert_eq!(
        refused(vec![world.base.join("dest/.claude/projects")], &mut options),
        Some(BulkloadRefusal::FieldDomainViolation.code())
    );
    // A `.claude` that is a symlink into the root.
    let archive = world.archive();
    let moved = archive.join("claude-home");
    fs::rename(world.base.join("dest/.claude"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, world.base.join("dest/.claude")).unwrap();
    let mut options = world.options();
    assert_eq!(
        refused(vec![archive], &mut options),
        Some(BulkloadRefusal::FieldDomainViolation.code())
    );
}

/// Review A2: an archive target some process holds open (a live session
/// would go on writing into the archive after the swap) is kept
/// `occupied`; once it is closed, the next pass replaces it.
#[test]
fn a106_a_target_held_open_is_not_replaced() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    let neo = transcript(SID, cwd, "neo", 12);
    world.land(SID, cwd, &neo);
    let target = archive.join("open.jsonl");
    World::archive_link(
        &world.dest_transcript(SID),
        &target,
        &transcript(SID, cwd, "fork", 2),
    );
    let options = world.replacing();
    let held = fs::OpenOptions::new().append(true).open(&target).unwrap();
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        0,
        "{lines:?}"
    );
    assert!(is_link(&world.dest_transcript(SID)));
    drop(held);
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        1,
        "{lines:?}"
    );
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), neo);
}

/// Review A3 and A4: a target that holds the landed lines and more (the
/// archive ahead, not a fork) is kept, `dest-ahead`, and not read again
/// while nothing changes; `--check` counts it proved. When the archive
/// target changes (rewritten as a fork), the next pass decides again and
/// replaces it.
#[test]
fn a106_an_archive_ahead_is_kept_and_a_changed_target_is_decided_again() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    let neo = transcript(SID, cwd, "neo", 6);
    world.land(SID, cwd, &neo);
    let target = archive.join("ahead.jsonl");
    let mut ahead = neo.clone();
    ahead.extend_from_slice(b"{\"type\":\"user\",\"message\":\"more on sting\"}\n");
    World::archive_link(&world.dest_transcript(SID), &target, &ahead);
    let options = world.replacing();
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.files_of("claude", "dest-ahead"), 1, "{lines:?}");
    assert!(is_link(&world.dest_transcript(SID)));
    assert_eq!(fs::read(&target).unwrap(), ahead);
    let (checked, check) = check_lines(&options);
    assert_eq!(
        (checked.replace_pending, checked.replace_proved),
        (0, 1),
        "{check:?}"
    );
    let (summary, lines, read) = run(&options);
    assert_eq!(read, 0, "S3: {lines:?}");
    assert_eq!(summary.files_of("claude", "dest-ahead"), 1, "{lines:?}");
    // The archive target rewritten as a fork: decided again, replaced.
    fs::write(&target, transcript(SID, cwd, "fork", 9)).unwrap();
    let (checked, _) = check_lines(&options);
    assert_eq!(checked.replace_proved, 0);
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        1,
        "{lines:?}"
    );
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), neo);
}

/// Review M7: the archive target written under the swap puts everything
/// back: the link at the path with its text, the target holding what was
/// written, no replacement row, no temporary.
#[test]
fn a106_a_target_written_under_the_swap_is_put_back() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    world.land(SID, cwd, &transcript(SID, cwd, "neo", 8));
    let target = archive.join("live.jsonl");
    World::archive_link(
        &world.dest_transcript(SID),
        &target,
        &transcript(SID, cwd, "fork", 2),
    );
    let written = target.clone();
    set_replace_hook(Some(Box::new(move |_dest: &Path| {
        let mut file = fs::OpenOptions::new().append(true).open(&written).unwrap();
        std::io::Write::write_all(&mut file, b"{\"live\":1}\n").unwrap();
    })));
    let options = world.replacing();
    let (summary, lines, _) = run(&options);
    set_replace_hook(None);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        0,
        "{lines:?}"
    );
    assert_eq!(summary.files_of("claude", "occupied"), 1, "{lines:?}");
    let dest = world.dest_transcript(SID);
    assert_eq!(fs::read_link(&dest).unwrap(), target);
    assert!(fs::read(&target).unwrap().ends_with(b"{\"live\":1}\n"));
    let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
    assert!(ledger.regular(&dest).unwrap().is_none());
    let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".bulkload-reseat")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Merge-gate review of #280 (2026-10-19, a probe with this hook): a
/// destination writer appends through the live name while the landed copy
/// stands there, and the archive target is written in the same window, so
/// the swap is put back. The copy then holds the writer's line. Under SPEC
/// #298 (rules 3 and 4) it is no second copy: it is unioned into the live
/// file (the archive target), ledgered, and removed once the merged bytes
/// are durable. Nothing stays beside the live name (no dot-name at all in
/// its directory).
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn a106_a_copy_written_through_the_live_name_is_unioned_into_the_live_file() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = crate::counters::Counters::snapshot();
    let world = World::new();
    let archive = world.archive();
    let cwd = "/Users/jess/git/lab";
    let neo = transcript(SID, cwd, "neo", 8);
    world.land(SID, cwd, &neo);
    let target = archive.join("live.jsonl");
    World::archive_link(
        &world.dest_transcript(SID),
        &target,
        &transcript(SID, cwd, "fork", 2),
    );
    let (written, archived) = (world.dest_transcript(SID), target.clone());
    set_replace_hook(Some(Box::new(move |_dest: &Path| {
        // The live name now holds the landed copy: the writer's line lands
        // in it. Then the archive target is written too.
        let mut live = fs::OpenOptions::new().append(true).open(&written).unwrap();
        std::io::Write::write_all(&mut live, b"{\"sting\":\"next turn\"}\n").unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&archived).unwrap();
        std::io::Write::write_all(&mut file, b"{\"live\":1}\n").unwrap();
    })));
    let options = world.replacing();
    let (summary, lines, _) = run(&options);
    set_replace_hook(None);
    assert_eq!(summary.files_of("claude", "held-written"), 0, "{lines:?}");
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        0,
        "{lines:?}"
    );
    let dest = world.dest_transcript(SID);
    assert_eq!(fs::read_link(&dest).unwrap(), target, "the link is back");
    let dir = dest.parent().unwrap();
    let listed: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(
        listed.is_empty(),
        "no copy beside the live name: {listed:?}"
    );
    let mut want = neo;
    want.extend_from_slice(b"{\"sting\":\"next turn\"}\n");
    let unioned = fs::read(&target).unwrap();
    assert!(
        unioned.starts_with(&want),
        "the copy's bytes, the writer's line among them, first"
    );
    assert!(
        unioned.ends_with(b"{\"live\":1}\n"),
        "the archive's own line kept"
    );
    let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
    assert!(ledger.regular(&dest).unwrap().is_none());
    drop(ledger);
    let conn = rusqlite::Connection::open(options.state.join("consolidate.sqlite")).unwrap();
    let unions: i64 = conn
        .query_row("SELECT count(*) FROM append_log_union", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(unions, 1, "the union ledgered");
    let counted = crate::counters::Counters::snapshot().since(before);
    assert_eq!(counted.get(crate::counters::Counter::ReseatCopyFolded), 1);
}
