//! The operator's M2 round 2 (2026-10-17), from the first real in-place
//! re-seat: (a) a thread the destination already holds (its row, or its
//! rollout in another form) is `adopted` when the file the row names holds
//! the landed lines, else `destination-holds`, never a refusal; a thread
//! recorded outside every source root that the destination holds is
//! `destination-holds`, not `RESEAT_LANE_UNMAPPED`. (b) Every destination
//! row spelled through the home's `git` or a `--map` symlink is re-spelled,
//! lane or not, ledgered and undone like OI-1003-Q198 (1).

use std::fs;
use std::path::PathBuf;

use rusqlite::params;

use super::{check_lines, rel, rollout_bytes, zst, World, T1};
use crate::reseat::tests::{run, SERIAL};
use crate::reseat::{ledger, undo, Unmapped};

const T2: &str = "019a0000-0000-7000-8000-0000000000d2";
const T3: &str = "019a0000-0000-7000-8000-0000000000d3";
const T4: &str = "019a0000-0000-7000-8000-0000000000d4";
const T5: &str = "019a0000-0000-7000-8000-0000000000d5";

impl World {
    fn put_dest_row(&self, id: &str, rollout: &str, cwd: &str) {
        self.dest_index()
            .execute(
                "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
                 VALUES (?1, ?2, 1, 1, 'cli', ?3, 'sting title')",
                params![id, rollout, cwd],
            )
            .unwrap();
    }

    fn dest_cwd(&self, id: &str) -> Option<String> {
        self.dest_index()
            .query_row(
                "SELECT cwd FROM threads WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok()
    }
}

fn thread_line(lines: &[String], id: &str) -> String {
    lines
        .iter()
        .find(|line| line.contains(id) && line.contains("\"kind\":\"thread\""))
        .cloned()
        .unwrap_or_default()
}

/// (a) Tonight's 12,262: with `--unmapped=by-id`, a thread whose rollout
/// is recorded outside every source root ended `RESEAT_LANE_UNMAPPED` even
/// though the destination held it. It is `destination-holds`
/// (`rollout-root`) when the destination's index has its row; without a
/// row it stays the refusal, named `rollout-root`.
#[test]
fn m2r2_a_an_unrooted_thread_the_destination_holds_is_not_unmapped() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    for id in [T1, T2] {
        fs::write(world.codex_landed(&rel(id)), rollout_bytes(id, "legacy", 2)).unwrap();
        world
            .landed_index()
            .execute(
                "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
                 VALUES (?1, ?2, 1, 1, 'cli', '/Users/jess/git/lab', 't')",
                params![id, format!("/elsewhere/.codex/{}", rel(id))],
            )
            .unwrap();
    }
    // T1: the destination holds it as an archive's `.gz`, its row naming
    // the plain path.
    let gz = world.codex_dest(&format!("{}.gz", rel(T1)));
    fs::create_dir_all(gz.parent().unwrap()).unwrap();
    fs::write(&gz, b"\x1f\x8b not read").unwrap();
    world.put_dest_row(
        T1,
        world.codex_dest(&rel(T1)).to_str().unwrap(),
        &world.real_lab(),
    );
    let mut options = world.options();
    options.unmapped = Unmapped::ById;
    let (summary, lines, _) = run(&options);
    let one = thread_line(&lines, T1);
    assert!(
        one.contains("\"outcome\":\"destination-holds\"")
            && one.contains("\"proof\":\"rollout-root\"")
            && one.contains("\"refused\":null"),
        "{lines:?}"
    );
    let two = thread_line(&lines, T2);
    assert!(
        two.contains("RESEAT_LANE_UNMAPPED") && two.contains("\"row\":\"rollout-root\""),
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("codex", "destination-holds"), 1);
    assert_eq!(
        summary.sessions_of("codex", "RESEAT_LANE_UNMAPPED:rollout-root"),
        1,
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("codex", "RESEAT_LANE_UNMAPPED"), 0);
    assert!(!world.codex_dest(&rel(T1)).exists(), "nothing placed");
}

/// (a) A thread the destination holds in another form or spelling:
///
/// - T1: its row names the plain placement through a symlinked spelling
///   of the destination's `.codex` (sting's `~/.codex` link), nothing
///   beside: placed there, the row adopted, the check passes.
/// - T2: its row names the `.zst` at its place, holding the landed lines
///   exactly: adopted as it is, nothing placed.
/// - T3: its row names an archive `.gz`: `destination-holds`
///   (`unreadable-form`).
/// - T4: its row names the plain file, absent, with a `.zst` symlink into
///   an archive beside it: nothing placed beside the destination's form,
///   `destination-holds` (`absent`).
/// - T5: its row names a `.zst` holding less than the landing:
///   `destination-holds` (`size-differs`).
///
/// None is a refusal; a rerun reads no landed byte (S3) and says the same.
#[test]
#[allow(clippy::too_many_lines)]
fn m2r2_a_a_destination_form_is_adopted_or_held() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = |id: &str| rollout_bytes(id, "legacy", 4);
    for (order, id) in [T1, T2, T3, T4, T5].into_iter().enumerate() {
        world.land_thread(id, "", &bytes(id), i64::try_from(order).unwrap() + 1);
    }
    drop(world.dest_index());
    let day = world.codex_dest(&rel(T1));
    fs::create_dir_all(day.parent().unwrap()).unwrap();
    let spelled = world.base.join("codex-link");
    std::os::unix::fs::symlink(world.base.join("dest/.codex"), &spelled).unwrap();
    world.put_dest_row(
        T1,
        spelled.join(rel(T1)).to_str().unwrap(),
        &world.real_lab(),
    );
    let t2_zst = world.codex_dest(&format!("{}.zst", rel(T2)));
    fs::write(&t2_zst, zst(&bytes(T2))).unwrap();
    world.put_dest_row(T2, t2_zst.to_str().unwrap(), &world.real_lab());
    let t3_gz = world.codex_dest(&format!("{}.gz", rel(T3)));
    fs::write(&t3_gz, b"\x1f\x8b archived").unwrap();
    world.put_dest_row(T3, t3_gz.to_str().unwrap(), &world.real_lab());
    let archive = world.base.join("archive.jsonl.zst");
    fs::write(&archive, zst(&bytes(T4))).unwrap();
    std::os::unix::fs::symlink(&archive, world.codex_dest(&format!("{}.zst", rel(T4)))).unwrap();
    world.put_dest_row(
        T4,
        world.codex_dest(&rel(T4)).to_str().unwrap(),
        &world.real_lab(),
    );
    let t5_zst = world.codex_dest(&format!("{}.zst", rel(T5)));
    fs::write(&t5_zst, zst(&rollout_bytes(T5, "legacy", 2))).unwrap();
    world.put_dest_row(T5, t5_zst.to_str().unwrap(), &world.real_lab());

    let options = world.options();
    let (summary, lines, _) = run(&options);
    assert_eq!(
        fs::read(world.codex_dest(&rel(T1))).ok(),
        Some(bytes(T1)),
        "{lines:?}"
    );
    assert!(
        thread_line(&lines, T1).contains("\"row\":\"adopted\""),
        "{lines:?}"
    );
    let expect = [
        (T2, "\"outcome\":\"adopted\"", "exactly"),
        (T3, "\"outcome\":\"destination-holds\"", "unreadable-form"),
        (T4, "\"outcome\":\"destination-holds\"", "absent"),
        (T5, "\"outcome\":\"destination-holds\"", "size-differs"),
    ];
    for (id, outcome, proof) in expect {
        let line = thread_line(&lines, id);
        assert!(
            line.contains(outcome)
                && line.contains(&format!("\"proof\":\"{proof}\""))
                && line.contains("\"refused\":null"),
            "{id}: {lines:?}"
        );
        assert!(
            !world.codex_dest(&rel(id)).exists(),
            "{id}: nothing placed beside the destination's own"
        );
    }
    assert!(!lines
        .iter()
        .any(|line| line.contains("RESEAT_INDEX_CONFLICT")));
    assert_eq!(summary.sessions_of("codex", "adopted"), 1);
    assert_eq!(summary.sessions_of("codex", "destination-holds"), 3);
    assert_eq!(summary.rows.get("held"), Some(&3));
    // The rows are never changed.
    assert_eq!(
        world.dest_row(T2),
        Some((world.real_lab(), t2_zst.to_str().unwrap().to_owned()))
    );
    let (again, relines, read) = run(&options);
    assert_eq!(read, 0, "S3: {relines:?}");
    assert_eq!(again.sessions_of("codex", "adopted"), 1, "{relines:?}");
    assert_eq!(again.sessions_of("codex", "destination-holds"), 3);
    // The check holds T1 (spelled) and T2 (the `.zst`); the held ones fail.
    let (checked, check) = check_lines(&options);
    let lane = checked.lanes.values().next().unwrap();
    assert_eq!((lane.threads, lane.threads_ok), (5, 2), "{check:?}");
    // The destination continues T5's form past the landing: proved again
    // (its identity changed) and adopted.
    fs::write(
        &t5_zst,
        zst(&[bytes(T5), b"{\"more\":1}\n".to_vec()].concat()),
    )
    .unwrap();
    let (later, lines, _) = run(&options);
    assert!(
        thread_line(&lines, T5).contains("\"proof\":\"continued\""),
        "{lines:?}"
    );
    assert_eq!(later.sessions_of("codex", "adopted"), 2);
}

/// (b) Tonight 9,242 rows spelled `/home/jess/git/…` were re-spelled by
/// hand: the ruled re-spelling touched only rows of mapped lanes. Every
/// destination row spelled through the home's `git` symlink or a `--map`
/// destination symlink is re-spelled, lane or not, existing or not; a row
/// that reaches another directory through a further link, one with `..`,
/// or one under no such link is never touched. `--check` counts them
/// first, a rerun re-spells nothing, `--undo` puts them back and an undone
/// row stays undone; `--no-respell` leaves every row.
#[test]
#[allow(clippy::too_many_lines)]
fn m2r2_b_every_symlink_spelled_row_is_respelled() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 2), 1);
    drop(world.dest_index());
    let link = |rest: &str| world.base.join("dest/git").join(rest);
    let canonical = |rest: &str| world.base.join("real-git").join(rest);
    fs::create_dir_all(canonical("unmapped-repo")).unwrap();
    fs::create_dir_all(world.base.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(world.base.join("elsewhere"), canonical("aside")).unwrap();
    fs::create_dir_all(canonical("mapped")).unwrap();
    let map_link = world.base.join("map-link");
    std::os::unix::fs::symlink(canonical("mapped"), &map_link).unwrap();
    let text = |path: PathBuf| path.to_str().unwrap().to_owned();
    let rows = [
        (
            "019a0000-0000-7000-8000-00000000b001",
            text(link("unmapped-repo")),
        ),
        (
            "019a0000-0000-7000-8000-00000000b002",
            text(link("gone/deeper")),
        ),
        (
            "019a0000-0000-7000-8000-00000000b003",
            text(link("aside/x")),
        ),
        (
            "019a0000-0000-7000-8000-00000000b004",
            format!("{}/../x", text(link("unmapped-repo"))),
        ),
        (
            "019a0000-0000-7000-8000-00000000b005",
            text(world.base.join("other/place")),
        ),
        (
            "019a0000-0000-7000-8000-00000000b006",
            text(map_link.join("sub")),
        ),
        ("019a0000-0000-7000-8000-00000000b007", text(link(""))),
    ];
    for (id, cwd) in &rows {
        world.put_dest_row(id, &format!("/x/{id}.jsonl"), cwd);
    }
    let mut options = world.options();
    options
        .maps
        .push((PathBuf::from("/Users/jess/src"), map_link));
    let (before, _) = check_lines(&options);
    assert_eq!(before.respell_pending, 4, "{before:?}");
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), Some(&4), "{lines:?}");
    let expect = [
        (rows[0].0, text(canonical("unmapped-repo"))),
        (rows[1].0, text(canonical("gone/deeper"))),
        (rows[2].0, rows[2].1.clone()),
        (rows[3].0, rows[3].1.clone()),
        (rows[4].0, rows[4].1.clone()),
        (rows[5].0, text(canonical("mapped/sub"))),
        (rows[6].0, text(world.base.join("real-git"))),
    ];
    for (id, cwd) in &expect {
        assert_eq!(
            world.dest_cwd(id).as_deref(),
            Some(cwd.as_str()),
            "{id}: {lines:?}"
        );
    }
    {
        let ledger = ledger::Ledger::open_read_only(&options.state)
            .unwrap()
            .unwrap();
        let updates = ledger.cwd_updates().unwrap();
        assert_eq!(updates.len(), 4);
        assert!(updates.iter().all(|row| row.state == "applied"));
    }
    let (after, _) = check_lines(&options);
    assert_eq!(after.respell_pending, 0);
    let (summary, _, read) = run(&options);
    assert_eq!(summary.rows.get("respelled"), None, "once");
    assert_eq!(read, 0, "S3");
    let mut lines = Vec::new();
    let (_, undone) = undo(&options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(undone, 4, "{lines:?}");
    for (id, cwd) in &rows {
        assert_eq!(world.dest_cwd(id).as_deref(), Some(cwd.as_str()), "{id}");
    }
    let (summary, _, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), None, "undone stays undone");
    let (pending, _) = check_lines(&options);
    assert_eq!(pending.respell_pending, 0, "undone rows are not pending");
    // `--no-respell` on a fresh state: nothing.
    let mut off = world.options();
    off.state = world.base.join("state-off");
    off.respell_cwd = false;
    let (summary, _, _) = run(&off);
    assert_eq!(summary.rows.get("respelled"), None);
    assert_eq!(world.dest_cwd(rows[0].0), Some(rows[0].1.clone()));
}
