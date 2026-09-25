//! W6 M1 PR 2: crash-resume of the git carry v2 ingest (R-N60, R-N58).
//!
//! Each scenario builds a source and a bare destination, plants an existing
//! `refs/carry/*` ref in the destination, then re-runs this test binary as a
//! child that performs one whole carry in process: probe, first round, a plan
//! of at least three segments (persisted), and the destination ingest, with
//! one `git_ingest.*` fault point armed. The child ends itself with `_exit` at
//! that point; nothing here signals it (R-N11).
//!
//! The parent then restarts both sides as a real restart would:
//! - the destination resumes from its journal alone, and `next_segment` is
//!   exactly what the crash had journaled (the `GitResume` value);
//! - the sender reads its plan back by `pack_id` and packs only segments from
//!   `next_segment` on ("a crash in segment k re-sends only segments >= k");
//! - the ingest finishes: the plan ref is published, the planted
//!   `refs/carry/*` ref is byte-identical and the receipt's carry digests
//!   agree, no quarantine, `tmp_*` or bulkload `.keep` remains, and
//!   `fsck --strict` passes.
//!
//! Before the resume the parent also checks what the crash left: no plan ref
//! before the transaction, the ref present after it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};
use bulkload_agent::git_carry::carry_v2::{
    first_round, Ingest, IngestPlan, JournalStore, ListStore, PackPlan, RefUpdate, Source, Target,
};

/// Names the child's scenario directory.
const CHILD_ENV: &str = "BULKLOAD_GIT_INGEST_CHILD";
/// Stored bytes per segment: small, so the fixture has several segments.
const CAP: u64 = 32 * 1024;
/// A child that never reaches its point ends itself after this.
const WATCHDOG: Duration = Duration::from_mins(2);
const WATCHDOG_EXIT_CODE: i32 = 88;
const PLAN_REF: &str = "refs/carry/v1/test/crash/state";
const PLANTED_REF: &str = "refs/carry/v1/existing/state";

static NEXT: AtomicU64 = AtomicU64::new(0);

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_QUARANTINE_PATH",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        FAULT_ENV,
    ] {
        command.env_remove(key);
    }
    // Fixture repositories only: hooks off (R-N98).
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "gc.auto=0",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-C",
        ])
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "bulkload")
        .env("GIT_AUTHOR_EMAIL", "bulkload@invalid")
        .env("GIT_COMMITTER_NAME", "bulkload")
        .env("GIT_COMMITTER_EMAIL", "bulkload@invalid")
        .env("GIT_AUTHOR_DATE", "1790121600 +0000")
        .env("GIT_COMMITTER_DATE", "1790121600 +0000")
        .env("LC_ALL", "C");
    command
}

fn run(repo: &Path, args: &[&str]) -> String {
    let output = git(repo).args(args).stdin(Stdio::null()).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .to_owned()
}

fn ref_value(repo: &Path, name: &str) -> Option<String> {
    let output = git(repo)
        .args(["rev-parse", "--verify", "-q", name])
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// xorshift64*: deterministic content.
fn noise(seed: u64, bytes: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..bytes)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes()[0]
        })
        .collect()
}

struct Scenario {
    base: PathBuf,
}

impl Scenario {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w6-ingest-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let base = base.canonicalize().unwrap();
        for state in ["source-state", "destination-state"] {
            fs::create_dir(base.join(state)).unwrap();
            fs::set_permissions(
                base.join(state),
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
        }
        let scenario = Self { base };
        scenario.build();
        scenario
    }

    fn source(&self) -> PathBuf {
        self.base.join("source")
    }

    fn destination(&self) -> PathBuf {
        self.base.join("destination.git")
    }

    /// A source with a base commit the destination holds, then four
    /// incompressible 16 KiB blobs and some text history on top: several
    /// 32 KiB segments. The destination also holds a planted carry ref.
    fn build(&self) {
        let source = self.source();
        run(&self.base, &["init", "-q", "source"]);
        fs::write(source.join("base.txt"), noise(1, 2048)).unwrap();
        run(&source, &["add", "-A"]);
        run(&source, &["commit", "-q", "-m", "base"]);
        let base = run(&source, &["rev-parse", "HEAD"]);
        for index in 0..4_u64 {
            fs::write(
                source.join(format!("blob{index}.bin")),
                noise(10 + index, 16 * 1024),
            )
            .unwrap();
            fs::write(source.join("text.txt"), noise(20 + index, 4096)).unwrap();
            run(&source, &["add", "-A"]);
            run(&source, &["commit", "-q", "-m", &format!("c{index}")]);
        }
        run(&self.base, &["init", "-q", "--bare", "destination.git"]);
        let destination = self.destination();
        run(
            &source,
            &[
                "push",
                "-q",
                destination.to_str().unwrap(),
                &format!("{base}:refs/heads/main"),
            ],
        );
        run(&destination, &["update-ref", PLANTED_REF, &base]);
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The whole carry, in process: what the crash child runs, and (disarmed)
/// what a clean run does.
fn carry(base: &Path) -> String {
    let source = base.join("source");
    let destination = base.join("destination.git");
    let lists = ListStore::open(&base.join("source-state")).unwrap();
    let journals = JournalStore::open(&base.join("destination-state")).unwrap();
    let sender = Source::probe(&source, None).unwrap();
    let target = Target::probe(&destination, None).unwrap();
    let round = first_round(&sender, target.offer(), &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, CAP, None).unwrap();
    lists.persist(&plan, &sender).unwrap();
    let updates = vec![RefUpdate {
        name: PLAN_REF.to_owned(),
        oid: plan.wants()[0].clone(),
    }];
    let mut session = Ingest::open(
        &target,
        &journals,
        IngestPlan::of(&plan, updates).unwrap(),
        None,
    )
    .unwrap();
    for index in 0..plan.segments() {
        let mut pack = Vec::new();
        plan.send_segment(&sender, index, &mut pack, None).unwrap();
        session.receive(index, &mut &pack[..]).unwrap();
    }
    session.finish().unwrap();
    plan.pack_id().to_owned()
}

#[test]
fn git_ingest_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        eprintln!("git ingest child watchdog: no fault point within {WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself (R-N11).
        unsafe { libc::_exit(WATCHDOG_EXIT_CODE) }
    });
    let pack_id = carry(&base);
    eprintln!("fault point not reached; carry finished {pack_id}");
}

/// The one `.list` the child persisted names the plan.
fn persisted_pack_id(base: &Path) -> String {
    let lists = base.join("source-state/git-carry-v2/lists");
    let names: Vec<String> = fs::read_dir(lists)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    names[0].strip_suffix(".list").unwrap().to_owned()
}

fn crash_resume(point: Point, nth: u64) {
    let label = format!("{}:{nth}", point.name());
    let scenario = Scenario::new(&point.name().replace('.', "-"));
    let planted = ref_value(&scenario.destination(), PLANTED_REF);
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "git_ingest::git_ingest_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &scenario.base)
        .env(FAULT_ENV, &label)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{label}: the child must stop at its fault point (status {:?}, stderr {})",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
    let destination = scenario.destination();
    let published = matches!(
        point,
        Point::GitIngestAfterPublish | Point::GitIngestBeforeDone
    );
    assert_eq!(
        ref_value(&destination, PLAN_REF).is_some(),
        published,
        "{label}: the plan ref exists exactly when the transaction ran"
    );

    // Restart both sides.
    let pack_id = persisted_pack_id(&scenario.base);
    let journals = JournalStore::open(&scenario.base.join("destination-state")).unwrap();
    let target = Target::probe(&destination, None).unwrap();
    let mut session = Ingest::resume(&target, &journals, &pack_id, None).unwrap();
    let lists = ListStore::open(&scenario.base.join("source-state")).unwrap();
    let plan = lists.load(&pack_id).unwrap();
    let n = plan.segments();
    assert!(n >= 3, "{label}: the fixture has {n} segments");
    let expected = match point {
        Point::GitIngestAfterIndexPack => usize::try_from(nth).unwrap() - 1,
        Point::GitIngestAfterSegment => usize::try_from(nth).unwrap(),
        _ => n,
    };
    assert_eq!(
        session.next_segment(),
        expected,
        "{label}: GitResume asks for the first unjournaled segment"
    );
    let sender = Source::probe(&scenario.source(), None).unwrap();
    let mut resent = 0;
    for index in session.next_segment()..n {
        let mut pack = Vec::new();
        plan.send_segment(&sender, index, &mut pack, None).unwrap();
        session.receive(index, &mut &pack[..]).unwrap();
        resent += 1;
    }
    assert_eq!(resent, n - expected, "{label}: only segments >= k re-sent");
    let receipt = session.finish().unwrap();
    assert_eq!(receipt.segments.len(), n);
    assert_eq!(
        receipt.carry_refs_before, receipt.carry_refs_after,
        "{label}: receipt carry digests"
    );
    assert_eq!(
        ref_value(&destination, PLAN_REF).as_deref(),
        Some(plan.wants()[0].as_str()),
        "{label}: published"
    );
    assert_eq!(
        ref_value(&destination, PLANTED_REF),
        planted,
        "{label}: the existing carry ref is unchanged"
    );
    assert_clean(&destination, &label);
    println!(
        "m1 crash point={label} segments={n} next_segment={expected} resent={resent} \
         carry_refs_unchanged=true fsck=clean"
    );
}

fn assert_clean(destination: &Path, label: &str) {
    let objects = destination.join("objects");
    for entry in fs::read_dir(&objects).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().starts_with("incoming-"),
            "{label}: quarantine left"
        );
    }
    for entry in fs::read_dir(objects.join("pack")).unwrap() {
        let path = entry.unwrap().path();
        let leaf = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!leaf.starts_with("tmp_"), "{label}: {leaf}");
        if path.extension().is_some_and(|x| x == "keep") {
            assert!(
                !fs::read_to_string(&path)
                    .unwrap()
                    .starts_with("bulkload git-carry-v2"),
                "{label}: our keep left: {leaf}"
            );
        }
    }
    let fsck = git(destination)
        .args(["fsck", "--strict", "--no-progress", "--no-dangling"])
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "{label}: fsck: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// With no point armed the child's flow completes, and a second finish of
/// the same session from its journal is a no-op returning the same receipt.
#[test]
fn git_ingest_clean_run_converges() {
    let scenario = Scenario::new("clean");
    let pack_id = carry(&scenario.base);
    let journals = JournalStore::open(&scenario.base.join("destination-state")).unwrap();
    let target = Target::probe(&scenario.destination(), None).unwrap();
    let again = Ingest::resume(&target, &journals, &pack_id, None)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(again.pack_id, pack_id);
    assert_clean(&scenario.destination(), "clean");
}

macro_rules! git_scenarios {
    ($($name:ident => $point:ident : $nth:expr;)*) => {$(
        #[test]
        fn $name() {
            crash_resume(Point::$point, $nth);
        }
    )*};
}

git_scenarios! {
    git_ingest_after_index_pack_first => GitIngestAfterIndexPack: 1;
    git_ingest_after_index_pack_mid => GitIngestAfterIndexPack: 2;
    git_ingest_after_segment_first => GitIngestAfterSegment: 1;
    git_ingest_after_segment_mid => GitIngestAfterSegment: 2;
    git_ingest_after_connected => GitIngestAfterConnected: 1;
    git_ingest_mid_migrate_first => GitIngestMidMigrate: 1;
    git_ingest_mid_migrate_mid => GitIngestMidMigrate: 5;
    git_ingest_after_migrate => GitIngestAfterMigrate: 1;
    git_ingest_after_publish => GitIngestAfterPublish: 1;
    git_ingest_before_done => GitIngestBeforeDone: 1;
}
