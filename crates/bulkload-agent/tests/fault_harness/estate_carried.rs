//! `estate-carry-files` crash row (M2r2 b): the `estate.carry.recorded`
//! fault point.
//!
//! A source checkout with no index is planned, captured (refused) and
//! applied (`CAPTURE_ABSENT`). The binary, armed at the point, carries it
//! and ends itself with `_exit` after the carry record is written and
//! before the outcome; nothing here sends it a signal (R-N11). The parent
//! asserts:
//!
//! - **ordered**: the landed tree is the source's, byte for byte, and the
//!   carry record exists, but the outcome is still `CAPTURE_ABSENT`, so
//!   closure reads the item `refused-pending-review`, never `carried`;
//! - **finishes**: the rerun carries it with no destination write (every
//!   landed file adopted) and closure reads it `carried`;
//! - **idempotent**: a second rerun leaves it `carried` and writes nothing
//!   under the destination.
//!
//! `_exit` models a process crash only (see the harness header).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};

use super::{Scenario, CHILD_DEADLINE, NEXT};

/// What is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateCarried {
    /// A capture-refused item carried into its absent workspace.
    Workspace,
}

impl Scenario for EstateCarried {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth);
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
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
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Run the binary with `args`, armed with `fault` when given, against a
/// deadline; a child past it is reported and left alone, never signalled.
fn agent(args: &[OsString], fault: Option<&str>) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
    command.env(
        bulkload_agent::space::TEST_SPACE_ENV,
        bulkload_agent::space::TEST_SPACE_ROOMY.test_env_value(),
    );
    command
        .arg("--min-free-percent=0")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove(FAULT_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(fault) = fault {
        command.env(FAULT_ENV, fault);
    }
    let running = command.spawn().unwrap();
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let out = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!("carry child still running after {CHILD_DEADLINE:?} ({timeout}); not signalled")
        })
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn os(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

fn tree(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>, u64, i64)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let body = if meta.is_dir() {
                pending.push(path.clone());
                Vec::new()
            } else if meta.is_symlink() {
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else {
                fs::read(&path).unwrap()
            };
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (meta.mode(), body, meta.ino(), meta.mtime_nsec()),
            );
        }
    }
    out
}

fn content(
    tree: &BTreeMap<PathBuf, (u32, Vec<u8>, u64, i64)>,
) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    tree.iter()
        .map(|(path, (mode, body, _, _))| (path.clone(), (*mode, body.clone())))
        .collect()
}

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[allow(clippy::too_many_lines)] // One crash row, its checks in order.
fn scenario(point: Point, nth: u64) {
    let base = std::env::temp_dir().join(format!(
        "bulkload-w7-carried-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&base).unwrap();
    let world = World {
        base: fs::canonicalize(&base).unwrap(),
    };
    let at = |name: &str| world.base.join(name);
    let source = at("src");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("a.txt"), b"alpha\n").unwrap();
    fs::create_dir(source.join("d")).unwrap();
    fs::write(source.join("d/b.txt"), b"beta\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "c0"]);
    fs::remove_file(source.join(".git/index")).unwrap();
    let workspace = at("dst");
    let plan = at("plan");
    agent(
        &os(&[&"estate-add", &plan, &source, &workspace, &workspace]),
        None,
    );
    agent(
        &os(&[
            &"estate-capture",
            &plan,
            &at("capture-state"),
            &at("corpus"),
            &"1",
        ]),
        None,
    );
    let apply = || {
        agent(
            &os(&[
                &"estate-apply",
                &plan,
                &at("corpus"),
                &at("state"),
                &"neo",
                &"1",
            ]),
            None,
        )
    };
    let (_, applied) = apply();
    assert!(applied.contains("CAPTURE_ABSENT"), "{applied}");
    let carry = os(&[
        &"estate-carry-files",
        &plan,
        &at("corpus"),
        &at("state"),
        &"neo",
        &at("carry"),
    ]);
    let closure = || {
        agent(
            &os(&[
                &"closure-report",
                &plan,
                &at("corpus"),
                &"neo",
                &at("state"),
            ]),
            None,
        )
        .1
    };

    let spec = format!("{}:{nth}", point.name());
    let (status, crashed) = agent(&carry, Some(&spec));
    assert_eq!(status, Some(FAULT_EXIT_CODE), "{crashed}");
    // Ordered: bytes and carry record durable, outcome not yet written.
    assert_eq!(content(&tree(&workspace)), content(&tree(&source)));
    let carried_records = fs::read_dir(at("state"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".carried")
        })
        .count();
    assert_eq!(carried_records, 1);
    let report = closure();
    assert!(
        report.contains("\"refusal\":\"CAPTURE_ABSENT\""),
        "{report}"
    );
    assert!(!report.contains("\"disposition\":\"carried\""), "{report}");

    // Finishes, writing nothing under the destination.
    let landed = tree(&workspace);
    let (status, rerun) = agent(&carry, None);
    assert_eq!(status, Some(0), "{rerun}");
    assert!(rerun.contains(" outcome=carried-as-files "), "{rerun}");
    assert_eq!(tree(&workspace), landed);
    let report = closure();
    assert!(report.contains("\"disposition\":\"carried\""), "{report}");
    assert!(report.contains("\"gate\":\"pass\""), "{report}");

    // Idempotent.
    let (status, again) = agent(&carry, None);
    assert_eq!(status, Some(0), "{again}");
    assert_eq!(tree(&workspace), landed);
    assert!(closure().contains("\"disposition\":\"carried\""));
}
