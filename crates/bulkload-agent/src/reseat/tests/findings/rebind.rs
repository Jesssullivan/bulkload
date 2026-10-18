//! `--rebind-landing OLD=NEW` (A-106 follow-up): a second landing of the
//! same home under a new root collided with every placement the first one
//! made. A row of this ledger's own whose destination is still the file it
//! last wrote, and whose placed bytes are proved the new landed file's, is
//! re-bound instead: `rebound` when the bytes are exactly them, re-bound
//! and extended when the new file is longer; anything else stays a
//! collision. Never inferred from size; `--undo` puts the binding back.

use std::fs;
use std::path::{Path, PathBuf};

use super::{check_lines, rel, rollout_bytes, zst, World, T1};
use crate::reseat::ledger::Ledger;
use crate::reseat::tests::{append, run, transcript, SERIAL, SID, SID2};
use crate::reseat::{undo, Options};

const CWD: &str = "/Users/jess/git/lab";

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

impl World {
    /// The second landing (a copy of the first), and options reading it.
    fn second_landing(&self) -> PathBuf {
        let second = self.base.join("landing2");
        copy_tree(&self.base.join("landing"), &second);
        second
    }

    /// Options reading the landing `name`, re-binding `pairs` of landing
    /// names (`landing` → `landing2`, …).
    fn landing_options(&self, name: &str, pairs: &[(&str, &str)]) -> Options {
        let mut options = self.options();
        options.landing_claude = self.base.join(name).join(".claude");
        options.landing_codex = self.base.join(name).join(".codex");
        options.rebind = pairs
            .iter()
            .map(|(old, new)| {
                (
                    self.base.join(old),
                    fs::canonicalize(self.base.join(new)).unwrap(),
                )
            })
            .collect();
        options
    }

    fn second_options(&self, rebind: bool) -> Options {
        let mut options = self.options();
        options.landing_claude = self.base.join("landing2/.claude");
        options.landing_codex = self.base.join("landing2/.codex");
        if rebind {
            options.rebind = vec![(
                self.base.join("landing"),
                fs::canonicalize(self.base.join("landing2")).unwrap(),
            )];
        }
        options
    }
}

/// The same bytes under a new landing root: a collision without the flag,
/// `rebound` with it (nothing written), counted by `--check` first; an
/// unchanged rerun reads nothing; a session the new landing grew is
/// re-bound and extended; one whose bytes differ, or whose destination
/// changed, stays a collision; `--undo` puts the old binding back and a
/// later pass does not re-bind it.
#[test]
#[allow(clippy::too_many_lines)]
fn rebind_landing_rebinds_proved_rows_only() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let same = transcript(SID, CWD, "same", 30);
    world.land(SID, CWD, &same);
    let grows = "77777777-8888-4999-8aaa-bbbbbbbbbbbb";
    world.land(grows, CWD, &transcript(grows, CWD, "grows", 10));
    let differs = "88888888-9999-4aaa-8bbb-cccccccccccc";
    world.land(differs, CWD, &transcript(differs, CWD, "differs", 10));
    world.land(SID2, CWD, &transcript(SID2, CWD, "touched", 10));
    world.land_thread(T1, "", &rollout_bytes(T1, "legacy", 3), 1);
    drop(world.dest_index());
    let (first, lines, _) = run(&world.options());
    assert_eq!(first.sessions_of("claude", "placed"), 4, "{lines:?}");
    let second = world.second_landing();
    let landed =
        |sid: &str| second.join(format!(".claude/projects/-Users-jess-git-lab/{sid}.jsonl"));
    // The new landing grew one session, rewrote another (same size), and
    // the destination changed a third since it was placed.
    append(
        &landed(grows),
        b"{\"type\":\"user\",\"message\":\"more\"}\n",
    );
    fs::write(landed(differs), transcript(differs, CWD, "DIFFERS", 10)).unwrap();
    append(&world.dest_transcript(SID2), b"{\"resumed\":1}\n");
    // A pass from the first landing finds SID2 continued at the
    // destination: its row is the destination's now, never re-bound.
    let (continued, lines, _) = run(&world.options());
    assert_eq!(
        continued.sessions_of("claude", "dest-continued"),
        1,
        "{lines:?}"
    );
    // The new landing holds the rollout compressed (another form).
    let plain = second.join(".codex").join(rel(T1));
    let packed = second.join(".codex").join(format!("{}.zst", rel(T1)));
    fs::write(&packed, zst(&fs::read(&plain).unwrap())).unwrap();
    fs::remove_file(&plain).unwrap();
    rusqlite::Connection::open(second.join(".codex/state_5.sqlite"))
        .unwrap()
        .execute(
            "UPDATE threads SET rollout_path = ?1 WHERE id = ?2",
            rusqlite::params![format!("/Users/jess/.codex/{}.zst", rel(T1)), T1],
        )
        .unwrap();
    let (without, lines, _) = run(&world.second_options(false));
    assert_eq!(without.sessions_of("claude", "collision"), 4, "{lines:?}");
    let options = world.second_options(true);
    let (before, check) = check_lines(&options);
    assert_eq!((before.rebind_pending, before.rebound), (4, 0), "{check:?}");
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "rebound"), 1, "{lines:?}");
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{lines:?}");
    assert_eq!(summary.sessions_of("claude", "collision"), 2, "{lines:?}");
    assert_eq!(summary.files_of("codex", "rebound"), 1, "{lines:?}");
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), same);
    assert_eq!(
        fs::read(world.dest_transcript(grows)).unwrap(),
        fs::read(landed(grows)).unwrap(),
        "re-bound, then extended"
    );
    assert_eq!(
        fs::read(world.dest_transcript(differs)).unwrap(),
        transcript(differs, CWD, "differs", 10),
        "never written over"
    );
    let (after, check) = check_lines(&options);
    assert_eq!(
        (after.rebind_pending, after.rebound),
        (0, 3),
        "refused ones are not pending: {check:?}"
    );
    let (again, lines, read) = run(&options);
    assert_eq!(read, 0, "S3: {lines:?}");
    assert_eq!(again.sessions_of("claude", "unchanged"), 2, "{lines:?}");
    {
        let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
        let row = ledger
            .placement(&world.dest_transcript(SID))
            .unwrap()
            .unwrap();
        assert!(row.landing.starts_with(fs::canonicalize(&second).unwrap()));
    }
    // --undo: the old binding back (ledger only); not re-bound again.
    let mut undo_lines = Vec::new();
    undo(&options, &mut |line| {
        undo_lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        undo_lines
            .iter()
            .filter(|line| line.contains("\"tool\":\"rebind\"") && line.contains("\"undone\""))
            .count(),
        2,
        "{undo_lines:?}"
    );
    // The grown one was extended after its rebind: its row holds bytes the
    // old landed file never had, so it is kept, not put back.
    assert_eq!(
        undo_lines
            .iter()
            .filter(|line| line.contains("\"tool\":\"rebind\"") && line.contains("undo-kept"))
            .count(),
        1,
        "{undo_lines:?}"
    );
    let (later, lines, _) = run(&options);
    assert_eq!(later.sessions_of("claude", "rebound"), 0, "{lines:?}");
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), same);
}

/// Review finding 2: the rebind history is append-only. A refused second
/// hop never erases the first (`--check` still counts it, `--undo` still
/// puts it back); a proved chain of two hops is put back hop by hop.
#[test]
fn rebind_landing_history_is_kept_across_hops() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let same = transcript(SID, CWD, "same", 12);
    world.land(SID, CWD, &same);
    let other = transcript(SID2, CWD, "other", 12);
    world.land(SID2, CWD, &other);
    run(&world.options());
    copy_tree(&world.base.join("landing"), &world.base.join("landing2"));
    let (hop1, lines, _) = run(&world.landing_options("landing2", &[("landing", "landing2")]));
    assert_eq!(hop1.sessions_of("claude", "rebound"), 2, "{lines:?}");
    // Third landing: SID2 rewritten (refused), SID the same (proved).
    copy_tree(&world.base.join("landing2"), &world.base.join("landing3"));
    let third = |sid: &str| {
        world.base.join(format!(
            "landing3/.claude/projects/-Users-jess-git-lab/{sid}.jsonl"
        ))
    };
    fs::write(third(SID2), transcript(SID2, CWD, "OTHER", 12)).unwrap();
    let options = world.landing_options("landing3", &[("landing2", "landing3")]);
    let (hop2, lines, _) = run(&options);
    assert_eq!(hop2.sessions_of("claude", "rebound"), 1, "{lines:?}");
    assert_eq!(hop2.sessions_of("claude", "collision"), 1, "{lines:?}");
    let (checked, check) = check_lines(&options);
    assert_eq!(checked.rebound, 3, "the first hop kept: {check:?}");
    // Undo: SID back two hops, SID2 back one.
    let mut undo_lines = Vec::new();
    undo(&options, &mut |line| {
        undo_lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        undo_lines
            .iter()
            .filter(|line| line.contains("\"tool\":\"rebind\"") && line.contains("\"undone\""))
            .count(),
        3,
        "{undo_lines:?}"
    );
    let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
    for sid in [SID, SID2] {
        let row = ledger
            .placement(&world.dest_transcript(sid))
            .unwrap()
            .unwrap();
        assert!(
            row.landing.starts_with(world.base.join("landing/")),
            "{sid}: {row:?}"
        );
    }
}
