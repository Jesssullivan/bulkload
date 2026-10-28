//! M2 item 5 (2026-10-16): re-pointable captures.
//!
//! Item IDs hash `(source, repository, workspace)`, so a changed destination
//! used to force a full re-capture. `estate-retarget PLAN CORPUS --map
//! OLD=NEW` rewrites the plan's destinations and re-keys the capture
//! records; the next capture is a reuse hit that reads no source byte, and
//! the apply lands at the new path.
//!
//! Fixture repositories are created and destroyed here; their Git runs with
//! no global configuration and no hooks (R-N98).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "estate_adopt/gftb.rs"]
#[allow(dead_code)]
mod gftb;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gftb::{git, settle};

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn agent(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env(
            bulkload_agent::space::TEST_SPACE_ENV,
            bulkload_agent::space::TEST_SPACE_ROOMY.test_env_value(),
        )
        .args(args.iter().map(|arg| arg.as_ref().to_owned()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-retarget-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for name in ["one", "two"] {
            let source = root.join("src").join(name);
            fs::create_dir_all(&source).unwrap();
            git(&source, &["init", "--quiet", "--template="]);
            fs::write(source.join("file"), format!("{name}\n")).unwrap();
            git(&source, &["add", "."]);
            git(&source, &["commit", "--quiet", "-m", name]);
        }
        fs::create_dir_all(root.join("dest-old")).unwrap();
        fs::create_dir_all(root.join("dest-new")).unwrap();
        Self { _dir: dir, root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn plan(&self) -> PathBuf {
        self.path("plan.postcard")
    }

    fn add(&self, rows: &[(&str, &str)]) {
        let mut list: Vec<OsString> = vec!["estate-add-batch".into(), self.plan().into()];
        for (source, dest) in rows {
            let dest = self.path(dest);
            list.push(self.path(source).into());
            list.push(dest.clone().into());
            list.push(dest.into());
        }
        let refs: Vec<&dyn AsRef<std::ffi::OsStr>> = list
            .iter()
            .map(|arg| arg as &dyn AsRef<std::ffi::OsStr>)
            .collect();
        let added = agent(&refs);
        assert!(added.ok, "{}", added.stderr);
    }

    fn capture(&self) -> Ran {
        let ran = agent(&[
            &"estate-capture",
            &self.plan(),
            &self.path("private"),
            &self.path("corpus"),
            &"1",
        ]);
        assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        ran
    }

    fn retarget(&self, maps: &[(&str, &str)]) -> Ran {
        let mut list: Vec<OsString> = vec![
            "estate-retarget".into(),
            self.plan().into(),
            self.path("corpus").into(),
        ];
        for (old, new) in maps {
            list.push("--map".into());
            list.push(format!("{}={}", self.path(old).display(), self.path(new).display()).into());
        }
        let refs: Vec<&dyn AsRef<std::ffi::OsStr>> = list
            .iter()
            .map(|arg| arg as &dyn AsRef<std::ffi::OsStr>)
            .collect();
        agent(&refs)
    }

    fn apply(&self) -> Ran {
        let state = self.path("applied");
        if !state.exists() {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        }
        agent(&[
            &"estate-apply",
            &self.plan(),
            &self.path("corpus"),
            &state,
            &"neo",
            &"1",
        ])
    }
}

/// Each capture receipt's (outcome, `source_bytes_read`).
fn outcomes(stdout: &str) -> Vec<(String, u64)> {
    stdout
        .lines()
        .filter(|line| line.starts_with("item=") && line.contains(" outcome="))
        .map(|line| {
            let field = |key: &str| {
                let start = line.find(key).unwrap() + key.len();
                line[start..].split(' ').next().unwrap().to_owned()
            };
            (
                field(" outcome="),
                field(" source_bytes_read=").parse().unwrap(),
            )
        })
        .collect()
}

fn counter(stderr: &str, key: &str) -> u64 {
    stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .and_then(|line| {
            line.split(' ')
                .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
        })
        .unwrap()
        .parse()
        .unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// P108 RETARGET-NO-REREAD: a captured plan re-pointed to a new
/// destination prefix needs no new capture: the next capture reuses every
/// item and reads no source byte, the apply lands at the new paths and
/// nothing at the old ones, the old plan is kept beside, and a rerun of the
/// retarget changes nothing.
#[test]
fn p108_a_retargeted_capture_lands_at_the_new_path_without_a_source_read() {
    let world = World::new();
    world.add(&[
        ("src/one", "dest-old/one"),
        ("src/two", "dest-old/nested/two"),
    ]);
    settle();
    let first = world.capture();
    assert!(
        outcomes(&first.stdout).iter().all(|(_, read)| *read > 0),
        "{}",
        first.stdout
    );
    let corpus_before = listing(&world.path("corpus"));

    let moved = world.retarget(&[("dest-old", "dest-new")]);
    assert!(moved.ok, "{}\n{}", moved.stdout, moved.stderr);
    let rows: Vec<&str> = moved.stdout.lines().collect();
    assert_eq!(rows.len(), 2, "{}", moved.stdout);
    assert!(
        rows.iter().all(|row| row.ends_with(" capture=rekeyed")),
        "{rows:?}"
    );
    assert!(moved.stdout.contains(&format!(
        "workspace=Some({:?})",
        world.path("dest-new/nested/two")
    )));
    // Only new records: every bundle and sidecar is the one it was.
    let corpus_after = listing(&world.path("corpus"));
    let added: Vec<&String> = corpus_after
        .iter()
        .filter(|name| !corpus_before.contains(name))
        .collect();
    assert_eq!(added.len(), 2, "{added:?}");
    assert!(
        added.iter().all(|name| name.ends_with(".capture")),
        "{added:?}"
    );
    assert!(listing(&world.root)
        .iter()
        .any(|name| name.starts_with("plan.postcard.before-retarget-")));

    let second = world.capture();
    let reused = outcomes(&second.stdout);
    assert_eq!(reused.len(), 2, "{}", second.stdout);
    for (outcome, read) in &reused {
        assert_eq!(outcome, "capture-reused-after-census", "{}", second.stdout);
        assert_eq!(*read, 0);
    }
    assert_eq!(counter(&second.stderr, "read_source_file_bytes"), 0);
    assert_eq!(counter(&second.stderr, "write_bundle_stage_bytes"), 0);

    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert_eq!(fs::read(world.path("dest-new/one/file")).unwrap(), b"one\n");
    assert_eq!(
        fs::read(world.path("dest-new/nested/two/file")).unwrap(),
        b"two\n"
    );
    assert!(listing(&world.path("dest-old")).is_empty());

    let again = world.retarget(&[("dest-old", "dest-new")]);
    assert!(
        again.ok && again.stdout.is_empty(),
        "{}\n{}",
        again.stdout,
        again.stderr
    );
    // Back again: the old identities still hold the same records.
    let back = world.retarget(&[("dest-new", "dest-old")]);
    assert!(back.ok, "{}", back.stderr);
    assert_eq!(back.stdout.lines().count(), 2, "{}", back.stdout);
    assert!(
        back.stdout
            .lines()
            .all(|row| row.ends_with(" capture=present")),
        "{}",
        back.stdout
    );
}

/// P109 RETARGET-REFUSES: a map that would make two workspace targets
/// overlap, a map a rerun would apply again, and a CORPUS that is not
/// there each refuse with the plan and the corpus as they were.
#[test]
fn p109_a_retarget_that_would_collide_changes_nothing() {
    let world = World::new();
    world.add(&[("src/one", "dest-old/one"), ("src/two", "dest-new/two")]);
    settle();
    world.capture();
    let plan = fs::read(world.plan()).unwrap();
    let corpus = listing(&world.path("corpus"));
    // dest-old/one -> dest-new/two/one would nest inside another target.
    let refused = world.retarget(&[("dest-old", "dest-new/two")]);
    assert!(!refused.ok);
    assert!(
        refused.stderr.contains("GIT_DESTINATION_OCCUPIED"),
        "{}",
        refused.stderr
    );
    assert_eq!(fs::read(world.plan()).unwrap(), plan);
    assert_eq!(listing(&world.path("corpus")), corpus);
    assert!(!listing(&world.root)
        .iter()
        .any(|name| name.contains("before-retarget")));
    // A NEW under its OLD would move the items again on every rerun.
    let nested = world.retarget(&[("dest-old", "dest-old/v2")]);
    assert!(!nested.ok);
    assert!(
        nested.stderr.contains("FIELD_DOMAIN_VIOLATION"),
        "{}",
        nested.stderr
    );
    // A CORPUS that is not there.
    let mut list: Vec<OsString> = vec![
        "estate-retarget".into(),
        world.plan().into(),
        world.path("no-corpus").into(),
        "--map".into(),
    ];
    list.push(
        format!(
            "{}={}",
            world.path("dest-old").display(),
            world.path("dest-x").display()
        )
        .into(),
    );
    let refs: Vec<&dyn AsRef<std::ffi::OsStr>> = list
        .iter()
        .map(|arg| arg as &dyn AsRef<std::ffi::OsStr>)
        .collect();
    let missing = agent(&refs);
    assert!(!missing.ok, "{}", missing.stdout);
    assert_eq!(fs::read(world.plan()).unwrap(), plan);
}

/// P109b: re-pointing back after a re-capture under the new destination
/// finds the old identity's record differing, and refuses
/// `RECEIPT_BINDING_INVALID` with nothing written.
#[test]
fn p109b_retargeting_back_after_a_recapture_refuses() {
    let world = World::new();
    world.add(&[("src/one", "dest-old/one")]);
    settle();
    world.capture();
    assert!(world.retarget(&[("dest-old", "dest-new")]).ok);
    let source = world.path("src/one");
    fs::write(source.join("file"), b"changed\n").unwrap();
    git(&source, &["commit", "--quiet", "-am", "change"]);
    settle();
    world.capture();
    let plan = fs::read(world.plan()).unwrap();
    let back = world.retarget(&[("dest-new", "dest-old")]);
    assert!(!back.ok, "{}", back.stdout);
    assert!(
        back.stderr.contains("RECEIPT_BINDING_INVALID"),
        "{}",
        back.stderr
    );
    assert_eq!(fs::read(world.plan()).unwrap(), plan);
}
