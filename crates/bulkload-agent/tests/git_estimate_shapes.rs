//! P46's estimate leg over the fixture shapes that P66 does not generate
//! (OI-1003-Q42, OI-1003-Q44, OI-1003-Q56; R-N74, R-N75, R-N97, R-N113,
//! R-N116, R-N131).
//!
//! Until WP2 PR 3, every fixture in `tests/git_carry_v2.rs` (tag
//! `carry-v2-final`) also asserted "sent == estimate" against the
//! upload-pack oracle, so `git-carry-estimate` was checked on shallow
//! frontiers, annotated tags (of commits, trees, blobs and tags), refs
//! straight at a blob, gitlinks, raw-byte paths and bitmapped sources. P66
//! (`tests/git_estimate_dag.rs`) builds only full, random commit DAGs. This
//! file keeps those shapes, rebuilt as they were built there, now judged
//! against the estimate alone. It uses nothing WP2 PR 3 deleted: the product
//! code under test is `bulkload_agent::git_carry::estimate::estimate`, and
//! the request the oracle is fed is derived here from the two repositories
//! with plain `git`, as P66 derives it.
//!
//! The oracle is R-N113's: `upload-pack --stateless-rpc` (protocol v2) fed
//! M1's first round, under the pack pins. The wants are the source's tips
//! that the destination does not hold. The haves are exactly the
//! destination's tips that the source holds, ancestors first, with tips that
//! peel to no commit last (R-N116, spike D1). The request carries one
//! `shallow <oid>` line per line of the destination's shallow file (R-N75
//! makes it the source's frontier too).
//!
//! [`check`] asserts on every shape:
//! - **Request.** `destination_tip_count` counts every destination tip,
//!   `haves_used` the held tips, and both shallow counts the shallow files.
//! - **Size.** `thin_pack` equals the oracle's pack: the same header count and
//!   the same byte length. A zero-object oracle pack counts as the estimate's
//!   "no pack" (`ThinPack::default()`).
//! - **Object set.** The oracle's pack indexes on its own against the
//!   destination's store, and the per-type count and `%(objectsize:disk)`
//!   tally of exactly the oids it carries equals `missing`.
//! - **Closure.** `source` is the same tally over the closure of the
//!   source's tips.
//!
//! Each test then keeps the extra assertions its `git_carry_v2.rs` fixture
//! made about the estimate or the oracle (have order is load-bearing,
//! aggressive edges send fewer objects, the exact carried count).
//!
//! Every test builds its repositories under the system temp dir, runs the
//! real `git` on `PATH`, and removes them afterwards. Measurements print as
//! `p46e case=<name> key=value ...` lines (`--nocapture`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use bulkload_agent::git_carry::estimate::{estimate, CarryEstimate, Destination, Tally, ThinPack};

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// Fixture Git: hooks off (R-N98 allows that in fixtures this test creates
/// and removes), the estimate's pack pins plus `pack.useSparse=false` and
/// `pack.useBitmaps=false` (so upload-pack's `pack-objects` packs exactly the
/// walked set, as the estimate's does), a fixed identity and date, no user or
/// system configuration, and no redirecting variable from the parent
/// environment.
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
    ] {
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
            "pack.threads=2",
            "-c",
            "pack.windowMemory=64m",
            "-c",
            "pack.useSparse=false",
            "-c",
            "pack.useBitmaps=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-C",
        ])
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "bulkload")
        .env("GIT_AUTHOR_EMAIL", "bulkload@invalid")
        .env("GIT_COMMITTER_NAME", "bulkload")
        .env("GIT_COMMITTER_EMAIL", "bulkload@invalid")
        .env("GIT_AUTHOR_DATE", "1790121600 +0000")
        .env("GIT_COMMITTER_DATE", "1790121600 +0000");
    command
}

/// Run `command` with `stdin` written from a thread, so a large answer never
/// deadlocks against an unread request.
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

fn text_of(output: Output, what: &str) -> String {
    String::from_utf8(ok(output, what))
        .expect("utf8")
        .trim_end()
        .to_owned()
}

fn text(mut command: Command, what: &str) -> String {
    text_of(command.output().expect("spawn git"), what)
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
            "bulkload-estimate-shapes-{name}-{}-{}-{nanos}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("scratch");
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
}

fn lines(rng: &mut Rng, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("line {i} {:016x}\n", rng.next()))
        .collect()
}

/// `tests/git_carry_v2.rs`'s `noise`, byte for byte, so each shape is the one
/// that file built.
fn noise(seed: u64, count: usize) -> String {
    lines(&mut Rng(seed.wrapping_mul(2_654_435_761) + 1), count).concat()
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

/// A commit with a distinct date, `n` minutes after a fixed epoch.
fn dated(repo: &Path, file: &str, content: &str, n: u64) -> String {
    write(repo, file, content.as_bytes());
    run(args(repo, ["add", "-A"]), "add");
    let date = format!("{} +0000", 1_790_000_000 + n * 60);
    let mut command = git(repo);
    command
        .args(["commit", "-q", "--allow-empty", "-m", file])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date);
    run(command, "commit");
    rev(repo, "HEAD")
}

/// Give `destination` (bare) the objects of `oid` under `reference`.
fn push(source: &Path, destination: &Path, oid: &str, reference: &str) {
    let mut command = git(source);
    command
        .args(["push", "-q"])
        .arg(destination)
        .arg(format!("{oid}:{reference}"));
    run(command, "push");
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

/// A bare destination holding `have` under `refs/heads/main`.
fn destination_at(scratch: &Scratch, source: &Path, have: &str) -> PathBuf {
    let destination = scratch.init("destination.git", true);
    push(source, &destination, have, "refs/heads/main");
    destination
}

/// The git dir of a work tree or a bare repository.
fn git_dir(repo: &Path) -> PathBuf {
    let dotgit = repo.join(".git");
    if dotgit.is_dir() {
        dotgit
    } else {
        repo.to_path_buf()
    }
}

fn objects_dir(repo: &Path) -> PathBuf {
    git_dir(repo).join("objects")
}

/// The lines of `repo`'s shallow file (none when it has none).
fn shallow_of(repo: &Path) -> Vec<String> {
    fs::read_to_string(git_dir(repo).join("shallow"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn has_object(repo: &Path, oid: &str) -> bool {
    args(repo, ["cat-file", "-e", oid])
        .status()
        .expect("cat-file")
        .success()
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

/// Every ref tip and `HEAD` of `repo`.
fn tips_of(repo: &Path) -> BTreeSet<String> {
    let mut tips: BTreeSet<String> = text(
        args(repo, ["for-each-ref", "--format=%(objectname)"]),
        "for-each-ref",
    )
    .lines()
    .map(str::to_owned)
    .collect();
    let head = args(repo, ["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .expect("rev-parse HEAD");
    if head.status.success() {
        tips.insert(String::from_utf8_lossy(&head.stdout).trim().to_owned());
    }
    tips
}

fn peeled_commit(repo: &Path, oid: &str) -> Option<String> {
    let out = args(
        repo,
        ["rev-parse", "--verify", "-q", &format!("{oid}^{{commit}}")],
    )
    .output()
    .expect("rev-parse peel");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `held` ordered as M1 offers it (R-N116, spike D1): the topological order
/// of the peeled commits, ancestors first, then any tip that peels to no
/// commit, by oid.
fn ancestors_first(repo: &Path, held: &BTreeSet<String>) -> Vec<String> {
    let peeled: Vec<(String, Option<String>)> = held
        .iter()
        .map(|tip| (tip.clone(), peeled_commit(repo, tip)))
        .collect();
    let commits: BTreeSet<&String> = peeled
        .iter()
        .filter_map(|(_, commit)| commit.as_ref())
        .collect();
    let mut haves = Vec::new();
    if !commits.is_empty() {
        let mut list = git(repo);
        list.args(["rev-list", "--topo-order", "--reverse"])
            .args(&commits);
        for commit in text(list, "rev-list --topo-order").lines() {
            for (tip, peel) in &peeled {
                if peel.as_deref() == Some(commit) {
                    haves.push(tip.clone());
                }
            }
        }
    }
    haves.extend(
        peeled
            .iter()
            .filter(|(_, peel)| peel.is_none())
            .map(|(tip, _)| tip.clone()),
    );
    assert_eq!(haves.len(), held.len(), "every held tip is offered once");
    haves
}

/// The per-type count and `%(objectsize:disk)` of `oids` in `repo`, shaped
/// as the estimate's [`Tally`].
fn tally_of<'a>(repo: &Path, oids: impl IntoIterator<Item = &'a String>) -> Tally {
    let mut input = String::new();
    for oid in oids {
        input.push_str(oid);
        input.push('\n');
    }
    let mut tally = Tally::default();
    if input.is_empty() {
        return tally;
    }
    let listing = text_of(
        feed(
            args(
                repo,
                [
                    "cat-file",
                    "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
                ],
            ),
            input.as_bytes(),
        ),
        "cat-file sizes",
    );
    for line in listing.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields.len(), 3, "batch-check line: {line}");
        let slot = match fields[1] {
            "commit" => &mut tally.commit,
            "tree" => &mut tally.tree,
            "blob" => &mut tally.blob,
            "tag" => &mut tally.tag,
            other => panic!("unexpected object type {other} in {line}"),
        };
        slot.count += 1;
        slot.bytes_disk += fields[2].parse::<u64>().expect("objectsize:disk");
    }
    tally
}

/// The whole object closure of `tips` in `repo` (bounded by its shallow
/// file, as the estimate's walk is).
fn closure(repo: &Path, tips: &BTreeSet<String>) -> Vec<String> {
    let mut input = String::new();
    for tip in tips {
        input.push_str(tip);
        input.push('\n');
    }
    text_of(
        feed(
            args(
                repo,
                ["rev-list", "--objects", "--no-object-names", "--stdin"],
            ),
            input.as_bytes(),
        ),
        "rev-list --objects",
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

// ---------------------------------------------------------------------------
// The oracle and the object-set reader
// ---------------------------------------------------------------------------

fn pkt_line(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(line.as_bytes());
}

/// R-N113's exactness oracle: `upload-pack --stateless-rpc` (protocol v2)
/// given exactly `haves`, in that order, plus the `shallow` lines, under the
/// pack pins. Returns the pack it streams on sideband 1 (empty when none).
fn upload_pack(
    source: &Path,
    wants: &BTreeSet<String>,
    haves: &[String],
    shallow: &[String],
) -> Vec<u8> {
    let mut request = Vec::new();
    pkt_line(&mut request, "command=fetch\n");
    request.extend_from_slice(b"0001");
    for line in ["thin-pack\n", "ofs-delta\n", "no-progress\n"] {
        pkt_line(&mut request, line);
    }
    for oid in shallow {
        pkt_line(&mut request, &format!("shallow {oid}\n"));
    }
    for oid in wants {
        pkt_line(&mut request, &format!("want {oid}\n"));
    }
    for oid in haves {
        pkt_line(&mut request, &format!("have {oid}\n"));
    }
    pkt_line(&mut request, "done\n");
    request.extend_from_slice(b"0000");
    let mut command = git(source);
    command
        .args(["upload-pack", "--stateless-rpc", "."])
        .env("GIT_PROTOCOL", "version=2");
    let answer = ok(feed(command, &request), "upload-pack");
    let mut rest = &answer[..];
    let mut in_pack = false;
    let mut pack = Vec::new();
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if length < 4 {
            rest = &rest[4..];
            if length == 0 && in_pack {
                break;
            }
            continue;
        }
        let payload = &rest[4..length];
        rest = &rest[length..];
        if in_pack {
            assert_ne!(
                payload[0],
                3,
                "upload-pack error: {}",
                String::from_utf8_lossy(&payload[1..])
            );
            if payload[0] == 1 {
                pack.extend_from_slice(&payload[1..]);
            }
        } else if payload == b"packfile\n" {
            in_pack = true;
        }
    }
    pack
}

fn header_count(pack: &[u8]) -> u32 {
    if pack.is_empty() {
        return 0;
    }
    assert_eq!(&pack[..4], b"PACK", "pack signature");
    u32::from_be_bytes(pack[8..12].try_into().unwrap())
}

/// The oids a thin pack carries, and proof that it indexes alone: it is
/// indexed with `--fix-thin` into a throwaway object directory whose only
/// alternate is `destination`'s store, and the set is the `.idx` entries
/// whose offset lies inside the received bytes (`--fix-thin` appends its
/// bases after them).
fn carried(scratch: &Scratch, destination: &Path, pack: &[u8]) -> BTreeSet<String> {
    if header_count(pack) == 0 {
        return BTreeSet::new();
    }
    let quarantine = scratch.path(&format!(
        "quarantine-{}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(quarantine.join("pack")).unwrap();
    let mut command = git(destination);
    command
        .args(["index-pack", "--stdin", "--fix-thin"])
        .env("GIT_OBJECT_DIRECTORY", &quarantine)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects_dir(destination))
        .env("GIT_QUARANTINE_PATH", &quarantine);
    let out = String::from_utf8(ok(feed(command, pack), "index-pack alone")).unwrap();
    let hash = out.trim().strip_prefix("pack\t").unwrap().to_owned();
    let idx = quarantine.join("pack").join(format!("pack-{hash}.idx"));
    let listing = ok(
        feed(args(destination, ["show-index"]), &fs::read(idx).unwrap()),
        "show-index",
    );
    fs::remove_dir_all(&quarantine).unwrap();
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

// ---------------------------------------------------------------------------
// The estimate, judged
// ---------------------------------------------------------------------------

/// What one judged shape measured, for its test's extra assertions.
struct Judged {
    haves: Vec<String>,
    wants: BTreeSet<String>,
    shallow: Vec<String>,
    destination_tips: usize,
    carried: BTreeSet<String>,
    estimate: CarryEstimate,
}

/// Estimate `source` -> `destination` and assert the gate (module docs)
/// against the oracle fed M1's first round, derived here from the
/// repositories.
fn check(scratch: &Scratch, name: &str, source: &Path, destination: &Path) -> Judged {
    let destination_tips = tips_of(destination);
    let held: BTreeSet<String> = destination_tips
        .iter()
        .filter(|tip| has_object(source, tip))
        .cloned()
        .collect();
    let haves = ancestors_first(source, &held);
    let source_tips = tips_of(source);
    let wants: BTreeSet<String> = source_tips.difference(&held).cloned().collect();
    let shallow = shallow_of(destination);
    if !shallow.is_empty() {
        let frontier = |lines: &[String]| lines.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            frontier(&shallow),
            frontier(&shallow_of(source)),
            "{name}: fixture: a shallow destination is shallow at the source's frontier (R-N75)"
        );
    }

    let pack = upload_pack(source, &wants, &haves, &shallow);
    let objects = header_count(&pack);
    let oracle = if objects == 0 {
        ThinPack::default()
    } else {
        ThinPack {
            bytes: u64::try_from(pack.len()).unwrap(),
            objects: u64::from(objects),
        }
    };
    let oids = carried(scratch, destination, &pack);
    assert_eq!(
        u64::try_from(oids.len()).unwrap(),
        oracle.objects,
        "{name}: the oracle pack indexes alone and carries its header count"
    );
    let oracle_tally = tally_of(source, &oids);
    let source_tally = tally_of(source, &closure(source, &source_tips));

    let measured = estimate(source, &Destination::Local(destination.to_path_buf()))
        .unwrap_or_else(|refused| panic!("{name}: estimate refused: {refused}"));
    println!(
        "p46e case={name} destination_tips={} held={} wants={} shallow={} \
         oracle_stream_bytes={} oracle_objects={} oracle_bytes={} estimate_objects={} \
         estimate_bytes={} missing_objects={} source_objects={}",
        destination_tips.len(),
        haves.len(),
        wants.len(),
        shallow.len(),
        pack.len(),
        oracle.objects,
        oracle.bytes,
        measured.thin_pack.objects,
        measured.thin_pack.bytes,
        measured.missing.objects(),
        measured.source.objects(),
    );
    assert_eq!(
        measured.destination_tip_count,
        destination_tips.len(),
        "{name}: every destination tip counted"
    );
    assert_eq!(
        measured.haves_used,
        haves.len(),
        "{name}: haves are exactly the held tips"
    );
    assert_eq!(
        measured.destination_shallow_count,
        shallow.len(),
        "{name}: destination shallow lines"
    );
    assert_eq!(
        measured.source_shallow_count,
        shallow_of(source).len(),
        "{name}: source shallow lines"
    );
    assert!(!measured.source_partial, "{name}: not partial");
    assert_eq!(measured.stash_entries, 0, "{name}: no stash");
    assert_eq!(
        measured.thin_pack, oracle,
        "{name}: estimate.thin_pack == upload-pack (objects and bytes)"
    );
    assert_eq!(
        measured.missing, oracle_tally,
        "{name}: missing == the oracle's object set, per type and disk size"
    );
    assert_eq!(
        measured.source, source_tally,
        "{name}: source == the closure of the source's tips"
    );
    Judged {
        haves,
        wants,
        shallow,
        destination_tips: destination_tips.len(),
        carried: oids,
        estimate: measured,
    }
}

// ---------------------------------------------------------------------------
// Spike Q1 shapes
// ---------------------------------------------------------------------------

#[test]
fn delta_heavy() {
    let scratch = Scratch::new("delta");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 7, 4, 12);
    let destination = destination_at(&scratch, &source, &commits[4]);
    check(&scratch, "delta-heavy", &source, &destination);
}

#[test]
fn rename_heavy() {
    let scratch = Scratch::new("rename");
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
    commit_all(&source, "rename b -> c/d");
    let destination = destination_at(&scratch, &source, &base);
    check(&scratch, "rename-heavy", &source, &destination);
}

/// Wants that are only annotated tags (one a tag of a tag) on a held commit.
#[test]
fn tag_only() {
    let scratch = Scratch::new("tag");
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
    let judged = check(&scratch, "tag-only", &source, &destination);
    assert_eq!(judged.carried.len(), 2, "exactly the two tag objects");
    assert_eq!(judged.estimate.missing.tag.count, 2);
}

/// A gitlink (mode 160000) to a commit the repository does not hold: the
/// estimate's walk skips it, as upload-pack does.
#[test]
fn submodule_gitlink() {
    let scratch = Scratch::new("gitlink");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 5, 2, 1);
    let base = commits.last().unwrap().clone();
    write(
        &source,
        ".gitmodules",
        b"[submodule \"sub\"]\n\tpath = vendor/sub\n\turl = https://example.invalid/sub.git\n",
    );
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
    commit_all(&source, "add submodule gitlink");
    let destination = destination_at(&scratch, &source, &base);
    let judged = check(&scratch, "submodule-gitlink", &source, &destination);
    assert!(
        !judged
            .carried
            .contains("1111111111111111111111111111111111111111"),
        "the gitlink's commit is not carried"
    );
}

#[test]
fn matching_shallow_frontier() {
    let scratch = Scratch::new("shallow");
    let origin = scratch.init("origin", false);
    delta_history(&origin, 13, 2, 8);
    let source = clone(&scratch, &origin, "source", Some(3), false);
    let destination = clone(&scratch, &origin, "destination.git", Some(3), true);
    assert_eq!(shallow_of(&source), shallow_of(&destination));
    let mut rng = Rng(17);
    for i in 0..3 {
        write(
            &source,
            "src/file0.txt",
            lines(&mut rng, 1500).concat().as_bytes(),
        );
        commit_all(&source, &format!("shallow work {i}"));
    }
    let judged = check(&scratch, "matching-shallow", &source, &destination);
    assert!(!judged.shallow.is_empty());
}

/// A want that forks below the have and takes the have's tree.
fn fork_below_have(scratch: &Scratch, depth: Option<u32>) -> (PathBuf, PathBuf) {
    let origin = scratch.init("origin", false);
    delta_history(&origin, 19, 3, 8);
    let source = clone(scratch, &origin, "source", depth, false);
    let destination = clone(scratch, &origin, "destination.git", depth, true);
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
    commit_all(&source, "side work");
    (source, destination)
}

#[test]
fn fork_below_have_full_destination() {
    let scratch = Scratch::new("fork-full");
    let (source, destination) = fork_below_have(&scratch, None);
    check(&scratch, "fork-below-have-full", &source, &destination);
}

/// A shallow destination needs aggressive edges, as upload-pack's
/// `--shallow` selects; the plain rule over-counts here (spike Q1 caveat 2).
/// The estimate equals the oracle, so it walks edge-aggressive too.
#[test]
fn fork_below_have_shallow_destination() {
    let scratch = Scratch::new("fork-shallow");
    let (source, destination) = fork_below_have(&scratch, Some(3));
    let judged = check(&scratch, "fork-below-have-shallow", &source, &destination);
    let mut request = String::new();
    for want in &judged.wants {
        let _ = writeln!(request, "{want}");
    }
    request.push_str("--not\n");
    for have in &judged.haves {
        let _ = writeln!(request, "{have}");
    }
    let plain = String::from_utf8(ok(
        feed(
            args(&source, ["rev-list", "--objects-edge", "--stdin"]),
            request.as_bytes(),
        ),
        "rev-list",
    ))
    .unwrap()
    .lines()
    .filter(|line| !line.starts_with('-'))
    .count();
    assert!(
        judged.estimate.thin_pack.objects < u64::try_from(plain).unwrap(),
        "aggressive edges count fewer than the plain rule ({} vs {plain})",
        judged.estimate.thin_pack.objects
    );
}

// ---------------------------------------------------------------------------
// Reviewer fixtures (#55, R-N113, R-N116)
// ---------------------------------------------------------------------------

/// Reviewer fixture B (#55 R3-1): two held tips on a full destination, one
/// of which `git fetch` would never offer, a destination-only branch, and a
/// source branch deleted after it was merged. Both held tips are haves.
#[test]
fn fixture_b_two_held_tips() {
    let scratch = Scratch::new("fixture-b");
    let origin = scratch.init("origin", false);
    dated(&origin, "base.txt", "base", 1);
    run(
        args(&origin, ["checkout", "-q", "-b", "feature"]),
        "checkout",
    );
    dated(&origin, "f.txt", &noise(31, 2000), 2);
    run(args(&origin, ["checkout", "-q", "main"]), "checkout");
    for i in 1..=5 {
        dated(&origin, &format!("m{i}.txt"), &format!("m{i}"), 100 + i);
    }
    let source = clone(&scratch, &origin, "source.git", None, true);
    let destination = clone(&scratch, &origin, "destination", None, false);
    let root = text(
        args(&destination, ["rev-list", "--max-parents=0", "HEAD"]),
        "root",
    );
    run(
        args(&destination, ["checkout", "-q", "-b", "local", &root]),
        "checkout",
    );
    for i in 1..=20 {
        dated(&destination, &format!("l{i}.txt"), &format!("l{i}"), 10 + i);
    }
    run(args(&destination, ["checkout", "-q", "main"]), "checkout");
    let work = clone(&scratch, &origin, "work", None, false);
    run(
        args(&work, ["merge", "-q", "--no-edit", "origin/feature"]),
        "merge",
    );
    let merged = rev(&work, "HEAD");
    let mut fetch = git(&source);
    fetch
        .args(["fetch", "-q"])
        .arg(&work)
        .arg(format!("{merged}:refs/heads/merged"));
    run(fetch, "fetch");
    run(
        args(&source, ["update-ref", "-d", "refs/heads/feature"]),
        "delete",
    );
    let judged = check(&scratch, "fixture-b", &source, &destination);
    assert!(judged.haves.len() >= 2, "multi-held-tip");
    assert!(
        judged.destination_tips > judged.haves.len(),
        "a destination tip unknown to the source is not a have"
    );
}

/// Reviewer fixture P1 (R-N116): a shallow destination holding a commit and
/// its parent. The estimate offers the parent first; child first, upload-pack
/// drops it and sends more than the estimate counts.
#[test]
fn fixture_p1_have_order_matters() {
    for seed in 1..=2_u64 {
        let scratch = Scratch::new("fixture-p1");
        let origin = scratch.init("origin", false);
        dated(&origin, "base.txt", &format!("base{seed}"), 1);
        let boundary = dated(&origin, "keep.txt", &noise(80 + seed, 50), 2);
        let parent = dated(&origin, "g.txt", &noise(82, 400), 3);
        run(args(&origin, ["branch", "old", &parent]), "branch");
        run(args(&origin, ["rm", "-q", "g.txt"]), "rm");
        let child = dated(&origin, "rm.txt", "rm", 4);
        let source = clone(&scratch, &origin, "source", None, false);
        let destination = clone(&scratch, &origin, "destination", None, false);
        for repo in [&source, &destination] {
            fs::write(repo.join(".git/shallow"), format!("{boundary}\n")).unwrap();
        }
        run(
            args(&destination, ["branch", "-q", "old", "origin/old"]),
            "branch",
        );
        dated(&source, "g.txt", &noise(82, 400), 6);
        let name = format!("fixture-p1-seed{seed}");
        let judged = check(&scratch, &name, &source, &destination);
        assert_eq!(judged.haves, [parent.clone(), child.clone()]);
        let child_first = upload_pack(&source, &judged.wants, &[child, parent], &judged.shallow);
        assert!(
            u64::from(header_count(&child_first)) > judged.estimate.thin_pack.objects,
            "{name}: child-first haves send more than the estimate counts"
        );
    }
}

/// Reviewer fixture M (R-N116): a merge, annotated tags and three branches,
/// full and then shallow at one frontier.
#[test]
fn fixture_m_merges_tags_and_branches() {
    for shallow in [false, true] {
        let scratch = Scratch::new("fixture-m");
        let origin = scratch.init("origin", false);
        let boundary = dated(&origin, "keep.txt", &noise(111, 50), 1);
        run(args(&origin, ["checkout", "-q", "-b", "side"]), "checkout");
        let side = dated(&origin, "s.txt", &noise(112, 300), 2);
        run(args(&origin, ["tag", "-a", "-m", "t", "vs", &side]), "tag");
        run(args(&origin, ["rm", "-q", "s.txt"]), "rm");
        dated(&origin, "rm.txt", "rm", 3);
        run(args(&origin, ["checkout", "-q", "main"]), "checkout");
        dated(&origin, "m.txt", &noise(113, 100), 4);
        run(args(&origin, ["merge", "-q", "--no-edit", "side"]), "merge");
        run(args(&origin, ["checkout", "-q", "-b", "other"]), "checkout");
        dated(&origin, "t.txt", &noise(114, 200), 6);
        run(args(&origin, ["checkout", "-q", "main"]), "checkout");
        let destination = clone(&scratch, &origin, "destination", None, false);
        let source = clone(&scratch, &origin, "source", None, false);
        for repo in [&destination, &source] {
            run(args(repo, ["fetch", "-q", "--tags", "origin"]), "fetch");
            run(
                args(repo, ["branch", "-q", "side", "origin/side"]),
                "branch",
            );
        }
        run(
            args(&destination, ["branch", "-q", "other", "origin/other"]),
            "branch",
        );
        let tip = dated(&source, "s.txt", &noise(112, 300), 7);
        run(args(&source, ["tag", "-a", "-m", "t2", "v2", &tip]), "tag");
        run(
            args(&source, ["checkout", "-q", "-b", "feat", "origin/other"]),
            "checkout",
        );
        dated(&source, "t2.txt", &format!("{}x", noise(114, 200)), 9);
        run(args(&source, ["checkout", "-q", "main"]), "checkout");
        if shallow {
            for repo in [&source, &destination] {
                fs::write(repo.join(".git/shallow"), format!("{boundary}\n")).unwrap();
            }
        }
        let judged = check(
            &scratch,
            &format!("fixture-m-shallow-{shallow}"),
            &source,
            &destination,
        );
        assert_eq!(judged.estimate.missing.tag.count, 1, "v2 travels");
    }
}

/// Reviewer fixture F (R-N116): shallow at two frontiers, the held
/// grandparent on the second root.
#[test]
fn fixture_f_two_frontiers() {
    let scratch = Scratch::new("fixture-f");
    let origin = scratch.init("origin", false);
    dated(&origin, "a0.txt", "a0", 1);
    let first = dated(&origin, "a1.txt", &noise(121, 50), 2);
    run(
        args(&origin, ["checkout", "-q", "--orphan", "r2"]),
        "orphan",
    );
    run(args(&origin, ["rm", "-r", "-f", "-q", "."]), "rm");
    dated(&origin, "b0.txt", "b0", 3);
    let second = dated(&origin, "b1.txt", &noise(122, 50), 4);
    let held = dated(&origin, "h.txt", &noise(123, 300), 5);
    run(args(&origin, ["branch", "hold", &held]), "branch");
    run(args(&origin, ["rm", "-q", "h.txt"]), "rm");
    dated(&origin, "rm.txt", "rm", 6);
    dated(&origin, "z.txt", "z", 7);
    run(args(&origin, ["checkout", "-q", "main"]), "checkout");
    dated(&origin, "a2.txt", "a2", 8);
    let source = clone(&scratch, &origin, "source", None, false);
    let destination = clone(&scratch, &origin, "destination", None, false);
    for repo in [&source, &destination] {
        fs::write(repo.join(".git/shallow"), format!("{first}\n{second}\n")).unwrap();
        for branch in ["r2", "hold"] {
            run(
                args(repo, ["branch", "-q", branch, &format!("origin/{branch}")]),
                "branch",
            );
        }
    }
    run(args(&source, ["checkout", "-q", "r2"]), "checkout");
    dated(&source, "h.txt", &noise(123, 300), 9);
    run(args(&source, ["checkout", "-q", "main"]), "checkout");
    let judged = check(&scratch, "fixture-f", &source, &destination);
    assert_eq!(judged.shallow.len(), 2);
}

/// Reviewer fixture A3 (#55 R3): a shallow destination holding a commit and
/// its grandparent, neither advertised by the source, which re-adds a file
/// deleted in between.
#[test]
fn fixture_a3_grandparent_have() {
    let scratch = Scratch::new("fixture-a3");
    let origin = scratch.init("origin", false);
    dated(&origin, "base.txt", "base", 1);
    let boundary = dated(&origin, "keep.txt", &noise(71, 50), 2);
    let grandparent = dated(&origin, "g.txt", &noise(72, 400), 3);
    run(args(&origin, ["branch", "old", &grandparent]), "branch");
    run(args(&origin, ["rm", "-q", "g.txt"]), "rm");
    dated(&origin, "rm.txt", "rm", 4);
    dated(&origin, "y.txt", "y", 5);
    let destination = clone(&scratch, &origin, "destination", None, false);
    fs::write(destination.join(".git/shallow"), format!("{boundary}\n")).unwrap();
    run(
        args(&destination, ["branch", "-q", "old", "origin/old"]),
        "branch",
    );
    let source = scratch.init("source.git", true);
    let work = clone(&scratch, &origin, "work", None, false);
    let readded = dated(&work, "g.txt", &noise(72, 400), 6);
    let mut fetch = git(&source);
    fetch
        .args(["fetch", "-q"])
        .arg(&work)
        .arg(format!("{readded}:refs/heads/main"));
    run(fetch, "fetch");
    fs::write(source.join("shallow"), format!("{boundary}\n")).unwrap();
    let judged = check(&scratch, "fixture-a3", &source, &destination);
    assert_eq!(judged.haves.len(), 2);
    assert_eq!(judged.carried.len(), 2, "the re-added commit and its tree");
}

// ---------------------------------------------------------------------------
// Bitmapped sources, non-commit tips, nothing to send, raw paths
// ---------------------------------------------------------------------------

/// Source c1 (f original), c2 (f changed; the have), c3 (f reverted; the
/// want), repacked with a bitmap. Returns (source, have).
fn bitmap_source(scratch: &Scratch) -> (PathBuf, String) {
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
    commit_all(&source, "c3 reverts f.txt");
    run(args(&source, ["repack", "-adbq"]), "repack with bitmap");
    (source, have)
}

/// `pack.useBitmaps=false` is pinned: the bitmapped source still counts the
/// whole walk, as upload-pack under the same pin packs it.
#[test]
fn bitmap_source_full_destination() {
    let scratch = Scratch::new("bitmap");
    let (source, have) = bitmap_source(&scratch);
    let destination = destination_at(&scratch, &source, &have);
    let judged = check(&scratch, "bitmap-source", &source, &destination);
    assert_eq!(judged.carried.len(), 4, "c3, its tree, f.txt and h.txt");
}

/// The bitmapped source with a shallow destination at the have: the needed
/// reverted blob is counted.
#[test]
fn bitmap_source_shallow_destination() {
    let scratch = Scratch::new("bitmap-shallow");
    let (source, have) = bitmap_source(&scratch);
    run(args(&source, ["branch", "at-have", &have]), "branch");
    let mut command = git(&scratch.root);
    command
        .args([
            "clone", "-q", "--bare", "--depth", "1", "--branch", "at-have",
        ])
        .arg(format!("file://{}", source.display()))
        .arg("destination.git");
    run(command, "shallow clone");
    run(args(&source, ["branch", "-D", "at-have"]), "branch -D");
    let destination = scratch.path("destination.git");
    fs::write(source.join(".git/shallow"), format!("{have}\n")).unwrap();
    let reverted = rev(&source, "HEAD:f.txt");
    assert!(!has_object(&destination, &reverted));
    let judged = check(
        &scratch,
        "bitmap-source-shallow-destination",
        &source,
        &destination,
    );
    assert!(judged.carried.contains(&reverted), "the needed blob counts");
}

/// Spike D1: held tips that peel to no commit (an annotated tag of a tree, a
/// ref straight at a blob, a tag of a blob) go last, where they cannot make
/// upload-pack drop a have; a tag of a commit sorts with its commit. The
/// commit haves are a parent and child on a shallow destination, so the
/// order is load-bearing (fixture P1's shape).
#[test]
fn d1_non_commit_held_tips_go_last() {
    let scratch = Scratch::new("d1");
    let origin = scratch.init("origin", false);
    dated(&origin, "base.txt", "base", 1);
    let boundary = dated(&origin, "keep.txt", &noise(90, 50), 2);
    let parent = dated(&origin, "g.txt", &noise(91, 400), 3);
    run(args(&origin, ["branch", "old", &parent]), "branch");
    run(
        args(&origin, ["tag", "-a", "-m", "c", "commit-tag", &parent]),
        "tag",
    );
    let tree = rev(&origin, &format!("{parent}^{{tree}}"));
    run(
        args(&origin, ["tag", "-a", "-m", "t", "tree-tag", &tree]),
        "tag",
    );
    let blob = rev(&origin, "HEAD:keep.txt");
    run(
        args(&origin, ["tag", "-a", "-m", "b", "blob-tag", &blob]),
        "tag",
    );
    run(
        args(&origin, ["update-ref", "refs/blobs/keep", &blob]),
        "ref",
    );
    run(args(&origin, ["rm", "-q", "g.txt"]), "rm");
    let child = dated(&origin, "rm.txt", "rm", 4);
    let source = clone(&scratch, &origin, "source", None, false);
    let destination = clone(&scratch, &origin, "destination", None, false);
    for repo in [&source, &destination] {
        run(
            args(repo, ["fetch", "-q", "origin", "+refs/*:refs/mirror/*"]),
            "fetch",
        );
        fs::write(repo.join(".git/shallow"), format!("{boundary}\n")).unwrap();
    }
    dated(&source, "g.txt", &noise(91, 400), 6);
    let judged = check(&scratch, "d1-non-commit-tips", &source, &destination);
    let tree_tag = rev(&source, "refs/mirror/tags/tree-tag");
    let blob_tag = rev(&source, "refs/mirror/tags/blob-tag");
    let commit_tag = rev(&source, "refs/mirror/tags/commit-tag");
    let haves = &judged.haves;
    let mut tail: Vec<String> = haves[haves.len() - 3..].to_vec();
    tail.sort();
    let mut expected = vec![tree_tag, blob_tag, blob];
    expected.sort();
    assert_eq!(tail, expected, "non-commit tips last, by oid");
    let position = |oid: &str| haves.iter().position(|have| have == oid).unwrap();
    assert!(position(&parent) < position(&child), "ancestors first");
    assert!(
        position(&commit_tag) < position(&child),
        "a commit tag sorts with its commit"
    );
}

/// Nothing to send: the destination holds every want. The estimate builds no
/// pack.
#[test]
fn destination_holding_everything_gets_no_pack() {
    let scratch = Scratch::new("nothing");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 41, 2, 3);
    let destination = destination_at(&scratch, &source, commits.last().unwrap());
    let judged = check(&scratch, "nothing-to-send", &source, &destination);
    assert!(judged.wants.is_empty());
    assert_eq!(judged.estimate.thin_pack, ThinPack::default());
}

/// Spike caveat 5: tree paths that are not UTF-8, that carry spaces, or a
/// leading `-`, `#` or hex-looking name. The estimate still equals
/// upload-pack.
#[test]
fn paths_that_are_not_utf8() {
    let scratch = Scratch::new("raw-paths");
    let source = scratch.init("source.git", true);
    let names: [&[u8]; 5] = [
        // fast-import unquotes the C-style octal escapes to raw bytes.
        br"d\377ir/caf\351 x.txt",
        b"-edge-looking",
        b"#segment 0",
        b"0123456789abcdef0123456789abcdef01234567",
        b"end",
    ];
    let mut stream = Vec::new();
    for index in 0..names.len() {
        let body = noise(200 + u64::try_from(index).unwrap(), 30);
        let _ = write!(
            stream,
            "blob\nmark :{}\ndata {}\n{body}\n",
            index + 10,
            body.len()
        );
    }
    let commit = |mark: usize, from: Option<usize>, files: &[usize], stream: &mut Vec<u8>| {
        let message = format!("c{mark}");
        let _ = write!(
            stream,
            "commit refs/heads/main\nmark :{mark}\ncommitter T <t@invalid> {} +0000\ndata {}\n{message}\n",
            1_790_000_000 + mark * 60,
            message.len()
        );
        if let Some(from) = from {
            let _ = writeln!(stream, "from :{from}");
        }
        for file in files {
            stream.extend_from_slice(b"M 100644 :");
            let _ = write!(stream, "{} \"", file + 10);
            stream.extend_from_slice(names[*file]);
            stream.extend_from_slice(b"\"\n");
        }
        stream.push(b'\n');
    };
    commit(1, None, &[4], &mut stream);
    commit(2, Some(1), &[0, 1, 2, 3], &mut stream);
    ok(
        feed(args(&source, ["fast-import", "--quiet"]), &stream),
        "fast-import",
    );
    let listed = run(
        args(&source, ["ls-tree", "-r", "--name-only", "-z", "main"]),
        "ls-tree",
    );
    assert!(
        listed
            .split(|b| *b == 0)
            .any(|name| name.starts_with(b"d\xffir/")),
        "a path that is not UTF-8 is in the tree"
    );
    let destination = destination_at(&scratch, &source, &rev(&source, "main~1"));
    check(&scratch, "raw-paths", &source, &destination);
}
