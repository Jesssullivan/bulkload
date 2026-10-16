//! The two re-reviews of `8a89f9a` (TIN-6587): data safety (`ds_*`) and
//! user outcome (`uo_*`), one row per finding, each written against the
//! API `8a89f9a` already had and seen failing there before its fix (the
//! receipt is `docs/agent-notes/2026-10-16-estate-reseat-rereview-fixes.md`).
//! The reviewers' scenario trees were `/dev/shm/reseat-rereview-datasafety`
//! and `/dev/shm/reseat-rereview-useroutcome`; each row ports one of them
//! into a throwaway home.

use std::fs;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::{check_lines, rel, rollout_bytes, zst, World, T1, T9};
use crate::reseat::reseat;
use crate::reseat::tests::{r0, run, transcript, SERIAL, SID, SID2, SOURCE_HOME};

const T2: &str = "019a0000-0000-7000-8000-0000000000f2";
const T3: &str = "019a0000-0000-7000-8000-0000000000f3";

/// A rollout in Codex 0.163's shape: `session_meta` recorded in `cwd`, a
/// turn context, an environment message, then the user's prompt.
fn rich_rollout(id: &str, cwd: &str, prompt: &str) -> Vec<u8> {
    format!(
        "{{\"timestamp\":\"2026-10-16T10:00:00.000Z\",\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{id}\",\"id\":\"{id}\",\"timestamp\":\"2026-10-16T10:00:00.000Z\",\"cwd\":\"{cwd}\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0-alpha.11\",\"source\":\"cli\",\"thread_source\":\"user\",\"model_provider\":\"openai\",\"history_mode\":\"legacy\"}}}}\n\
         {{\"timestamp\":\"2026-10-16T10:00:01.000Z\",\"ordinal\":1,\"type\":\"turn_context\",\"payload\":{{\"cwd\":\"{cwd}\",\"approval_policy\":\"never\",\"sandbox_policy\":{{\"type\":\"workspace-write\",\"network_access\":false}},\"model\":\"gpt-6.1-sol\",\"effort\":\"high\"}}}}\n\
         {{\"timestamp\":\"2026-10-16T10:00:01.000Z\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"<environment_context>\\n  <cwd>{cwd}</cwd>\\n</environment_context>\"}}]}}}}\n\
         {{\"timestamp\":\"2026-10-16T10:00:02.000Z\",\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"{prompt}\",\"images\":[]}}}}\n\
         {{\"timestamp\":\"2026-10-16T10:00:02.000Z\",\"ordinal\":4,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{prompt}\"}}]}}}}\n"
    )
    .into_bytes()
}

/// The landed snapshot with the threads table and no rows.
fn empty_snapshot(world: &World) {
    drop(world.landed_index());
}

/// Every file under `root` whose name ends with `suffix`.
fn files_ending(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(suffix) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Data safety, MAJOR (scenario C): a landed rollout with no snapshot row
/// was placed beside the destination's own row and form for the same
/// thread (a `.jsonl.zst` the destination continued); a real `codex resume`
/// then opened the older plain file and cut the destination's continued
/// lines off the thread, and `--check` passed. The destination's row or
/// form is the destination's: `RESEAT_INDEX_CONFLICT`, nothing placed.
#[test]
fn ds_1_a_rowless_rollout_never_lands_beside_a_destination_row() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    empty_snapshot(&world);
    let landed = rollout_bytes(T9, "legacy", 1);
    fs::write(world.codex_landed(&rel(T9)), &landed).unwrap();
    // The destination's own continued history for T9, compressed, with its
    // row naming it.
    let mut continued = landed;
    for n in 0..3 {
        continued
            .extend_from_slice(format!("{{\"type\":\"DEST-CONTINUED\",\"n\":{n}}}\n").as_bytes());
    }
    let dest_zst = PathBuf::from(format!("{}.zst", world.codex_dest(&rel(T9)).display()));
    fs::create_dir_all(dest_zst.parent().unwrap()).unwrap();
    fs::write(&dest_zst, zst(&continued)).unwrap();
    world
        .dest_index()
        .execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
             VALUES (?1, ?2, 1, 1, 'cli', ?3, 't')",
            params![T9, dest_zst.to_str().unwrap(), world.real_lab()],
        )
        .unwrap();
    // Scenario B: a destination row in another directory, no form beside.
    let elsewhere = world.base.join("dest/.codex/archived_sessions/x.jsonl");
    fs::write(world.codex_landed(&rel(T2)), rollout_bytes(T2, "legacy", 2)).unwrap();
    world
        .dest_index()
        .execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
             VALUES (?1, ?2, 1, 1, 'cli', ?3, 't')",
            params![T2, elsewhere.to_str().unwrap(), world.real_lab()],
        )
        .unwrap();
    // A form at the destination with no row at all.
    fs::write(world.codex_landed(&rel(T3)), rollout_bytes(T3, "legacy", 2)).unwrap();
    let orphan_zst = PathBuf::from(format!("{}.zst", world.codex_dest(&rel(T3)).display()));
    fs::write(&orphan_zst, zst(&rollout_bytes(T3, "legacy", 5))).unwrap();
    let options = world.options();
    let (_, lines, _) = run(&options);
    for id in [T9, T2, T3] {
        assert!(
            !world.codex_dest(&rel(id)).exists(),
            "{id}: placed beside the destination's own: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains(id) && line.contains("RESEAT_INDEX_CONFLICT")),
            "{id}: {lines:?}"
        );
    }
    assert_eq!(
        world.dest_row(T9),
        Some((world.real_lab(), dest_zst.to_str().unwrap().to_owned())),
        "the destination's row is never changed"
    );
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err(), "{lines:?}");
}

/// Data safety, MEDIUM (scenario F): two runs on one destination took no
/// lock and shared one temporary name, so one could publish the other's
/// half-written file. A run refuses `RESEAT_LOCKED` while another holds the
/// destination root or its state, and never touches another run's
/// temporary.
#[test]
fn ds_2_concurrent_runs_are_excluded() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 5);
    world.land(SID, "/Users/jess/git/lab", &bytes);
    let options = world.options();
    // Another run holds the destination's .claude.
    let root = fs::File::open(world.base.join("dest/.claude")).unwrap();
    // SAFETY: the descriptor is live for the call; flock takes no pointers.
    assert_eq!(
        unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let refused = reseat(&options, &mut |_| Ok(()));
    assert_eq!(refused.err().map(|e| e.code()), Some("RESEAT_LOCKED"));
    assert!(!world.dest_transcript(SID).exists(), "nothing written");
    unlock(&root);
    // Another run holds this state.
    fs::create_dir_all(&options.state).unwrap();
    let state = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(options.state.join("reseat.lock"))
        .unwrap();
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::flock(state.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let refused = reseat(&options, &mut |_| Ok(()));
    assert_eq!(refused.err().map(|e| e.code()), Some("RESEAT_LOCKED"));
    assert!(!world.dest_transcript(SID).exists(), "nothing written");
    unlock(&state);
    // Another run's temporary (its name, half written) is left alone; this
    // run's placement is the landed transcript.
    let key_dir = world.dest_transcript(SID).parent().unwrap().to_path_buf();
    fs::create_dir_all(&key_dir).unwrap();
    let foreign = key_dir.join(format!(".{SID}.jsonl.bulkload-reseat"));
    fs::write(&foreign, &bytes[..bytes.len() / 3]).unwrap();
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "placed"), 1, "{lines:?}");
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), bytes);
    assert_eq!(
        fs::read(&foreign).unwrap(),
        &bytes[..bytes.len() / 3],
        "another run's temporary is never removed or published"
    );
}

/// Release a test-held `flock` explicitly, as a run's own locks are
/// released: closing the descriptor alone leaves the lock held while a
/// child that another test thread is spawning still shares the open file
/// description (between its fork and its exec).
fn unlock(file: &fs::File) {
    // SAFETY: the descriptor is live for the call; flock takes no pointers.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) }, 0);
}

/// Data safety, MEDIUM (scenario D): one truncated `.jsonl.zst` aborted
/// every pass (`SOURCE_CHANGED_AFTER_SNAPSHOT`), left a staged temporary
/// in the live `sessions/`, and kept every later thread out. It is that
/// file's `unreadable-form`; the others are placed and registered.
#[test]
fn ds_3_a_truncated_zst_is_unreadable_for_that_file_only() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    for (index, id) in [T1, T2, T3].into_iter().enumerate() {
        world.land_thread(
            id,
            ".zst",
            &rollout_bytes(id, "legacy", 4_000),
            i64::try_from(index).unwrap() + 1,
        );
    }
    let cut = world.codex_landed(&format!("{}.zst", rel(T2)));
    let whole = fs::read(&cut).unwrap();
    fs::write(&cut, &whole[..whole.len() / 2]).unwrap();
    drop(world.dest_index());
    let options = world.options();
    let mut lines = Vec::new();
    let outcome = reseat(&options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    });
    assert!(outcome.is_ok(), "{outcome:?} {lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains(T2) && line.contains("RESEAT_UNREADABLE_FORM")),
        "{lines:?}"
    );
    for id in [T1, T3] {
        assert!(world.codex_dest(&rel(id)).is_file(), "{id}: {lines:?}");
        assert!(world.dest_row(id).is_some(), "{id} registered");
    }
    assert!(!world.codex_dest(&rel(T2)).exists());
    assert_eq!(
        files_ending(&world.base.join("dest/.codex"), ".bulkload-reseat"),
        Vec::<PathBuf>::new(),
        "no temporary left in the live sessions/"
    );
}

/// Data safety, LOW (scenario H): a destination transcript hardlinked to an
/// archive copy (or to the landing itself) was adopted and extended in
/// place, changing the other link. A file with more than one link is never
/// written: `occupied`.
#[test]
fn ds_5_a_hardlinked_destination_is_never_extended() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 10);
    world.land(SID, "/Users/jess/git/lab", &bytes);
    let archive = world.base.join("archive.jsonl");
    let prefix_end = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(4)
        .map(|(at, _)| at + 1)
        .unwrap();
    fs::write(&archive, &bytes[..prefix_end]).unwrap();
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::hard_link(&archive, &dest).unwrap();
    // A second session whose destination is a hardlink to its landing.
    let landed2 = world.land(
        SID2,
        "/Users/jess/git/lab",
        &transcript(SID2, "/Users/jess/git/lab", "t", 3),
    );
    fs::hard_link(&landed2, world.dest_transcript(SID2)).unwrap();
    let (summary, lines, _) = run(&world.options());
    assert_eq!(
        fs::read(&archive).unwrap(),
        &bytes[..prefix_end],
        "the archive copy is never extended: {lines:?}"
    );
    assert_eq!(summary.sessions_of("claude", "occupied"), 2, "{lines:?}");
    assert_eq!(fs::metadata(&landed2).unwrap().nlink(), 2);
}

/// Data safety, LOW (scenario G): the snapshot names the plain rollout (3
/// lines) and the landing also holds a longer `.jsonl.zst` form (11 lines)
/// that is never placed; `--check` passed R0. It fails it.
#[test]
fn ds_6_check_fails_an_unplaced_longer_extra_form() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 2), 1);
    fs::write(
        world.codex_landed(&format!("{}.zst", rel(T1))),
        zst(&rollout_bytes(T1, "legacy", 10)),
    )
    .unwrap();
    drop(world.dest_index());
    let options = world.options();
    run(&options);
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err(), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("extra-form")),
        "{lines:?}"
    );
}

/// User outcome, minor 1 (world A): a session refused as unmapped or
/// unresolved was read again on every rerun (no ledger row): S3 did not
/// hold on neo's shape. Refused sessions are cached by landed identity.
#[test]
fn uo_1_refused_sessions_are_not_read_again() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/Documents/notes",
        &transcript(SID, "/Users/jess/Documents/notes", "", 200),
    );
    let unresolved = "abababab-0000-4000-8000-0000000000aa";
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{unresolved}.jsonl")),
        b"{\"type\":\"summary\",\"summary\":\"no cwd here\"}\n".repeat(100),
    )
    .unwrap();
    empty_snapshot(&world);
    fs::write(
        world.codex_landed(&rel(T9)),
        rich_rollout(T9, "/Users/jess/Documents/notes", "unmapped prompt"),
    )
    .unwrap();
    drop(world.dest_index());
    let options = world.options();
    let (_, first, _) = run(&options);
    assert!(
        first
            .iter()
            .any(|line| line.contains("RESEAT_LANE_UNMAPPED")),
        "{first:?}"
    );
    let (_, lines, read) = run(&options);
    assert_eq!(read, 0, "S3 with refused sessions: {lines:?}");
}

/// User outcome, minor 2 (world B): a thread whose rollout is recorded
/// outside the default source root (sting's `~/.codex` is a symlink to
/// `/srv/fast-local/jess/state/codex`) is refused `RESEAT_LANE_UNMAPPED` by
/// the run, but `--check` called it `landing-absent`. Both name the
/// rollout root.
#[test]
fn uo_2_a_rollout_outside_the_source_root_is_unmapped_in_check() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let rel_path = rel(T1);
    fs::write(
        world.codex_landed(&rel_path),
        rollout_bytes(T1, "legacy", 2),
    )
    .unwrap();
    world
        .landed_index()
        .execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
             VALUES (?1, ?2, 1, 1, 'cli', '/Users/jess/git/lab', 't')",
            params![T1, format!("/srv/fast-local/jess/state/codex/{rel_path}")],
        )
        .unwrap();
    drop(world.dest_index());
    let options = world.options();
    run(&options);
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"kind\":\"unlaned\"")
                && line.contains(T1)
                && line.contains("RESEAT_LANE_UNMAPPED")
                && line.contains("rollout-root")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("landing-absent")),
        "{lines:?}"
    );
    // With the root named, it places and holds.
    let mut options = world.options();
    options.codex_source_roots = vec![
        PathBuf::from(format!("{SOURCE_HOME}/.codex")),
        PathBuf::from("/srv/fast-local/jess/state/codex"),
    ];
    run(&options);
    assert!(
        r0(&options).verdict().is_ok(),
        "{:?}",
        check_lines(&options)
    );
}

/// User outcome, minor 3 (world C) and OI-1003-Q198 (D): a rollout with no
/// snapshot row was placed with no row; resuming it by ID in the lane made
/// Codex write a row with the source `cwd` (hiding it from the lane's
/// picker for good). It now gets a row built from its rollout header, with
/// the lane's `cwd` and its first prompt as the preview, so the lane's
/// picker lists it; and a row that does carry the source `cwd` is named as
/// such by `--check`.
#[test]
fn uo_3_a_rowless_rollout_gets_a_row_from_its_header() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    empty_snapshot(&world);
    fs::write(
        world.codex_landed(&rel(T9)),
        rich_rollout(T9, "/Users/jess/git/lab", "RRNOROW prompt"),
    )
    .unwrap();
    drop(world.dest_index());
    let options = world.options();
    let (_, lines, _) = run(&options);
    let placed = world.codex_dest(&rel(T9));
    assert_eq!(
        world.dest_row(T9),
        Some((world.real_lab(), placed.to_str().unwrap().to_owned())),
        "{lines:?}"
    );
    let (preview, source): (String, String) = world
        .dest_index()
        .query_row(
            "SELECT title, source FROM threads WHERE id = ?1",
            params![T9],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (preview.as_str(), source.as_str()),
        ("RRNOROW prompt", "cli")
    );
    assert!(
        r0(&options).verdict().is_ok(),
        "{:?}",
        check_lines(&options)
    );
    // Codex resumed it by ID and wrote the source cwd: named as such.
    world
        .dest_index()
        .execute(
            "UPDATE threads SET cwd = '/Users/jess/git/lab' WHERE id = ?1",
            params![T9],
        )
        .unwrap();
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err());
    assert!(
        lines.iter().any(|line| line.contains("row-cwd-is-source")),
        "{lines:?}"
    );
}

/// User outcome, minor 4 (world A): directories under a project key that
/// are neither a session's sidecar nor `memory/` (neo's `drafts/` and
/// `vercel-plugin/`) stayed in the landing with no row (S4, Q185). They
/// are carried to the lane's key; a key with no lane names them.
#[test]
fn uo_4_other_directories_under_a_key_are_carried() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 2),
    );
    fs::write(
        world.landed("projects/-Users-jess-git-lab/drafts/pr575-body.md"),
        b"# body\n",
    )
    .unwrap();
    fs::write(
        world.landed("projects/-Users-jess-git-lab/vercel-plugin/skill-injections.jsonl"),
        b"{\"skill\":1}\n",
    )
    .unwrap();
    fs::write(
        world.landed("projects/-Users-jess-git-gone/drafts/orphan.md"),
        b"orphan\n",
    )
    .unwrap();
    let options = world.options();
    let (_, lines, _) = run(&options);
    let key = world.lab_key();
    assert_eq!(
        fs::read(world.dest(&format!("projects/{key}/drafts/pr575-body.md"))).ok(),
        Some(b"# body\n".to_vec()),
        "{lines:?}"
    );
    assert!(world
        .dest(&format!(
            "projects/{key}/vercel-plugin/skill-injections.jsonl"
        ))
        .is_file());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("orphan.md") || line.contains("-Users-jess-git-gone")),
        "a key with no lane is named: {lines:?}"
    );
}
