//! `estate-reseat` (R0, TIN-6587) against synthetic transcripts and Codex
//! stores in throwaway homes. Every rule of `place` and the check is a row
//! here; the real-binary canary and the CLI are `tests/estate_reseat.rs`,
//! the crash rows `tests/fault_harness/reseat.rs`.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::check::check;
use super::key::project_key;
use super::{reseat, Options, Summary, Unmapped};
use crate::counters::{Counter, Counters};
use crate::BulkloadRefusal;

mod findings;

/// The landed-read counter is process-wide: the rows that measure it run
/// one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

const SOURCE_HOME: &str = "/Users/jess";
const SID: &str = "11111111-2222-4333-8444-555555555555";
const SID2: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";

/// A source transcript of `lines` lines recorded in `cwd`, with `slug`.
fn transcript(sid: &str, cwd: &str, slug: &str, lines: usize) -> Vec<u8> {
    let mut out = format!(
        "{{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\"sessionId\":\"{sid}\",\"content\":\"\\\"cwd\\\":\\\"/not/this\\\"\"}}\n"
    )
    .into_bytes();
    for index in 0..lines {
        out.extend_from_slice(
            format!(
                "{{\"parentUuid\":null,\"attachment\":{{\"cwd\":\"/nested\"}},\"type\":\"user\",\"cwd\":\"{cwd}\",\"sessionId\":\"{sid}\",\"slug\":\"{slug}\",\"message\":\"prompt {index}\"}}\n"
            )
            .as_bytes(),
        );
    }
    out
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn set_mtime(path: &Path, seconds_ago: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(seconds_ago))
        .unwrap();
}

/// A landing, a destination home whose `git` is a symlink (as on sting),
/// and a private state.
struct World {
    _temp: tempfile::TempDir,
    base: PathBuf,
}

impl World {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("bulkload-reseat-")
            .tempdir()
            .unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(base.join("real-git/lab")).unwrap();
        fs::create_dir_all(base.join("dest/.claude")).unwrap();
        // Both hosts keep transcripts ten years (investigation 1.4).
        fs::write(
            base.join("dest/.claude/settings.json"),
            b"{\n  \"cleanupPeriodDays\": 3650\n}\n",
        )
        .unwrap();
        fs::create_dir_all(base.join("dest/.codex")).unwrap();
        std::os::unix::fs::symlink(base.join("real-git"), base.join("dest/git")).unwrap();
        fs::create_dir_all(base.join("landing/.claude/projects")).unwrap();
        fs::create_dir_all(base.join("landing/.codex")).unwrap();
        Self { _temp: temp, base }
    }

    fn options(&self) -> Options {
        let mut options = Options::new(
            Path::new(SOURCE_HOME),
            &self.base.join("landing"),
            &self.base.join("dest"),
            &self.base.join("state"),
        );
        options.sessions_only = true;
        // Never the host's own managed settings.
        options.managed_settings = vec![self.base.join("managed-settings.json")];
        options
    }

    /// The landed transcript of `sid` recorded in `cwd`.
    fn land(&self, sid: &str, cwd: &str, bytes: &[u8]) -> PathBuf {
        let directory = self
            .base
            .join("landing/.claude/projects")
            .join(project_key(cwd));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{sid}.jsonl"));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn landed(&self, rel: &str) -> PathBuf {
        let path = self.base.join("landing/.claude").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    /// The lab lane's key at the destination: of the real path.
    fn lab_key(&self) -> String {
        project_key(self.base.join("real-git/lab").to_str().unwrap())
    }

    fn dest(&self, rel: &str) -> PathBuf {
        self.base.join("dest/.claude").join(rel)
    }

    fn dest_transcript(&self, sid: &str) -> PathBuf {
        self.dest(&format!("projects/{}/{sid}.jsonl", self.lab_key()))
    }
}

fn run(options: &Options) -> (Summary, Vec<String>, u64) {
    let before = Counters::snapshot();
    let mut lines = Vec::new();
    let summary = reseat(options, &mut |line| {
        lines.push(line.to_owned());
        Ok(())
    })
    .unwrap();
    let read = Counters::snapshot()
        .since(before)
        .get(Counter::ReseatLandingRead);
    (summary, lines, read)
}

fn r0(options: &Options) -> super::CheckSummary {
    check(options, &mut |_| Ok(())).unwrap()
}

/// The core expectation, failing first: today the landed sessions are where
/// no picker looks, so R0 fails; after a pass it holds, byte-identical,
/// as regular files under the key of the destination's real path, with the
/// source mtime, the sidecars, the per-session state and the plan.
#[test]
fn r0_fails_on_the_landing_and_holds_after_a_pass() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "quiet-seal", 40);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    set_mtime(&landed, 3_600);
    let sidecar = world.landed(&format!(
        "projects/-Users-jess-git-lab/{SID}/subagents/agent-a1.jsonl"
    ));
    fs::write(&sidecar, b"{\"isSidechain\":true}\n").unwrap();
    fs::write(
        world.landed(&format!(
            "projects/-Users-jess-git-lab/{SID}/tool-results/r1.txt"
        )),
        b"result",
    )
    .unwrap();
    fs::write(world.landed(&format!("file-history/{SID}/abc@v1")), b"v1").unwrap();
    fs::write(
        world.landed(&format!("todos/{SID}-agent-{SID}.json")),
        b"[]",
    )
    .unwrap();
    fs::write(world.landed(&format!("debug/{SID}.txt")), b"debug").unwrap();
    fs::write(world.landed("plans/quiet-seal.md"), b"# plan\n").unwrap();
    fs::write(world.landed("plans/other-slug.md"), b"# not ours\n").unwrap();
    fs::write(
        world.landed("projects/-Users-jess-git-lab/memory/MEMORY.md"),
        b"- note\n",
    )
    .unwrap();
    fs::create_dir_all(world.landed("sessions/x").parent().unwrap()).unwrap();
    fs::write(world.landed("sessions/4242.json"), b"{\"pid\":4242}").unwrap();
    let options = world.options();

    let before = r0(&options);
    assert_eq!(before.count("fail"), 1, "{before:?}");
    assert!(before.verdict().is_err());

    let (summary, lines, read) = run(&options);
    assert_eq!(summary.sessions_of("claude", "placed"), 1, "{lines:?}");
    assert!(read >= bytes.len() as u64);
    let dest = world.dest_transcript(SID);
    let meta = fs::symlink_metadata(&dest).unwrap();
    assert!(meta.is_file(), "a regular file, never a symlink");
    assert_ne!(
        meta.ino(),
        fs::metadata(&landed).unwrap().ino(),
        "never a hardlink"
    );
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert_eq!(
        meta.mtime(),
        fs::metadata(&landed).unwrap().mtime(),
        "--continue order"
    );
    let key = world.lab_key();
    assert_eq!(
        fs::read(world.dest(&format!("projects/{key}/{SID}/subagents/agent-a1.jsonl"))).unwrap(),
        b"{\"isSidechain\":true}\n"
    );
    assert!(world
        .dest(&format!("projects/{key}/{SID}/tool-results/r1.txt"))
        .is_file());
    assert!(world.dest(&format!("file-history/{SID}/abc@v1")).is_file());
    assert!(world
        .dest(&format!("todos/{SID}-agent-{SID}.json"))
        .is_file());
    assert!(world.dest(&format!("debug/{SID}.txt")).is_file());
    assert_eq!(
        fs::read(world.dest("plans/quiet-seal.md")).unwrap(),
        b"# plan\n"
    );
    assert!(
        !world.dest("plans/other-slug.md").exists(),
        "only the session's plans"
    );
    assert!(world
        .dest(&format!("projects/{key}/memory/MEMORY.md"))
        .is_file());
    assert!(
        !world.dest("sessions").exists(),
        "the live-process registry is host-local"
    );
    let session = lines
        .iter()
        .find(|line| line.contains("\"kind\":\"session\""))
        .unwrap();
    assert!(session.contains("\"outcome\":\"placed\""), "{session}");
    assert!(
        session.contains(&format!("\"dest_key\":\"{key}\"")),
        "{session}"
    );

    let after = r0(&options);
    assert_eq!(after.count("pass"), 1, "{after:?}");
    assert!(after.verdict().is_ok());
}

/// S3: an unchanged rerun reads 0 landed bytes and writes nothing.
#[test]
fn an_unchanged_rerun_reads_zero_landed_bytes() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 30),
    );
    fs::write(
        world.landed(&format!(
            "projects/-Users-jess-git-lab/{SID}/subagents/a.jsonl"
        )),
        b"x\n",
    )
    .unwrap();
    fs::write(world.landed("plans/s.md"), b"plan").unwrap();
    let options = world.options();
    run(&options);
    let stamp = fs::symlink_metadata(world.dest_transcript(SID)).unwrap();
    let (summary, _, read) = run(&options);
    assert_eq!(read, 0, "R25/S3: nothing landed is read again");
    assert_eq!(summary.sessions_of("claude", "unchanged"), 1);
    assert_eq!(summary.files_of("claude", "unchanged"), 3);
    let again = fs::symlink_metadata(world.dest_transcript(SID)).unwrap();
    assert_eq!(
        (again.ino(), again.mtime_nsec(), again.ctime_nsec()),
        (stamp.ino(), stamp.mtime_nsec(), stamp.ctime_nsec())
    );
}

/// A live lane: the landing grows between passes; the next pass proves
/// every placed byte against the landing (its BLAKE3, review 1 finding 8),
/// appends the new tail only, and leaves the destination byte-identical.
/// It reads the landing once, never the source (R25).
#[test]
fn a_grown_transcript_is_extended_by_its_tail() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 3_000);
    let landed = world.land(SID, "/Users/jess/git/lab", &bytes);
    let options = world.options();
    run(&options);
    let tail = b"{\"type\":\"assistant\",\"cwd\":\"/Users/jess/git/lab\"}\n".repeat(50);
    append(&landed, &tail);
    let inode = fs::metadata(world.dest_transcript(SID)).unwrap().ino();
    let (summary, _, read) = run(&options);
    assert_eq!(summary.sessions_of("claude", "extended"), 1);
    assert_eq!(
        fs::read(world.dest_transcript(SID)).unwrap(),
        fs::read(&landed).unwrap()
    );
    assert_eq!(
        fs::metadata(world.dest_transcript(SID)).unwrap().ino(),
        inode,
        "appended in place"
    );
    assert_eq!(
        read,
        (bytes.len() + tail.len()) as u64,
        "the landing read once: the placed bytes proven, the tail appended"
    );
    assert!(r0(&options).verdict().is_ok());
}

/// A copy already at the destination: equal is adopted, a byte prefix is
/// extended, anything else is kept and reported, never overwritten.
#[test]
fn existing_copies_are_adopted_extended_or_kept() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let bytes = transcript(SID, "/Users/jess/git/lab", "s", 20);
    world.land(SID, "/Users/jess/git/lab", &bytes);
    let other = transcript(SID2, "/Users/jess/git/lab", "t", 20);
    world.land(SID2, "/Users/jess/git/lab", &other);
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, bytes.get(..bytes.len() / 2).unwrap()).unwrap();
    fs::write(world.landed("plans/s.md"), b"plan s").unwrap();
    fs::write(world.landed("plans/u.md"), b"plan u").unwrap();
    let divergent = b"{\"sessionId\":\"rewritten on the destination\"}\n".to_vec();
    fs::write(world.dest_transcript(SID2), &divergent).unwrap();
    let options = world.options();
    let (summary, lines, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "extended"), 1, "{lines:?}");
    assert_eq!(summary.sessions_of("claude", "diverged"), 1, "{lines:?}");
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert_eq!(
        fs::read(world.dest_transcript(SID2)).unwrap(),
        divergent,
        "kept as it is"
    );
    assert!(lines
        .iter()
        .any(|line| line.contains("RESEAT_DEST_DIVERGED")));
    let (summary, _, read) = run(&options);
    assert_eq!(
        read, 0,
        "a kept copy is not read again while nothing changes"
    );
    assert_eq!(summary.sessions_of("claude", "diverged"), 1);
    assert_eq!(summary.sessions_of("claude", "unchanged"), 1);
    // A third session already identical at the destination is adopted.
    let third = "77777777-7777-4777-8777-777777777777";
    let same = transcript(third, "/Users/jess/git/lab", "u", 5);
    world.land(third, "/Users/jess/git/lab", &same);
    fs::write(world.dest_transcript(third), &same).unwrap();
    let (summary, _, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "adopted"), 1);
    assert_eq!(
        fs::read(world.dest("plans/s.md")).unwrap(),
        b"plan s",
        "slug read from the proven prefix"
    );
    assert_eq!(
        fs::read(world.dest("plans/u.md")).unwrap(),
        b"plan u",
        "slug read while adopting"
    );
    let checked = r0(&options);
    assert_eq!(
        checked.count("fail"),
        1,
        "the divergent session fails R0: {checked:?}"
    );
}

/// A destination resume appends to the placed transcript: from then on the
/// destination owns it. A later landed tail is never appended after it.
#[test]
fn a_destination_resume_hands_the_session_over() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let landed = world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 10),
    );
    let options = world.options();
    run(&options);
    let dest = world.dest_transcript(SID);
    append(&dest, b"{\"type\":\"permission-mode\"}\n");
    let resumed = fs::read(&dest).unwrap();
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.sessions_of("claude", "dest-continued"),
        1,
        "{lines:?}"
    );
    append(
        &landed,
        b"{\"type\":\"user\",\"message\":\"neo kept going\"}\n",
    );
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.sessions_of("claude", "both-continued"),
        1,
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .any(|line| line.contains("RESEAT_DEST_CONTINUED")));
    assert_eq!(fs::read(&dest).unwrap(), resumed, "never extended again");
}

/// A symlink at the transcript's place, or a symlinked key directory, is
/// never written through.
#[test]
fn symlinks_at_the_destination_are_occupied() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 3),
    );
    world.land(
        SID2,
        "/Users/jess/git/other",
        &transcript(SID2, "/Users/jess/git/other", "t", 3),
    );
    fs::create_dir_all(world.base.join("real-git/other")).unwrap();
    let dest = world.dest_transcript(SID);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(world.base.join("archived.jsonl"), b"archive").unwrap();
    std::os::unix::fs::symlink(world.base.join("archived.jsonl"), &dest).unwrap();
    let other_key = project_key(world.base.join("real-git/other").to_str().unwrap());
    fs::create_dir_all(world.base.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(
        world.base.join("elsewhere"),
        world.dest(&format!("projects/{other_key}")),
    )
    .unwrap();
    let (summary, lines, _) = run(&world.options());
    assert_eq!(summary.sessions_of("claude", "occupied"), 2, "{lines:?}");
    assert_eq!(
        fs::read(world.base.join("archived.jsonl")).unwrap(),
        b"archive"
    );
    assert_eq!(
        fs::read_dir(world.base.join("elsewhere")).unwrap().count(),
        0
    );
    assert!(lines.iter().all(
        |line| !line.contains("\"kind\":\"session\"") || line.contains("DESTINATION_OCCUPIED")
    ));
}

/// Retention: the destination's `cleanupPeriodDays` (default 30) would
/// delete a transcript that keeps its 40-day-old source mtime; nothing is
/// written. With the hosts' 3650 it is placed.
#[test]
fn retention_is_refused_before_anything_is_written() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    let landed = world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "s", 3),
    );
    set_mtime(&landed, 40 * 86_400);
    fs::remove_file(world.dest("settings.json")).unwrap();
    let options = world.options();
    let refused = reseat(&options, &mut |_| Ok(()));
    assert_eq!(refused.err(), Some(BulkloadRefusal::ReseatRetentionUnsafe));
    assert!(!world.dest("projects").exists());
    assert!(
        !world.base.join("state").exists(),
        "refused before the ledger"
    );
    assert!(!r0(&options).retention_ok);
    fs::write(
        world.dest("settings.json"),
        b"{\n  \"cleanupPeriodDays\": 3650,\n  \"x\": 1\n}\n",
    )
    .unwrap();
    let (summary, _, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "placed"), 1);
    assert!(r0(&options).verdict().is_ok());
}

/// Lanes: `--map` wins over the default; a cwd with no lane is reported
/// (or kept by ID); a transcript whose cwd does not key to its directory
/// is unresolved; a long destination path gets the hashed key.
#[test]
fn lanes_map_report_and_hash() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/private/tmp/scratch",
        &transcript(SID, "/private/tmp/scratch", "s", 2),
    );
    let path =
        world.landed("projects/-Users-jess-git-wrong/88888888-8888-4888-8888-888888888888.jsonl");
    fs::write(
        &path,
        transcript(
            "88888888-8888-4888-8888-888888888888",
            "/Users/jess/git/lab",
            "s",
            2,
        ),
    )
    .unwrap();
    let long = format!("/Users/jess/git/deep{}", "/abcdefghij".repeat(20));
    world.land(SID2, &long, &transcript(SID2, &long, "t", 2));
    let mapped = "99999999-9999-4999-8999-999999999999";
    world.land(
        mapped,
        "/Users/jess/Documents/notes",
        &transcript(mapped, "/Users/jess/Documents/notes", "n", 2),
    );
    fs::create_dir_all(world.base.join("notes")).unwrap();
    let mut options = world.options();
    options.maps = vec![(PathBuf::from("/Users/jess/Documents"), world.base.clone())];
    let (summary, lines, _) = run(&options);
    assert_eq!(
        summary.sessions_of("claude", "RESEAT_LANE_UNMAPPED"),
        1,
        "{lines:?}"
    );
    assert_eq!(
        summary.sessions_of("claude", "RESEAT_LANE_UNRESOLVED"),
        1,
        "{lines:?}"
    );
    assert_eq!(summary.sessions_of("claude", "placed"), 2, "{lines:?}");
    let notes_key = project_key(world.base.join("notes").to_str().unwrap());
    assert!(world
        .dest(&format!("projects/{notes_key}/{mapped}.jsonl"))
        .is_file());
    let deep = world
        .base
        .join("real-git/deep")
        .join("abcdefghij/".repeat(20).trim_end_matches('/'));
    let deep_key = project_key(deep.to_str().unwrap());
    assert!(deep_key.len() > 200);
    assert!(
        world
            .dest(&format!("projects/{deep_key}/{SID2}.jsonl"))
            .is_file(),
        "{lines:?}"
    );
    options.unmapped = Unmapped::ById;
    let (summary, _, _) = run(&options);
    assert_eq!(summary.sessions_of("claude", "placed"), 1);
    assert!(world
        .dest(&format!("projects/-private-tmp-scratch/{SID}.jsonl"))
        .is_file());
}

/// A plan that changes at the landing supersedes this ledger's placement;
/// a plan the destination already holds differently is kept.
#[test]
fn plans_supersede_their_placement_and_keep_a_collision() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new();
    world.land(
        SID,
        "/Users/jess/git/lab",
        &transcript(SID, "/Users/jess/git/lab", "own-plan", 2),
    );
    world.land(
        SID2,
        "/Users/jess/git/lab",
        &transcript(SID2, "/Users/jess/git/lab", "taken", 2),
    );
    fs::write(world.landed("plans/own-plan.md"), b"v1").unwrap();
    fs::write(world.landed("plans/taken.md"), b"neo's").unwrap();
    fs::create_dir_all(world.dest("plans")).unwrap();
    fs::write(world.dest("plans/taken.md"), b"sting's").unwrap();
    let options = world.options();
    let (summary, _, _) = run(&options);
    assert_eq!(summary.files_of("claude", "diverged"), 1);
    fs::write(world.landed("plans/own-plan.md"), b"v2, longer").unwrap();
    let (summary, _, _) = run(&options);
    assert_eq!(summary.files_of("claude", "superseded"), 1);
    assert_eq!(
        fs::read(world.dest("plans/own-plan.md")).unwrap(),
        b"v2, longer"
    );
    assert_eq!(fs::read(world.dest("plans/taken.md")).unwrap(), b"sting's");
    let leftovers: Vec<_> = fs::read_dir(world.dest("plans"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 2, "no temporary left: {leftovers:?}");
    // A destination edit of the plan is never superseded.
    fs::write(world.dest("plans/own-plan.md"), b"edited on sting").unwrap();
    fs::write(world.landed("plans/own-plan.md"), b"v3").unwrap();
    run(&options);
    assert_eq!(
        fs::read(world.dest("plans/own-plan.md")).unwrap(),
        b"edited on sting"
    );
}

/// The hand-off note and the Git class decide a lane's verdict.
#[test]
fn verdicts_need_git_and_the_hand_off() {
    let options = {
        let mut options = Options::new(
            Path::new("/s"),
            Path::new("/l"),
            Path::new("/d"),
            Path::new("/x"),
        );
        options.handoff_marker = Some("bulkload-handoff".to_owned());
        options
    };
    let temp = tempfile::tempdir().unwrap();
    let cwd = fs::canonicalize(temp.path()).unwrap();
    let mut lane = super::check::LaneCheck {
        exists: true,
        sessions: 1,
        sessions_ok: 1,
        ..super::check::LaneCheck::default()
    };
    let mut classes = std::collections::BTreeMap::new();
    super::check::judge(&options, Some(&classes), &cwd, &mut lane).unwrap();
    assert_eq!(
        (lane.git.as_str(), lane.handoff.as_str(), lane.verdict),
        ("unplanned", "absent", "fail")
    );
    fs::write(
        cwd.join("AGENTS.md"),
        b"<!-- bulkload-handoff -->\n# repo\n",
    )
    .unwrap();
    classes.insert(cwd.clone(), "identical".to_owned());
    super::check::judge(&options, Some(&classes), &cwd.join("sub"), &mut lane).unwrap();
    assert_eq!(lane.verdict, "fail", "a missing note in the subdirectory");
    super::check::judge(&options, Some(&classes), &cwd, &mut lane).unwrap();
    assert_eq!(
        (lane.git.as_str(), lane.handoff.as_str(), lane.verdict),
        ("identical", "present", "pass")
    );
    classes.insert(cwd.clone(), "dest-ahead".to_owned());
    super::check::judge(&options, Some(&classes), &cwd, &mut lane).unwrap();
    assert_eq!(lane.verdict, "fail");
    super::check::judge(&options, None, &cwd, &mut lane).unwrap();
    assert_eq!(lane.verdict, "incomplete");
}

mod codex {
    use super::*;
    use rusqlite::{params, Connection};

    const THREADS: &str = "CREATE TABLE threads (
        id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL, source TEXT NOT NULL, cwd TEXT NOT NULL,
        title TEXT NOT NULL, preview TEXT NOT NULL DEFAULT '', project_id TEXT,
        archived INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL,
        child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);
        CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL, position INTEGER NOT NULL,
        name TEXT NOT NULL, PRIMARY KEY(thread_id, position));";

    const T1: &str = "019a0000-0000-7000-8000-000000000001";
    const T2: &str = "019a0000-0000-7000-8000-000000000002";
    const T3: &str = "019a0000-0000-7000-8000-000000000003";

    fn rollout(world: &World, id: &str, suffix: &str, bytes: &[u8]) -> String {
        let rel = format!("sessions/2026/10/16/rollout-2026-10-16T10-00-00-{id}.jsonl{suffix}");
        let path = world.base.join("landing/.codex").join(&rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        format!("{SOURCE_HOME}/.codex/{rel}")
    }

    fn landed_db(world: &World) -> Connection {
        let conn = Connection::open(world.base.join("landing/.codex/state_5.sqlite")).unwrap();
        conn.execute_batch(THREADS).unwrap();
        conn
    }

    fn dest_db(world: &World, extra: &str) -> Connection {
        let conn = Connection::open(world.base.join("dest/.codex/state_5.sqlite")).unwrap();
        conn.execute_batch(THREADS).unwrap();
        conn.execute_batch(extra).unwrap();
        conn
    }

    fn insert(conn: &Connection, id: &str, rollout: &str, cwd: &str, created: i64) {
        conn.execute(
            "INSERT INTO threads(id, rollout_path, created_at, updated_at, source, cwd, title, preview, project_id)
             VALUES (?1, ?2, ?3, ?3, 'cli', ?4, 'title', 'hello', 'neo-project')",
            params![id, rollout, created, cwd],
        )
        .unwrap();
    }

    /// Threads are placed byte-identical and registered with the
    /// destination's real cwd and rollout path, insert only; a row already
    /// there under another cwd is reported, never updated; a `.gz` stays in
    /// the landing; the rerun reads nothing.
    #[test]
    fn rows_are_registered_with_the_destination_cwd() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let world = World::new();
        let source = landed_db(&world);
        let r1 = rollout(
            &world,
            T1,
            "",
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\"}}\n",
        );
        let r2 = rollout(&world, T2, "", b"{\"type\":\"session_meta\"}\n");
        let r3 = rollout(&world, T3, ".gz", b"\x1f\x8b");
        insert(&source, T1, &r1, "/Users/jess/git/lab", 1);
        insert(&source, T2, &r2, "/Users/jess/git/lab", 2);
        insert(
            &source,
            T3,
            r3.trim_end_matches(".gz"),
            "/Users/jess/git/lab",
            3,
        );
        source
            .execute(
                "INSERT INTO thread_spawn_edges VALUES (?1, ?2, 'open')",
                params![T1, T2],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO thread_dynamic_tools VALUES (?1, 0, 'tool')",
                params![T1],
            )
            .unwrap();
        drop(source);
        let dest = dest_db(
            &world,
            "ALTER TABLE threads ADD COLUMN thread_section_id TEXT;",
        );
        insert(&dest, T2, "/somewhere/else.jsonl", "/home/jess/git/lab", 2);
        drop(dest);
        let options = world.options();

        assert_eq!(r0(&options).count("fail"), 1);
        let (summary, lines, _) = run(&options);
        assert_eq!(summary.rows.get("registered"), Some(&1), "{lines:?}");
        assert_eq!(summary.rows.get("conflict"), Some(&1), "{lines:?}");
        assert_eq!(
            summary.sessions_of("codex", "unreadable-form"),
            1,
            "{lines:?}"
        );
        let real_lab = world.base.join("real-git/lab");
        let dest = Connection::open(world.base.join("dest/.codex/state_5.sqlite")).unwrap();
        let (cwd, path, project, title): (String, String, Option<String>, String) = dest
            .query_row(
                "SELECT cwd, rollout_path, project_id, title FROM threads WHERE id = ?1",
                params![T1],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(cwd, real_lab.to_str().unwrap());
        let dest_rollout = world
            .base
            .join("dest/.codex")
            .join(r1.strip_prefix("/Users/jess/.codex/").unwrap());
        assert_eq!(path, dest_rollout.to_str().unwrap());
        assert_eq!(project, None, "a source-local link is cleared");
        assert_eq!(title, "title");
        assert_eq!(
            fs::read(&dest_rollout).unwrap(),
            fs::read(
                world
                    .base
                    .join("landing/.codex")
                    .join(r1.strip_prefix("/Users/jess/.codex/").unwrap())
            )
            .unwrap()
        );
        let tools: i64 = dest
            .query_row("SELECT count(*) FROM thread_dynamic_tools", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(tools, 1);
        let (conflict_cwd,): (String,) = dest
            .query_row(
                "SELECT cwd FROM threads WHERE id = ?1",
                params![T2],
                |row| Ok((row.get(0)?,)),
            )
            .unwrap();
        assert_eq!(conflict_cwd, "/home/jess/git/lab", "never updated");
        assert!(lines
            .iter()
            .any(|line| line.contains("RESEAT_INDEX_CONFLICT")));
        drop(dest);

        let (summary, _, read) = run(&options);
        assert_eq!(read, 0);
        assert_eq!(summary.rows.get("unchanged"), Some(&1));
        let checked = r0(&options);
        let lane = checked.lanes.values().next().unwrap();
        assert_eq!((lane.threads, lane.threads_ok), (3, 1), "{lane:?}");
    }

    /// A destination column the snapshot cannot fill refuses the Codex half
    /// as a schema mismatch.
    #[test]
    fn a_required_destination_column_refuses() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let world = World::new();
        let source = landed_db(&world);
        let r1 = rollout(&world, T1, "", b"{}\n");
        insert(&source, T1, &r1, "/Users/jess/git/lab", 1);
        drop(source);
        drop(dest_db(
            &world,
            "ALTER TABLE threads ADD COLUMN must TEXT NOT NULL DEFAULT 'x'; CREATE TABLE z(a);",
        ));
        assert!(reseat(&world.options(), &mut |_| Ok(())).is_ok());
        let world = World::new();
        let source = landed_db(&world);
        let r1 = rollout(&world, T1, "", b"{}\n");
        insert(&source, T1, &r1, "/Users/jess/git/lab", 1);
        drop(source);
        let conn = Connection::open(world.base.join("dest/.codex/state_5.sqlite")).unwrap();
        conn.execute_batch(&THREADS.replace(
            "archived INTEGER NOT NULL DEFAULT 0",
            "archived INTEGER NOT NULL DEFAULT 0, required TEXT NOT NULL",
        ))
        .unwrap();
        drop(conn);
        assert_eq!(
            reseat(&world.options(), &mut |_| Ok(())).err(),
            Some(BulkloadRefusal::SchemaMismatch)
        );
    }
}
