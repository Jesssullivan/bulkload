//! REFS-SCALE, distinct-heavy (OI-1003-Q54, #178 review): one commit per ref.
//!
//! The v1 ref table's header grows by one tip line per distinct object
//! (111 B on SHA-1), and a thin bundle's by a prerequisite line more (54 B).
//! The `refs_scale_tests` rows hold few distinct objects per ref, where the
//! header is a few hundred KB whatever the ref count. These rows take the
//! other end: every ref names its own commit, so the distinct objects are
//! the refs.
//!
//! - **The header law** at that end: under the cap, within the fixed lines
//!   plus 111 B per distinct object; a chained pass within 165 B per
//!   distinct object.
//! - **The import is linear in the distinct objects.** Every import runs as
//!   the agent binary's `git-import` verb, reaped with `wait4`, so its CPU
//!   (user and system, its git children included) is that process's own,
//!   never a share of concurrent tests'. It must stay under
//!   [`CPU_PER_DISTINCT`] per distinct object plus [`CPU_FIXED`]. The import
//!   once fetched one exact refspec per distinct object, which git matches
//!   by a scan of every advertised ref: sting (git 2.52) measured 56 s, 232 s
//!   and 1,001 s of CPU at 10,000, 20,000 and 40,000 tips, so this row's
//!   import would have cost several hundred seconds.
//! - **Exact import**: every source ref under the import's namespace at its
//!   commit, the chained pass imported on top in its own namespace.
//! - **Deep tier** (`BULKLOAD_PROPTEST_DEEP=1`, local): 110,000 distinct
//!   commits. The self-contained header fits (about 12.2 MB); the chained
//!   pass's thin header would not (about 18.2 MB), so that pass is written
//!   self-contained instead of refused, and imports exactly.
//!
//! Every row builds under the system temp dir with the real `git` on `PATH`
//! and removes it afterwards. Measurements print as `REFS-SCALE-DISTINCT`
//! lines (`--nocapture`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use bulkload_agent::git_carry::{
    export_repository, export_repository_with_drift, shared, ExportOptions,
};

/// `test_support::DEEP`, mirrored: the switch for the deep local tier.
const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";
/// The import's CPU budget per distinct object (a debug build). These rows
/// measured 0.53 to 0.95 ms per distinct object on sting at load 3 to 22:
/// 17.4 to 21.2 s self-contained and 24.8 to 31.2 s thin at 32,768
/// distinct, 61.1 s at 110,000 (3.4x the objects, 3.5x the CPU). So this
/// allows 2.6x and more. The quadratic fetch (about 0.63 us x D^2) exceeds
/// the budget from about 8,000 distinct objects, and at this row's 32,768
/// cost about 670 s against 102 s.
const CPU_PER_DISTINCT: Duration = Duration::from_micros(2_500);
/// The import's fixed CPU budget: process start, verify, the walk's setup.
const CPU_FIXED: Duration = Duration::from_secs(20);
/// A tip line: `<oid> refs/carry-export/ref-tip-v1/<oid>\n`.
const TIP_LINE: usize = 40 + 1 + 29 + 40 + 1;
/// A prerequisite line: `-<oid> shared base\n`.
const PREREQUISITE_LINE: usize = 1 + 40 + 12 + 1;
/// The capture metadata, table and signature lines of a header, generously.
const HEADER_FIXED: usize = 2048;
/// The source slug every import here uses.
const SOURCE: &str = "neo";

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn root(name: &str) -> Root {
    let path = std::env::temp_dir().join(format!(
        "bulkload-refs-scale-distinct-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    Root(path)
}

// A git child in `repo` with a fixed identity and no user configuration.
fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Refs Scale")
        .env("GIT_AUTHOR_EMAIL", "refs-scale@localhost")
        .env("GIT_COMMITTER_NAME", "Refs Scale")
        .env("GIT_COMMITTER_EMAIL", "refs-scale@localhost")
        .env("GIT_AUTHOR_DATE", "1759622400 +0000")
        .env("GIT_COMMITTER_DATE", "1759622400 +0000")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .arg("-C")
        .arg(repo);
    command
}

fn run(repo: &Path, args: &[&str]) -> String {
    let output = git(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_with(repo: &Path, args: &[&str], stdin: &[u8]) {
    use std::io::Write as _;
    let mut child = git(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    assert!(child.wait().unwrap().success(), "git {args:?}");
}

/// A source repository of `refs` branches, each at its own commit of one
/// linear history (each commit changes one file), packed but for one in
/// sixteen left loose. Returns its path.
fn distinct_source(root: &Path, refs: usize) -> PathBuf {
    let path = root.join("source");
    run(
        root,
        &["init", "--quiet", "--template=", "-b", "main", "source"],
    );
    let mut stream = String::new();
    for i in 0..refs {
        let message = format!("commit {i}\n");
        let content = format!("version {i}\n");
        write!(
            stream,
            "commit refs/heads/main\nmark :{}\ncommitter Refs Scale <refs-scale@localhost> {} +0000\ndata {}\n{message}M 100644 inline f{}\ndata {}\n{content}\n",
            i + 1,
            1_759_622_400 + i,
            message.len(),
            i % 8,
            content.len(),
        )
        .unwrap();
    }
    let marks = root.join("marks");
    run_with(
        &path,
        &[
            "fast-import",
            "--quiet",
            &format!("--export-marks={}", marks.display()),
        ],
        stream.as_bytes(),
    );
    let marks = fs::read_to_string(&marks).unwrap();
    let mut by_mark = BTreeMap::new();
    for line in marks.lines() {
        let (mark, value) = line.split_once(' ').unwrap();
        by_mark.insert(mark[1..].parse::<usize>().unwrap(), value.to_owned());
    }
    let mut packed = String::from("# pack-refs with: sorted \n");
    let mut loose = Vec::new();
    for i in 0..refs {
        let name = format!("refs/heads/d{i:06}");
        let value = &by_mark[&(i + 1)];
        if i % 16 == 5 {
            loose.extend_from_slice(format!("create {name}\0{value}\0").as_bytes());
        } else {
            writeln!(packed, "{value} {name}").unwrap();
        }
    }
    fs::write(path.join(".git/packed-refs"), packed).unwrap();
    run_with(&path, &["update-ref", "--stdin", "-z"], &loose);
    run(&path, &["reset", "--quiet", "--hard", "main"]);
    path
}

/// The source's refs as a capture reads them: name to object.
fn inventory(source: &Path) -> BTreeMap<String, String> {
    run(
        source,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect()
}

fn header_len(bundle: &Path) -> usize {
    let bytes = fs::read(bundle).unwrap();
    bytes.windows(2).position(|pair| pair == b"\n\n").unwrap() + 2
}

fn tip_lines(header: &[u8]) -> usize {
    String::from_utf8_lossy(header)
        .lines()
        .filter(|line| line.contains(" refs/carry-export/ref-tip-v1/"))
        .count()
}

/// Run the agent binary's `git-import` of `bundle` into `repository`, and
/// return the CPU it and its reaped children used, from `wait4`. Its output
/// goes to files beside `repository`, so no pipe can fill while it runs.
fn timed_import(repository: &Path, bundle: &Path, log: &str) -> Duration {
    let out = repository.with_file_name(format!("{log}.out"));
    let err = repository.with_file_name(format!("{log}.err"));
    let child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args([
            OsStr::new("git-import"),
            repository.as_os_str(),
            bundle.as_os_str(),
            OsStr::new(SOURCE),
        ])
        .stdin(Stdio::null())
        .stdout(fs::File::create(&out).unwrap())
        .stderr(fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is a plain C struct of integers; all-zero is a valid
    // value, and wait4 overwrites it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is this test's own child, spawned above and not yet
        // reaped: the `Child` handle is never waited on, and std never reaps
        // on drop. Both out-pointers are valid, exclusive borrows for the
        // duration of the call.
        let reaped = unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) };
        if reaped == pid {
            break;
        }
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
    drop(child);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "git-import {log}: {}",
        fs::read_to_string(&err).unwrap_or_default()
    );
    let micros = |time: libc::timeval| {
        u64::try_from(time.tv_sec).unwrap() * 1_000_000 + u64::try_from(time.tv_usec).unwrap()
    };
    Duration::from_micros(micros(usage.ru_utime) + micros(usage.ru_stime))
}

/// Every ref under `refs/carry/v1/<SOURCE>/` of `repository`: name to object.
fn imported(repository: &Path) -> BTreeMap<String, String> {
    run(
        repository,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            &format!("refs/carry/v1/{SOURCE}/"),
        ],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect()
}

/// The import namespace `refs/carry/v1/<SOURCE>/<digest>/` that `after`
/// holds and `before` did not: exactly one.
fn new_namespace(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> String {
    let namespaces: BTreeSet<String> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .filter_map(|name| {
            let rest = name.strip_prefix(&format!("refs/carry/v1/{SOURCE}/"))?;
            let (digest, leaf) = rest.split_once('/')?;
            (leaf == "head").then(|| format!("refs/carry/v1/{SOURCE}/{digest}/"))
        })
        .collect();
    assert_eq!(namespaces.len(), 1, "{namespaces:?}");
    namespaces.into_iter().next().unwrap()
}

/// Every source ref is under `namespace` at its object, and the namespace
/// holds nothing else but the capture's own one-component metadata.
fn assert_exact(
    namespace: &str,
    source: &BTreeMap<String, String>,
    restored: &BTreeMap<String, String>,
) {
    let held: BTreeMap<&str, &String> = restored
        .iter()
        .filter_map(|(name, value)| Some((name.strip_prefix(namespace)?, value)))
        .collect();
    let missing: Vec<&String> = source
        .iter()
        .filter(|(name, value)| held.get(name.as_str()) != Some(value))
        .map(|(name, _)| name)
        .take(5)
        .collect();
    let extra: Vec<&&str> = held
        .keys()
        .filter(|leaf| leaf.contains('/') && !source.contains_key(**leaf))
        .take(5)
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "missing or different {missing:?}, unexpected {extra:?}"
    );
}

/// One distinct-heavy row: capture, the header law, a timed exact import;
/// then a chained pass (one new commit and branch), its header (thin within
/// 165 B per distinct object, or self-contained when that would be over the
/// cap), and its timed exact import on top. Returns whether the chained pass
/// stayed thin.
fn distinct_row(name: &str, refs: usize) -> bool {
    let root = root(name);
    let source = distinct_source(&root.0, refs);
    let first_refs = inventory(&source);
    let distinct = first_refs.values().collect::<BTreeSet<_>>().len();
    assert!(distinct >= refs, "{distinct} distinct objects");
    let first = export_repository(&source, &root.0.join("first")).unwrap();
    let header = header_len(&first);
    let bound = HEADER_FIXED + TIP_LINE * distinct;
    assert!(header <= shared::HEADER_CAP, "header {header} over the cap");
    assert!(header <= bound, "header {header} over {bound}");
    let destination = root.0.join("destination.git");
    run(
        &root.0,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            destination.to_str().unwrap(),
        ],
    );
    let cpu = timed_import(&destination, &first, "first");
    let budget = CPU_FIXED + CPU_PER_DISTINCT * u32::try_from(distinct).unwrap();
    let after_first = imported(&destination);
    assert_exact(
        &new_namespace(&BTreeMap::new(), &after_first),
        &first_refs,
        &after_first,
    );
    eprintln!(
        "REFS-SCALE-DISTINCT row={name} pass=1 refs={} distinct={distinct} header={header} import_cpu_ms={} budget_ms={}",
        first_refs.len(),
        cpu.as_millis(),
        budget.as_millis(),
    );
    assert!(cpu <= budget, "import CPU {cpu:?} over {budget:?}");
    // The chained pass: one more commit and branch.
    fs::write(source.join("f0"), b"after the first capture\n").unwrap();
    run(
        &source,
        &["commit", "--quiet", "-am", "after the first capture"],
    );
    run(&source, &["update-ref", "refs/heads/added", "HEAD"]);
    let second_refs = inventory(&source);
    let distinct = second_refs.values().collect::<BTreeSet<_>>().len();
    let export = export_repository_with_drift(
        &source,
        &root.0.join("second"),
        &ExportOptions {
            chain: Some(&first),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let header = header_len(&export.bundle);
    let prerequisites = shared::prerequisites(&export.bundle).unwrap();
    assert_eq!(export.chained, !prerequisites.is_empty());
    assert!(header <= shared::HEADER_CAP, "header {header} over the cap");
    let bytes = fs::read(&export.bundle).unwrap();
    assert_eq!(tip_lines(&bytes[..header]), distinct);
    let bound = HEADER_FIXED + TIP_LINE * distinct + PREREQUISITE_LINE * prerequisites.len();
    assert!(header <= bound, "chained header {header} over {bound}");
    if export.chained {
        assert!(
            header <= HEADER_FIXED + (TIP_LINE + PREREQUISITE_LINE) * distinct,
            "thin header {header} over 165 B per distinct object"
        );
    } else {
        // Self-contained only because the thin header would be over the cap.
        assert!((TIP_LINE + PREREQUISITE_LINE) * (distinct - 1) > shared::HEADER_CAP);
    }
    let cpu = timed_import(&destination, &export.bundle, "second");
    let budget = CPU_FIXED + CPU_PER_DISTINCT * u32::try_from(distinct).unwrap();
    let after_second = imported(&destination);
    assert_exact(
        &new_namespace(&after_first, &after_second),
        &second_refs,
        &after_second,
    );
    eprintln!(
        "REFS-SCALE-DISTINCT row={name} pass=2 chained={} refs={} distinct={distinct} header={header} prerequisites={} import_cpu_ms={} budget_ms={}",
        export.chained,
        second_refs.len(),
        prerequisites.len(),
        cpu.as_millis(),
        budget.as_millis(),
    );
    assert!(cpu <= budget, "chained import CPU {cpu:?} over {budget:?}");
    export.chained
}

/// The CI row: 32,768 refs, each at its own commit. The chained pass stays
/// thin (about 5.4 MB of header, under the cap).
#[test]
fn distinct_heavy_32768_refs_import_linearly_and_chain_thin() {
    assert!(distinct_row("ci", 32_768), "the chained pass fits thin");
}

/// The deep row (`BULKLOAD_PROPTEST_DEEP=1`): 110,000 distinct commits. The
/// chained pass's thin header would be over the cap, so it is written
/// self-contained, and imports exactly; before, it refused
/// `GIT_INVENTORY_OVER_CAP` on every pass.
#[test]
fn distinct_heavy_110000_refs_chain_falls_back_self_contained() {
    if std::env::var_os(DEEP).is_none_or(|value| value != "1") {
        eprintln!("REFS-SCALE-DISTINCT row=deep skipped: set {DEEP}=1");
        return;
    }
    assert!(
        !distinct_row("deep", 110_000),
        "the chained pass falls back self-contained"
    );
}
