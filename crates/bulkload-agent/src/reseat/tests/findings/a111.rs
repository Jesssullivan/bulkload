//! The operator's A-20261018-111 (Linear asks ledger, 2026-10-18):
//! "Extend regularize to companions" and "Keep sting's, waive it".
//!
//! On sting about 37,900 companion files (subagent transcripts and their
//! `.meta.json`, tool results, workflows, `file-history` and the rest) are
//! symlinks into the archive, so a lane whose companions are links fails
//! `--check` (`companion-missing`) even where the bytes are the landed
//! ones; `--replace-archived-dest` kept an identical link `occupied`; an
//! unknown actor re-linked 384 regularized transcripts, which a `done` row
//! skipped for ever; and one legalab `file-history` snapshot diverged
//! (sting's kept by never-overwrite). One row each.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{check_lines, rel, rollout_bytes, World, T1};
use crate::reseat::ledger::{Ledger, RegularState, FILE};
use crate::reseat::regularize::{set_hook, Step};
use crate::reseat::tests::{append, run, set_mtime, transcript, SERIAL, SID, SID2};
use crate::reseat::{reseat, undo, Options};
use crate::BulkloadRefusal;

const SID3: &str = "33333333-4444-4555-8666-777777777777";
const CWD: &str = "/Users/jess/git/lab";

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn is_regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

/// `target` holding `bytes`, linked (absolute text) at `link`, the target
/// an hour old.
fn archive_link(link: &Path, target: &Path, bytes: &[u8]) {
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
    set_mtime(target, 3_600);
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn line_for<'a>(lines: &'a [String], needle: &str, word: &str) -> Option<&'a String> {
    lines
        .iter()
        .find(|line| line.contains(needle) && line.contains(word))
}

fn undo_lines(options: &Options) -> ((u64, u64), Vec<String>) {
    let mut lines = Vec::new();
    let counts = undo(options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    (counts, lines)
}

/// A landed session whose every companion the destination holds as a
/// symlink into the archive (sting's shape), byte-identical to the landed
/// one, plus sting's own companions: `(companion links, archive)`.
struct Linked {
    /// `(link, target, bytes, kind word)`.
    files: Vec<(PathBuf, PathBuf, Vec<u8>, &'static str)>,
    archive: PathBuf,
}

impl World {
    #[allow(clippy::too_many_lines)]
    fn linked_companions(&self) -> Linked {
        let archive = self.base.join("archive/claude");
        let key = self.lab_key();
        let neo = transcript(SID, CWD, "plan-a", 6);
        self.land(SID, CWD, &neo);
        // The transcript itself is a regular copy already (regularized
        // before; the companions were left links).
        let dest_transcript = self.dest_transcript(SID);
        fs::create_dir_all(dest_transcript.parent().unwrap()).unwrap();
        fs::write(&dest_transcript, &neo).unwrap();
        let mut files = Vec::new();
        let mut companion = |rel_landed: Option<String>, rel_dest: String, bytes: &[u8], word| {
            if let Some(rel) = rel_landed {
                fs::write(self.landed(&rel), bytes).unwrap();
            }
            let link = self.dest(&rel_dest);
            let target = archive.join(&rel_dest);
            archive_link(&link, &target, bytes);
            files.push((link, target, bytes.to_vec(), word));
        };
        let src = "projects/-Users-jess-git-lab";
        for (rel, bytes) in [
            (
                format!("{SID}/subagents/agent-a1.jsonl"),
                b"{\"isSidechain\":true}\n".as_slice(),
            ),
            (
                format!("{SID}/subagents/agent-a1.meta.json"),
                b"{\"agentType\":\"general\"}".as_slice(),
            ),
            (
                format!("{SID}/subagents/workflows/wf_1/agent-b2.jsonl"),
                b"{\"wf\":1}\n".as_slice(),
            ),
            (
                format!("{SID}/tool-results/r1.txt"),
                b"tool output\n".as_slice(),
            ),
        ] {
            companion(
                Some(format!("{src}/{rel}")),
                format!("projects/{key}/{rel}"),
                bytes,
                "sidecar",
            );
        }
        for rel in [
            format!("file-history/{SID}/cb44c51ba7602a0d@v2"),
            format!("tasks/{SID}/1.json"),
            format!("session-env/{SID}/env.sh"),
            format!("debug/{SID}.txt"),
            format!("todos/{SID}-agent-{SID}.json"),
        ] {
            companion(Some(rel.clone()), rel, b"global state\n", "global");
        }
        companion(
            Some("plans/plan-a.md".to_owned()),
            "plans/plan-a.md".to_owned(),
            b"# plan a\n",
            "plan",
        );
        companion(
            Some(format!("{src}/memory/note.md")),
            format!("projects/{key}/memory/note.md"),
            b"a memory\n",
            "memory",
        );
        // Sting's own: a sidecar of a session neo never had, and a key
        // file that is no session's.
        companion(
            None,
            format!("projects/{key}/{SID3}/subagents/agent-own.jsonl"),
            b"{\"own\":true}\n",
            "sidecar",
        );
        companion(
            None,
            format!("projects/{key}/neo-session-support/notes.txt"),
            b"support\n",
            "extra",
        );
        Linked { files, archive }
    }
}

/// A-20261018-111 (1): every companion link is regularized before the
/// placement, as a transcript is: a regular copy of its target with the
/// target's mtime, the archive kept, a `regularized` row each, a line
/// naming its kind. `--check` then passes the lane (it failed on the
/// links); an unchanged rerun reads no landed byte and changes nothing;
/// `--undo` puts every link back over its unchanged copy, and an undone
/// link is not regularized again. A directory link is reported
/// `dir-link` and never written through; a dangling link is kept and
/// named; a link within the live state is left (`inside-root`); a
/// temporary is never listed.
#[test]
#[allow(clippy::too_many_lines)]
fn a111_companion_links_are_regularized_checked_and_undone() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let linked = world.linked_companions();
    // A directory link (the 2026-10-01 repair's shape), a dangling link, a
    // link within the live state, and an interrupted undo's link at a
    // temporary name.
    let fork_dir = world.base.join("repair/forks/x/file-history").join(SID2);
    fs::create_dir_all(&fork_dir).unwrap();
    fs::write(fork_dir.join("a@v1"), b"x").unwrap();
    std::os::unix::fs::symlink(&fork_dir, world.dest(&format!("file-history/{SID2}"))).unwrap();
    let dangling = world.dest(&format!("tasks/{SID}/gone.json"));
    std::os::unix::fs::symlink(world.base.join("gone"), &dangling).unwrap();
    let inside = world.dest(&format!("todos/{SID2}-agent-x.json"));
    std::os::unix::fs::symlink(world.dest("settings.json"), &inside).unwrap();
    let temp = world.dest(&format!(
        "projects/{}/.agent.jsonl.0123456789ab.bulkload-reseat",
        world.lab_key()
    ));
    std::os::unix::fs::symlink(&linked.archive, &temp).unwrap();
    let mut options = world.options();
    options.codex = false;

    let (before, lines) = check_lines(&options);
    assert!(before.verdict().is_err(), "the links fail R0: {lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("companion-missing")),
        "{lines:?}"
    );

    let (summary, lines, _) = run(&options);
    for (link, target, bytes, word) in &linked.files {
        assert!(is_regular(link), "{link:?}: {lines:?}");
        assert_eq!(&fs::read(link).unwrap(), bytes, "{link:?}");
        assert_eq!(
            fs::symlink_metadata(link).unwrap().mtime(),
            fs::metadata(target).unwrap().mtime(),
            "the target's mtime: {link:?}"
        );
        assert_eq!(&fs::read(target).unwrap(), bytes, "the archive kept");
        let line = line_for(&lines, &link.to_string_lossy(), "\"kind\":\"regularize\"")
            .unwrap_or_else(|| panic!("{link:?}: {lines:?}"));
        assert!(line.contains("\"outcome\":\"regularized\""), "{line}");
        assert!(
            line.contains(&format!("\"companion\":\"{word}\"")),
            "{line}"
        );
    }
    assert_eq!(
        summary.rows.get("companion-regularized"),
        Some(&(linked.files.len() as u64)),
        "{lines:?}"
    );
    assert!(is_link(&world.dest(&format!("file-history/{SID2}"))));
    assert!(line_for(&lines, &format!("file-history/{SID2}"), "dir-link").is_some());
    assert_eq!(fs::read(fork_dir.join("a@v1")).unwrap(), b"x");
    assert!(is_link(&dangling) && line_for(&lines, "gone.json", "dangling").is_some());
    assert!(is_link(&inside) && line_for(&lines, "agent-x.json", "inside-root").is_some());
    assert!(is_link(&temp), "a temporary is never listed");
    {
        let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
        let rows = ledger.regulars().unwrap();
        assert_eq!(rows.len(), linked.files.len(), "{rows:?}");
        assert!(rows
            .iter()
            .all(|row| row.state == RegularState::Done && !row.replace && row.digest.is_some()));
    }
    let (after, lines) = check_lines(&options);
    assert!(after.verdict().is_ok(), "{lines:?}");

    let (again, lines, read) = run(&options);
    assert_eq!(read, 0, "S3: {lines:?}");
    assert_eq!(again.rows.get("companion-regularized"), None, "{lines:?}");

    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(put_back, linked.files.len() as u64, "{lines:?}");
    for (link, target, bytes, _) in &linked.files {
        assert_eq!(fs::read_link(link).unwrap(), *target, "{link:?}");
        assert_eq!(&fs::read(target).unwrap(), bytes);
    }
    let (_, lines, _) = run(&options);
    assert!(
        linked.files.iter().all(|(link, ..)| is_link(link)),
        "undone stays undone: {lines:?}"
    );
}

/// `--no-regularize` leaves every companion link as it is.
#[test]
fn a111_no_regularize_leaves_companion_links() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let linked = world.linked_companions();
    let mut options = world.options();
    options.codex = false;
    options.regularize = false;
    let (summary, lines, _) = run(&options);
    assert!(
        linked.files.iter().all(|(link, ..)| is_link(link)),
        "{lines:?}"
    );
    assert!(
        summary
            .rows
            .keys()
            .all(|word| !word.starts_with("companion-")),
        "{summary:?}"
    );
}

/// The live race rule holds for a companion: a write through the link
/// between the read and the swap keeps both copies (the staged one aside,
/// named), no row; the next pass regularizes the whole file.
#[test]
fn a111_a_companion_written_before_the_swap_keeps_both() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let key_dir = world.dest(&format!("projects/{}", world.lab_key()));
    let link = key_dir.join(format!("{SID3}/subagents/agent-live.jsonl"));
    let target = world.base.join("archive/agent-live.jsonl");
    archive_link(&link, &target, b"{\"n\":1}\n");
    let mut options = world.options();
    options.codex = false;
    let appended = b"{\"n\":2}\n";
    set_hook(Some(Box::new(move |step, at: &Path| {
        if step == Step::Staged && at.ends_with("agent-live.jsonl") {
            append(at, appended);
        }
    })));
    let (_, lines, _) = run(&options);
    set_hook(None);
    let report = line_for(&lines, "agent-live.jsonl", "\"kind\":\"regularize\"").unwrap();
    assert!(report.contains("raced-skipped"), "{report}");
    assert!(report.contains("RESEAT_REGULARIZE_RACED"), "{report}");
    assert!(is_link(&link));
    assert_eq!(fs::read(&target).unwrap(), b"{\"n\":1}\n{\"n\":2}\n");
    let (_, lines, _) = run(&options);
    assert!(is_regular(&link), "{lines:?}");
    assert_eq!(fs::read(&link).unwrap(), b"{\"n\":1}\n{\"n\":2}\n");
}

/// A companion whose target a process holds open is not regularized
/// (`held-open`, `DESTINATION_OCCUPIED`); once it is closed, the next pass
/// regularizes it.
#[test]
fn a111_a_target_held_open_is_left_for_the_next_pass() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let link = world.dest(&format!("file-history/{SID3}/f@v1"));
    let target = world.base.join("archive/f@v1");
    archive_link(&link, &target, b"held\n");
    let mut options = world.options();
    options.codex = false;
    let held = fs::OpenOptions::new().append(true).open(&target).unwrap();
    let (_, lines, _) = run(&options);
    let report = line_for(&lines, "f@v1", "\"kind\":\"regularize\"").unwrap();
    assert!(
        report.contains("held-open")
            && report.contains(BulkloadRefusal::DestinationOccupied.code()),
        "{report}"
    );
    assert!(is_link(&link));
    drop(held);
    let (_, lines, _) = run(&options);
    assert!(is_regular(&link), "{lines:?}");
}

/// A-20261018-111 (1), re-links: a path this ledger regularized (a
/// transcript the pass then extended, and a companion) that someone
/// re-linked into the archive since is never skipped as handled: the pass
/// names it (`relinked`, its row marked and its placement row dropped),
/// regularizes it again (`"relinked":true`), and the placement decides it
/// again (the transcript extended back to the landed one). `--check`
/// passes. A path `--undo` put back stays a link.
#[test]
#[allow(clippy::too_many_lines)]
fn a111_a_relinked_path_is_judged_again() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let neo = transcript(SID, CWD, "s", 8);
    world.land(SID, CWD, &neo);
    let cut = neo
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(3)
        .map(|(at, _)| at + 1)
        .unwrap();
    let archive = world.base.join("archive");
    let transcript_target = archive.join(format!("{SID}.jsonl"));
    archive_link(&world.dest_transcript(SID), &transcript_target, &neo[..cut]);
    let sidecar_rel = format!("{SID}/subagents/agent-a.jsonl");
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{sidecar_rel}")),
        b"{\"a\":1}\n",
    )
    .unwrap();
    let sidecar = world.dest(&format!("projects/{}/{sidecar_rel}", world.lab_key()));
    let sidecar_target = archive.join("agent-a.jsonl");
    archive_link(&sidecar, &sidecar_target, b"{\"a\":1}\n");
    let mut options = world.options();
    options.codex = false;
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{lines:?}");
    assert_eq!(fs::read(world.dest_transcript(SID)).unwrap(), neo);
    assert!(is_regular(&sidecar));
    // An unknown actor re-links both into the archive.
    for (path, target) in [
        (world.dest_transcript(SID), &transcript_target),
        (sidecar.clone(), &sidecar_target),
    ] {
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(target, &path).unwrap();
    }
    let (relinked, lines) = check_lines(&options);
    assert!(relinked.verdict().is_err(), "{lines:?}");
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("relinked"), Some(&2), "{lines:?}");
    for path in [world.dest_transcript(SID), sidecar.clone()] {
        let shown = path.to_string_lossy().into_owned();
        assert!(
            line_for(&lines, &shown, "\"kind\":\"relinked\"").is_some(),
            "{lines:?}"
        );
        let again = line_for(&lines, &shown, "\"kind\":\"regularize\"").unwrap();
        assert!(
            again.contains("\"relinked\":true") && again.contains("\"outcome\":\"regularized\""),
            "{again}"
        );
        assert!(is_regular(&path), "{path:?}");
    }
    assert_eq!(
        fs::read(world.dest_transcript(SID)).unwrap(),
        neo,
        "the placement decided it again: extended to the landed one"
    );
    assert_eq!(
        fs::read(&transcript_target).unwrap(),
        &neo[..cut],
        "archive kept"
    );
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{summary:?}");
    let (after, lines) = check_lines(&options);
    assert!(after.verdict().is_ok(), "{lines:?}");
    {
        let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
        assert!(ledger
            .regulars()
            .unwrap()
            .iter()
            .all(|row| row.state == RegularState::Done));
    }
    // Undone, then re-linked by the undo itself: not judged again.
    let ((put_back, _), lines) = undo_lines(&options);
    assert_eq!(
        put_back, 1,
        "the sidecar (the transcript was extended): {lines:?}"
    );
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.rows.get("relinked"), None, "{lines:?}");
    assert!(is_link(&sidecar), "{lines:?}");
}

/// A-20261018-111 with `--replace-archived-dest` (the #270 gap): links into
/// the root that the placement proves equal to (or a prefix of) their
/// landed files, which it keeps links (`occupied`, `dest-ahead`), are
/// regularized after it, a transcript, a sidecar and a Codex rollout
/// alike, and so is a sting-only file in a placed session's sidecar
/// directory that no landed file names; a fork is still replaced by the
/// landed file (A-106). `--check` passes; the next pass adopts them and
/// reads no landed byte after that.
#[test]
#[allow(clippy::too_many_lines)]
fn a111_archive_links_the_placement_keeps_are_regularized_after_it() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let archive = fs::canonicalize({
        let path = world.base.join("archive");
        fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let key = world.lab_key();
    let same = transcript(SID, CWD, "same", 5);
    world.land(SID, CWD, &same);
    archive_link(
        &world.dest_transcript(SID),
        &archive.join("same.jsonl"),
        &same,
    );
    let equal_rel = format!("{SID}/subagents/agent-eq.jsonl");
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{equal_rel}")),
        b"{\"eq\":1}\n",
    )
    .unwrap();
    let equal = world.dest(&format!("projects/{key}/{equal_rel}"));
    archive_link(&equal, &archive.join("agent-eq.jsonl"), b"{\"eq\":1}\n");
    let fork_rel = format!("{SID}/subagents/agent-fork.jsonl");
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{fork_rel}")),
        b"{\"neo\":1}\n{\"neo\":2}\n",
    )
    .unwrap();
    let fork = world.dest(&format!("projects/{key}/{fork_rel}"));
    archive_link(&fork, &archive.join("agent-fork.jsonl"), b"{\"sting\":1}\n");
    let own = world.dest(&format!(
        "projects/{key}/{SID}/subagents/agent-sting-only.jsonl"
    ));
    archive_link(
        &own,
        &archive.join("agent-sting-only.jsonl"),
        b"{\"own\":1}\n",
    );
    // The archive ahead of a landed sidecar: regularized as sting's.
    let ahead_rel = format!("{SID}/subagents/agent-ahead.jsonl");
    fs::write(
        world.landed(&format!("projects/-Users-jess-git-lab/{ahead_rel}")),
        b"{\"a\":1}\n",
    )
    .unwrap();
    let ahead = world.dest(&format!("projects/{key}/{ahead_rel}"));
    archive_link(
        &ahead,
        &archive.join("agent-ahead.jsonl"),
        b"{\"a\":1}\n{\"a\":2}\n",
    );
    // A Codex rollout equal to its archive link.
    let rollout = rollout_bytes(T1, "legacy", 3);
    world.land_thread(T1, "", &rollout, 1);
    drop(world.dest_index());
    let rollout_dest = world.codex_dest(&rel(T1));
    archive_link(&rollout_dest, &archive.join("codex/r.jsonl"), &rollout);
    let mut options = world.options();
    options.replace_archived = vec![archive.clone()];

    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.files_of("claude", "replaced-archived"),
        1,
        "the fork: {lines:?}"
    );
    assert_eq!(fs::read(&fork).unwrap(), b"{\"neo\":1}\n{\"neo\":2}\n");
    assert_eq!(
        fs::read(archive.join("agent-fork.jsonl")).unwrap(),
        b"{\"sting\":1}\n"
    );
    for (path, bytes) in [
        (world.dest_transcript(SID), same.clone()),
        (equal, b"{\"eq\":1}\n".to_vec()),
        (own.clone(), b"{\"own\":1}\n".to_vec()),
        (ahead.clone(), b"{\"a\":1}\n{\"a\":2}\n".to_vec()),
        (rollout_dest, rollout),
    ] {
        assert!(is_regular(&path), "{path:?}: {lines:?}");
        assert_eq!(fs::read(&path).unwrap(), bytes, "{path:?}");
        assert!(
            line_for(
                &lines,
                &path.to_string_lossy(),
                "\"outcome\":\"regularized\""
            )
            .is_some(),
            "{path:?}: {lines:?}"
        );
    }
    assert_eq!(
        fs::read(archive.join("same.jsonl")).unwrap(),
        same,
        "archive kept"
    );
    // The archive ahead of a landed sidecar is sting's, longer: the
    // check fails the session on it until it is waived.
    let (checked, lines) = check_lines(&options);
    assert!(checked.verdict().is_err(), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("claude:{SID}:companion-missing"))),
        "{lines:?}"
    );
    // The next pass adopts what is there (the ahead sidecar kept,
    // `dest-continued`), and waives the ahead one by path.
    let mut waiving = options.clone();
    waiving.waive_companions = vec![ahead];
    let (next, lines, _) = run(&waiving);
    assert_eq!(next.rows.get("waiver-waived"), Some(&1), "{lines:?}");
    assert_eq!(next.files_of("claude", "occupied"), 0, "{lines:?}");
    assert_eq!(next.files_of("codex", "occupied"), 0, "{lines:?}");
    let (checked, lines) = check_lines(&options);
    assert!(checked.verdict().is_ok(), "{lines:?}");
    let (_, lines, read) = run(&options);
    assert_eq!(read, 0, "S3: {lines:?}");
    // `--undo` puts the regularized links and the replaced fork back.
    let ((put_back, _), lines) = undo_lines(&options);
    assert!(put_back >= 4, "{lines:?}");
    assert!(is_link(&own) && is_link(&fork), "{lines:?}");
}

/// The legalab shape (A-20261018-111, "Keep sting's, waive it"): a
/// `file-history` snapshot sting already held with other bytes is kept
/// (`diverged`), and `--check` fails the session (`companion-missing`).
/// `--waive-companion` on a pass, spelled through a symlink to the live
/// `.claude`, records the waiver (`waived`, the ledger row binding both
/// files); `--check` then passes the lane and names the waived file; the
/// same flag again is `already-waived`. A path that is no kept companion
/// is `waive-refused` with nothing recorded; one outside the live
/// `.claude` refuses the run before anything is written. A destination
/// changed since fails the session (`companion-waiver-stale`); `--undo`
/// revokes the waiver.
#[test]
#[allow(clippy::too_many_lines)]
fn a111_a_kept_companion_is_waived_by_path() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let neo = transcript(SID, CWD, "s", 3);
    world.land(SID, CWD, &neo);
    let snapshot = format!("file-history/{SID}/cb44c51ba7602a0d@v2");
    fs::write(world.landed(&snapshot), vec![b'n'; 15_557]).unwrap();
    let kept = world.dest(&snapshot);
    fs::create_dir_all(kept.parent().unwrap()).unwrap();
    fs::write(&kept, vec![b's'; 3_414]).unwrap();
    let mut options = world.options();
    options.codex = false;
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.files_of("claude", "diverged"), 1, "{lines:?}");
    let (failing, lines) = check_lines(&options);
    assert!(failing.verdict().is_err());
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("claude:{SID}:companion-missing"))),
        "{lines:?}"
    );
    // Through a symlink to the live `.claude`, as `~/.claude` is on sting.
    let spelled = world.base.join("home-claude");
    std::os::unix::fs::symlink(world.dest(""), &spelled).unwrap();
    let mut waiving = options.clone();
    waiving.waive_companions = vec![
        spelled.join(&snapshot),
        world.dest_transcript(SID),
        world.dest(&format!("file-history/{SID}/absent@v9")),
    ];
    let (summary, lines, _) = run(&waiving);
    assert_eq!(summary.rows.get("waiver-waived"), Some(&1), "{lines:?}");
    assert_eq!(
        summary.rows.get("waiver-waive-refused"),
        Some(&2),
        "{lines:?}"
    );
    let waived = line_for(&lines, &kept.to_string_lossy(), "\"kind\":\"waiver\"").unwrap();
    assert!(
        waived.contains("\"outcome\":\"waived\"")
            && waived.contains("\"dest_size\":3414")
            && waived.contains("\"landed_size\":15557"),
        "{waived}"
    );
    assert!(
        line_for(&lines, "not-a-companion", "waive-refused").is_some(),
        "{lines:?}"
    );
    assert!(
        line_for(&lines, "no-placement-row", "waive-refused").is_some(),
        "{lines:?}"
    );
    assert_eq!(fs::read(&kept).unwrap(), vec![b's'; 3_414], "sting's kept");
    {
        let ledger = Ledger::open_read_only(&options.state).unwrap().unwrap();
        let rows = ledger.waivers().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].dest, kept);
        assert_eq!(rows[0].state, "active");
        assert_eq!(rows[0].dest_identity.size, 3_414);
        assert_eq!(rows[0].landed.size, 15_557);
    }
    let (passing, lines) = check_lines(&options);
    assert!(passing.verdict().is_ok(), "{lines:?}");
    assert_eq!(
        passing.companions_waived,
        vec![(SID.to_owned(), kept.clone())]
    );
    assert!(
        line_for(
            &lines,
            &kept.to_string_lossy(),
            "\"kind\":\"waived-companion\""
        )
        .is_some(),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"kind\":\"lane\"") && line.contains("\"waived\":1")),
        "{lines:?}"
    );
    let (summary, lines, _) = run(&waiving);
    assert_eq!(
        summary.rows.get("waiver-already-waived"),
        Some(&1),
        "{lines:?}"
    );
    // Outside the live `.claude`: refused before anything is written.
    let mut outside = options.clone();
    outside.waive_companions = vec![world.base.join("landing/.claude").join(&snapshot)];
    assert_eq!(
        reseat(&outside, &mut |_| Ok(())).unwrap_err(),
        BulkloadRefusal::FieldDomainViolation
    );
    // The destination changes: the waiver no longer holds, by name.
    append(&kept, b"more");
    let (stale, lines) = check_lines(&options);
    assert!(stale.verdict().is_err(), "{lines:?}");
    assert_eq!(stale.companion_waivers_stale, 1);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("claude:{SID}:companion-waiver-stale"))),
        "{lines:?}"
    );
    // `--undo` revokes it.
    fs::write(&kept, vec![b's'; 3_414]).unwrap();
    let (summary, lines, _) = run(&waiving);
    assert_eq!(summary.rows.get("waiver-rewaived"), Some(&1), "{lines:?}");
    assert!(check_lines(&options).0.verdict().is_ok());
    let (_, lines) = undo_lines(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"tool\":\"waiver\"") && line.contains("undone")),
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .any(|line| line.contains("\"kind\":\"undo-summary\"") && line.contains("\"waivers\":1")));
    let (revoked, lines) = check_lines(&options);
    assert!(revoked.verdict().is_err(), "{lines:?}");
    assert!(revoked.companions_waived.is_empty());
}

/// A version 5 ledger (no `waiver` table) is read as it is and migrated
/// in place by the next pass.
#[test]
fn a111_a_version_5_ledger_is_migrated() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(SID, CWD, &transcript(SID, CWD, "s", 2));
    let mut options = world.options();
    options.codex = false;
    run(&options);
    let path = options.state.join(FILE);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE waiver; PRAGMA user_version = 5;")
        .unwrap();
    drop(conn);
    let read_only = Ledger::open_read_only(&options.state).unwrap().unwrap();
    assert!(read_only.waivers().unwrap().is_empty());
    assert!(read_only.session("claude", SID).unwrap().is_some());
    drop(read_only);
    let (_, lines, read) = run(&options);
    assert_eq!(read, 0, "{lines:?}");
    let version: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    assert!(Ledger::open_read_only(&options.state)
        .unwrap()
        .unwrap()
        .waivers()
        .unwrap()
        .is_empty());
}

/// A-20261018-112: a lane whose working directory is absent on the
/// destination (and, by the operator's finding, on the source: a worktree
/// removed after its work) fails `--check` (`dest-cwd-absent`) though its
/// transcript is carried and listed under its key. `--waive-lane DIR
/// --reason source-absent` on a pass records the waiver (spelled through
/// the destination's `git` link, resolved as the lane is); `--check` then
/// passes the lane, names the waiver on its line and the summary, and
/// still checks its sessions. A directory that exists is `waive-refused`;
/// a waiver without a ruled reason refuses the run before anything is
/// written; `--undo` revokes it.
#[test]
fn a112_an_absent_lane_is_waived_with_its_reason() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let gone = "/Users/jess/git/gone.worktrees/removed-20261006";
    let bytes = transcript(SID, gone, "g", 3);
    world.land(SID, gone, &bytes);
    let mut options = world.options();
    options.codex = false;
    run(&options);
    let (failing, lines) = check_lines(&options);
    assert!(failing.verdict().is_err());
    assert!(
        lines.iter().any(|line| line.contains("dest-cwd-absent")),
        "{lines:?}"
    );
    let real = world.base.join("real-git/gone.worktrees/removed-20261006");
    let key = crate::reseat::key::project_key(real.to_str().unwrap());
    assert_eq!(
        fs::read(world.dest(&format!("projects/{key}/{SID}.jsonl"))).unwrap(),
        bytes,
        "carried and listed under the lane's key"
    );
    let mut no_reason = options.clone();
    no_reason.waive_lanes = vec![world.base.join("dest/git/gone.worktrees/removed-20261006")];
    assert_eq!(
        reseat(&no_reason, &mut |_| Ok(())).unwrap_err(),
        BulkloadRefusal::FieldDomainViolation
    );
    let mut wrong = no_reason.clone();
    wrong.lane_waiver_reason = Some("tired".to_owned());
    assert_eq!(
        reseat(&wrong, &mut |_| Ok(())).unwrap_err(),
        BulkloadRefusal::FieldDomainViolation
    );
    let mut waiving = no_reason;
    waiving.lane_waiver_reason = Some("source-absent".to_owned());
    waiving.waive_lanes.push(world.base.join("dest/git/lab"));
    let (summary, lines, _) = run(&waiving);
    assert_eq!(
        summary.rows.get("lane-waiver-waived"),
        Some(&1),
        "{lines:?}"
    );
    assert_eq!(
        summary.rows.get("lane-waiver-waive-refused"),
        Some(&1),
        "the lab lane exists: {lines:?}"
    );
    let (passing, lines) = check_lines(&options);
    assert!(passing.verdict().is_ok(), "{lines:?}");
    let lane = passing.lanes.get(&real).unwrap();
    assert_eq!(lane.lane_waiver.as_deref(), Some("source-absent"));
    assert_eq!((lane.sessions, lane.sessions_ok), (1, 1));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"lane_waiver\":\"source-absent\"")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("\"lanes_waived\":1")),
        "{lines:?}"
    );
    let (summary, _, _) = run(&waiving);
    assert_eq!(summary.rows.get("lane-waiver-already-waived"), Some(&1));
    // Its sessions are still judged: a transcript changed at the lane's
    // key fails the waived lane.
    let dest = world.dest(&format!("projects/{key}/{SID}.jsonl"));
    let original = fs::read(&dest).unwrap();
    fs::write(&dest, b"{\"other\":1}\n").unwrap();
    assert!(check_lines(&options).0.verdict().is_err());
    fs::write(&dest, &original).unwrap();
    let (_, lines) = undo_lines(&options);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"tool\":\"lane-waiver\"") && line.contains("undone")),
        "{lines:?}"
    );
    let (revoked, lines) = check_lines(&options);
    assert!(revoked.verdict().is_err(), "{lines:?}");
}
