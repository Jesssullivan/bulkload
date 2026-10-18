//! The two adversarial reviews of `550396c` (TIN-6587), one row per
//! finding, each written before its fix and seen failing against
//! `550396c` (the receipt is `docs/agent-notes/2026-10-16-estate-reseat-review-fixes.md`).
//!
//! Review 0 (real binaries, Codex and live layouts): `r0_*`. Review 1
//! (placement and crash rules): `r1_*`.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;

use rusqlite::{params, Connection};

use super::{append, r0, run, set_mtime, transcript, World, SERIAL, SID, SID2, SOURCE_HOME};
use crate::reseat::{check, reseat, Options, Unmapped};

mod m2r2;
mod q198;
mod rereview;

const T1: &str = "019a0000-0000-7000-8000-0000000000f1";
const T9: &str = "019a0000-0000-7000-8000-0000000000f9";

const THREADS: &str = "CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL, source TEXT NOT NULL, cwd TEXT NOT NULL,
    title TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0);";

/// A rollout's bytes: a `session_meta` line (with `history_mode`) and
/// `lines` event lines.
fn rollout_bytes(id: &str, mode: &str, lines: usize) -> Vec<u8> {
    let mut out = format!(
        "{{\"timestamp\":\"2026-10-16T10:00:00.000Z\",\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/Users/jess/git/lab\",\"history_mode\":\"{mode}\"}}}}\n"
    )
    .into_bytes();
    for index in 0..lines {
        out.extend_from_slice(
            format!(
                "{{\"timestamp\":\"2026-10-16T10:00:01.000Z\",\"ordinal\":{},\"type\":\"event_msg\",\"payload\":{{\"n\":{index}}}}}\n",
                index + 1
            )
            .as_bytes(),
        );
    }
    out
}

fn zst(bytes: &[u8]) -> Vec<u8> {
    zstd::encode_all(bytes, 3).unwrap()
}

/// The rollout's path relative to `.codex`, plain form.
fn rel(id: &str) -> String {
    format!("sessions/2026/10/16/rollout-2026-10-16T10-00-00-{id}.jsonl")
}

impl World {
    fn codex_landed(&self, rel: &str) -> PathBuf {
        let path = self.base.join("landing/.codex").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    fn codex_dest(&self, rel: &str) -> PathBuf {
        self.base.join("dest/.codex").join(rel)
    }

    fn landed_index(&self) -> Connection {
        let conn = Connection::open(self.base.join("landing/.codex/state_5.sqlite")).unwrap();
        conn.execute_batch(THREADS).unwrap();
        conn
    }

    fn dest_index(&self) -> Connection {
        let conn = Connection::open(self.base.join("dest/.codex/state_5.sqlite")).unwrap();
        conn.execute_batch(THREADS).unwrap();
        conn
    }

    /// Land thread `id` (cwd `~/git/lab`) with its rollout as `form`
    /// (`""` or `".zst"`) holding `plain` (compressed for `.zst`).
    fn land_thread(&self, id: &str, form: &str, plain: &[u8], created: i64) {
        let rel = format!("{}{form}", rel(id));
        let bytes = if form == ".zst" {
            zst(plain)
        } else {
            plain.to_vec()
        };
        fs::write(self.codex_landed(&rel), bytes).unwrap();
        self.landed_index()
            .execute(
                "INSERT OR REPLACE INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title)
                 VALUES (?1, ?2, ?3, ?3, 'cli', '/Users/jess/git/lab', 'title')",
                params![id, format!("{SOURCE_HOME}/.codex/{rel}"), created],
            )
            .unwrap();
    }

    fn dest_row(&self, id: &str) -> Option<(String, String)> {
        self.dest_index()
            .query_row(
                "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
    }

    fn real_lab(&self) -> String {
        self.base.join("real-git/lab").to_str().unwrap().to_owned()
    }
}

fn check_lines(options: &Options) -> (crate::reseat::CheckSummary, Vec<String>) {
    let mut lines = Vec::new();
    let summary = check(options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    (summary, lines)
}

/// Every `.jsonl` / `.jsonl.zst` file under the destination's `sessions/`.
fn dest_rollouts(world: &World) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![world.base.join("dest/.codex/sessions")];
    while let Some(at) = pending.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Review 0, finding 1: a paginated thread whose rollout landed only as
/// `.jsonl.zst` does not resume when placed in that form (Codex stats the
/// plain `.jsonl`). It is placed decompressed, as `.jsonl`, and its row
/// names that path; `--check` compares the decompressed streams.
#[test]
fn r0_1_a_zst_rollout_is_placed_decompressed() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let plain = rollout_bytes(T1, "paginated", 40);
    world.land_thread(T1, ".zst", &plain, 1);
    drop(world.dest_index());
    let options = world.options();
    let (_, lines, _) = run(&options);
    let placed = world.codex_dest(&rel(T1));
    assert_eq!(
        fs::read(&placed).ok(),
        Some(plain),
        "the plain .jsonl, decompressed: {lines:?}"
    );
    assert!(
        !world.codex_dest(&format!("{}.zst", rel(T1))).exists(),
        "no .zst form at the destination"
    );
    assert_eq!(
        world.dest_row(T1),
        Some((world.real_lab(), placed.to_str().unwrap().to_owned()))
    );
    assert!(r0(&options).verdict().is_ok(), "{:?}", r0(&options));
}

/// Review 0, finding 5: the source compressing a rollout between passes
/// (`.jsonl` → `.jsonl.zst`, the snapshot row re-pointed) changes only the
/// landed form: the destination keeps one rollout, the row is unchanged,
/// no conflict, `--check` passes.
#[test]
fn r0_5_a_landed_form_change_keeps_one_rollout() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let plain = rollout_bytes(T1, "legacy", 30);
    world.land_thread(T1, "", &plain, 1);
    drop(world.dest_index());
    let options = world.options();
    run(&options);
    let placed = world.codex_dest(&rel(T1));
    let inode = fs::metadata(&placed).unwrap().ino();
    fs::remove_file(world.codex_landed(&rel(T1))).unwrap();
    world.land_thread(T1, ".zst", &plain, 1);
    let (summary, lines, _) = run(&options);
    assert_eq!(dest_rollouts(&world), vec![placed.clone()], "{lines:?}");
    assert_eq!(fs::metadata(&placed).unwrap().ino(), inode);
    assert_eq!(summary.rows.get("conflict"), None, "{lines:?}");
    assert!(r0(&options).verdict().is_ok(), "{:?}", r0(&options));
    let (_, _, read) = run(&options);
    assert_eq!(read, 0, "the new form is read once");
}

/// Review 0, finding 2: a missing landing is a refusal, not an empty pass,
/// and a check that found nothing to check does not pass.
#[test]
fn r0_2_a_missing_landing_refuses_and_an_empty_check_fails() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let mut options = world.options();
    options.landing_claude = world.base.join("no-such-landing/.claude");
    options.landing_codex = world.base.join("no-such-landing/.codex");
    assert_eq!(
        reseat(&options, &mut |_| Ok(())).err().map(|e| e.code()),
        Some("RESEAT_LANDING_ABSENT")
    );
    assert!(
        !world.base.join("state").exists(),
        "refused before the ledger"
    );
    let checked = check(&options, &mut |_| Ok(()));
    assert!(
        checked
            .as_ref()
            .map_or(true, |summary| summary.verdict().is_err()),
        "{checked:?}"
    );
    // A landing whose .codex is a dangling symlink (neo's /Volumes link).
    let options = world.options();
    fs::remove_dir(world.base.join("landing/.codex")).unwrap();
    std::os::unix::fs::symlink(
        "/Volumes/TinylandState/tinyland-state/codex",
        world.base.join("landing/.codex"),
    )
    .unwrap();
    assert_eq!(
        reseat(&options, &mut |_| Ok(())).err().map(|e| e.code()),
        Some("RESEAT_LANDING_ABSENT")
    );
    // Landings that exist but hold nothing: the check has nothing to pass.
    let world = World::new();
    let options = world.options();
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err(), "{lines:?}");
}

/// Review 0, finding 3: a transcript kept as `occupied` (or any outcome
/// that leaves the landed bytes out) places none of its companions.
#[test]
fn r0_3_a_kept_transcript_places_no_companions() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "kept-plan", 3),
    );
    fs::write(
        world.landed(&format!(
            "projects/-Users-jess-git-lab/{SID}/subagents/agent-a.jsonl"
        )),
        b"{}\n",
    )
    .unwrap();
    fs::write(world.landed(&format!("file-history/{SID}/h@v1")), b"v1").unwrap();
    fs::write(world.landed("plans/kept-plan.md"), b"plan").unwrap();
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(world.base.join("archived.jsonl"), b"{}\n").unwrap();
    std::os::unix::fs::symlink(world.base.join("archived.jsonl"), &dest).unwrap();
    // The placement rule, with OI-1003-Q198 (2)'s regularization off.
    let mut options = world.options();
    options.regularize = false;
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "occupied"), 1, "{lines:?}");
    assert!(
        !world
            .dest(&format!("projects/{}/{SID}", world.lab_key()))
            .exists(),
        "no sidecar: {lines:?}"
    );
    assert!(!world.dest(&format!("file-history/{SID}")).exists());
    assert!(!world.dest("plans/kept-plan.md").exists());
}

/// Review 0, finding 4: with no Codex index at the destination, the first
/// real `codex` start would backfill the placed rollout with the source
/// cwd, archived, and lock the thread out. The run refuses before writing.
#[test]
fn r0_4_no_destination_index_refuses_before_writing() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 3),
    );
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 3), 1);
    let options = world.options();
    assert_eq!(
        reseat(&options, &mut |_| Ok(())).err().map(|e| e.code()),
        Some("RESEAT_INDEX_ABSENT")
    );
    assert!(!world.codex_dest("sessions").exists());
    assert!(
        !world.dest("projects").exists(),
        "nothing of Claude's either"
    );
    let mut options = world.options();
    options.codex = false;
    assert!(reseat(&options, &mut |_| Ok(())).is_ok(), "--no-codex");
}

/// Review 0, finding 6 and review 1, finding 5: sessions with no lane
/// (unmapped, unresolved) or kept by ID and not re-seated fail R0 unless
/// named in a waiver, and the check names them.
#[test]
fn r0_6_unlaned_sessions_fail_r0() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 2),
    );
    // Landed under blahaj's key, but its first cwd is a worktree.
    let unresolved = "abababab-0000-4000-8000-000000000001";
    let path = world.landed(&format!(
        "projects/-Users-jess-git-blahaj/{unresolved}.jsonl"
    ));
    fs::write(
        &path,
        transcript(unresolved, "/Users/jess/git/blahaj.worktrees/x", "u", 2),
    )
    .unwrap();
    let unmapped = "abababab-0000-4000-8000-000000000002";
    world.land(
        unmapped,
        "/Users/jess/o/lab",
        &transcript(unmapped, "/Users/jess/o/lab", "m", 2),
    );
    let options = world.options();
    run(&options);
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err(), "{lines:?}");
    let text = lines.join("\n");
    assert!(
        text.contains(unresolved) && text.contains(unmapped),
        "{text}"
    );
}

/// Review 1, finding 5: a by-ID session that did not re-seat fails R0.
#[test]
fn r1_5_a_failed_by_id_session_fails_r0() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 2),
    );
    world.land(
        SID2,
        "/private/tmp/scratch",
        &transcript(SID2, "/private/tmp/scratch", "t", 2),
    );
    let occupied = world.dest(&format!("projects/-private-tmp-scratch/{SID2}.jsonl"));
    fs::create_dir_all(occupied.parent().unwrap()).unwrap();
    fs::write(&occupied, b"{\"different\":true}\n").unwrap();
    let mut options = world.options();
    options.unmapped = Unmapped::ById;
    run(&options);
    let checked = r0(&options);
    assert_eq!((checked.by_id, checked.by_id_ok), (1, 0));
    assert!(checked.verdict().is_err(), "{checked:?}");
}

/// Review 0, finding 7: a session resumed on the destination after its
/// placement (the landed bytes an exact prefix of the destination's) is
/// `continued`, which holds R0; so is a rollout Codex compressed at the
/// destination. A destination copy that lost landed bytes still fails.
#[test]
fn r0_7_a_destination_resume_still_passes_the_check() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 5),
    );
    let plain = rollout_bytes(T1, "legacy", 5);
    world.land_thread(T1, "", &plain, 1);
    drop(world.dest_index());
    let options = world.options();
    run(&options);
    append(
        &world.dest_transcript(SID),
        b"{\"type\":\"permission-mode\"}\n",
    );
    // Codex at the destination resumed the thread (appended), then
    // compressed the rollout and re-pointed its row.
    let placed = world.codex_dest(&rel(T1));
    let mut grown = plain;
    grown.extend_from_slice(b"{\"type\":\"thread_settings_applied\"}\n");
    fs::write(format!("{}.zst", placed.display()), zst(&grown)).unwrap();
    fs::remove_file(&placed).unwrap();
    world
        .dest_index()
        .execute(
            "UPDATE threads SET rollout_path = ?1 WHERE id = ?2",
            params![format!("{}.zst", placed.display()), T1],
        )
        .unwrap();
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_ok(), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("\"continued\":2")),
        "{lines:?}"
    );
    // A destination copy missing landed bytes fails.
    fs::write(world.dest_transcript(SID), b"{}\n").unwrap();
    assert!(r0(&options).verdict().is_err());
}

/// Review 1, finding 1: a destination writer appending while an extension
/// is in flight is never spliced into a landed line, and the file it grew
/// is handed over, its mtime not reset.
#[test]
fn r1_1_a_concurrent_destination_append_is_never_spliced() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 50);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    let options = world.options();
    run(&options);
    let dest = world.dest_transcript(SID);
    let placed = fs::metadata(&dest).unwrap().len();
    let mut tail = Vec::new();
    let mut index = 0;
    while tail.len() < 24 * 1024 * 1024 {
        tail.extend_from_slice(
            format!(
                "{{\"type\":\"user\",\"n\":{index},\"pad\":\"{}\"}}\n",
                "p".repeat(333)
            )
            .as_bytes(),
        );
        index += 1;
    }
    append(&landed, &tail);
    let injected: &[u8] = b"{\"type\":\"user\",\"from\":\"sting-resume\"}\n";
    let watched = dest.clone();
    let writer = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_mins(1);
        while std::time::Instant::now() < deadline {
            if fs::metadata(&watched).map_or(0, |m| m.len()) > placed + 1 {
                append(&watched, injected);
                return true;
            }
            std::hint::spin_loop();
        }
        false
    });
    let (summary, lines, _) = run(&options);
    assert!(writer.join().unwrap(), "the writer never saw the extension");
    let landed_lines: BTreeSet<&[u8]> = fs::read(&landed)
        .unwrap()
        .leak()
        .split_inclusive(|byte| *byte == b'\n')
        .collect();
    let got = fs::read(&dest).unwrap();
    for line in got.split_inclusive(|byte| *byte == b'\n') {
        assert!(
            line == injected || landed_lines.contains(line),
            "a spliced line: {:?}",
            String::from_utf8_lossy(&line[..line.len().min(200)])
        );
    }
    // Unless the writer came after the whole tail, the file it grew is
    // the destination's from then on.
    if !got.ends_with(injected) {
        assert_eq!(
            summary.sessions_of("claude", "extended"),
            0,
            "the grown file is the destination's: {lines:?}"
        );
    }
}

/// Review 1, finding 2: a crash after a supersede exchange, then an
/// in-place edit at the destination before the rerun. The edit is kept.
#[test]
fn r1_2_an_edit_after_a_supersede_crash_is_kept() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "own-plan", 2),
    );
    fs::write(world.landed("plans/own-plan.md"), b"v1").unwrap();
    let options = world.options();
    run(&options);
    let plan = world.dest("plans/own-plan.md");
    fs::write(world.landed("plans/own-plan.md"), b"v2 from neo").unwrap();
    // The state supersede() leaves at its exchange point.
    let root = fs::canonicalize(world.base.join("dest/.claude")).unwrap();
    let dest = root.join("plans/own-plan.md");
    // This ledger's temporary name (its token, re-review of `8a89f9a`).
    let temp_name = {
        let ledger =
            crate::reseat::ledger::Ledger::open(&options.state, crate::reseat::now_ns(), "test")
                .unwrap();
        crate::reseat::place::temp_name(std::ffi::OsStr::new("own-plan.md"), ledger.temp_token())
    };
    let temp = root.join("plans").join(&temp_name);
    fs::write(&temp, b"v2 from neo").unwrap();
    let staged = crate::reseat::ledger::Identity::of(&fs::metadata(&temp).unwrap());
    {
        let ledger =
            crate::reseat::ledger::Ledger::open(&options.state, crate::reseat::now_ns(), "test")
                .unwrap();
        let mut row = ledger.placement(&dest).unwrap().unwrap();
        row.intent = Some(crate::reseat::ledger::Intent::Supersede(staged));
        ledger.put(&row).unwrap();
    }
    let directory = fs::File::open(root.join("plans")).unwrap();
    crate::io::exchange(
        &directory,
        &std::ffi::CString::new(temp_name.as_encoded_bytes()).unwrap(),
        &std::ffi::CString::new("own-plan.md").unwrap(),
    )
    .unwrap();
    // The crash; then the plan is edited in place at the destination.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&plan, b"v2 from neo + USER EDIT ON STING").unwrap();
    let (_, lines, _) = run(&options);
    assert_eq!(
        fs::read(&plan).unwrap(),
        b"v2 from neo + USER EDIT ON STING",
        "{lines:?}"
    );
    assert!(!temp.exists(), "the old placement (ours) is gone");
}

/// Review 1, finding 3: a landed transcript ending inside a line is placed
/// up to its last newline; the held-back bytes follow once the line is
/// whole. A destination transcript ending mid-line fails the check.
#[test]
fn r1_3_a_partial_last_line_is_held_back() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let whole = transcript(SID, "/Users/jess/git/lab", "s", 5);
    let mut cut = whole.clone();
    cut.extend_from_slice(b"{\"type\":\"user\",\"message\":\"second prompt cut mid-wri");
    let landed = world.land(SID, "/Users/jess/git/lab", &cut);
    let options = world.options();
    run(&options);
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), whole);
    assert!(r0(&options).verdict().is_ok(), "{:?}", r0(&options));
    append(&landed, b"te\"}\n");
    run(&options);
    assert_eq!(
        fs::read(world.dest_transcript(SID)).unwrap(),
        fs::read(&landed).unwrap()
    );
    // A copy already at the destination that ends mid-line is flagged.
    world.land(SID2, "/Users/jess/git/lab", &cut);
    fs::write(world.dest_transcript(SID2), &cut).unwrap();
    run(&options);
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_err());
    assert!(
        lines.iter().any(|line| line.contains("ends-mid-line")),
        "{lines:?}"
    );
}

/// Review 1, finding 4: two source keys mapped to one destination key do
/// not supersede each other's memory notes on every pass; the second is a
/// typed collision, and reruns read nothing.
#[test]
fn r1_4_two_source_keys_on_one_destination_key_collide_once() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "a", 2),
    );
    world.land(
        SID2,
        "/Users/jess/git/lab.worktrees/wt",
        &transcript(SID2, "/Users/jess/git/lab.worktrees/wt", "b", 2),
    );
    fs::write(
        world.landed("projects/-Users-jess-git-lab/memory/MEMORY.md"),
        b"- note from the main checkout\n",
    )
    .unwrap();
    fs::write(
        world.landed("projects/-Users-jess-git-lab-worktrees-wt/memory/MEMORY.md"),
        b"- note from the worktree\n",
    )
    .unwrap();
    let mut options = world.options();
    options.maps = vec![(
        PathBuf::from("/Users/jess/git/lab.worktrees/wt"),
        world.base.join("real-git/lab"),
    )];
    let memory = world.dest(&format!("projects/{}/memory/MEMORY.md", world.lab_key()));
    let (_, lines, _) = run(&options);
    let first = fs::read(&memory).unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("RESEAT_LANE_COLLISION")),
        "{lines:?}"
    );
    for _ in 0..2 {
        let (summary, _, read) = run(&options);
        assert_eq!(read, 0, "{summary:?}");
        assert_eq!(summary.files_of("claude", "superseded"), 0);
        assert_eq!(fs::read(&memory).unwrap(), first);
    }
}

/// Review 1, finding 6: a landed rollout with no row in the landed
/// snapshot is placed (resumable by ID) and reported. Superseded in part by
/// OI-1003-Q198 D: it now gets a row built from its header, so the lane's
/// picker lists it and R0 holds; it is reported `synthesized`.
#[test]
fn r1_6_a_rollout_with_no_snapshot_row_is_reported() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 3), 1);
    fs::write(world.codex_landed(&rel(T9)), rollout_bytes(T9, "legacy", 3)).unwrap();
    drop(world.dest_index());
    let options = world.options();
    let (_, lines, _) = run(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(T9) && line.contains("\"row\":\"synthesized\"")),
        "{lines:?}"
    );
    assert!(world.codex_dest(&rel(T9)).is_file(), "placed");
    assert_eq!(
        world.dest_row(T9),
        Some((
            world.real_lab(),
            world.codex_dest(&rel(T9)).to_str().unwrap().to_owned()
        ))
    );
    let (summary, lines) = check_lines(&options);
    assert!(summary.verdict().is_ok(), "{lines:?}");
    let (summary, _, read) = run(&options);
    assert_eq!(read, 0, "S3: {summary:?}");
    assert_eq!(summary.rows.get("unchanged"), Some(&2), "{summary:?}");
}

/// Review 1, finding 7: retention keeps a margin, and reads every settings
/// source Claude Code merges (a lane's project settings here), and the
/// companions' modification times.
#[test]
fn r1_7_retention_has_a_margin_and_reads_every_source() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // 29 days old under the default 30: deleted the next day.
    let world = World::new();
    let landed = world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 2),
    );
    set_mtime(&landed, 29 * 86_400);
    fs::remove_file(world.dest("settings.json")).unwrap();
    assert_eq!(
        reseat(&world.options(), &mut |_| Ok(()))
            .err()
            .map(|e| e.code()),
        Some("RESEAT_RETENTION_UNSAFE")
    );
    // The user setting keeps it; the lane's project setting does not.
    fs::write(
        world.dest("settings.json"),
        b"{\"cleanupPeriodDays\": 3650}",
    )
    .unwrap();
    // Managed settings: a drop-in lower than the user's.
    fs::create_dir_all(world.base.join("managed-settings.d")).unwrap();
    fs::write(
        world.base.join("managed-settings.d/10-retention.json"),
        b"{\"cleanupPeriodDays\": 20}",
    )
    .unwrap();
    assert_eq!(
        reseat(&world.options(), &mut |_| Ok(()))
            .err()
            .map(|e| e.code()),
        Some("RESEAT_RETENTION_UNSAFE")
    );
    fs::remove_file(world.base.join("managed-settings.d/10-retention.json")).unwrap();
    set_mtime(&landed, 20 * 86_400);
    fs::create_dir_all(world.base.join("real-git/lab/.claude")).unwrap();
    fs::write(
        world.base.join("real-git/lab/.claude/settings.json"),
        b"{\"cleanupPeriodDays\": 10}",
    )
    .unwrap();
    assert_eq!(
        reseat(&world.options(), &mut |_| Ok(()))
            .err()
            .map(|e| e.code()),
        Some("RESEAT_RETENTION_UNSAFE")
    );
    fs::remove_file(world.base.join("real-git/lab/.claude/settings.json")).unwrap();
    // A companion older than the cutoff.
    let old = world.landed(&format!("file-history/{SID}/h@v1"));
    fs::write(&old, b"v1").unwrap();
    set_mtime(&old, 3_600 * 86_400);
    assert_eq!(
        reseat(&world.options(), &mut |_| Ok(()))
            .err()
            .map(|e| e.code()),
        Some("RESEAT_RETENTION_UNSAFE")
    );
    set_mtime(&old, 86_400);
    assert!(reseat(&world.options(), &mut |_| Ok(())).is_ok());
}

/// Review 1, finding 8: a landed change outside the two digest windows is
/// a rewrite, never extended into a hybrid.
#[test]
fn r1_8_a_change_outside_the_windows_is_a_rewrite() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 2000);
    assert!(bytes.len() > 3 * 64 * 1024);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    let options = world.options();
    run(&options);
    let mut changed = bytes.clone();
    let middle = bytes.len() / 2;
    changed[middle] = if changed[middle] == b'Z' { b'Y' } else { b'Z' };
    changed.extend_from_slice(&transcript(SID, "/Users/jess/git/lab", "s", 3));
    fs::write(&landed, &changed).unwrap();
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.sessions_of("claude", "source-rewritten"),
        1,
        "{lines:?}"
    );
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), bytes);
}

/// Review 1, finding 9: an unchanged rerun does not read the landed Codex
/// snapshot either (it is unreadable here; its identity is unchanged).
#[test]
fn r1_9_an_unchanged_rerun_does_not_read_the_snapshot() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 3), 1);
    drop(world.dest_index());
    let options = world.options();
    run(&options);
    let snapshot = world.base.join("landing/.codex/state_5.sqlite");
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o000)).unwrap();
    let outcome = reseat(&options, &mut |_| Ok(()));
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o600)).unwrap();
    let summary = outcome.unwrap();
    assert_eq!(summary.rows.get("unchanged"), Some(&1), "{summary:?}");
}

/// The ledger's schema changed with these fixes (full digests, whole
/// staged identities): a ledger the first build wrote is refused, never
/// read as this one.
#[test]
fn a_ledger_of_the_first_build_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let conn = Connection::open(temp.path().join(crate::reseat::ledger::FILE)).unwrap();
    conn.execute_batch(
        "CREATE TABLE placement(dest BLOB PRIMARY KEY, head BLOB, tail BLOB); PRAGMA user_version = 0;",
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        crate::reseat::ledger::Ledger::open(temp.path(), 0, "test").err(),
        Some(crate::BulkloadRefusal::SchemaMismatch)
    );
    let fresh = tempfile::tempdir().unwrap();
    drop(crate::reseat::ledger::Ledger::open(fresh.path(), 0, "test").unwrap());
    assert!(crate::reseat::ledger::Ledger::open(fresh.path(), 0, "test").is_ok());
    assert!(crate::reseat::ledger::Ledger::open_read_only(fresh.path())
        .unwrap()
        .is_some());
}
