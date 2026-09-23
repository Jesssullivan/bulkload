//! W6 M1 git-internals spike (R-N93; #48, TIN-4545).
//!
//! Fixture-only evidence for the plan's "To confirm in an M1 spike" list under
//! "D2 Git carry v2". Every test builds its repositories in a fresh directory
//! under the system temp dir, runs the real `git` on `PATH`, and removes the
//! directory afterwards. Nothing here is wired into a product verb, and no
//! test reads or writes a repository it did not create.
//!
//! Questions (numbers in docs/evidence/w6-m1-spike-2026-09-23.md):
//! 1. `rev-list --objects-edge | pack-objects --stdout` (list mode, `-<oid>`
//!    edge lines) against #55's revs-mode `missing_thin_pack_bytes`.
//! 2. Self-contained thin segments, their byte cost and the resume claim.
//! 3. receive-pack's tmp-objdir quarantine, the refs-only connectivity check,
//!    `.idx`-last migration and one `update-ref --stdin -z` transaction.
//! 4. A state commit with 1,000 and 2,000 parents.
//! 5. `fast-import` `M <mode> <oid> <path>` naming an alternates-only object.
//! 6. Byte identity of the state commit on an unchanged re-run.
//!
//! Measurements are printed as `m1 <question> key=value ...` lines; run
//! `cargo test -p bulkload-agent --test git_m1_spike -- --nocapture` to see
//! them.
//!
//! #55's hardened invocation (`git_carry/estimate.rs::hardened` and
//! `thin_pack`) is reproduced here from a read of its branch, not
//! cherry-picked, so this file depends on nothing unmerged.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Command builder
// ---------------------------------------------------------------------------

/// Everything `git()` in `git_carry.rs` strips, plus `GIT_QUARANTINE_PATH`.
/// Nothing the parent process exports can redirect a call's objects, refs or
/// configuration; the builder sets the object-directory triple itself.
const STRIPPED: [&str; 10] = [
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
];

/// Where a call's new objects land.
#[derive(Clone, Copy)]
enum Objects<'a> {
    /// The repository's own object store. Refs may be updated.
    Repository,
    /// receive-pack's tmp-objdir: new objects go to `dir`, reads fall through
    /// to `main` as an alternate, and Git itself refuses every ref update
    /// while `GIT_QUARANTINE_PATH` is set.
    Quarantine { dir: &'a Path, main: &'a Path },
}

/// The command builder the plan asks for (D2 "Destination ingest").
///
/// It is `git()` from `git_carry.rs` plus #55's `hardened()` additions
/// (`maintenance.auto=false`, `GIT_NO_LAZY_FETCH=1`, `GIT_OPTIONAL_LOCKS=0`)
/// and #55's `pack.useSparse=false` and `pack.useBitmaps=false` (re-review). The difference from `git()` is the
/// explicit [`Objects`] scope: the three quarantine variables are stripped
/// like every other redirecting variable and then set only from the typed
/// scope, never inherited.
///
/// `pack.threads=1` (not `git()`'s 2) so pack bytes are a function of the
/// object list alone; Q1 measures 2 as well.
fn git_in(repo: &Path, objects: Objects<'_>) -> Command {
    let mut command = Command::new("git");
    for key in STRIPPED {
        command.env_remove(key);
    }
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "pack.threads=1",
            "-c",
            "pack.windowMemory=64m",
            "-c",
            "pack.useSparse=false",
            "-c",
            "pack.useBitmaps=false",
            "-c",
            "init.defaultBranch=main",
            "-C",
        ])
        .arg(repo);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Fixed identity and date: fixture commits and the state commit S
        // are deterministic (Q6).
        .env("GIT_AUTHOR_NAME", "bulkload")
        .env("GIT_AUTHOR_EMAIL", "bulkload@invalid")
        .env("GIT_COMMITTER_NAME", "bulkload")
        .env("GIT_COMMITTER_EMAIL", "bulkload@invalid")
        .env("GIT_AUTHOR_DATE", "1790121600 +0000")
        .env("GIT_COMMITTER_DATE", "1790121600 +0000");
    if let Objects::Quarantine { dir, main } = objects {
        command
            .env("GIT_OBJECT_DIRECTORY", dir)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", main)
            .env("GIT_QUARANTINE_PATH", dir);
    }
    command
}

fn git(repo: &Path) -> Command {
    git_in(repo, Objects::Repository)
}

/// Run to completion with `stdin`, capturing stdout and stderr.
fn feed(mut command: Command, stdin: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    let mut pipe = child.stdin.take().expect("stdin");
    let input = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        // A child that exits early closes the pipe; that is its answer.
        let _ = pipe.write_all(&input);
    });
    let output = child.wait_with_output().expect("wait git");
    writer.join().expect("writer");
    output
}

fn ok(output: Output, what: &str) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn run(mut command: Command, what: &str) -> Vec<u8> {
    ok(command.output().expect("spawn git"), what)
}

fn text(command: Command, what: &str) -> String {
    String::from_utf8(run(command, what))
        .expect("utf8")
        .trim_end()
        .to_owned()
}

fn args<const N: usize>(repo: &Path, list: [&str; N]) -> Command {
    let mut command = git(repo);
    command.args(list);
    command
}

fn rev(repo: &Path, name: &str) -> String {
    text(
        args(repo, ["rev-parse", "--verify", "-q", name]),
        "rev-parse",
    )
}

fn ms(start: Instant) -> u128 {
    start.elapsed().as_millis()
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// A temp directory removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!(
            "bulkload-m1-{name}-{}-{}-{nanos}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("scratch");
        // Canonical, so alternates and GIT_OBJECT_DIRECTORY paths agree with
        // what Git resolves (macOS /var -> /private/var).
        let root = root.canonicalize().expect("canonical scratch");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn init(&self, name: &str, bare: bool) -> PathBuf {
        let path = self.path(name);
        let mut command = git(&self.root);
        command.args(["init", "-q", "-b", "main"]);
        if bare {
            command.arg("--bare");
        }
        command.arg(&path);
        run(command, "init");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// xorshift64*: deterministic fixture content with no dependency.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next().to_le_bytes()[0]).collect()
    }
}

fn lines(rng: &mut Rng, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("line {i} {:016x}\n", rng.next()))
        .collect()
}

fn write(repo: &Path, relative: &str, bytes: &[u8]) {
    let path = repo.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn commit_all(repo: &Path, message: &str) -> String {
    run(args(repo, ["add", "-A"]), "add");
    run(
        args(repo, ["commit", "-q", "--allow-empty", "-m", message]),
        "commit",
    );
    rev(repo, "HEAD")
}

/// Give `destination` (bare) the objects of `oid` under `reference`, via a
/// local push. `receive.unpackLimit` is raised so the objects land loose and
/// `prune` can remove them in Q3's negative cases.
fn push(source: &Path, destination: &Path, oid: &str, reference: &str) {
    run(
        args(destination, ["config", "receive.unpackLimit", "100000"]),
        "config",
    );
    let mut command = git(source);
    command
        .args(["push", "-q"])
        .arg(destination)
        .arg(format!("{oid}:{reference}"));
    run(command, "push");
}

fn objects_dir(bare: &Path) -> PathBuf {
    bare.join("objects")
}

fn loose_path(bare: &Path, oid: &str) -> PathBuf {
    objects_dir(bare).join(&oid[..2]).join(&oid[2..])
}

fn packs(bare: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(objects_dir(bare).join("pack"))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "pack"))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn fsck(repo: &Path) -> Output {
    args(repo, ["fsck", "--strict", "--no-progress", "--no-dangling"])
        .output()
        .expect("fsck")
}

fn has_object(repo: &Path, oid: &str) -> bool {
    args(repo, ["cat-file", "-e", oid])
        .status()
        .expect("cat-file")
        .success()
}

// ---------------------------------------------------------------------------
// Pack plans
// ---------------------------------------------------------------------------

/// `rev-list`/`pack-objects --revs` stdin: wants, then `--not` and haves.
fn request(wants: &[String], haves: &[String]) -> String {
    let mut out = String::new();
    for want in wants {
        out.push_str(want);
        out.push('\n');
    }
    if !haves.is_empty() {
        out.push_str("--not\n");
        for have in haves {
            out.push_str(have);
            out.push('\n');
        }
    }
    out
}

/// #55's missing set: `rev-list --objects --no-object-names --missing=print`.
fn missing(source: &Path, request: &str) -> BTreeSet<String> {
    let mut command = git(source);
    command.args([
        "rev-list",
        "--objects",
        "--no-object-names",
        "--missing=print",
        "--stdin",
    ]);
    let out = ok(feed(command, request.as_bytes()), "rev-list missing");
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The destination shape that selects the edge rule (R-N97). upload-pack
/// passes `--shallow` to pack-objects for a shallow client, and `--shallow`
/// selects `--objects-edge-aggressive`; a full client gets `--objects-edge`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Full,
    Shallow,
}

/// #55's `missing_thin_pack_bytes`, reproduced from its `thin_pack()`: revs
/// mode, `--thin`, `--missing=allow-any`, `pack.useSparse=false`, plus the
/// re-review pins (`pack.useBitmaps=false` from [`git_in`], and `--shallow`
/// for a shallow destination).
fn oracle_pack(source: &Path, request: &str, threads: u8, shape: Shape) -> Vec<u8> {
    let mut command = git(source);
    command
        .args(["-c", &format!("pack.threads={threads}")])
        .args([
            "pack-objects",
            "--stdout",
            "--thin",
            "--revs",
            "--delta-base-offset",
            "--missing=allow-any",
            "-q",
        ]);
    if shape == Shape::Shallow {
        command.arg("--shallow");
    }
    ok(feed(command, request.as_bytes()), "pack-objects --revs")
}

/// The plan's object list: `rev-list --objects-edge[-aggressive]
/// --missing=allow-any`, split into `-<oid>` edge lines and `<oid>[ <path>]`
/// object lines.
struct EdgeList {
    edges: Vec<String>,
    objects: Vec<String>,
}

impl EdgeList {
    fn lines(&self) -> Vec<String> {
        self.edges.iter().chain(&self.objects).cloned().collect()
    }

    fn oids(&self) -> BTreeSet<String> {
        self.objects.iter().map(|l| oid_of(l).to_owned()).collect()
    }
}

fn oid_of(line: &str) -> &str {
    line.split(' ').next().unwrap_or(line)
}

fn edge_list(source: &Path, request: &str, shape: Shape) -> EdgeList {
    let edge = match shape {
        Shape::Full => "--objects-edge",
        Shape::Shallow => "--objects-edge-aggressive",
    };
    let mut command = git(source);
    command.args(["rev-list", edge, "--missing=allow-any", "--stdin"]);
    let out = String::from_utf8(ok(feed(command, request.as_bytes()), "rev-list edge")).unwrap();
    let (edges, objects) = out
        .lines()
        .map(str::to_owned)
        .partition(|l| l.starts_with('-'));
    EdgeList { edges, objects }
}

/// Exact reachability, `wants` minus everything the haves reach:
/// `rev-list --objects --use-bitmap-index`. With a bitmap this is what
/// upload-pack sends; without one it is the ordinary walk.
fn exact(source: &Path, request: &str) -> BTreeSet<String> {
    let mut command = git(source);
    command.args([
        "rev-list",
        "--objects",
        "--no-object-names",
        "--use-bitmap-index",
        "--stdin",
    ]);
    let out = ok(feed(command, request.as_bytes()), "rev-list exact");
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The sender's list: the edge list restricted to the exact set. Returns the
/// list and how many walk objects the exact set dropped.
fn plan(source: &Path, request: &str, shape: Shape) -> (EdgeList, usize) {
    let mut list = edge_list(source, request, shape);
    let keep = exact(source, request);
    let before = list.objects.len();
    list.objects.retain(|l| keep.contains(oid_of(l)));
    let dropped = before - list.objects.len();
    (list, dropped)
}

/// List mode. `--thin` is NOT passed: in pack-objects `--thin` switches on the
/// internal rev-list, which would read these lines as revisions (`fatal: not a
/// rev '-<oid>'`). Thinness comes from the `-<oid>` lines, which pack-objects
/// turns into preferred bases.
fn list_pack(source: &Path, lines: &[String], threads: u8) -> Vec<u8> {
    let mut command = git(source);
    command
        .args(["-c", &format!("pack.threads={threads}")])
        .args([
            "pack-objects",
            "--stdout",
            "--delta-base-offset",
            "--missing=allow-any",
            "-q",
        ]);
    let mut input = String::new();
    for line in lines {
        input.push_str(line);
        input.push('\n');
    }
    ok(feed(command, input.as_bytes()), "pack-objects list")
}

/// A plain Git for the real-fetch oracle: the redirecting environment,
/// hooks, global config and auto-maintenance are fenced off, but no `pack.*`
/// pin is set, so upload-pack packs with Git's defaults.
fn vanilla(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in STRIPPED {
        command.env_remove(key);
    }
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "fetch.writeCommitGraph=false",
            "-C",
        ])
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

/// What a real `git fetch` sends for `wants` to a copy of `destination`
/// (whose refs are the haves): the received pack, captured verbatim with
/// `GIT_TRACE_PACKFILE`. Empty when the fetch receives no pack.
fn fetched_pack(scratch: &Scratch, source: &Path, destination: &Path, wants: &[String]) -> Vec<u8> {
    let copy = scratch.path("fetch-copy.git");
    copy_dir(destination, &copy);
    let mut refs = String::new();
    for (i, want) in wants.iter().enumerate() {
        let _ = writeln!(refs, "create refs/m1-want/{i} {want}");
    }
    ok(
        feed(args(source, ["update-ref", "--stdin"]), refs.as_bytes()),
        "want refs",
    );
    let trace = scratch.path("fetch.pack");
    let mut command = vanilla(&copy);
    command
        .args(["fetch", "-q", "--no-tags"])
        .arg(format!("file://{}", source.display()))
        .arg("refs/m1-want/*:refs/m1-fetched/*")
        .env("GIT_TRACE_PACKFILE", &trace);
    run(command, "real fetch");
    let mut drop = String::new();
    for i in 0..wants.len() {
        let _ = writeln!(drop, "delete refs/m1-want/{i}");
    }
    ok(
        feed(args(source, ["update-ref", "--stdin"]), drop.as_bytes()),
        "drop want refs",
    );
    let pack = fs::read(&trace).unwrap_or_default();
    let _ = fs::remove_file(&trace);
    let _ = fs::remove_dir_all(&copy);
    pack
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The oids a thin pack carries: index it with `--fix-thin` into a throwaway
/// quarantine on `destination` and keep the objects whose offset lies inside
/// the received bytes (`--fix-thin` appends its bases after them).
fn carried(destination: &Path, pack: &[u8]) -> BTreeSet<String> {
    if pack.is_empty() || header_count(pack) == 0 {
        return BTreeSet::new();
    }
    let quarantine = Quarantine::open(destination);
    let hash = quarantine.index(pack).unwrap();
    let idx = quarantine.dir.join("pack").join(format!("pack-{hash}.idx"));
    let listing = ok(
        feed(args(destination, ["show-index"]), &fs::read(idx).unwrap()),
        "show-index",
    );
    quarantine.abandon();
    let limit = pack.len() - 20;
    String::from_utf8(listing)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let offset: usize = fields.next()?.parse().ok()?;
            let oid = fields.next()?;
            (offset < limit).then(|| oid.to_owned())
        })
        .collect()
}

fn header_count(pack: &[u8]) -> u32 {
    assert_eq!(&pack[..4], b"PACK", "pack signature");
    u32::from_be_bytes(pack[8..12].try_into().unwrap())
}

/// Object count of a v2 `.idx` (fanout[255]).
fn idx_count(idx: &Path) -> u32 {
    let bytes = fs::read(idx).unwrap();
    u32::from_be_bytes(bytes[8 + 255 * 4..8 + 256 * 4].try_into().unwrap())
}

// ---------------------------------------------------------------------------
// Quarantine ingest (Q3)
// ---------------------------------------------------------------------------

/// Why an ingest stopped. Nothing is published in any of these cases.
#[derive(Debug)]
enum Refused {
    Segment(usize, String),
    Connectivity(String),
    Refs(String),
}

impl Refused {
    /// The stage that refused and Git's own words.
    fn describe(&self) -> String {
        match self {
            Self::Segment(index, detail) => format!("segment[{index}]: {detail}"),
            Self::Connectivity(detail) => format!("connectivity: {detail}"),
            Self::Refs(detail) => format!("refs: {detail}"),
        }
    }
}

/// receive-pack's tmp-objdir, owned by bulkload: `objects/incoming-*`.
struct Quarantine {
    repo: PathBuf,
    main: PathBuf,
    dir: PathBuf,
}

impl Quarantine {
    fn open(repo: &Path) -> Self {
        let main = objects_dir(repo);
        let dir = main.join(format!(
            "incoming-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("pack")).unwrap();
        Self {
            repo: repo.to_owned(),
            main,
            dir,
        }
    }

    fn command(&self) -> Command {
        git_in(
            &self.repo,
            Objects::Quarantine {
                dir: &self.dir,
                main: &self.main,
            },
        )
    }

    /// `index-pack --stdin --fix-thin --keep` into the quarantine. Returns the
    /// pack hash.
    fn index(&self, pack: &[u8]) -> Result<String, String> {
        let mut command = self.command();
        command.args(["index-pack", "--stdin", "--fix-thin", "--keep=bulkload-m1"]);
        let output = feed(command, pack);
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let out = String::from_utf8(output.stdout).unwrap();
        let hash = out
            .trim()
            .strip_prefix("keep\t")
            .ok_or_else(|| format!("index-pack said {out:?}"))?;
        Ok(hash.to_owned())
    }

    /// The connectivity check that trusts only refs (receive-pack's
    /// `check_connected`): walk the new tips until every path reaches an
    /// object reachable from an existing ref.
    fn connected(&self, tips: &[String]) -> Result<(), String> {
        let mut command = self.command();
        command.args([
            "rev-list",
            "--objects",
            "--stdin",
            "--not",
            "--all",
            "--quiet",
        ]);
        let mut input = tips.join("\n");
        input.push('\n');
        let output = feed(command, input.as_bytes());
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// Leftovers other than finished packs (e.g. `tmp_pack_*` after a failed
    /// `index-pack`).
    fn leftovers(&self) -> Vec<String> {
        fs::read_dir(self.dir.join("pack"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| !n.starts_with("pack-"))
            .collect()
    }

    fn sweep(&self) {
        for name in self.leftovers() {
            fs::remove_file(self.dir.join("pack").join(name)).unwrap();
        }
    }

    /// Move each pack into `objects/pack`: `.keep` first so the pack is kept
    /// from the moment it is visible, `.pack`, `.rev`, then `.idx` last
    /// (Git finds a pack by its `.idx`). One fsync of the directory.
    fn migrate(self, hashes: &[String]) -> std::io::Result<()> {
        let target = self.main.join("pack");
        for hash in hashes {
            for ext in ["keep", "pack", "rev", "idx"] {
                let name = format!("pack-{hash}.{ext}");
                let from = self.dir.join("pack").join(&name);
                if from.exists() {
                    fs::rename(&from, target.join(&name))?;
                }
            }
        }
        fs::File::open(&target)?.sync_all()?;
        fs::remove_dir_all(&self.dir)?;
        fs::File::open(&self.main)?.sync_all()
    }

    fn abandon(self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// One `update-ref --stdin -z` transaction outside any quarantine.
fn publish(repo: &Path, updates: &[Update]) -> Result<(), String> {
    let mut input = Vec::new();
    for update in updates {
        match update {
            Update::Create(name, oid) => {
                input.extend_from_slice(format!("create {name}\0{oid}\0").as_bytes());
            }
            Update::Verify(name, oid) => {
                input.extend_from_slice(format!("verify {name}\0{oid}\0").as_bytes());
            }
        }
    }
    let mut command = git(repo);
    command.args(["update-ref", "--stdin", "-z"]);
    let output = feed(command, &input);
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

enum Update {
    Create(String, String),
    Verify(String, String),
}

fn drop_keeps(repo: &Path, hashes: &[String]) {
    for hash in hashes {
        fs::remove_file(
            objects_dir(repo)
                .join("pack")
                .join(format!("pack-{hash}.keep")),
        )
        .unwrap();
    }
}

/// The whole destination sequence: every segment into one quarantine, the
/// connectivity check, migration, one ref transaction, then the `.keep`s go.
fn ingest(
    repo: &Path,
    segments: &[Vec<u8>],
    tips: &[String],
    updates: &[Update],
) -> Result<Vec<String>, Refused> {
    let quarantine = Quarantine::open(repo);
    let mut hashes = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        match quarantine.index(segment) {
            Ok(hash) => hashes.push(hash),
            Err(error) => {
                quarantine.abandon();
                return Err(Refused::Segment(index, error));
            }
        }
    }
    if let Err(error) = quarantine.connected(tips) {
        quarantine.abandon();
        return Err(Refused::Connectivity(error));
    }
    quarantine
        .migrate(&hashes)
        .map_err(|e| Refused::Refs(e.to_string()))?;
    publish(repo, updates).map_err(Refused::Refs)?;
    drop_keeps(repo, &hashes);
    Ok(hashes)
}

// ---------------------------------------------------------------------------
// Q1: pack-objects list mode with edges, judged against a real fetch (R-N97)
// ---------------------------------------------------------------------------

struct Case<'a> {
    name: &'a str,
    scratch: &'a Scratch,
    source: &'a Path,
    destination: &'a Path,
    wants: &'a [String],
    haves: &'a [String],
    shape: Shape,
}

/// One Q1 case. Gate (R-N74 as amended by R-N97): the sent object set equals
/// what a real `git fetch` with the same haves sends; sent objects <= the
/// estimate's; sent bytes <= 1.1x `missing_thin_pack_bytes`. Then the pack is
/// ingested through the quarantine.
fn q1_case(case: &Case<'_>) {
    let name = case.name;
    let request = request(case.wants, case.haves);
    let walk = missing(case.source, &request);
    assert!(
        walk.iter().all(|l| !l.starts_with('?')),
        "{name}: complete source"
    );

    let estimate = oracle_pack(case.source, &request, 1, case.shape);
    let estimate2 = oracle_pack(case.source, &request, 2, case.shape);
    let (list, dropped) = plan(case.source, &request, case.shape);
    let listed = list_pack(case.source, &list.lines(), 1);
    let listed2 = list_pack(case.source, &list.lines(), 2);
    let fetched = fetched_pack(case.scratch, case.source, case.destination, case.wants);

    let sent = carried(case.destination, &listed);
    let by_fetch = carried(case.destination, &fetched);
    assert_eq!(
        sent,
        list.oids(),
        "{name}: the pack carries exactly the list"
    );
    assert_eq!(sent, by_fetch, "{name}: R-N97 sent set == real fetch set");
    assert_eq!(carried(case.destination, &listed2), sent, "{name}: t2 set");
    let count = header_count(&listed);
    assert!(
        count <= header_count(&estimate),
        "{name}: R-N97 count bound"
    );
    assert!(
        listed.len() * 10 <= estimate.len() * 11,
        "{name}: byte gate"
    );
    assert!(
        listed2.len() * 10 <= estimate2.len() * 11,
        "{name}: byte gate t2"
    );
    if count == header_count(&estimate) {
        assert_eq!(listed, estimate, "{name}: same set gives the same bytes");
    }
    let fetch_count = if fetched.is_empty() {
        0
    } else {
        header_count(&fetched)
    };
    println!(
        "m1 q1 case={name} shape={:?} walk_objects={} estimate_objects={} estimate_bytes={} \
         sent_objects={count} sent_bytes={} fetch_objects={fetch_count} fetch_bytes={} \
         sent_set_eq_fetch=true exact_dropped={dropped} edges={} byte_equal_estimate={} \
         estimate_bytes_t2={} sent_bytes_t2={} byte_equal_estimate_t2={}",
        case.shape,
        walk.len(),
        header_count(&estimate),
        estimate.len(),
        listed.len(),
        fetched.len(),
        list.edges.len(),
        listed == estimate,
        estimate2.len(),
        listed2.len(),
        listed2 == estimate2,
    );

    let before = packs(case.destination).len();
    let updates: Vec<Update> = case
        .wants
        .iter()
        .enumerate()
        .map(|(i, oid)| Update::Create(format!("refs/carry/v1/spike/{name}/{i}"), oid.clone()))
        .collect();
    let hashes = ingest(case.destination, &[listed], case.wants, &updates)
        .unwrap_or_else(|e| panic!("{name}: ingest refused: {}", e.describe()));
    let idx = objects_dir(case.destination)
        .join("pack")
        .join(format!("pack-{}.idx", hashes[0]));
    let appended = idx_count(&idx) - count;
    let fsck = fsck(case.destination);
    assert!(
        fsck.status.success(),
        "{name}: fsck: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    assert_eq!(packs(case.destination).len(), before + 1);
    println!("m1 q1 case={name} ingest=ok fix_thin_appended_bases={appended} fsck=clean");
}

fn delta_history(repo: &Path, seed: u64, files: usize, rounds: usize) -> Vec<String> {
    let mut rng = Rng(seed);
    let mut contents: Vec<Vec<String>> = (0..files).map(|_| lines(&mut rng, 1500)).collect();
    for (i, content) in contents.iter().enumerate() {
        write(
            repo,
            &format!("src/file{i}.txt"),
            content.concat().as_bytes(),
        );
    }
    let mut commits = vec![commit_all(repo, "base")];
    for round in 0..rounds {
        for _ in 0..3 {
            let file = rng.below(files);
            for _ in 0..40 {
                let line = rng.below(1500);
                contents[file][line] = format!("edit {round} {:016x}\n", rng.next());
            }
            write(
                repo,
                &format!("src/file{file}.txt"),
                contents[file].concat().as_bytes(),
            );
        }
        commits.push(commit_all(repo, &format!("round {round}")));
    }
    commits
}

/// A bare destination holding `have` under `refs/heads/main`.
fn destination_at(scratch: &Scratch, source: &Path, have: &str) -> PathBuf {
    let destination = scratch.init("destination.git", true);
    push(source, &destination, have, "refs/heads/main");
    destination
}

fn clone(scratch: &Scratch, origin: &Path, name: &str, depth: Option<u32>, bare: bool) -> PathBuf {
    let mut command = git(&scratch.root);
    command.args(["clone", "-q"]);
    if bare {
        command.arg("--bare");
    }
    if let Some(depth) = depth {
        command.args(["--depth", &depth.to_string()]);
    }
    command
        .arg(format!("file://{}", origin.display()))
        .arg(name);
    run(command, "clone");
    scratch.path(name)
}

#[test]
fn q1_delta_heavy() {
    let scratch = Scratch::new("q1-delta");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 7, 4, 12);
    let destination = destination_at(&scratch, &source, &commits[4]);
    q1_case(&Case {
        name: "delta-heavy",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: std::slice::from_ref(&commits[12]),
        haves: std::slice::from_ref(&commits[4]),
        shape: Shape::Full,
    });
}

#[test]
fn q1_rename_heavy() {
    let scratch = Scratch::new("q1-rename");
    let source = scratch.init("source", false);
    let mut rng = Rng(11);
    for i in 0..30 {
        write(
            &source,
            &format!("a/f{i}.txt"),
            lines(&mut rng, 120).concat().as_bytes(),
        );
    }
    let base = commit_all(&source, "base");
    run(args(&source, ["mv", "a", "b"]), "mv");
    for i in 0..30 {
        let path = source.join(format!("b/f{i}.txt"));
        let mut body = fs::read_to_string(&path).unwrap();
        let _ = writeln!(body, "renamed {:016x}", rng.next());
        fs::write(&path, body).unwrap();
    }
    commit_all(&source, "rename a -> b with edits");
    fs::create_dir_all(source.join("c")).unwrap();
    run(args(&source, ["mv", "b", "c/d"]), "mv");
    let tip = commit_all(&source, "rename b -> c/d");
    let destination = destination_at(&scratch, &source, &base);
    q1_case(&Case {
        name: "rename-heavy",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[tip],
        haves: &[base],
        shape: Shape::Full,
    });
}

#[test]
fn q1_tag_only() {
    let scratch = Scratch::new("q1-tag");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 3, 2, 2);
    let tip = commits.last().unwrap().clone();
    let destination = destination_at(&scratch, &source, &tip);
    run(
        args(&source, ["tag", "-a", "-m", "release", "v1", &tip]),
        "tag",
    );
    run(
        args(
            &source,
            ["tag", "-a", "-m", "tag of tag", "v1-signed", "v1"],
        ),
        "tag",
    );
    let wants = [
        rev(&source, "refs/tags/v1"),
        rev(&source, "refs/tags/v1-signed"),
    ];
    q1_case(&Case {
        name: "tag-only",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &wants,
        haves: &[tip],
        shape: Shape::Full,
    });
}

#[test]
fn q1_submodule_gitlink() {
    let scratch = Scratch::new("q1-gitlink");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 5, 2, 1);
    let base = commits.last().unwrap().clone();
    write(
        &source,
        ".gitmodules",
        b"[submodule \"sub\"]\n\tpath = vendor/sub\n\turl = https://example.invalid/sub.git\n",
    );
    // A gitlink to a commit this repository does not hold.
    run(
        args(
            &source,
            [
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,vendor/sub",
            ],
        ),
        "gitlink",
    );
    let tip = commit_all(&source, "add submodule gitlink");
    let destination = destination_at(&scratch, &source, &base);
    q1_case(&Case {
        name: "submodule-gitlink",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[tip],
        haves: &[base],
        shape: Shape::Full,
    });
}

#[test]
fn q1_matching_shallow_frontier() {
    let scratch = Scratch::new("q1-shallow");
    let origin = scratch.init("origin", false);
    delta_history(&origin, 13, 2, 8);
    let source = clone(&scratch, &origin, "source", Some(3), false);
    let destination = clone(&scratch, &origin, "destination.git", Some(3), true);
    let frontier = fs::read(source.join(".git/shallow")).unwrap();
    assert_eq!(frontier, fs::read(destination.join("shallow")).unwrap());
    let have = rev(&destination, "refs/heads/main");
    let mut rng = Rng(17);
    for i in 0..3 {
        write(
            &source,
            "src/file0.txt",
            lines(&mut rng, 1500).concat().as_bytes(),
        );
        commit_all(&source, &format!("shallow work {i}"));
    }
    let tip = rev(&source, "HEAD");
    q1_case(&Case {
        name: "matching-shallow",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[tip],
        haves: &[have],
        shape: Shape::Shallow,
    });
    // The destination's frontier is untouched by ingest.
    assert_eq!(frontier, fs::read(destination.join("shallow")).unwrap());
}

/// A want that forks below the have and takes the have's tree: the edge is
/// the fork point, not the have, so `--objects-edge` counts objects only the
/// have's tree holds. `--objects-edge-aggressive` also marks the have's tree.
fn fork_below_have(scratch: &Scratch, depth: Option<u32>) -> (PathBuf, PathBuf, String, String) {
    let origin = scratch.init("origin", false);
    delta_history(&origin, 19, 3, 8);
    let source = clone(scratch, &origin, "source", depth, false);
    let destination = clone(scratch, &origin, "destination.git", depth, true);
    let have = rev(&destination, "refs/heads/main");
    run(
        args(&source, ["checkout", "-q", "-b", "side", "HEAD~1"]),
        "checkout",
    );
    run(
        args(&source, ["checkout", "-q", "main", "--", "."]),
        "take main's tree",
    );
    commit_all(&source, "side takes main's tree");
    write(&source, "side.txt", b"side\n");
    let want = commit_all(&source, "side work");
    (source, destination, want, have)
}

#[test]
fn q1_fork_below_have_full_destination() {
    let scratch = Scratch::new("q1-fork-full");
    let (source, destination, want, have) = fork_below_have(&scratch, None);
    q1_case(&Case {
        name: "fork-below-have-full",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[want],
        haves: &[have],
        shape: Shape::Full,
    });
}

/// Re-review pin: a shallow destination needs `--shallow` /
/// `--objects-edge-aggressive`, as upload-pack does. The plain edge rule
/// over-counts here.
#[test]
fn q1_fork_below_have_shallow_destination() {
    let scratch = Scratch::new("q1-fork-shallow");
    let (source, destination, want, have) = fork_below_have(&scratch, Some(3));
    let request = request(std::slice::from_ref(&want), std::slice::from_ref(&have));
    let plain = edge_list(&source, &request, Shape::Full).objects.len();
    let aggressive = edge_list(&source, &request, Shape::Shallow).objects.len();
    let unpinned = header_count(&oracle_pack(&source, &request, 1, Shape::Full));
    println!(
        "m1 q1 case=fork-below-have-shallow objects_edge={plain} objects_edge_aggressive={aggressive} \
         estimate_without_shallow_pin={unpinned}"
    );
    assert!(aggressive < plain, "the plain edge rule over-counts");
    q1_case(&Case {
        name: "fork-below-have-shallow",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[want],
        haves: &[have],
        shape: Shape::Shallow,
    });
}

/// Re-review pin: `pack.useBitmaps=false`. A reverted blob is reachable from
/// the haves only through an older commit, so the walk counts it and a
/// bitmap does not. With the bitmap on, #55's pack no longer matches its walk.
#[test]
fn q1_bitmap_source() {
    let scratch = Scratch::new("q1-bitmap");
    let source = scratch.init("source", false);
    let mut rng = Rng(23);
    let original = lines(&mut rng, 800).concat();
    write(&source, "f.txt", original.as_bytes());
    write(&source, "g.txt", b"steady\n");
    commit_all(&source, "c1");
    write(&source, "f.txt", lines(&mut rng, 800).concat().as_bytes());
    let have = commit_all(&source, "c2");
    write(&source, "f.txt", original.as_bytes());
    write(&source, "h.txt", b"new\n");
    let want = commit_all(&source, "c3 reverts f.txt");
    run(args(&source, ["repack", "-adbq"]), "repack with bitmap");
    let bitmaps = fs::read_dir(source.join(".git/objects/pack"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "bitmap"))
        })
        .count();
    assert_eq!(bitmaps, 1, "the source has a bitmap");
    let destination = destination_at(&scratch, &source, &have);
    let request = request(std::slice::from_ref(&want), std::slice::from_ref(&have));
    let walk = missing(&source, &request).len();
    let mut unpinned = git(&source);
    unpinned.args([
        "-c",
        "pack.useBitmaps=true",
        "pack-objects",
        "--stdout",
        "--thin",
        "--revs",
        "--delta-base-offset",
        "-q",
    ]);
    let unpinned = header_count(&ok(feed(unpinned, request.as_bytes()), "unpinned"));
    println!(
        "m1 q1 case=bitmap-source walk_objects={walk} estimate_without_bitmap_pin={unpinned} \
         exact_objects={}",
        exact(&source, &request).len()
    );
    assert!(
        unpinned < u32::try_from(walk).unwrap(),
        "bitmap and walk disagree"
    );
    q1_case(&Case {
        name: "bitmap-source",
        scratch: &scratch,
        source: &source,
        destination: &destination,
        wants: &[want],
        haves: &[have],
        shape: Shape::Full,
    });
}

// ---------------------------------------------------------------------------
// Q2: segmentation
// ---------------------------------------------------------------------------

/// Object lines with their type and stored size, from `cat-file --batch-check`.
struct Sized {
    line: String,
    rank: u8,
    path: String,
    disk: u64,
}

fn annotate(source: &Path, list: &EdgeList) -> Vec<Sized> {
    let mut command = git(source);
    command.args([
        "cat-file",
        "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
    ]);
    let mut input = String::new();
    for line in &list.objects {
        input.push_str(oid_of(line));
        input.push('\n');
    }
    let out = String::from_utf8(ok(feed(command, input.as_bytes()), "batch-check")).unwrap();
    out.lines()
        .zip(&list.objects)
        .map(|(check, line)| {
            let mut fields = check.split(' ');
            let _ = fields.next();
            let rank = match fields.next() {
                Some("commit") => 0,
                Some("tag") => 1,
                Some("tree") => 2,
                _ => 3,
            };
            let disk = fields.next().unwrap().parse().unwrap();
            let path = line.split_once(' ').map_or("", |(_, p)| p).to_owned();
            Sized {
                line: line.clone(),
                rank,
                path,
                disk,
            }
        })
        .collect()
}

/// Cut the list into segments of at most `cap` stored bytes (a single larger
/// object gets a segment of its own). `grouped` first orders by (type, path),
/// so the versions of one path share a segment and can delta within it.
fn cut(objects: &[Sized], cap: u64, grouped: bool) -> Vec<Vec<String>> {
    let mut order: Vec<&Sized> = objects.iter().collect();
    if grouped {
        order.sort_by(|a, b| (a.rank, &a.path).cmp(&(b.rank, &b.path)));
    }
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current = Vec::new();
    let mut size = 0_u64;
    for object in order {
        if !current.is_empty() && size + object.disk > cap {
            segments.push(std::mem::take(&mut current));
            size = 0;
        }
        size += object.disk;
        current.push(object.line.clone());
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// `<pack_id>.list`: edge lines, then `#<k>` before each segment's lines.
fn persist(path: &Path, edges: &[String], segments: &[Vec<String>]) {
    let mut out = edges.join("\n");
    out.push('\n');
    for (k, segment) in segments.iter().enumerate() {
        let _ = writeln!(out, "#{k}");
        for line in segment {
            out.push_str(line);
            out.push('\n');
        }
    }
    fs::write(path, out).unwrap();
}

/// Read `<pack_id>.list` back: (edges, segments).
fn reload(path: &Path) -> (Vec<String>, Vec<Vec<String>>) {
    let body = fs::read_to_string(path).unwrap();
    let mut edges = Vec::new();
    let mut segments: Vec<Vec<String>> = Vec::new();
    for line in body.lines() {
        if line.starts_with('-') {
            edges.push(line.to_owned());
        } else if line.starts_with('#') {
            segments.push(Vec::new());
        } else if let Some(segment) = segments.last_mut() {
            segment.push(line.to_owned());
        }
    }
    (edges, segments)
}

fn segment_pack(source: &Path, edges: &[String], segment: &[String]) -> Vec<u8> {
    let lines: Vec<String> = edges.iter().chain(segment).cloned().collect();
    list_pack(source, &lines, 1)
}

struct Segmented {
    scratch: Scratch,
    source: PathBuf,
    destination: PathBuf,
    tip: String,
    list: EdgeList,
    single: Vec<u8>,
}

/// Delta-heavy text plus incompressible blobs; the destination holds only the
/// base commit. The source is repacked so on-disk delta reuse is exercised.
fn segmented_fixture() -> Segmented {
    let scratch = Scratch::new("q2-segments");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 29, 8, 30);
    let mut rng = Rng(31);
    for i in 0..4 {
        write(&source, &format!("bin/blob{i}.bin"), &rng.bytes(24 * 1024));
        commit_all(&source, &format!("binary {i}"));
    }
    let tip = rev(&source, "HEAD");
    run(args(&source, ["repack", "-adq"]), "repack");
    let destination = scratch.init("destination.git", true);
    push(&source, &destination, &commits[0], "refs/heads/main");
    let request = request(
        std::slice::from_ref(&tip),
        std::slice::from_ref(&commits[0]),
    );
    let list = edge_list(&source, &request, Shape::Full);
    let single = oracle_pack(&source, &request, 1, Shape::Full);
    assert_eq!(single, list_pack(&source, &list.lines(), 1));
    Segmented {
        scratch,
        source,
        destination,
        tip,
        list,
        single,
    }
}

const CAP: u64 = 64 * 1024;

#[test]
fn q2_segments_are_self_contained_and_cost_is_measured() {
    let fixture = segmented_fixture();
    let objects = annotate(&fixture.source, &fixture.list);
    let total = u32::try_from(objects.len()).unwrap();
    for grouped in [false, true] {
        let segments = cut(&objects, CAP, grouped);
        let mut bytes = 0_usize;
        let mut counted = 0_u32;
        let mut largest = 0_usize;
        for (k, segment) in segments.iter().enumerate() {
            let pack = segment_pack(&fixture.source, &fixture.list.edges, segment);
            counted += header_count(&pack);
            bytes += pack.len();
            largest = largest.max(pack.len());
            // Alone: a fresh quarantine whose only alternate is the
            // destination's store (no other segment is visible).
            let quarantine = Quarantine::open(&fixture.destination);
            let result = quarantine.index(&pack);
            quarantine.abandon();
            assert!(
                result.is_ok(),
                "segment {k} (grouped={grouped}) alone: {result:?}"
            );
        }
        assert_eq!(counted, total, "segments partition the object list");
        let single = fixture.single.len();
        println!(
            "m1 q2 order={} cap_bytes={CAP} objects={total} segments={} single_pack_bytes={single} \
             segmented_bytes={bytes} overhead_pct={:.1} largest_segment_bytes={largest} \
             each_alone=ok",
            if grouped { "type-path" } else { "rev-list" },
            segments.len(),
            percent(bytes, single),
        );
        assert!(segments.len() >= 3, "fixture yields several segments");
    }
}

fn percent(value: usize, base: usize) -> f64 {
    let value = f64::from(u32::try_from(value).unwrap());
    let base = f64::from(u32::try_from(base).unwrap());
    (value - base) * 100.0 / base
}

#[test]
fn q2_resume_regenerates_only_later_segments() {
    let fixture = segmented_fixture();
    let objects = annotate(&fixture.source, &fixture.list);
    let segments = cut(&objects, CAP, true);
    let list_file = fixture.scratch.path("pack-0001.list");
    persist(&list_file, &fixture.list.edges, &segments);
    let (edges, reloaded) = reload(&list_file);
    assert_eq!(reloaded, segments);
    let n = reloaded.len();
    let k = n / 2;
    let first: Vec<Vec<u8>> = reloaded
        .iter()
        .map(|s| segment_pack(&fixture.source, &edges, s))
        .collect();

    let quarantine = Quarantine::open(&fixture.destination);
    let mut hashes = Vec::new();
    for pack in &first[..k] {
        hashes.push(quarantine.index(pack).unwrap());
    }
    // Segment k fails mid-stream.
    let broken = &first[k][..first[k].len() / 2];
    let failure = quarantine.index(broken).unwrap_err();
    let leftovers = quarantine.leftovers();
    // Not all objects are in: the refs-only check refuses the tip.
    let incomplete = quarantine.connected(std::slice::from_ref(&fixture.tip));
    assert!(incomplete.is_err(), "check refuses a partial segment set");
    quarantine.sweep();

    // Resume from the persisted list: regenerate k.. only.
    let (edges, reloaded) = reload(&list_file);
    let mut regenerated = 0_usize;
    let mut identical = 0_usize;
    for (j, segment) in reloaded.iter().enumerate().skip(k) {
        let pack = segment_pack(&fixture.source, &edges, segment);
        regenerated += pack.len();
        identical += usize::from(pack == first[j]);
        hashes.push(quarantine.index(&pack).unwrap());
    }
    quarantine
        .connected(std::slice::from_ref(&fixture.tip))
        .unwrap();
    quarantine.migrate(&hashes).unwrap();
    publish(
        &fixture.destination,
        &[Update::Create(
            "refs/carry/v1/spike/segmented/state".into(),
            fixture.tip.clone(),
        )],
    )
    .unwrap();
    drop_keeps(&fixture.destination, &hashes);
    let fsck = fsck(&fixture.destination);
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    let total: usize = first.iter().map(Vec::len).sum();
    println!(
        "m1 q2 resume segments={n} failed_at={k} leftovers_after_failure={leftovers:?} \
         index_pack_error={failure:?} regenerated_segments={} regenerated_bytes={regenerated} \
         total_bytes={total} regenerated_identical={identical} fsck=clean",
        n - k,
    );
}

// ---------------------------------------------------------------------------
// Q3: quarantine ingest, negative cases
// ---------------------------------------------------------------------------

/// Source chain base <- p <- x <- s. The destination holds `base` under
/// `refs/heads/main`; `p` and `x` are pushed under a temp ref which is then
/// deleted, so they are present but unreferenced (a `GitHaveQuery` answers
/// yes for `x`).
struct Chain {
    _scratch: Scratch,
    source: PathBuf,
    destination: PathBuf,
    base: String,
    p: String,
    x: String,
    s: String,
}

fn chain() -> Chain {
    let scratch = Scratch::new("q3-chain");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 41, 3, 4);
    let base = commits[1].clone();
    let p = commits[2].clone();
    let x = commits[3].clone();
    let s = commits[4].clone();
    let destination = scratch.init("destination.git", true);
    push(&source, &destination, &base, "refs/heads/main");
    push(&source, &destination, &x, "refs/heads/temp");
    run(
        args(&destination, ["update-ref", "-d", "refs/heads/temp"]),
        "delete temp",
    );
    assert!(has_object(&destination, &x) && has_object(&destination, &p));
    Chain {
        _scratch: scratch,
        source,
        destination,
        base,
        p,
        x,
        s,
    }
}

const STATE: &str = "refs/carry/v1/spike/chain/state";

fn state_absent(repo: &Path) -> bool {
    !args(repo, ["rev-parse", "--verify", "-q", STATE])
        .status()
        .unwrap()
        .success()
}

fn plan_with_x(chain: &Chain) -> Vec<u8> {
    let request = request(
        std::slice::from_ref(&chain.s),
        &[chain.base.clone(), chain.x.clone()],
    );
    let list = edge_list(&chain.source, &request, Shape::Full);
    list_pack(&chain.source, &list.lines(), 1)
}

#[test]
fn q3_have_from_refs_publishes() {
    let chain = chain();
    let request = request(
        std::slice::from_ref(&chain.s),
        std::slice::from_ref(&chain.base),
    );
    let list = edge_list(&chain.source, &request, Shape::Full);
    let pack = list_pack(&chain.source, &list.lines(), 1);
    let before = packs(&chain.destination).len();
    let hashes = ingest(
        &chain.destination,
        &[pack],
        std::slice::from_ref(&chain.s),
        &[Update::Create(STATE.into(), chain.s.clone())],
    )
    .unwrap();
    assert_eq!(rev(&chain.destination, STATE), chain.s);
    assert_eq!(packs(&chain.destination).len(), before + 1);
    let keep = objects_dir(&chain.destination)
        .join("pack")
        .join(format!("pack-{}.keep", hashes[0]));
    assert!(!keep.exists(), ".keep dropped after the ref landed");
    assert!(fsck(&chain.destination).status.success());
    println!("m1 q3 case=have-from-refs connectivity=ok published=yes keep_dropped=yes fsck=clean");
}

#[test]
fn q3_deleted_parent_never_publishes() {
    let chain = chain();
    let pack = plan_with_x(&chain);
    // x's parent vanishes after the have-query answered yes for x.
    fs::remove_file(loose_path(&chain.destination, &chain.p)).unwrap();
    let before = packs(&chain.destination).len();
    let refused = ingest(
        &chain.destination,
        &[pack],
        std::slice::from_ref(&chain.s),
        &[Update::Create(STATE.into(), chain.s.clone())],
    )
    .unwrap_err();
    assert!(matches!(refused, Refused::Connectivity(_)), "{refused:?}");
    assert!(state_absent(&chain.destination));
    assert_eq!(packs(&chain.destination).len(), before, "nothing migrated");
    assert!(no_incoming(&chain.destination));
    println!(
        "m1 q3 case=deleted-parent refused={:?} published=no migrated=no",
        refused.describe()
    );
}

#[test]
fn q3_prune_after_have_query_never_publishes() {
    let chain = chain();
    let pack = plan_with_x(&chain);
    // gc.auto=0 is per-call; an operator (or a foreign gc) prunes anyway.
    run(args(&chain.destination, ["prune", "--expire=now"]), "prune");
    assert!(!has_object(&chain.destination, &chain.x), "x pruned");
    let before = packs(&chain.destination).len();
    let refused = ingest(
        &chain.destination,
        &[pack],
        std::slice::from_ref(&chain.s),
        &[Update::Create(STATE.into(), chain.s.clone())],
    )
    .unwrap_err();
    assert!(
        matches!(refused, Refused::Connectivity(_) | Refused::Segment(..)),
        "{refused:?}"
    );
    assert!(state_absent(&chain.destination));
    assert_eq!(packs(&chain.destination).len(), before, "nothing migrated");
    assert!(no_incoming(&chain.destination));
    println!(
        "m1 q3 case=prune-after-have-query refused={:?} published=no migrated=no",
        refused.describe()
    );
}

/// "Trusts only refs" is literal: an object missing under an existing ref is
/// not detected by the check. That is R-N75's and a preflight fsck's job.
#[test]
fn q3_check_trusts_ref_closures() {
    let chain = chain();
    // x now sits under a ref, but its parent p (loose) is gone.
    run(
        args(&chain.destination, ["update-ref", "refs/heads/x", &chain.x]),
        "ref x",
    );
    fs::remove_file(loose_path(&chain.destination, &chain.p)).unwrap();
    let pack = plan_with_x(&chain);
    let quarantine = Quarantine::open(&chain.destination);
    let indexed = quarantine.index(&pack);
    let check = quarantine.connected(std::slice::from_ref(&chain.s));
    quarantine.abandon();
    assert!(indexed.is_ok());
    assert!(
        check.is_ok(),
        "refs are trusted, their closure is not audited"
    );
    assert!(
        !fsck(&chain.destination).status.success(),
        "fsck does see it"
    );
    println!("m1 q3 case=corrupt-ref-closure connectivity=ok(trusted-ref) fsck=fails");
}

#[test]
fn q3_quarantine_forbids_ref_updates() {
    let chain = chain();
    let quarantine = Quarantine::open(&chain.destination);
    let mut command = quarantine.command();
    command.args(["update-ref", "--stdin", "-z"]);
    let output = feed(
        command,
        format!("create {STATE}\0{}\0", chain.base).as_bytes(),
    );
    quarantine.abandon();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    assert!(state_absent(&chain.destination));
    println!("m1 q3 case=update-ref-inside-quarantine refused={stderr:?}");
}

fn no_incoming(repo: &Path) -> bool {
    fs::read_dir(objects_dir(repo))
        .unwrap()
        .filter_map(Result::ok)
        .all(|e| !e.file_name().to_string_lossy().starts_with("incoming-"))
}

// ---------------------------------------------------------------------------
// Q4: ~1,000-parent state commit
// ---------------------------------------------------------------------------

/// `parents` native tips (one commit and blob each, via fast-import), then S
/// over all of them with `commit-tree`, then every native ref deleted so the
/// state ref alone keeps the history. Returns (S, `commit-tree` ms).
fn octopus_source(source: &Path, parents: usize) -> (String, u128) {
    let mut stream = String::new();
    for i in 0..parents {
        let blob = format!("native tip {i}\n");
        let _ = write!(
            stream,
            "commit refs/heads/b{i:05}\ncommitter bulkload <bulkload@invalid> 1790121600 +0000\n\
             data 4\ntip\nM 100644 inline f\ndata {}\n{blob}\n",
            blob.len()
        );
    }
    let mut command = git(source);
    command.args(["fast-import", "--quiet"]);
    ok(feed(command, stream.as_bytes()), "fast-import");
    let mut tips: Vec<String> = text(
        args(
            source,
            ["for-each-ref", "--format=%(objectname)", "refs/heads/"],
        ),
        "tips",
    )
    .lines()
    .map(str::to_owned)
    .collect();
    tips.sort();
    tips.dedup();
    assert_eq!(tips.len(), parents);
    let tree = String::from_utf8(ok(feed(args(source, ["mktree"]), b""), "empty tree"))
        .unwrap()
        .trim()
        .to_owned();
    let mut command = git(source);
    command.args(["commit-tree", &tree, "-m", "bulkload state"]);
    for tip in &tips {
        command.args(["-p", tip]);
    }
    let start = Instant::now();
    let state = text(command, "commit-tree");
    let commit_tree_ms = ms(start);
    // One ref per snapshot: only the state ref remains.
    let mut deletes = String::new();
    for i in 0..parents {
        let _ = writeln!(deletes, "delete refs/heads/b{i:05}");
    }
    ok(
        feed(args(source, ["update-ref", "--stdin"]), deletes.as_bytes()),
        "delete",
    );
    run(
        args(
            source,
            ["update-ref", "refs/carry/v1/spike/octopus/state", &state],
        ),
        "ref",
    );
    (state, commit_tree_ms)
}

/// Run to completion; (succeeded, milliseconds).
fn timed(mut command: Command) -> (bool, u128) {
    let start = Instant::now();
    let ok = command.status().unwrap().success();
    (ok, ms(start))
}

fn octopus(parents: usize) {
    let scratch = Scratch::new("q4-octopus");
    let source = scratch.init("source.git", true);
    let (state, commit_tree_ms) = octopus_source(&source, parents);
    let size = text(args(&source, ["cat-file", "-s", &state]), "size");
    let start = Instant::now();
    let pack = ok(
        feed(
            args(
                &source,
                [
                    "pack-objects",
                    "--stdout",
                    "--revs",
                    "--delta-base-offset",
                    "-q",
                ],
            ),
            format!("{state}\n").as_bytes(),
        ),
        "pack",
    );
    let pack_ms = ms(start);
    let destination = scratch.init("destination.git", true);
    let start = Instant::now();
    ok(
        feed(
            args(&destination, ["index-pack", "--stdin", "--keep"]),
            &pack,
        ),
        "index-pack",
    );
    let index_ms = ms(start);
    run(
        args(
            &destination,
            ["update-ref", "refs/carry/v1/spike/octopus/state", &state],
        ),
        "ref",
    );
    let (fsck_ok, fsck_ms) = timed(args(
        &destination,
        ["fsck", "--strict", "--no-progress", "--no-dangling"],
    ));
    let (graph_ok, graph_ms) = timed(args(
        &destination,
        ["commit-graph", "write", "--reachable", "--no-progress"],
    ));
    let (verify_ok, _) = timed(args(
        &destination,
        ["commit-graph", "verify", "--no-progress"],
    ));
    for keep in fs::read_dir(objects_dir(&destination).join("pack")).unwrap() {
        let keep = keep.unwrap().path();
        if keep.extension().is_some_and(|x| x == "keep") {
            fs::remove_file(keep).unwrap();
        }
    }
    let (gc_ok, gc_ms) = timed(args(&destination, ["gc", "-q", "--prune=now"]));
    let count = text(args(&destination, ["rev-list", "--count", &state]), "count");
    let parent_line = text(
        args(&destination, ["rev-list", "--parents", "-1", &state]),
        "parents",
    );
    let after_gc_ok = fsck(&destination).status.success();
    let start = Instant::now();
    let graph = run(
        args(&destination, ["log", "--graph", "--oneline", &state]),
        "log --graph",
    );
    let log_ms = ms(start);
    println!(
        "m1 q4 parents={parents} commit_bytes={size} commit_tree_ms={commit_tree_ms} \
         pack_bytes={} pack_ms={pack_ms} index_pack_ms={index_ms} fsck={fsck_ok} fsck_ms={fsck_ms} \
         commit_graph={graph_ok} commit_graph_verify={verify_ok} commit_graph_ms={graph_ms} \
         gc={gc_ok} gc_ms={gc_ms} fsck_after_gc={after_gc_ok} reachable_commits={count} \
         log_graph_bytes={} log_graph_ms={log_ms}",
        pack.len(),
        graph.len(),
    );
    assert!(fsck_ok && graph_ok && verify_ok && gc_ok && after_gc_ok);
    assert_eq!(count, (parents + 1).to_string());
    assert_eq!(parent_line.split(' ').count(), parents + 1);
}

#[test]
fn q4_state_commit_with_1000_parents() {
    octopus(1000);
}

#[test]
fn q4_state_commit_with_2000_parents() {
    octopus(2000);
}

// ---------------------------------------------------------------------------
// Q5: fast-import M <mode> <oid> naming an alternates-only object
// ---------------------------------------------------------------------------

#[test]
fn q5_fast_import_references_alternate_object_without_copying() {
    let scratch = Scratch::new("q5-alternates");
    let live = scratch.init("live.git", true);
    let blob = ok(
        feed(
            args(&live, ["hash-object", "-w", "--stdin"]),
            b"only in the live store\n",
        ),
        "hash blob",
    );
    let blob = String::from_utf8(blob).unwrap().trim().to_owned();
    let subtree_input = format!("100644 blob {blob}\tinner.txt\n");
    let tree = String::from_utf8(ok(
        feed(args(&live, ["mktree"]), subtree_input.as_bytes()),
        "mktree",
    ))
    .unwrap()
    .trim()
    .to_owned();
    // P: the private store, alternates to the live store (plan D2).
    let private = scratch.init("store.git", true);
    fs::write(
        objects_dir(&private).join("info/alternates"),
        format!("{}\n", objects_dir(&live).display()),
    )
    .unwrap();
    let stream = format!(
        "commit refs/ledger/wt/current\ncommitter bulkload <bulkload@invalid> 1790121600 +0000\n\
         data 5\nseat\nM 100644 {blob} worktree/a.txt\nM 040000 {tree} worktree/dir\n\n"
    );
    let output = feed(
        args(&private, ["fast-import", "--quiet"]),
        stream.as_bytes(),
    );
    let imported = output.status.success();
    assert!(imported, "{}", String::from_utf8_lossy(&output.stderr));
    let read = text(
        args(
            &private,
            ["cat-file", "-p", "refs/ledger/wt/current:worktree/a.txt"],
        ),
        "read",
    );
    assert_eq!(read, "only in the live store");
    // Neither the blob nor the tree is in P's packs or loose store.
    let mut in_private = Vec::new();
    for pack in packs(&private) {
        let idx = pack.with_extension("idx");
        let listing = ok(
            feed(args(&private, ["show-index"]), &fs::read(&idx).unwrap()),
            "show-index",
        );
        in_private.push(String::from_utf8(listing).unwrap());
    }
    let copied = in_private
        .iter()
        .any(|l| l.contains(&blob) || l.contains(&tree))
        || loose_path(&private, &blob).exists()
        || loose_path(&private, &tree).exists();
    assert!(!copied, "fast-import referenced, did not copy");
    assert!(
        fsck(&private).status.success(),
        "complete through alternates"
    );
    // Without the alternate, the reference dangles: proof it was not copied.
    fs::remove_file(objects_dir(&private).join("info/alternates")).unwrap();
    let alone = fsck(&private);
    assert!(!alone.status.success());
    // An oid in neither store is refused.
    fs::write(
        objects_dir(&private).join("info/alternates"),
        format!("{}\n", objects_dir(&live).display()),
    )
    .unwrap();
    let absent = "2222222222222222222222222222222222222222";
    let bad = format!(
        "commit refs/ledger/wt/bad\ncommitter bulkload <bulkload@invalid> 1790121600 +0000\n\
         data 3\nbad\nM 100644 {absent} x\n\n"
    );
    let refused = feed(args(&private, ["fast-import", "--quiet"]), bad.as_bytes());
    assert!(!refused.status.success());
    let refusal = String::from_utf8_lossy(&refused.stderr);
    let first = refusal.lines().next().unwrap_or("").to_owned();
    println!(
        "m1 q5 fast_import_alternate_blob=ok alternate_tree=ok copied=no fsck_with_alternates=clean \
         fsck_without_alternates=fails absent_oid_refused={first:?}"
    );
}

// ---------------------------------------------------------------------------
// Q6: byte identity on an unchanged re-run
// ---------------------------------------------------------------------------

/// Build S from `meta` entries and `parents` (sorted, de-duplicated) with the
/// fixed identity and date. Returns S's oid.
fn state_commit(store: &Path, meta: &[(&str, &str)], parents: &[String]) -> String {
    let mut entries = String::new();
    for (name, body) in meta {
        let blob = String::from_utf8(ok(
            feed(
                args(store, ["hash-object", "-w", "--stdin"]),
                body.as_bytes(),
            ),
            "blob",
        ))
        .unwrap();
        let _ = writeln!(entries, "100644 blob {}\t{name}", blob.trim());
    }
    let meta_tree = String::from_utf8(ok(
        feed(args(store, ["mktree"]), entries.as_bytes()),
        "mktree",
    ))
    .unwrap()
    .trim()
    .to_owned();
    let root = String::from_utf8(ok(
        feed(
            args(store, ["mktree"]),
            format!("040000 tree {meta_tree}\tmeta\n").as_bytes(),
        ),
        "mktree root",
    ))
    .unwrap()
    .trim()
    .to_owned();
    let mut sorted = parents.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut command = git(store);
    command.args(["commit-tree", &root, "-m", "bulkload state v2"]);
    for parent in &sorted {
        command.args(["-p", parent]);
    }
    text(command, "commit-tree")
}

#[test]
fn q6_unchanged_rerun_is_byte_identical_empty_pack_and_verify_only() {
    let scratch = Scratch::new("q6-identity");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 53, 2, 3);
    let meta = [
        ("head", "ref: refs/heads/main\n"),
        ("refs", "refs/heads/main\n"),
    ];
    // Parents in a different order and duplicated: S must not care.
    let first = state_commit(&source, &meta, &[commits[3].clone(), commits[1].clone()]);
    let second = state_commit(
        &source,
        &meta,
        &[commits[1].clone(), commits[3].clone(), commits[1].clone()],
    );
    assert_eq!(first, second, "S is byte-identical");
    let bytes_first = run(args(&source, ["cat-file", "commit", &first]), "cat");
    let bytes_second = run(args(&source, ["cat-file", "commit", &second]), "cat");
    assert_eq!(bytes_first, bytes_second);

    let destination = scratch.init("destination.git", true);
    let reference = "refs/carry/v1/src/item/state";
    push(&source, &destination, &first, reference);
    // Re-run: the destination's tip is S itself.
    let request = request(std::slice::from_ref(&second), std::slice::from_ref(&first));
    let list = edge_list(&source, &request, Shape::Full);
    assert!(list.objects.is_empty(), "nothing to send");
    let empty = list_pack(&source, &list.lines(), 1);
    assert_eq!(header_count(&empty), 0);
    let ref_file_before = fs::metadata(destination.join(reference))
        .ok()
        .map(|m| m.modified().unwrap());
    // `create` would fail on the existing ref; the re-run is `verify` only.
    let create = publish(
        &destination,
        &[Update::Create(reference.into(), second.clone())],
    );
    assert!(create.is_err());
    publish(&destination, &[Update::Verify(reference.into(), second)]).unwrap();
    let ref_file_after = fs::metadata(destination.join(reference))
        .ok()
        .map(|m| m.modified().unwrap());
    assert_eq!(ref_file_before, ref_file_after, "verify wrote nothing");
    // A changed input changes S and yields a non-empty plan.
    let changed = state_commit(
        &source,
        &[("head", "ref: refs/heads/other\n")],
        &[commits[3].clone()],
    );
    assert_ne!(changed, first);
    let changed_list = edge_list(&source, &request_one(&changed, &first), Shape::Full);
    println!(
        "m1 q6 state={first} rerun_identical=true empty_pack_bytes={} ref_ops=verify \
         create_on_rerun_refused=true changed_input_objects={}",
        empty.len(),
        changed_list.objects.len(),
    );
    assert!(!changed_list.objects.is_empty());
}

fn request_one(want: &str, have: &str) -> String {
    request(&[want.to_owned()], &[have.to_owned()])
}
