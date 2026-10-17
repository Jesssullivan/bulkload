//! The operator's M2 round 2 (2026-10-17), from the first real in-place
//! re-seat, and the two reviews of its first head: (a) a thread the
//! destination already holds is `destination-adopted` when the file its
//! row names holds the landed lines, `destination-holds` when a
//! destination file is there unproved, `destination-row-only` when only a
//! row is; none is a refusal, and only the first passes `--check`. (b)
//! Every destination row spelled through the home's `git` or a `--map`
//! symlink is re-spelled, lane or not, landed threads or not, ledgered and
//! undone like OI-1003-Q198 (1).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use rusqlite::params;

use super::{check_lines, rel, rollout_bytes, zst, World, T1};
use crate::reseat::tests::{run, SERIAL};
use crate::reseat::{ledger, undo, Unmapped};

const T2: &str = "019a0000-0000-7000-8000-0000000000d2";
const T3: &str = "019a0000-0000-7000-8000-0000000000d3";
const T4: &str = "019a0000-0000-7000-8000-0000000000d4";
const T5: &str = "019a0000-0000-7000-8000-0000000000d5";
const T6: &str = "019a0000-0000-7000-8000-0000000000d6";
const T7: &str = "019a0000-0000-7000-8000-0000000000d7";
const T8: &str = "019a0000-0000-7000-8000-0000000000d8";

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
/// though the destination had its row. It is `destination-row-only`
/// (`rollout-root`, `"proved":false`): nothing landed ties to the row, so
/// it is reported, not claimed held. Without a row it stays the refusal,
/// named `rollout-root`.
#[test]
fn m2r2_a_an_unrooted_thread_with_a_destination_row_is_not_unmapped() {
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
        one.contains("\"outcome\":\"destination-row-only\"")
            && one.contains("\"proof\":\"rollout-root\"")
            && one.contains("\"proved\":false")
            && one.contains("\"refused\":null"),
        "{lines:?}"
    );
    let two = thread_line(&lines, T2);
    assert!(
        two.contains("RESEAT_LANE_UNMAPPED") && two.contains("\"row\":\"rollout-root\""),
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("codex", "destination-row-only"), 1);
    assert_eq!(summary.rows.get("destination-row-only"), Some(&1));
    assert_eq!(
        summary.sessions_of("codex", "RESEAT_LANE_UNMAPPED:rollout-root"),
        1,
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("codex", "RESEAT_LANE_UNMAPPED"), 0);
    assert!(!world.codex_dest(&rel(T1)).exists(), "nothing placed");
}

/// (a) A thread the destination holds in another form or place:
///
/// - T2: its row names the `.zst` at its place, holding the landed lines
///   exactly: `destination-adopted`, nothing placed.
/// - T3: its row names an archive `.gz`: `destination-holds`
///   (`unreadable-form`).
/// - T4: its row names the plain file, absent, with a `.zst` symlink into
///   an archive beside it: nothing placed beside the destination's form,
///   `destination-holds` (`row-file-absent`).
/// - T5: its row names a `.zst` holding less than the landing:
///   `destination-holds` (`size-differs`).
/// - T6: its row names the same file in `archived_sessions/`, holding the
///   landed lines: `destination-adopted`, and `--check` accepts it.
/// - T7: its row names the same file in `archived_sessions/`, not there,
///   no form beside: `destination-row-only` (`absent`), never claimed
///   held. (A row naming the plain placement itself with nothing there is
///   placed and adopted, as before.)
/// - T8: its row names a `.zst` that cannot be opened (mode 000):
///   `destination-holds` (`unreadable`) for that thread only; the pass and
///   the check go on (review A1).
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
    let ids = [T2, T3, T4, T5, T6, T7, T8];
    for (order, id) in ids.into_iter().enumerate() {
        world.land_thread(id, "", &bytes(id), i64::try_from(order).unwrap() + 1);
    }
    drop(world.dest_index());
    fs::create_dir_all(world.codex_dest(&rel(T2)).parent().unwrap()).unwrap();
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
    let t6_archived = world.codex_dest(&rel(T6).replacen("sessions/", "archived_sessions/", 1));
    fs::create_dir_all(t6_archived.parent().unwrap()).unwrap();
    fs::write(&t6_archived, bytes(T6)).unwrap();
    world.put_dest_row(T6, t6_archived.to_str().unwrap(), &world.real_lab());
    let t7_gone = world.codex_dest(&rel(T7).replacen("sessions/", "archived_sessions/", 1));
    world.put_dest_row(T7, t7_gone.to_str().unwrap(), &world.real_lab());
    let t8_zst = world.codex_dest(&format!("{}.zst", rel(T8)));
    fs::write(&t8_zst, zst(&bytes(T8))).unwrap();
    fs::set_permissions(&t8_zst, fs::Permissions::from_mode(0o000)).unwrap();
    world.put_dest_row(T8, t8_zst.to_str().unwrap(), &world.real_lab());

    let options = world.options();
    let (summary, lines, _) = run(&options);
    let expect = [
        (T2, "destination-adopted", "exactly", true),
        (T3, "destination-holds", "unreadable-form", false),
        (T4, "destination-holds", "row-file-absent", false),
        (T5, "destination-holds", "size-differs", false),
        (T6, "destination-adopted", "exactly", true),
        (T7, "destination-row-only", "absent", false),
        (T8, "destination-holds", "unreadable", false),
    ];
    for (id, outcome, proof, proved) in expect {
        let line = thread_line(&lines, id);
        assert!(
            line.contains(&format!("\"outcome\":\"{outcome}\""))
                && line.contains(&format!("\"proof\":\"{proof}\""))
                && line.contains(&format!("\"proved\":{proved}"))
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
    let counts = |summary: &crate::reseat::Summary| {
        (
            summary.sessions_of("codex", "destination-adopted"),
            summary.sessions_of("codex", "destination-holds"),
            summary.sessions_of("codex", "destination-row-only"),
        )
    };
    assert_eq!(counts(&summary), (2, 4, 1), "{lines:?}");
    assert_eq!(summary.rows.get("destination-holds"), Some(&4));
    assert_eq!(summary.sessions_of("codex", "adopted"), 0, "no word shared");
    // The rows are never changed.
    assert_eq!(
        world.dest_row(T2),
        Some((world.real_lab(), t2_zst.to_str().unwrap().to_owned()))
    );
    let (again, relines, read) = run(&options);
    assert_eq!(read, 0, "S3: {relines:?}");
    assert_eq!(counts(&again), (2, 4, 1), "{relines:?}");
    // The check holds T2 (the `.zst`) and T6 (elsewhere); the rest fail,
    // T8 as its own failure.
    let (checked, check) = check_lines(&options);
    let lane = checked.lanes.values().next().unwrap();
    assert_eq!((lane.threads, lane.threads_ok), (7, 2), "{check:?}");
    assert!(
        lane.failures
            .iter()
            .any(|failure| failure.contains(T8) && failure.contains("unreadable")),
        "{lane:?}"
    );
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
    assert_eq!(counts(&later), (3, 3, 1));
    fs::set_permissions(&t8_zst, fs::Permissions::from_mode(0o600)).unwrap();
}

/// (a) A row naming the plain placement through a symlinked spelling of
/// the destination's `.codex` (sting's `~/.codex` link), before the day
/// directory exists at the destination (review A3): the same place, so
/// the rollout is placed there and the row adopted; `--check` passes.
#[test]
fn m2r2_a_a_spelled_row_names_the_placement_before_its_directory_exists() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = rollout_bytes(T1, "legacy", 4);
    world.land_thread(T1, "", &bytes, 1);
    drop(world.dest_index());
    let spelled = world.base.join("codex-link");
    std::os::unix::fs::symlink(world.base.join("dest/.codex"), &spelled).unwrap();
    assert!(!world.codex_dest(&rel(T1)).parent().unwrap().exists());
    world.put_dest_row(
        T1,
        spelled.join(rel(T1)).to_str().unwrap(),
        &world.real_lab(),
    );
    let options = world.options();
    let (_, lines, _) = run(&options);
    assert_eq!(
        fs::read(world.codex_dest(&rel(T1))).ok(),
        Some(bytes),
        "{lines:?}"
    );
    assert!(
        thread_line(&lines, T1).contains("\"row\":\"adopted\""),
        "{lines:?}"
    );
    let (checked, check) = check_lines(&options);
    assert!(checked.verdict().is_ok(), "{check:?}");
}

/// (b) Review B1, B2, B3: a row whose `cwd` is not UTF-8 is skipped and
/// counted, never a panic, in the pass and the check; the sweep and the
/// check's count run with nothing landed; and `--undo` of a row re-spelled
/// twice (the destination spelled it again in between) puts back the
/// destination's latest spelling, not the first one.
#[test]
#[allow(clippy::too_many_lines)]
fn m2r2_b_text_empty_landing_and_latest_spelling() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    drop(world.dest_index());
    let link = |rest: &str| world.base.join("dest/git").join(rest);
    let canonical = |rest: &str| world.base.join("real-git").join(rest);
    fs::create_dir_all(canonical("one")).unwrap();
    fs::create_dir_all(canonical("two")).unwrap();
    let text = |path: PathBuf| path.to_str().unwrap().to_owned();
    let id = "019a0000-0000-7000-8000-00000000c001";
    world.put_dest_row(id, "/x/c001.jsonl", &text(link("one")));
    world
        .dest_index()
        .execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
             VALUES ('019a0000-0000-7000-8000-00000000c002', '/x/c002.jsonl', 1, 1, 'cli',
                     CAST(x'2f686f6d652fff' AS TEXT), 't')",
            [],
        )
        .unwrap();
    let options = world.options();
    let (before, _) = check_lines(&options);
    assert_eq!(before.respell_pending, 1, "nothing landed: {before:?}");
    let (summary, lines, _) = run(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"state\":\"empty\"")),
        "{lines:?}"
    );
    assert_eq!(summary.rows.get("respelled"), Some(&1), "{lines:?}");
    assert_eq!(summary.rows.get("respell-not-text"), Some(&1), "{lines:?}");
    assert_eq!(world.dest_cwd(id), Some(text(canonical("one"))));
    // The destination spells the row again, now another directory through
    // the link; the next pass re-spells that.
    world
        .dest_index()
        .execute(
            "UPDATE threads SET cwd = ?2 WHERE id = ?1",
            params![id, text(link("two"))],
        )
        .unwrap();
    let (summary, _, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), Some(&1));
    assert_eq!(world.dest_cwd(id), Some(text(canonical("two"))));
    let (_, undone) = undo(&options, &mut |_| Ok(())).unwrap();
    assert_eq!(undone, 1);
    assert_eq!(
        world.dest_cwd(id),
        Some(text(link("two"))),
        "the latest spelling, never the first one's other directory"
    );
}

/// (b) Tonight 9,242 rows spelled `/home/jess/git/…` were re-spelled by
/// hand: the ruled re-spelling touched only rows of mapped lanes. Every
/// destination row spelled through the home's `git` symlink or a `--map`
/// destination symlink is re-spelled, lane or not, existing or not, rebuilt from its components (`//`
/// and `./` dropped); a row
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
        (
            "019a0000-0000-7000-8000-00000000b008",
            format!("{}//sub/./x", text(link("unmapped-repo"))),
        ),
    ];
    for (id, cwd) in &rows {
        world.put_dest_row(id, &format!("/x/{id}.jsonl"), cwd);
    }
    let mut options = world.options();
    options
        .maps
        .push((PathBuf::from("/Users/jess/src"), map_link));
    let (before, _) = check_lines(&options);
    assert_eq!(before.respell_pending, 5, "{before:?}");
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("respelled"), Some(&5), "{lines:?}");
    let expect = [
        (rows[0].0, text(canonical("unmapped-repo"))),
        (rows[1].0, text(canonical("gone/deeper"))),
        (rows[2].0, rows[2].1.clone()),
        (rows[3].0, rows[3].1.clone()),
        (rows[4].0, rows[4].1.clone()),
        (rows[5].0, text(canonical("mapped/sub"))),
        (rows[6].0, text(world.base.join("real-git"))),
        (rows[7].0, text(canonical("unmapped-repo/sub/x"))),
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
        assert_eq!(updates.len(), 5);
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
    assert_eq!(undone, 5, "{lines:?}");
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
