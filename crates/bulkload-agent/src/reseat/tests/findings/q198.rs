//! The operator's rulings of 2026-10-16 (OI-1003-Q198, Linear TIN-6587
//! comment `fbccc49b`): (1) existing Codex rows spelled through `~/git`
//! get only their `cwd` re-spelled, ledgered, reversible; (2) symlinked
//! destination transcripts are regularized into regular files, ledgered,
//! reversible; A to E (lanes at `~/git/<name>`, gftb and `GloriousFlywheel`
//! through sting's links, home and `/private/tmp` by ID and waived, rows
//! from rollout headers, `.zst` decompressed). One row each, plus `--undo`.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use super::{check_lines, rel, rollout_bytes, World, T1};
use crate::reseat::regularize::{set_hook, Step};
use crate::reseat::tests::{append, r0, run, set_mtime, transcript, SERIAL, SID, SID2};
use crate::reseat::{ledger, undo};

const T2: &str = "019a0000-0000-7000-8000-0000000000e2";
const T3: &str = "019a0000-0000-7000-8000-0000000000e3";

impl World {
    /// The lab lane spelled through the destination's `git` symlink: a
    /// non-canonical spelling, as sting's `/home/jess/git/…` rows are.
    fn link_lab(&self) -> String {
        self.base.join("dest/git/lab").to_str().unwrap().to_owned()
    }

    fn dest_row_full(&self, id: &str) -> Option<(String, String, String)> {
        self.dest_index()
            .query_row(
                "SELECT cwd, rollout_path, title FROM threads WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    }

    fn insert_dest_row(&self, id: &str, rollout: &str, cwd: &str) {
        self.dest_index()
            .execute(
                "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
                 VALUES (?1, ?2, 1, 1, 'cli', ?3, 'sting title')",
                params![id, rollout, cwd],
            )
            .unwrap();
    }
}

fn undo_lines(options: &crate::reseat::Options) -> ((u64, u64), Vec<String>) {
    let mut lines = Vec::new();
    let counts = undo(options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    (counts, lines)
}

/// Q198 (1): a destination row whose `cwd` spells the lane through the
/// `git` symlink gets only that column re-spelled to the canonical path
/// (its rollout and every other column kept), with the old value ledgered;
/// a row naming this placement is then adopted, one naming the
/// destination's own rollout stays a conflict with nothing placed beside
/// it, and a row in another directory is never touched. `--undo` puts the
/// old spellings back, except where the destination changed the row since,
/// and a later pass does not re-spell an undone row.
#[test]
fn q198_1_a_symlink_spelled_row_gets_its_cwd_respelled_and_undone() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = rollout_bytes(T1, "legacy", 3);
    world.land_thread(T1, "", &bytes, 1);
    world.land_thread(T2, "", &rollout_bytes(T2, "legacy", 3), 2);
    world.land_thread(T3, "", &rollout_bytes(T3, "legacy", 3), 3);
    drop(world.dest_index());
    // T1: the destination already holds this placement, spelled through
    // the symlink. T2: its own rollout elsewhere, same spelling. T3: a
    // different directory.
    let placed = world.codex_dest(&rel(T1));
    fs::create_dir_all(placed.parent().unwrap()).unwrap();
    fs::write(&placed, &bytes).unwrap();
    world.insert_dest_row(T1, placed.to_str().unwrap(), &world.link_lab());
    let stings = world.base.join("dest/.codex/archived_sessions/t2.jsonl");
    world.insert_dest_row(T2, stings.to_str().unwrap(), &world.link_lab());
    fs::create_dir_all(world.base.join("other")).unwrap();
    let other = world.base.join("other").to_str().unwrap().to_owned();
    world.insert_dest_row(T3, "/x/t3.jsonl", &other);
    let options = world.options();
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), Some(&2), "{lines:?}");
    assert_eq!(
        world.dest_row_full(T1),
        Some((
            world.real_lab(),
            placed.to_str().unwrap().to_owned(),
            "sting title".to_owned()
        )),
        "only cwd changed"
    );
    assert_eq!(
        world.dest_row_full(T2),
        Some((
            world.real_lab(),
            stings.to_str().unwrap().to_owned(),
            "sting title".to_owned()
        ))
    );
    assert_eq!(world.dest_row(T3), Some((other, "/x/t3.jsonl".to_owned())));
    assert!(
        !world.codex_dest(&rel(T2)).exists() && !world.codex_dest(&rel(T3)).exists(),
        "nothing placed beside a destination's own row: {lines:?}"
    );
    assert!(lines
        .iter()
        .any(|line| line.contains(T2) && line.contains("RESEAT_INDEX_CONFLICT")));
    assert!(lines
        .iter()
        .any(|line| line.contains(T1) && line.contains("\"row\":\"adopted\"")));
    {
        let ledger = ledger::Ledger::open_read_only(&options.state)
            .unwrap()
            .unwrap();
        let rows = ledger.cwd_updates().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|row| row.old_cwd == world.link_lab() && row.state == "applied"));
    }
    let (summary, _, read) = run(&options);
    assert_eq!(read, 0, "S3");
    assert_eq!(summary.rows.get("respelled"), None, "once");
    // The destination changes T2's row since; undo keeps that.
    world
        .dest_index()
        .execute(
            "UPDATE threads SET cwd = '/changed/on/sting' WHERE id = ?1",
            params![T2],
        )
        .unwrap();
    let ((_, rows), lines) = undo_lines(&options);
    assert_eq!(rows, 1, "{lines:?}");
    assert_eq!(
        world.dest_row(T1).map(|row| row.0),
        Some(world.link_lab()),
        "the old spelling is back"
    );
    assert_eq!(
        world.dest_row(T2).map(|row| row.0),
        Some("/changed/on/sting".to_owned())
    );
    assert!(lines
        .iter()
        .any(|line| line.contains(T2) && line.contains("undo-kept")));
    // Undone stays undone: a later pass does not re-spell it.
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), None, "{lines:?}");
    assert_eq!(world.dest_row(T1).map(|row| row.0), Some(world.link_lab()));
    // Undo is idempotent.
    let ((_, rows), _) = undo_lines(&options);
    assert_eq!(rows, 0);
}

/// Q198 (1), off: `--no-respell` leaves every row as it is.
#[test]
fn q198_1_no_respell_leaves_rows_alone() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 3), 1);
    drop(world.dest_index());
    world.insert_dest_row(T1, "/elsewhere/t1.jsonl", &world.link_lab());
    let mut options = world.options();
    options.respell_cwd = false;
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), None, "{lines:?}");
    assert_eq!(world.dest_row(T1).map(|row| row.0), Some(world.link_lab()));
}

/// Q198 (2): a symlinked destination transcript (sting's own, into the
/// archive) becomes a regular-file copy of its target with the target's
/// mtime, the archive kept; one that is an older prefix of a landed neo
/// session is then extended by the pass; a dangling link is kept and
/// named. The picker's rule (regular files only) now holds for both.
/// `--undo` puts the link back over an unchanged copy and keeps one the
/// pass extended since; an undone link is not regularized again.
#[test]
fn q198_2_symlinked_transcripts_are_regularized_and_undone() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let key = world.lab_key();
    let key_dir = world.dest(&format!("projects/{key}"));
    fs::create_dir_all(&key_dir).unwrap();
    let archive = world.base.join("archive/claude/projects");
    fs::create_dir_all(&archive).unwrap();
    // Sting's own session, not in the landing.
    let own = "0e0e0e0e-1111-4222-8333-444444444444";
    let own_target = archive.join(format!("{own}.jsonl"));
    let own_bytes = transcript(own, &world.real_lab(), "own", 4);
    fs::write(&own_target, &own_bytes).unwrap();
    set_mtime(&own_target, 5 * 86_400);
    std::os::unix::fs::symlink(&own_target, key_dir.join(format!("{own}.jsonl"))).unwrap();
    // A neo session whose sting copy is an older prefix, symlinked.
    let neo = transcript(SID, "/Users/jess/git/lab", "neo", 8);
    world.land(SID, "/Users/jess/git/lab", &neo);
    let cut = neo
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(3)
        .map(|(at, _)| at + 1)
        .unwrap();
    let prefix_target = archive.join(format!("{SID}.jsonl"));
    fs::write(&prefix_target, &neo[..cut]).unwrap();
    std::os::unix::fs::symlink(&prefix_target, world.dest_transcript(SID)).unwrap();
    // A dangling link.
    let dangling = key_dir.join(format!("{SID2}.jsonl"));
    std::os::unix::fs::symlink(world.base.join("gone.jsonl"), &dangling).unwrap();
    let options = world.options();
    let (summary, lines, _) = run(&options);
    let own_path = key_dir.join(format!("{own}.jsonl"));
    let meta = fs::symlink_metadata(&own_path).unwrap();
    assert!(meta.is_file(), "regularized: {lines:?}");
    assert_eq!(fs::read(&own_path).unwrap(), own_bytes);
    assert_eq!(meta.mtime(), fs::metadata(&own_target).unwrap().mtime());
    assert_eq!(
        fs::read(&own_target).unwrap(),
        own_bytes,
        "the archive kept"
    );
    assert_eq!(
        fs::read(world.dest_transcript(SID)).unwrap(),
        neo,
        "the regularized prefix extended to the landed session"
    );
    assert_eq!(
        fs::read(&prefix_target).unwrap(),
        &neo[..cut],
        "archive kept"
    );
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{lines:?}");
    assert!(fs::symlink_metadata(&dangling).unwrap().is_symlink());
    assert!(lines
        .iter()
        .any(|line| line.contains("dangling") && line.contains(SID2)));
    assert!(
        r0(&options).verdict().is_ok(),
        "{:?}",
        check_lines(&options)
    );
    let (summary, _, read) = run(&options);
    assert_eq!(read, 0, "S3: {summary:?}");
    // Undo: the unchanged copy goes back to its link; the extended one is
    // kept.
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, 1, "{lines:?}");
    assert_eq!(fs::read_link(&own_path).unwrap(), own_target);
    assert_eq!(fs::read(&own_target).unwrap(), own_bytes);
    assert!(fs::symlink_metadata(world.dest_transcript(SID))
        .unwrap()
        .is_file());
    assert!(lines
        .iter()
        .any(|line| line.contains(SID) && line.contains("undo-kept")));
    let leftovers: Vec<PathBuf> = fs::read_dir(&key_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".bulkload-reseat"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // Not regularized again by itself.
    let (_, lines, _) = run(&options);
    assert!(
        fs::symlink_metadata(&own_path).unwrap().is_symlink(),
        "{lines:?}"
    );
}

/// Q198 (2): a destination resume that appends after regularization makes
/// the copy the destination's: `--undo` keeps it.
#[test]
fn q198_2_undo_keeps_a_copy_written_since() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let key_dir = world.dest(&format!("projects/{}", world.lab_key()));
    fs::create_dir_all(&key_dir).unwrap();
    let target = world.base.join("archive.jsonl");
    let bytes = transcript(SID2, &world.real_lab(), "s", 2);
    fs::write(&target, &bytes).unwrap();
    let link = key_dir.join(format!("{SID2}.jsonl"));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut options = world.options();
    options.claude = true;
    options.codex = false;
    run(&options);
    assert!(fs::symlink_metadata(&link).unwrap().is_file());
    append(&link, b"{\"type\":\"resumed-on-sting\"}\n");
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, 0, "{lines:?}");
    assert!(fs::symlink_metadata(&link).unwrap().is_file());
    assert_eq!(fs::read(&target).unwrap(), bytes);
}

/// One symlinked transcript in the lab key, into an archive file outside
/// the destination: `(link, target, bytes)`.
fn archived_link(world: &World, sid: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let key_dir = world.dest(&format!("projects/{}", world.lab_key()));
    fs::create_dir_all(&key_dir).unwrap();
    let archive = world.base.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let target = archive.join(format!("{sid}.jsonl"));
    let bytes = transcript(sid, &world.real_lab(), "s", 3);
    fs::write(&target, &bytes).unwrap();
    let link = key_dir.join(format!("{sid}.jsonl"));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    (link, target, bytes)
}

/// The kept copies beside `link` (`.<name>.<token>.kept-<inode>.bulkload-reseat`).
fn kept_beside(link: &Path) -> Vec<PathBuf> {
    let name = link.file_name().unwrap().to_string_lossy().into_owned();
    let mut kept: Vec<PathBuf> = fs::read_dir(link.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            file.starts_with(&format!(".{name}."))
                && file.contains(".kept-")
                && file.ends_with(".bulkload-reseat")
        })
        .collect();
    kept.sort();
    kept
}

/// Run one claude-only pass with `hook` acting as a live session at each
/// regularization step.
fn run_hooked(
    options: &crate::reseat::Options,
    hook: impl FnMut(Step, &Path) + 'static,
) -> Vec<String> {
    set_hook(Some(Box::new(hook)));
    let (_, lines, _) = run(options);
    set_hook(None);
    lines
}

/// Q198 (2) re-review, MEDIUM 1: a live session appends through the link
/// after the target was read and before the swap. The append is never
/// orphaned or deleted: the transcript is refused
/// (`RESEAT_REGULARIZE_RACED`), the link and the archive (with the append)
/// are untouched, the staged copy is kept aside (named on the line), and
/// no row is written, so the next pass regularizes it whole while the kept
/// copy stays.
#[test]
fn q198_2_rr_append_between_read_and_swap_keeps_both_copies() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let (link, target, bytes) = archived_link(&world, SID2);
    let mut options = world.options();
    options.codex = false;
    let appended = b"{\"type\":\"live-append-before-swap\"}\n";
    let lines = run_hooked(&options, move |step, at| {
        if step == Step::Staged {
            append(at, appended);
        }
    });
    let report = lines
        .iter()
        .find(|line| line.contains("\"kind\":\"regularize\""))
        .unwrap();
    assert!(
        report.contains("\"outcome\":\"raced-skipped\""),
        "{lines:?}"
    );
    assert!(report.contains("RESEAT_REGULARIZE_RACED"), "{report}");
    assert!(
        fs::symlink_metadata(&link).unwrap().is_symlink(),
        "{report}"
    );
    assert_eq!(fs::read_link(&link).unwrap(), target);
    let mut whole = bytes.clone();
    whole.extend_from_slice(appended);
    assert_eq!(fs::read(&target).unwrap(), whole, "the append kept");
    let kept = kept_beside(&link);
    assert_eq!(kept.len(), 1, "{kept:?} {report}");
    assert_eq!(fs::read(&kept[0]).unwrap(), bytes, "the staged copy kept");
    assert!(report.contains(&*kept[0].to_string_lossy()), "{report}");
    let ledger = ledger::Ledger::open_read_only(&options.state)
        .unwrap()
        .unwrap();
    assert!(ledger.regular(&link).unwrap().is_none());
    drop(ledger);
    // The next pass, with the session quiet: the whole transcript.
    let (_, lines, _) = run(&options);
    assert!(fs::symlink_metadata(&link).unwrap().is_file(), "{lines:?}");
    assert_eq!(fs::read(&link).unwrap(), whole);
    assert_eq!(fs::read(&target).unwrap(), whole, "archive kept");
    assert_eq!(kept_beside(&link), kept, "a kept copy is never removed");
}

/// Q198 (2) re-review, MEDIUM 1, under the swap: the target is written
/// just after the exchange. With the copy unchanged, the link goes back
/// over it and the copy is kept aside; with the copy written too (a
/// session opened the path after the swap), both stay: the copy at the
/// path, the archive untouched, and `--undo` refuses the row.
#[test]
fn q198_2_rr_append_under_the_swap_keeps_both_copies() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let (link, target, bytes) = archived_link(&world, SID2);
    let mut options = world.options();
    options.codex = false;
    let archived = b"{\"type\":\"archive-append-under-swap\"}\n";
    let hook_target = target.clone();
    let lines = run_hooked(&options, move |step, _| {
        if step == Step::Exchanged {
            append(&hook_target, archived);
        }
    });
    let report = lines
        .iter()
        .find(|line| line.contains("\"kind\":\"regularize\""))
        .unwrap();
    assert!(
        report.contains("\"outcome\":\"raced-restored\""),
        "{lines:?}"
    );
    assert!(report.contains("RESEAT_REGULARIZE_RACED"), "{report}");
    assert_eq!(fs::read_link(&link).unwrap(), target, "the link back");
    let mut whole = bytes.clone();
    whole.extend_from_slice(archived);
    assert_eq!(fs::read(&target).unwrap(), whole);
    let kept = kept_beside(&link);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), bytes);

    // Both written under the swap.
    let world = World::new();
    let (link, target, bytes) = archived_link(&world, SID);
    let mut options = world.options();
    options.codex = false;
    let resumed = b"{\"type\":\"resumed-through-the-path\"}\n";
    let hook_target = target.clone();
    let lines = run_hooked(&options, move |step, at| {
        if step == Step::Exchanged {
            append(&hook_target, archived);
            append(at, resumed);
        }
    });
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"outcome\":\"raced-kept\"")
                && line.contains("RESEAT_REGULARIZE_RACED")),
        "{lines:?}"
    );
    let mut at_path = bytes.clone();
    at_path.extend_from_slice(resumed);
    let mut in_archive = bytes;
    in_archive.extend_from_slice(archived);
    assert!(fs::symlink_metadata(&link).unwrap().is_file());
    assert_eq!(fs::read(&link).unwrap(), at_path);
    assert_eq!(fs::read(&target).unwrap(), in_archive);
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, 0, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("RESEAT_UNDO_TARGET_CHANGED")),
        "{lines:?}"
    );
    assert_eq!(fs::read(&link).unwrap(), at_path);
    assert_eq!(fs::read(&target).unwrap(), in_archive);
}

/// Q198 (2) re-review, MEDIUM 2: `--undo` puts a link back (and removes
/// the regular file) only over an archive target that exists and holds the
/// recorded bytes. A target rewritten in place (same size) or removed is
/// refused (`RESEAT_UNDO_TARGET_CHANGED`) and nothing changes for that
/// row: the regular file stays, the row stays `done`; once the target is
/// back as recorded, the same row is undone.
#[test]
fn q198_2_rr_undo_verifies_the_archive_target() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let (good, good_target, good_bytes) = archived_link(&world, SID);
    let (edited, edited_target, edited_bytes) = archived_link(&world, SID2);
    let gone_sid = "0e0e0e0e-1111-4222-8333-444444444444";
    let (gone, gone_target, gone_bytes) = archived_link(&world, gone_sid);
    let mut options = world.options();
    options.codex = false;
    run(&options);
    for link in [&good, &edited, &gone] {
        assert!(fs::symlink_metadata(link).unwrap().is_file());
    }
    // Rewritten in place, same length; and removed.
    let mut other = edited_bytes.clone();
    let last = other.len() - 2;
    other[last] = b'X';
    fs::write(&edited_target, &other).unwrap();
    fs::rename(&gone_target, world.base.join("moved.jsonl")).unwrap();
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, 1, "{lines:?}");
    assert_eq!(fs::read_link(&good).unwrap(), good_target);
    assert_eq!(fs::read(&good_target).unwrap(), good_bytes);
    for (link, sid, bytes) in [
        (&edited, SID2, &edited_bytes),
        (&gone, gone_sid, &gone_bytes),
    ] {
        assert!(
            lines.iter().any(|line| line.contains(sid)
                && line.contains("\"outcome\":\"undo-refused\"")
                && line.contains("RESEAT_UNDO_TARGET_CHANGED")),
            "{sid}: {lines:?}"
        );
        assert!(fs::symlink_metadata(link).unwrap().is_file(), "{sid}");
        assert_eq!(
            &fs::read(link).unwrap(),
            bytes,
            "{sid}: the regular file kept"
        );
    }
    assert_eq!(fs::read(&edited_target).unwrap(), other, "never written");
    let ledger = ledger::Ledger::open_read_only(&options.state)
        .unwrap()
        .unwrap();
    for link in [&edited, &gone] {
        assert_eq!(
            ledger.regular(link).unwrap().unwrap().state,
            ledger::RegularState::Done
        );
    }
    drop(ledger);
    // The target back as recorded: the row is undone.
    fs::rename(world.base.join("moved.jsonl"), &gone_target).unwrap();
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, 1, "{lines:?}");
    assert_eq!(fs::read_link(&gone).unwrap(), gone_target);
    assert!(fs::symlink_metadata(&edited).unwrap().is_file());
}

/// Q198 C: home and `/private/tmp` sessions (and threads) are placed
/// resumable by ID under their source key or `cwd` by default, and are
/// in no lane, so R0 holds without a waiver.
#[test]
fn q198_c_home_and_tmp_sessions_are_placed_by_id() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(SID, "/Users/jess", &transcript(SID, "/Users/jess", "h", 2));
    world.land(
        SID2,
        "/private/tmp/claude-501/x",
        &transcript(SID2, "/private/tmp/claude-501/x", "t", 2),
    );
    let mut meta = rollout_bytes(T1, "legacy", 2);
    let text =
        String::from_utf8(meta.clone())
            .unwrap()
            .replacen("/Users/jess/git/lab", "/Users/jess", 1);
    meta = text.into_bytes();
    fs::write(world.codex_landed(&rel(T1)), &meta).unwrap();
    drop(world.landed_index());
    drop(world.dest_index());
    let options = world.options();
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "placed"), 2, "{lines:?}");
    assert!(world
        .dest(&format!("projects/-Users-jess/{SID}.jsonl"))
        .is_file());
    assert!(world
        .dest(&format!("projects/-private-tmp-claude-501-x/{SID2}.jsonl"))
        .is_file());
    assert_eq!(
        world.dest_row(T1).map(|row| row.0),
        Some("/Users/jess".to_owned())
    );
    let checked = r0(&options);
    assert_eq!((checked.by_id, checked.by_id_ok), (3, 3), "{checked:?}");
    assert!(checked.verdict().is_ok(), "{checked:?}");
    // Without the defaults they have no lane and fail R0.
    let mut options = world.options();
    options.by_id_exact.clear();
    options.by_id_prefixes.clear();
    assert!(r0(&options).verdict().is_err());
}

/// Q198 A and B: a lane resumes at `~/git/<name>` (its real path), and a
/// gftb lane whose `~/git` entry on the destination is a link into the
/// estate (sting's `greatfallstoolbus.org`) keys by that estate path.
#[test]
fn q198_ab_lanes_follow_the_destination_git_links() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let estate = world
        .base
        .join("estate/gftb-20261015/git/greatfallstoolbus.org");
    fs::create_dir_all(&estate).unwrap();
    std::os::unix::fs::symlink(&estate, world.base.join("real-git/greatfallstoolbus.org")).unwrap();
    let cwd = "/Users/jess/git/greatfallstoolbus.org";
    world.land(SID, cwd, &transcript(SID, cwd, "g", 2));
    world.land(
        SID2,
        "/Users/jess/git/lab",
        &transcript(SID2, "/Users/jess/git/lab", "l", 2),
    );
    let options = world.options();
    run(&options);
    let estate_key = crate::reseat::key::project_key(estate.to_str().unwrap());
    assert!(world
        .dest(&format!("projects/{estate_key}/{SID}.jsonl"))
        .is_file());
    assert!(world.dest_transcript(SID2).is_file());
    let checked = r0(&options);
    assert!(
        checked.lanes.contains_key(Path::new(&estate)),
        "{checked:?}"
    );
    assert!(checked.verdict().is_ok(), "{checked:?}");
}

/// The ledger's version 2 (`8a89f9a`) is migrated in place: its rows read,
/// its temporaries keep the shared name, and a read-only open of it works
/// before any pass migrates it.
#[test]
fn q198_a_version_2_ledger_is_migrated() {
    let temp = tempfile::tempdir().unwrap();
    let conn = Connection::open(temp.path().join(ledger::FILE)).unwrap();
    conn.execute_batch(
        "CREATE TABLE placement(dest BLOB PRIMARY KEY, landing BLOB NOT NULL, kind TEXT NOT NULL,
            session TEXT NOT NULL, state TEXT NOT NULL, l_dev INTEGER NOT NULL, l_ino INTEGER NOT NULL,
            l_size INTEGER NOT NULL, l_mtime INTEGER NOT NULL, d_dev INTEGER NOT NULL,
            d_ino INTEGER NOT NULL, d_size INTEGER NOT NULL, d_mtime INTEGER NOT NULL,
            placed INTEGER NOT NULL, digest BLOB, intent TEXT, i_dev INTEGER, i_ino INTEGER,
            i_size INTEGER, i_mtime INTEGER, pass INTEGER NOT NULL);
         CREATE TABLE session(tool TEXT NOT NULL, id TEXT NOT NULL, landing BLOB NOT NULL,
            source_cwd TEXT, dest_cwd BLOB, dest_key TEXT, slug TEXT, outcome TEXT NOT NULL,
            pass INTEGER NOT NULL, PRIMARY KEY(tool, id));
         CREATE TABLE thread_row(id TEXT PRIMARY KEY, db BLOB NOT NULL, cwd TEXT NOT NULL,
            rollout BLOB NOT NULL, state TEXT NOT NULL, pass INTEGER NOT NULL);
         CREATE TABLE snapshot(path BLOB PRIMARY KEY, dev INTEGER NOT NULL, ino INTEGER NOT NULL,
            size INTEGER NOT NULL, mtime INTEGER NOT NULL);
         CREATE TABLE snapshot_thread(ord INTEGER NOT NULL, id TEXT PRIMARY KEY,
            rollout TEXT NOT NULL, cwd TEXT NOT NULL);
         CREATE TABLE pass(pass INTEGER PRIMARY KEY, started_ns INTEGER NOT NULL, mode TEXT NOT NULL);
         INSERT INTO session VALUES ('claude', 's1', x'2f6c', '/Users/jess/git/lab', NULL, 'k', 'slug', 'placed', 1);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    drop(conn);
    let read_only = ledger::Ledger::open_read_only(temp.path())
        .unwrap()
        .unwrap();
    let row = read_only.session("claude", "s1").unwrap().unwrap();
    assert_eq!(row.slug.as_deref(), Some("slug"));
    assert!(read_only.cwd_updates().unwrap().is_empty());
    drop(read_only);
    let migrated = ledger::Ledger::open(temp.path(), 0, "test").unwrap();
    assert_eq!(migrated.temp_token(), "", "the shared name of version 2");
    assert_eq!(
        migrated
            .session("claude", "s1")
            .unwrap()
            .unwrap()
            .source_cwd
            .as_deref(),
        Some("/Users/jess/git/lab")
    );
    drop(migrated);
    let fresh = tempfile::tempdir().unwrap();
    let ledger = ledger::Ledger::open(fresh.path(), 0, "test").unwrap();
    assert_eq!(ledger.temp_token().len(), 12);
}
