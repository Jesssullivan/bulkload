//! P70 SIDECAR-ORDER (Q42 lane L7, OI-1003-Q42, OI-1003-Q45, OI-1003-Q94):
//! the `estate.*` fault points around a Git capture's publication.
//!
//! `estate::capture_item` publishes, in this order, the bundle (under its
//! content name), its dependency sidecars (`.base`, `.prior`, `.drift`,
//! `.nested`, `.parts`), its reuse manifest (`{bundle}.reuse`) and then the
//! `{item}.capture` record. Each scenario crashes one real `estate::capture`
//! at one point of that sequence (the child ends itself with `_exit`; nothing
//! here sends a signal, R-N11), on a first capture or on a changed capture
//! over a retained one, and asserts:
//!
//! - **record implies manifest.** Whatever bundle the record names after the
//!   crash has its manifest, bound to that bundle's bytes. Reversing the
//!   order (the record before the manifest) fails this at
//!   `estate.before_reuse_sidecar`: the record would name a bundle with no
//!   manifest, and every later pass would fetch it for good;
//! - **every manifest matches its bundle.** Each `.reuse` in the corpus,
//!   named by a record or orphaned by the crash, is bound to the bytes at
//!   its bundle's name and lists exactly the blobs of that bundle's worktree
//!   tree, as Git reads them from the bundle itself;
//! - **the crash landed where it says.** The record is the one from before
//!   the crash until `estate.after_capture_record`, and the new bundle has
//!   its manifest exactly from `estate.after_reuse_sidecar` on, so no point
//!   is vacuous;
//! - **the next pass converges and reuses by manifest.** A clean capture
//!   after the crash succeeds, and the changed capture after that reads its
//!   retained capture's manifest and no bundle byte (the P69 law survives
//!   every crash point), counted in a child of its own;
//! - **restores are exact.** `estate::apply` of the corpus restores `HEAD`
//!   and every file byte for byte.
//!
//! `_exit` models a process crash only (see the harness header): these
//! scenarios prove the order of the publication steps, not their fsyncs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::estate;
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the scratch base whose estate it captures.
const CHILD_ENV: &str = "BULKLOAD_W7_ESTATE_CHILD_BASE";

/// The prefix of the line a child prints with its own process counters.
const COUNTERS: &str = "P70-COUNTERS";

/// Which capture the crash interrupts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateSidecar {
    /// The item's first capture: no record before the crash.
    First,
    /// A changed capture over a retained one: the record names the capture
    /// before, whose manifest the interrupted pass was reusing.
    Changed,
}

impl Scenario for EstateSidecar {
    fn run(self, point: Point, nth: u64) {
        assert_eq!(nth, 1, "one item: every estate point is hit once");
        scenario(point, self);
    }
}

struct Estate {
    base: PathBuf,
}

impl Estate {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-estate-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let estate = Self { base };
        let source = estate.source();
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(source.join("history"), super::noise(7, 64 * 1024)).unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/deep"), b"a nested seat").unwrap();
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "base"]);
        estate::add(
            &estate.plan(),
            &source,
            &estate.target(),
            Some(&estate.target()),
        )
        .unwrap();
        estate
    }
    fn source(&self) -> PathBuf {
        self.base.join("source")
    }
    fn target(&self) -> PathBuf {
        self.base.join("target")
    }
    fn plan(&self) -> PathBuf {
        self.base.join("plan")
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn corpus(&self) -> PathBuf {
        self.base.join("corpus")
    }
    /// One clean, in-process capture; its receipts' outcomes.
    fn capture(&self) -> Vec<&'static str> {
        let outcomes = std::sync::Mutex::new(Vec::new());
        estate::capture(&self.plan(), &self.state(), &self.corpus(), 1, &|row| {
            outcomes.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        outcomes.into_inner().unwrap()
    }
    /// A commit that edits `file`.
    fn commit(&self, round: u32) {
        fs::write(self.source().join("file"), format!("changed {round}")).unwrap();
        git(&self.source(), &["commit", "--quiet", "-am", "change"]);
    }
    /// The bundle the item's record names, if it has a record.
    fn recorded(&self) -> Option<String> {
        let mut recorded = estate::custody_probe::recorded(&self.plan(), &self.corpus()).unwrap();
        assert_eq!(recorded.len(), 1);
        recorded.pop().unwrap()
    }
    /// Every bundle name in the corpus.
    fn bundles(&self) -> Vec<String> {
        self.named("bundle")
    }
    /// The corpus file names with `extension`.
    fn named(&self, extension: &str) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.corpus()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == extension)
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for Estate {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repo);
    command
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = git_command(repo).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

// Seats must be older than one timestamp tick to be reused by identity (R-N76).
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

/// The blobs of `bundle`'s worktree tree, by path, as Git reads them from
/// the bundle itself: fetched into a scratch repository that borrows the
/// source's objects (a chained bundle is thin).
fn bundle_blobs(estate: &Estate, bundle: &str) -> BTreeMap<Vec<u8>, String> {
    let scratch = estate.base.join(format!("inspect-{bundle}"));
    let _ = fs::remove_dir_all(&scratch);
    git(
        &estate.base,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            scratch.to_str().unwrap(),
        ],
    );
    fs::write(
        scratch.join("objects/info/alternates"),
        format!("{}\n", estate.source().join(".git/objects").display()),
    )
    .unwrap();
    git(
        &scratch,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            estate.corpus().join(bundle).to_str().unwrap(),
            "+refs/carry-export/worktree:refs/inspect/worktree",
        ],
    );
    let listed = git(&scratch, &["ls-tree", "-r", "-z", "refs/inspect/worktree"]);
    let blobs = listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let tab = entry.iter().position(|byte| *byte == b'\t').unwrap();
            let header = std::str::from_utf8(&entry[..tab]).unwrap();
            let mut fields = header.split(' ');
            let (_mode, kind, object) = (
                fields.next().unwrap(),
                fields.next().unwrap(),
                fields.next().unwrap(),
            );
            assert_eq!(kind, "blob", "{header}");
            (entry[tab + 1..].to_vec(), object.to_owned())
        })
        .collect();
    fs::remove_dir_all(&scratch).unwrap();
    blobs
}

/// Every manifest in the corpus matches the bundle it is named for: bound
/// to the bytes at that name, and listing exactly that bundle's blobs.
fn assert_manifests_match(label: &str, estate: &Estate) -> Vec<String> {
    let mut listed = Vec::new();
    for sidecar in estate.named("reuse") {
        let bundle = sidecar.strip_suffix(".reuse").unwrap();
        assert!(
            estate.corpus().join(bundle).exists(),
            "{label}: {sidecar} has no bundle"
        );
        let seats = estate::custody_probe::manifest(&estate.corpus(), bundle)
            .unwrap_or_else(|refusal| panic!("{label}: {sidecar} is not bound: {refusal:?}"))
            .unwrap();
        let seats: BTreeMap<Vec<u8>, String> = seats.into_iter().collect();
        assert_eq!(
            seats,
            bundle_blobs(estate, bundle),
            "{label}: {sidecar} does not list its bundle's blobs"
        );
        listed.push(bundle.to_owned());
    }
    listed
}

/// Run this test binary as a child that captures `estate` once. Armed with
/// `fault`, it must end itself at that point; unarmed, it must finish, and
/// its own process counters are returned.
fn child(estate: &Estate, fault: Option<&str>) -> Option<BTreeMap<String, u64>> {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let label = fault.unwrap_or("unarmed");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "estate_sidecar::crash_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &estate.base)
        .env_remove(super::CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(fault) = fault {
        command.env(FAULT_ENV, fault);
    }
    let running = command.spawn().unwrap();
    // A detached waiter reaps the child; this thread waits on it against a
    // deadline. On timeout the test fails and the child is left alone: it is
    // never signalled (R-N11).
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let child = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!(
                "{label}: estate child still running after {CHILD_DEADLINE:?} ({timeout}); \
                 left running, not signalled"
            )
        })
        .unwrap();
    let stderr = String::from_utf8_lossy(&child.stderr).into_owned();
    assert_ne!(
        child.status.code(),
        Some(CHILD_WATCHDOG_EXIT_CODE),
        "{label}: hung; the child's watchdog ended it after {CHILD_WATCHDOG:?} (stderr {stderr})"
    );
    if fault.is_some() {
        assert_eq!(
            child.status.code(),
            Some(FAULT_EXIT_CODE),
            "{label}: the child must stop at its fault point (status {:?}, stderr {stderr})",
            child.status
        );
        return None;
    }
    assert!(child.status.success(), "{label}: {stderr}");
    let line = stderr
        .lines()
        .find_map(|line| line.strip_prefix(COUNTERS))
        .unwrap_or_else(|| panic!("{label}: no {COUNTERS} line in {stderr}"));
    Some(
        line.split_whitespace()
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.parse().unwrap()))
            .collect(),
    )
}

/// The child half of every scenario; a no-op in an ordinary test run.
#[test]
fn crash_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    // A scenario that never reaches its fault point must not hold the CI job
    // open: past the watchdog this child ends itself, never signalled.
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("estate child watchdog: not done within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself, with a status the parent reports by scenario.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
    // An armed fault point ends this process inside `capture`. An unarmed
    // child finishes and prints the counters of this one capture.
    let before = Counters::snapshot();
    let outcome = estate::capture(
        &base.join("plan"),
        &base.join("state"),
        &base.join("corpus"),
        1,
        &|_| Ok(()),
    );
    let spent = Counters::snapshot().since(before);
    assert!(outcome.is_ok(), "capture returned {outcome:?}");
    eprintln!(
        "{COUNTERS} bundle={} manifest={} misses={}",
        spent.get(Counter::SourceCaptureReuseRead),
        spent.get(Counter::ReuseManifestRead),
        spent.get(Counter::ReuseDirtyMiss),
    );
}

fn restored(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, relative: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let path = relative.join(entry.file_name());
            if path == Path::new(".git") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(path, fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, Path::new(""), &mut files);
    files
}

fn scenario(point: Point, stage: EstateSidecar) {
    let label = format!("{}/{stage:?}", point.name());
    let estate = Estate::new(&point.name().replace('.', "-"));
    settle();
    let before = match stage {
        EstateSidecar::First => None,
        EstateSidecar::Changed => {
            assert_eq!(estate.capture(), vec!["captured"], "{label}");
            estate.commit(1);
            let retained = estate.recorded();
            assert!(retained.is_some(), "{label}: no retained record");
            retained
        }
    };
    let earlier = estate.bundles();
    assert!(child(&estate, Some(point.name())).is_none());

    // The crash state.
    let published: Vec<String> = estate
        .bundles()
        .into_iter()
        .filter(|name| !earlier.contains(name))
        .collect();
    assert_eq!(published.len(), 1, "{label}: one bundle was published");
    let new = &published[0];
    let recorded = estate.recorded();
    if point == Point::EstateAfterCaptureRecord {
        assert_eq!(recorded.as_ref(), Some(new), "{label}: the record moved");
    } else {
        assert_eq!(recorded, before, "{label}: the record did not move");
    }
    let listed = assert_manifests_match(&label, &estate);
    // Record implies manifest: the order under test.
    if let Some(bundle) = &recorded {
        assert!(
            listed.contains(bundle),
            "{label}: the record names {bundle}, which has no manifest"
        );
    }
    assert_eq!(
        listed.contains(new),
        matches!(
            point,
            Point::EstateAfterReuseSidecar | Point::EstateAfterCaptureRecord
        ),
        "{label}: the interrupted capture's manifest"
    );

    // The next pass converges on a record with its manifest.
    let outcomes = estate.capture();
    assert_eq!(outcomes.len(), 1, "{label}");
    assert!(
        matches!(outcomes[0], "captured" | "capture-reused-after-census"),
        "{label}: {outcomes:?}"
    );
    let head = estate
        .recorded()
        .unwrap_or_else(|| panic!("{label}: no record"));
    assert!(assert_manifests_match(&label, &estate).contains(&head));

    // The capture recorded through the crash is reused from its manifest:
    // the P69 law, counted in a child of its own.
    estate.commit(2);
    let counters = child(&estate, None).unwrap();
    assert_eq!(counters["bundle"], 0, "{label}: {counters:?}");
    assert_eq!(counters["misses"], 0, "{label}: {counters:?}");
    assert!(counters["manifest"] > 0, "{label}: {counters:?}");
    assert_manifests_match(&label, &estate);

    // Restores are exact.
    estate::apply(
        &estate.plan(),
        &estate.corpus(),
        &estate.base.join("applied"),
        "neo",
        1,
        &|_| Ok(()),
    )
    .unwrap_or_else(|refusal| panic!("{label}: apply refused {refusal:?}"));
    let head = |repo: &Path| git(repo, &["rev-parse", "HEAD"]);
    assert_eq!(head(&estate.target()), head(&estate.source()), "{label}");
    assert!(
        restored(&estate.target()) == restored(&estate.source()),
        "{label}: a restored file differs from its source"
    );
}
