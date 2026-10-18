//! Re-seat crash rows (R0, TIN-6587): the `reseat.*` fault points.
//!
//! A landing holds one Claude session of `~/git/lab` (a transcript, a
//! subagent transcript, a plan) and one Codex thread; the destination home
//! has a `git` symlink (sting's shape) and a Codex index. A child (this test
//! binary, re-run) re-seats with one point armed and ends itself there with
//! `_exit`; nothing here sends it a signal (R-N11). The parent asserts:
//!
//! - **never overwritten**: at the crash, the destination transcript is
//!   absent or a byte prefix of the landed one, and a plan is the old or
//!   the new placement, never anything else;
//! - **settles forward**: the rerun finishes with no session handed over,
//!   diverged or rewritten, and every destination file equals its landed
//!   one, no temporary left, one Codex row with the destination cwd, and the
//!   R0 check passing;
//! - **S3 after**: one more rerun reads 0 landed bytes.
//!
//! `Fresh` crashes the first pass; `Grow` the pass after the transcript
//! grew by 1.5 MiB (so the tail is written in two chunks); `Plan` the pass
//! after the plan changed. `Q198` crashes a first pass over the operator's
//! 2026-10-16 rulings' mutation points (OI-1003-Q198): a symlinked
//! destination transcript regularized, an existing Codex row spelled
//! through the `git` symlink re-spelled, a row built from a rowless
//! rollout's header; `Undo` crashes the `--undo` that reverses the first
//! two after a full pass. `Archived` crashes a first pass with
//! `--replace-archived-dest` over a destination transcript that is a
//! symlink into the replace root holding a shorter fork (A-106), at its
//! intent, after its exchange and before it is `done`; `ArchivedUndo` the
//! `--undo` that puts that link back. `_exit` models a process crash only
//! (see the harness header).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};
use bulkload_agent::reseat::{self, Options};

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the world it re-seats.
const CHILD_ENV: &str = "BULKLOAD_W7_RESEAT_CHILD_BASE";
/// Set only in a child that runs `--undo` instead of a pass.
const CHILD_UNDO_ENV: &str = "BULKLOAD_W7_RESEAT_CHILD_UNDO";
/// The OI-1003-Q198 world's threads: one the destination already holds,
/// spelled through the `git` symlink, and one with no snapshot row.
const SPELLED: &str = "019a0000-0000-7000-8000-00000000c0c2";
const ROWLESS: &str = "019a0000-0000-7000-8000-00000000c0c3";
/// The OI-1003-Q198 world's own (sting's) symlinked session.
const OWN: &str = "c0c0c0c0-9999-4999-8999-999999999999";
const SID: &str = "c0c0c0c0-1111-4222-8333-444444444444";
const THREAD: &str = "019a0000-0000-7000-8000-00000000c0c0";
const ROLLOUT: &str =
    "sessions/2026/10/16/rollout-2026-10-16T10-00-00-019a0000-0000-7000-8000-00000000c0c0.jsonl";

/// The landed-read counter is process-wide.
static SERIAL: Mutex<()> = Mutex::new(());

/// Which pass is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reseat {
    /// The first pass.
    Fresh,
    /// The pass after the transcript grew.
    Grow,
    /// The pass after the plan changed.
    Plan,
    /// The first pass, with the rollout landed as `.jsonl.zst` (placed
    /// decompressed, review 0 finding 1).
    Zst,
    /// The first pass over the OI-1003-Q198 world.
    Q198,
    /// The `--undo` after a full pass over the OI-1003-Q198 world.
    Undo,
    /// The first pass with the destination transcript a link into the
    /// `--replace-archived-dest` root (A-106).
    Archived,
    /// The `--undo` after a full pass over the `Archived` world.
    ArchivedUndo,
}

impl Scenario for Reseat {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth, self);
    }
}

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn line(index: usize) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/Users/jess/git/lab\",\"sessionId\":\"{SID}\",\"slug\":\"crash-plan\",\"message\":\"prompt {index:06} {}\"}}\n",
        "x".repeat(96)
    )
}

impl World {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-reseat-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let world = Self {
            base: fs::canonicalize(&base).unwrap(),
        };
        let base = &world.base;
        fs::create_dir_all(base.join("real-git/lab")).unwrap();
        fs::create_dir_all(base.join("dest/.claude")).unwrap();
        fs::write(
            base.join("dest/.claude/settings.json"),
            b"{\"cleanupPeriodDays\": 3650}",
        )
        .unwrap();
        fs::create_dir_all(base.join("dest/.codex")).unwrap();
        std::os::unix::fs::symlink(base.join("real-git"), base.join("dest/git")).unwrap();
        let project = base.join("landing/.claude/projects/-Users-jess-git-lab");
        fs::create_dir_all(project.join(SID).join("subagents")).unwrap();
        let transcript: String = (0..2_000).map(line).collect();
        fs::write(project.join(format!("{SID}.jsonl")), transcript).unwrap();
        fs::write(
            project.join(SID).join("subagents/agent-a.jsonl"),
            b"{\"isSidechain\":true}\n",
        )
        .unwrap();
        fs::create_dir_all(base.join("landing/.claude/plans")).unwrap();
        fs::write(
            base.join("landing/.claude/plans/crash-plan.md"),
            b"# plan v1\n",
        )
        .unwrap();
        let rollout = base.join("landing/.codex").join(ROLLOUT);
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(
            &rollout,
            b"{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/Users/jess/git/lab\"}}\n",
        )
        .unwrap();
        let schema = "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, source TEXT NOT NULL,
            cwd TEXT NOT NULL, title TEXT NOT NULL);";
        let landed =
            rusqlite::Connection::open(base.join("landing/.codex/state_5.sqlite")).unwrap();
        landed.execute_batch(schema).unwrap();
        landed
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, 1, 1, 'cli', '/Users/jess/git/lab', 't')",
                rusqlite::params![THREAD, format!("/Users/jess/.codex/{ROLLOUT}")],
            )
            .unwrap();
        drop(landed);
        rusqlite::Connection::open(base.join("dest/.codex/state_5.sqlite"))
            .unwrap()
            .execute_batch(schema)
            .unwrap();
        world
    }

    fn options(base: &Path) -> Options {
        let mut options = Options::new(
            Path::new("/Users/jess"),
            &base.join("landing"),
            &base.join("dest"),
            &base.join("state"),
        );
        options.sessions_only = true;
        options.managed_settings = vec![base.join("managed-settings.json")];
        let archive = base.join("replace-archive");
        if archive.is_dir() {
            options.replace_archived = vec![fs::canonicalize(archive).unwrap()];
        }
        options
    }

    fn reseat(&self) -> reseat::Summary {
        reseat::reseat(&Self::options(&self.base), &mut |_| Ok(())).unwrap()
    }

    fn landed_transcript(&self) -> PathBuf {
        self.base
            .join("landing/.claude/projects/-Users-jess-git-lab")
            .join(format!("{SID}.jsonl"))
    }

    fn key(&self) -> String {
        self.base
            .join("real-git/lab")
            .to_str()
            .unwrap()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }

    /// Every landed file and where it is re-seated.
    fn pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        let key = self.key();
        let (landing, dest) = (self.base.join("landing"), self.base.join("dest"));
        vec![
            (
                self.landed_transcript(),
                dest.join(format!(".claude/projects/{key}/{SID}.jsonl")),
            ),
            (
                landing.join(format!(
                    ".claude/projects/-Users-jess-git-lab/{SID}/subagents/agent-a.jsonl"
                )),
                dest.join(format!(
                    ".claude/projects/{key}/{SID}/subagents/agent-a.jsonl"
                )),
            ),
            (
                landing.join(".claude/plans/crash-plan.md"),
                dest.join(".claude/plans/crash-plan.md"),
            ),
            (
                landing.join(".codex").join(ROLLOUT),
                dest.join(".codex").join(ROLLOUT),
            ),
        ]
    }

    fn temporaries(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![self.base.join("dest")];
        while let Some(at) = pending.pop() {
            for entry in fs::read_dir(&at).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(path);
                } else if path.to_string_lossy().ends_with(".bulkload-reseat") {
                    found.push(path);
                }
            }
        }
        found
    }
}

impl World {
    fn rollout_rel(thread: &str) -> String {
        format!("sessions/2026/10/16/rollout-2026-10-16T10-00-00-{thread}.jsonl")
    }

    fn real_lab(&self) -> String {
        self.base
            .join("real-git/lab")
            .to_string_lossy()
            .into_owned()
    }

    fn spelled_lab(&self) -> String {
        self.base
            .join("dest/git/lab")
            .to_string_lossy()
            .into_owned()
    }

    fn own_link(&self) -> PathBuf {
        self.base
            .join("dest/.claude/projects")
            .join(self.key())
            .join(format!("{OWN}.jsonl"))
    }

    fn own_target(&self) -> PathBuf {
        self.base
            .join("archive/claude")
            .join(format!("{OWN}.jsonl"))
    }

    fn own_bytes() -> Vec<u8> {
        (0..40).map(line).collect::<String>().into_bytes()
    }

    /// The OI-1003-Q198 additions: a symlinked destination transcript
    /// (sting's own), a destination row spelled through `dest/git`
    /// naming the destination's own rollout, and a rowless rollout.
    fn q198(&self) {
        let target = self.own_target();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, Self::own_bytes()).unwrap();
        fs::create_dir_all(self.own_link().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, self.own_link()).unwrap();
        let landed = self.base.join("landing/.codex");
        for thread in [SPELLED, ROWLESS] {
            let path = landed.join(Self::rollout_rel(thread));
            fs::write(
                &path,
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\",\"cwd\":\"/Users/jess/git/lab\"}}}}\n"
                ),
            )
            .unwrap();
        }
        rusqlite::Connection::open(landed.join("state_5.sqlite"))
            .unwrap()
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, 2, 2, 'cli', '/Users/jess/git/lab', 't')",
                rusqlite::params![
                    SPELLED,
                    format!("/Users/jess/.codex/{}", Self::rollout_rel(SPELLED))
                ],
            )
            .unwrap();
        rusqlite::Connection::open(self.base.join("dest/.codex/state_5.sqlite"))
            .unwrap()
            .execute(
                "INSERT INTO threads VALUES (?1, '/srv/archive/own.jsonl', 2, 2, 'cli', ?2, 'sting')",
                rusqlite::params![SPELLED, self.spelled_lab()],
            )
            .unwrap();
    }

    /// A-106: the destination transcript a symlink into the replace root,
    /// its target a fork holding the landed transcript's first lines.
    fn archived(&self) {
        let target = self.fork_target();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, Self::fork_bytes()).unwrap();
        let link = &self.pairs()[0].1;
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, link).unwrap();
    }

    fn fork_target(&self) -> PathBuf {
        self.base.join("replace-archive/fork.jsonl")
    }

    fn fork_bytes() -> Vec<u8> {
        (0..3).map(line).collect::<String>().into_bytes()
    }

    fn row(&self, id: &str) -> Option<(String, String, String)> {
        rusqlite::Connection::open(self.base.join("dest/.codex/state_5.sqlite"))
            .unwrap()
            .query_row(
                "SELECT cwd, rollout_path, title FROM threads WHERE id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    }

    fn undo(&self) -> (u64, u64) {
        reseat::undo(&Self::options(&self.base), &mut |_| Ok(())).unwrap()
    }
}

/// `path` with `.zst` appended.
fn compressed(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".zst");
    PathBuf::from(name)
}

/// A landed file's content: as is, or decompressed from its `.zst` form.
fn landed_bytes(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok().or_else(|| {
        fs::read(compressed(path))
            .ok()
            .map(|bytes| zstd::decode_all(bytes.as_slice()).unwrap())
    })
}

/// Run this test binary as a child that re-seats (or, with `undo`, runs
/// `--undo`) with `fault` armed: it must end itself at that point.
fn crash_mode(world: &World, fault: &str, undo: bool) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "reseat::reseat_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &world.base)
        .env(CHILD_UNDO_ENV, if undo { "1" } else { "0" })
        .env(FAULT_ENV, fault)
        .env_remove(super::CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A detached waiter reaps the child; on timeout the test fails and the
    // child is left alone, never signalled (R-N11).
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let child = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!("{fault}: reseat child still running ({timeout}); left running, not signalled")
        })
        .unwrap();
    let stderr = String::from_utf8_lossy(&child.stderr).into_owned();
    assert_ne!(
        child.status.code(),
        Some(CHILD_WATCHDOG_EXIT_CODE),
        "{fault}: hung; the watchdog ended it after {CHILD_WATCHDOG:?} ({stderr})"
    );
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{fault}: the child must stop at its fault point ({:?}, {stderr})",
        child.status
    );
}

/// The child half of every re-seat scenario; a no-op in an ordinary run.
#[test]
fn reseat_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("reseat child watchdog: not done within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
    if std::env::var_os(CHILD_UNDO_ENV).is_some_and(|mode| mode == "1") {
        let outcome = reseat::undo(&World::options(&base), &mut |_| Ok(()));
        panic!("the armed point was not reached: undo returned {outcome:?}");
    }
    let outcome = reseat::reseat(&World::options(&base), &mut |_| Ok(()));
    panic!("the armed point was not reached: reseat returned {outcome:?}");
}

#[allow(clippy::too_many_lines)]
fn scenario(point: Point, nth: u64, kind: Reseat) {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::new(&point.name().replace('.', "-"));
    let pairs = world.pairs();
    let (transcript_dest, plan_dest) = (pairs[0].1.clone(), pairs[2].1.clone());
    let old_plan = b"# plan v1\n".to_vec();
    match kind {
        Reseat::Fresh => {}
        Reseat::Grow => {
            world.reseat();
            let tail: String = (2_000..14_000).map(line).collect();
            assert!(
                tail.len() > 3 * 512 * 1024,
                "{label}: the tail spans two chunks"
            );
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(world.landed_transcript())
                .unwrap();
            std::io::Write::write_all(&mut file, tail.as_bytes()).unwrap();
        }
        Reseat::Plan => {
            world.reseat();
            fs::write(&pairs[2].0, b"# plan v2, longer\n").unwrap();
        }
        Reseat::Zst => {
            let plain = fs::read(&pairs[3].0).unwrap();
            fs::write(
                compressed(&pairs[3].0),
                zstd::encode_all(plain.as_slice(), 3).unwrap(),
            )
            .unwrap();
            fs::remove_file(&pairs[3].0).unwrap();
            rusqlite::Connection::open(world.base.join("landing/.codex/state_5.sqlite"))
                .unwrap()
                .execute(
                    "UPDATE threads SET rollout_path = ?1 WHERE id = ?2",
                    rusqlite::params![format!("/Users/jess/.codex/{ROLLOUT}.zst"), THREAD],
                )
                .unwrap();
        }
        Reseat::Q198 => world.q198(),
        Reseat::Undo => {
            world.q198();
            world.reseat();
        }
        Reseat::Archived => world.archived(),
        Reseat::ArchivedUndo => {
            world.archived();
            world.reseat();
        }
    }
    let landed = fs::read(world.landed_transcript()).unwrap();

    crash_mode(
        &world,
        &format!("{}:{nth}", point.name()),
        matches!(kind, Reseat::Undo | Reseat::ArchivedUndo),
    );
    match kind {
        Reseat::Q198 => return settled_q198(&world, &pairs, &label),
        Reseat::Undo => return settled_undo(&world, &label),
        Reseat::ArchivedUndo => return settled_archived_undo(&world, &label),
        Reseat::Archived => assert_eq!(
            fs::read(world.fork_target()).unwrap(),
            World::fork_bytes(),
            "{label}: the archive target is never written"
        ),
        Reseat::Fresh | Reseat::Grow | Reseat::Plan | Reseat::Zst => {}
    }

    // Never overwritten: a prefix of the landed transcript, or absent; the
    // plan old or new. A crash inside an extension leaves a strict, longer
    // prefix: the first chunk of the tail is in place.
    if let Ok(placed) = fs::read(&transcript_dest) {
        assert!(
            landed.starts_with(&placed),
            "{label}: not a prefix of the landed transcript"
        );
        if point == Point::ReseatMidExtend {
            assert!(
                placed.len() > 2_000 * line(0).len() && placed.len() < landed.len(),
                "{label}"
            );
        }
    }
    if let Ok(plan) = fs::read(&plan_dest) {
        assert!(
            plan == old_plan || plan == fs::read(&pairs[2].0).unwrap(),
            "{label}: the plan is neither placement"
        );
    }

    settled(&world, &pairs, &label);
    if kind == Reseat::Archived {
        assert_eq!(fs::read(world.fork_target()).unwrap(), World::fork_bytes());
        let ledger = reseat::ledger::Ledger::open_read_only(&world.base.join("state"))
            .unwrap()
            .unwrap();
        let rows = ledger.regulars().unwrap();
        assert_eq!(rows.len(), 1, "{label}: {rows:?}");
        assert!(
            rows[0].replace && rows[0].state == reseat::ledger::RegularState::Done,
            "{label}: {rows:?}"
        );
    }

    // S3 after the crash is settled.
    let before = Counters::snapshot();
    let summary = world.reseat();
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::ReseatLandingRead),
        0,
        "{label}: {summary:?}"
    );
    assert_eq!(
        summary.sessions_of("claude", "unchanged"),
        1,
        "{label}: {summary:?}"
    );
    assert_eq!(
        summary.rows.get("unchanged"),
        Some(&1),
        "{label}: {summary:?}"
    );
}

/// The rerun after a crash finishes it: nothing handed over, diverged or
/// rewritten; every destination file its landed one; no temporary; one
/// Codex row with the destination cwd; the R0 check passing.
fn settled(world: &World, pairs: &[(PathBuf, PathBuf)], label: &str) {
    let summary = world.reseat();
    for word in [
        "dest-continued",
        "both-continued",
        "dest-removed",
        "diverged",
        "source-rewritten",
        "occupied",
    ] {
        assert_eq!(
            summary.sessions_of("claude", word),
            0,
            "{label}: {word} {summary:?}"
        );
        assert_eq!(
            summary.files_of("claude", word),
            0,
            "{label}: {word} {summary:?}"
        );
        assert_eq!(
            summary.files_of("codex", word),
            0,
            "{label}: {word} {summary:?}"
        );
    }
    assert_eq!(summary.rows.get("conflict"), None, "{label}: {summary:?}");
    for (landed, dest) in pairs {
        assert_eq!(
            fs::read(dest).ok(),
            landed_bytes(landed),
            "{label}: {dest:?}"
        );
        assert!(
            fs::symlink_metadata(dest).unwrap().is_file(),
            "{label}: {dest:?}"
        );
    }
    assert_eq!(
        world.temporaries(),
        Vec::<PathBuf>::new(),
        "{label}: a temporary left"
    );
    let index = rusqlite::Connection::open(world.base.join("dest/.codex/state_5.sqlite")).unwrap();
    let rows: Vec<(String, String)> = index
        .prepare("SELECT cwd, rollout_path FROM threads")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(
            world
                .base
                .join("real-git/lab")
                .to_string_lossy()
                .into_owned(),
            pairs[3].1.to_string_lossy().into_owned()
        )],
        "{label}"
    );
    let checked = reseat::check(&World::options(&world.base), &mut |_| Ok(())).unwrap();
    assert!(checked.verdict().is_ok(), "{label}: {checked:?}");
}

/// The rerun after a crash in a first pass over the OI-1003-Q198 world:
/// the symlinked transcript a regular copy of its target (the archive
/// kept), the spelled row re-spelled in `cwd` only (its rollout and title
/// the destination's, the ledger `applied`, nothing placed beside it), the
/// rowless rollout placed with a row from its header, no temporary; then
/// an unchanged rerun reads 0 landed bytes.
fn settled_q198(world: &World, pairs: &[(PathBuf, PathBuf)], label: &str) {
    let summary = world.reseat();
    assert_eq!(
        fs::read(world.own_link()).unwrap(),
        World::own_bytes(),
        "{label}: {summary:?}"
    );
    assert!(
        fs::symlink_metadata(world.own_link()).unwrap().is_file(),
        "{label}"
    );
    assert_eq!(
        fs::read(world.own_target()).unwrap(),
        World::own_bytes(),
        "{label}"
    );
    for (landed, dest) in pairs {
        assert_eq!(
            fs::read(dest).ok(),
            landed_bytes(landed),
            "{label}: {dest:?}"
        );
    }
    assert_eq!(
        world.temporaries(),
        Vec::<PathBuf>::new(),
        "{label}: a temporary left"
    );
    assert_eq!(
        world.row(SPELLED),
        Some((
            world.real_lab(),
            "/srv/archive/own.jsonl".to_owned(),
            "sting".to_owned()
        )),
        "{label}"
    );
    let rowless = world
        .base
        .join("dest/.codex")
        .join(World::rollout_rel(ROWLESS));
    assert!(rowless.is_file(), "{label}");
    assert_eq!(
        world.row(ROWLESS).map(|row| (row.0, row.1)),
        Some((world.real_lab(), rowless.to_string_lossy().into_owned())),
        "{label}"
    );
    assert!(
        !world
            .base
            .join("dest/.codex")
            .join(World::rollout_rel(SPELLED))
            .exists(),
        "{label}: placed beside the destination's own row"
    );
    {
        let ledger = reseat::ledger::Ledger::open_read_only(&world.base.join("state"))
            .unwrap()
            .unwrap();
        let rows = ledger.cwd_updates().unwrap();
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].state, "applied", "{label}");
        assert_eq!(rows[0].old_cwd, world.spelled_lab(), "{label}");
        let regulars = ledger.regulars().unwrap();
        assert_eq!(regulars.len(), 1, "{label}");
        assert_eq!(
            regulars[0].state,
            reseat::ledger::RegularState::Done,
            "{label}"
        );
    }
    let before = Counters::snapshot();
    let summary = world.reseat();
    assert_eq!(
        Counters::snapshot()
            .since(before)
            .get(Counter::ReseatLandingRead),
        0,
        "{label}: {summary:?}"
    );
    assert_eq!(
        summary.rows.get("unchanged"),
        Some(&2),
        "{label}: {summary:?}"
    );
}

/// The rerun of an `--undo` crashed after a full pass over the `Archived`
/// world (A-106): the exact link back over the replaced file, the archive
/// target untouched, no temporary, the row `undone` and the placement
/// row gone; a second undo has nothing left to do.
fn settled_archived_undo(world: &World, label: &str) {
    let (links, _) = world.undo();
    assert!(links <= 1, "{label}");
    let link = &world.pairs()[0].1;
    assert_eq!(
        fs::read_link(link).ok(),
        Some(world.fork_target()),
        "{label}"
    );
    assert_eq!(fs::read(world.fork_target()).unwrap(), World::fork_bytes());
    assert_eq!(
        world.temporaries(),
        Vec::<PathBuf>::new(),
        "{label}: a temporary left"
    );
    {
        let ledger = reseat::ledger::Ledger::open_read_only(&world.base.join("state"))
            .unwrap()
            .unwrap();
        assert!(
            ledger
                .regulars()
                .unwrap()
                .iter()
                .all(|row| row.state == reseat::ledger::RegularState::Undone),
            "{label}"
        );
        assert_eq!(ledger.placement(link).unwrap(), None, "{label}");
    }
    assert_eq!(world.undo(), (0, 0), "{label}");
}

/// The rerun of an `--undo` crashed after a full pass over the
/// OI-1003-Q198 world: the symlink back with its text, the copy gone, the
/// archive kept, the spelled row's old `cwd` back, the ledger `undone`, no
/// temporary; a second undo has nothing left to do.
fn settled_undo(world: &World, label: &str) {
    let (links, rows) = world.undo();
    assert!(links <= 1 && rows <= 1, "{label}");
    assert_eq!(
        fs::read_link(world.own_link()).ok(),
        Some(world.own_target()),
        "{label}"
    );
    assert_eq!(
        fs::read(world.own_target()).unwrap(),
        World::own_bytes(),
        "{label}"
    );
    assert_eq!(
        world.temporaries(),
        Vec::<PathBuf>::new(),
        "{label}: a temporary left"
    );
    assert_eq!(
        world.row(SPELLED).map(|row| row.0),
        Some(world.spelled_lab()),
        "{label}"
    );
    {
        let ledger = reseat::ledger::Ledger::open_read_only(&world.base.join("state"))
            .unwrap()
            .unwrap();
        assert!(
            ledger
                .cwd_updates()
                .unwrap()
                .iter()
                .all(|row| row.state == "undone"),
            "{label}"
        );
        assert!(
            ledger
                .regulars()
                .unwrap()
                .iter()
                .all(|row| row.state == reseat::ledger::RegularState::Undone),
            "{label}"
        );
    }
    assert_eq!(world.undo(), (0, 0), "{label}");
}
