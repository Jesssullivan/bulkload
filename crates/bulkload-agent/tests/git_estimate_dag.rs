//! P66 ESTIMATE-DAG: `git-carry-estimate` equals the upload-pack oracle over
//! random DAGs (OI-1003-Q42, OI-1003-Q44; the estimate leg of P46; R-N74,
//! R-N97, R-N113, R-N116).
//!
//! This is the estimate half of `random_dags_equal_upload_pack` in
//! `tests/git_carry_v2.rs` (deleted with carry v2 by architecture review WP2
//! PR 3, OI-1003-Q56; tag `carry-v2-final` holds it), rehomed so it outlives
//! carry v2. It uses nothing PR 3 deleted: the product
//! code under test is `bulkload_agent::git_carry::estimate::estimate`, and
//! the request the oracle is fed is derived here from the two repositories
//! with plain `git`, not from the sender's first round.
//!
//! The oracle is R-N113's: `upload-pack --stateless-rpc` (protocol v2) fed M1's
//! first round, under the pack pins. The wants are the source's tips that the
//! destination does not hold. The haves are exactly the destination's tips
//! that the source holds, ancestors first (R-N116).
//!
//! [`check`] asserts on every DAG:
//! - **Request.** `haves_used` is the held-tip count, and
//!   `destination_tip_count` counts every destination tip. A destination-only
//!   commit is a tip but not a have.
//! - **Size.** `thin_pack` equals the oracle's pack: the same header count and
//!   the same byte length. An oracle pack of zero objects counts as the
//!   estimate's "no pack" (`ThinPack::default()`). When every want is
//!   reachable from the haves, upload-pack still streams an empty 32-byte
//!   pack, while the estimate builds nothing.
//! - **Object set.** The oracle's pack indexes on its own against the
//!   destination's store (`index-pack --fix-thin` into a throwaway object
//!   directory). The per-type count and `%(objectsize:disk)` tally of exactly
//!   the oids it carries equals `missing`, with nothing unavailable.
//! - **Closure.** `source` is the same tally over the closure of the
//!   source's tips.
//! - **Encoding.** [`check`] returns how the oracle pack stores what it
//!   carries, read from each object's pack header: whole, `OFS_DELTA` on an
//!   earlier object in the pack, or `REF_DELTA` on a base that sits past the
//!   received bytes once `--fix-thin` has appended it (a have: the thin
//!   case). Byte equality with such a pack is what makes `--thin`,
//!   `--delta-base-offset` and the preferred bases observable. Each PINNED row
//!   requires the deltas its shape exists for, so no row can quietly stop
//!   being thin or stop holding an in-pack delta.
//!
//! **Content.** A DAG edits four files: one at the root, two under a
//! directory (`a/f1.txt`, `a/sub/f2.txt`) that a rename edit moves to `b/` and
//! back, and one under `doc/`. A file's first write is a fresh body of more
//! than 1 KB; every later write rewrites one to three lines of the file's
//! current version, inherited from the first parent (and on odd seeds appends
//! one), so a new blob deltifies against its previous version, inside the
//! pack or against a have.
//! A rename over a have moves an unchanged subtree to a path the have lacks,
//! the shape where `pack.useSparse` would pack it again.
//!
//! **Corpus.** CI runs a fixed seed and 12 cases (`test_support::prop_config`),
//! plus a PINNED table of shapes the generator reaches rarely.
//! `BULKLOAD_PROPTEST_DEEP=1` runs twenty times the cases from the same
//! fixed seed (OI-1003-Q78). The helper is `#[cfg(test)] pub(crate)` in the library, out of an
//! integration test's reach, so this file compiles the same source file as a
//! local module (`#[path]`) instead of mirroring it or widening the library
//! API. `tests/prop_seed_guard.rs` holds every property to that helper.
//!
//! Every test builds its repositories under the system temp dir, runs the
//! real `git` on `PATH`, and removes them afterwards. Measurements print as
//! `p66 case=<name> key=value ...` lines (`--nocapture`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use bulkload_agent::git_carry::estimate::{estimate, Destination, Tally, ThinPack};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Corpus configuration
// ---------------------------------------------------------------------------

/// The shared helper itself (OI-1003-Q7): the same source file as the
/// library's `test_support`, so the seed and the deep switch cannot drift.
#[path = "../src/test_support.rs"]
mod test_support;

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
            "bulkload-estimate-dag-{name}-{}-{}-{nanos}",
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

    /// A fresh bare repository.
    fn bare(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let mut command = git(&self.root);
        command
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&path);
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
}

/// `count` distinct-looking lines drawn from `seed` (the line shape
/// `tests/git_carry_v2.rs`'s `noise` writes), each 24 or 25 bytes.
fn noise(seed: u64, count: usize) -> Vec<String> {
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761) | 1);
    (0..count)
        .map(|i| format!("line {i} {:016x}\n", rng.next()))
        .collect()
}

/// `body` with one to three of its lines (chosen by `seed`) rewritten and,
/// on an odd `seed`, one line appended: a small change, so the result
/// deltifies against `body`.
fn rewrite(mut body: Vec<String>, seed: u64) -> Vec<String> {
    let mut rng = Rng(seed.rotate_left(17) | 1);
    let len = u64::try_from(body.len()).unwrap();
    for _ in 0..=seed % 3 {
        let at = usize::try_from(rng.next() % len).unwrap();
        body[at] = format!("edit {at} {:016x}\n", rng.next());
    }
    if seed % 2 == 1 {
        body.push(format!("more {:016x}\n", rng.next()));
    }
    body
}

fn objects_dir(repo: &Path) -> PathBuf {
    let dotgit = repo.join(".git");
    if dotgit.is_dir() {
        dotgit.join("objects")
    } else {
        repo.join("objects")
    }
}

fn has_object(repo: &Path, oid: &str) -> bool {
    args(repo, ["cat-file", "-e", oid])
        .status()
        .expect("cat-file")
        .success()
}

/// Every ref tip and `HEAD` of `repo` (a bare repository has no other
/// worktree `HEAD` for the estimate's probe to add).
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
/// commit.
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

/// The whole object closure of `tips` in `repo`.
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
/// given exactly `haves`, in that order, under the pack pins. Returns the
/// pack it streams on sideband 1 (empty when it streams none).
fn upload_pack(source: &Path, wants: &BTreeSet<String>, haves: &[String]) -> Vec<u8> {
    let mut request = Vec::new();
    pkt_line(&mut request, "command=fetch\n");
    request.extend_from_slice(b"0001");
    for line in ["thin-pack\n", "ofs-delta\n", "no-progress\n"] {
        pkt_line(&mut request, line);
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

/// How a pack stores the objects it carries, by pack object type. Under
/// `ofs-delta` a delta on an object in the pack is an `OFS_DELTA`, so a
/// `REF_DELTA` names a base the pack does not carry: `thin_delta` counts
/// those whose base `--fix-thin` appended past the received bytes, and
/// `ref_delta` any whose base is inside them (none expected).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Encoding {
    whole: usize,
    ofs_delta: usize,
    thin_delta: usize,
    ref_delta: usize,
}

/// What a thin pack carries and how it stores it.
#[derive(Debug, Default)]
struct Carried {
    oids: BTreeSet<String>,
    encoding: Encoding,
}

/// The oids a thin pack carries, and proof that it indexes on its own: it is
/// indexed with `--fix-thin` into a throwaway object directory whose only
/// alternate is `destination`'s store, and the set is the `.idx` entries
/// whose offset lies inside the received bytes (`--fix-thin` appends its
/// bases after them). Each carried object's type is read from its header in
/// the received bytes; a `REF_DELTA`'s base is located through the same
/// `.idx`.
fn carried(scratch: &Scratch, destination: &Path, pack: &[u8]) -> Carried {
    if header_count(pack) == 0 {
        return Carried::default();
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
    let offsets: BTreeMap<String, usize> = String::from_utf8(listing)
        .unwrap()
        .lines()
        .map(|line| {
            let mut fields = line.split(' ');
            let offset = fields.next().unwrap().parse().unwrap();
            (fields.next().unwrap().to_owned(), offset)
        })
        .collect();
    let mut carried = Carried::default();
    for (oid, &offset) in &offsets {
        if offset >= limit {
            continue;
        }
        carried.oids.insert(oid.clone());
        // The type is bits 4-6 of the first header byte; the size runs on
        // while the high bit is set, and a REF_DELTA's base id follows.
        let kind = (pack[offset] >> 4) & 0x7;
        let mut at = offset;
        while pack[at] & 0x80 != 0 {
            at += 1;
        }
        at += 1;
        match kind {
            1..=4 => carried.encoding.whole += 1,
            6 => carried.encoding.ofs_delta += 1,
            7 => {
                use std::fmt::Write as _;
                let mut base = String::new();
                for byte in &pack[at..at + oid.len() / 2] {
                    let _ = write!(base, "{byte:02x}");
                }
                let base_offset = *offsets.get(&base).unwrap_or_else(|| {
                    panic!("REF_DELTA {oid}: base {base} is not in the indexed pack")
                });
                if base_offset >= limit {
                    carried.encoding.thin_delta += 1;
                } else {
                    carried.encoding.ref_delta += 1;
                }
            }
            other => panic!("object {oid} at {offset}: pack object type {other}"),
        }
    }
    carried
}

// ---------------------------------------------------------------------------
// The estimate, judged
// ---------------------------------------------------------------------------

/// Estimate `source` -> `destination` and assert P66 (module docs) against
/// the oracle fed M1's first round, derived here from the repositories.
/// Returns the oracle pack's [`Encoding`], which the estimate's bytes equal.
fn check(scratch: &Scratch, name: &str, source: &Path, destination: &Path) -> Encoding {
    let destination_tips = tips_of(destination);
    let held: BTreeSet<String> = destination_tips
        .iter()
        .filter(|tip| has_object(source, tip))
        .cloned()
        .collect();
    let haves = ancestors_first(source, &held);
    let source_tips = tips_of(source);
    let wants: BTreeSet<String> = source_tips.difference(&held).cloned().collect();

    let pack = upload_pack(source, &wants, &haves);
    let objects = header_count(&pack);
    let oracle = if objects == 0 {
        ThinPack::default()
    } else {
        ThinPack {
            bytes: u64::try_from(pack.len()).unwrap(),
            objects: u64::from(objects),
        }
    };
    let Carried { oids, encoding } = carried(scratch, destination, &pack);
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
        "p66 case={name} destination_tips={} held={} wants={} oracle_stream_bytes={} \
         oracle_objects={} oracle_bytes={} oracle_whole={} oracle_ofs_delta={} \
         oracle_thin_delta={} oracle_ref_delta={} estimate_objects={} estimate_bytes={} \
         missing_objects={} missing_bytes_disk={} source_objects={}",
        destination_tips.len(),
        haves.len(),
        wants.len(),
        pack.len(),
        oracle.objects,
        oracle.bytes,
        encoding.whole,
        encoding.ofs_delta,
        encoding.thin_delta,
        encoding.ref_delta,
        measured.thin_pack.objects,
        measured.thin_pack.bytes,
        measured.missing.objects(),
        measured.missing.bytes_disk(),
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
    assert_eq!(measured.destination_shallow_count, 0, "{name}: full");
    assert_eq!(measured.source_shallow_count, 0, "{name}: full");
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
    encoding
}

// ---------------------------------------------------------------------------
// Random DAGs
// ---------------------------------------------------------------------------

/// The four files a DAG edits.
const FILES: usize = 4;

/// File `file`'s path. Files 1 and 2 live under the directory an
/// [`Edit::Rename`] toggles between `a` and `b`, file 2 in its subdirectory
/// `sub`, so a rename moves a subtree a have may hold unchanged under the old
/// path.
fn path(file: usize, renamed: bool) -> String {
    let dir = if renamed { "b" } else { "a" };
    match file {
        0 => "f0.txt".to_owned(),
        1 => format!("{dir}/f1.txt"),
        2 => format!("{dir}/sub/f2.txt"),
        _ => "doc/f3.txt".to_owned(),
    }
}

/// One change a commit makes to its first parent's tree.
#[derive(Debug, Clone, Copy)]
enum Edit {
    /// Write file `.0` from seed `.1`: a fresh body of 48 to 99 lines (over
    /// 1 KB) when the tree lacks the file, else [`rewrite`] of the body it
    /// holds.
    Write(usize, u64),
    /// Move directory `a` to `b`, or `b` back to `a`, contents unchanged.
    Rename,
}

/// Parent indexes, and the edits made to the first parent's tree.
type Commit = (Vec<usize>, Vec<Edit>);

/// A commit's tree as [`build`] tracks it: where the renamed directory is,
/// and each file's lines (`None` until its first write).
#[derive(Debug, Clone, Default)]
struct Files {
    renamed: bool,
    bodies: [Option<Vec<String>>; FILES],
}

impl Files {
    fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Rename => self.renamed = !self.renamed,
            Edit::Write(file, seed) => {
                let body = self.bodies[file].take().map_or_else(
                    || noise(seed, 48 + usize::try_from(seed % 52).unwrap()),
                    |body| rewrite(body, seed),
                );
                self.bodies[file] = Some(body);
            }
        }
    }
}

/// A random history: each commit names up to two earlier parents and makes
/// one to three edits to its first parent's tree. `source_tips` and `held`
/// index commits; `stranger` adds a destination-only commit the source never
/// saw.
#[derive(Debug, Clone)]
struct Dag {
    commits: Vec<Commit>,
    source_tips: Vec<usize>,
    held: Vec<usize>,
    stranger: bool,
}

/// A write of one of the four files (five in six), or a directory rename.
fn any_edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        5 => (0..FILES, any::<u64>()).prop_map(|(file, seed)| Edit::Write(file, seed)),
        1 => Just(Edit::Rename),
    ]
}

/// `tests/git_carry_v2.rs`'s generator without the sender's segment-cap
/// dimension, which the estimate does not have, and with [`any_edit`]'s
/// derived writes and renames in place of fresh content in a flat namespace.
/// The last commit is always a source tip, and the source's `HEAD`.
fn dag() -> impl Strategy<Value = Dag> {
    (2_usize..9).prop_flat_map(|n| {
        let commits = (0..n)
            .map(|i| {
                let parents = if i == 0 {
                    Just(Vec::new()).boxed()
                } else {
                    proptest::collection::btree_set(0..i, 1..=2.min(i))
                        .prop_map(|set| set.into_iter().collect())
                        .boxed()
                };
                let edits = proptest::collection::vec(any_edit(), 1..=3);
                (parents, edits)
            })
            .collect::<Vec<_>>();
        (
            commits,
            proptest::collection::btree_set(0..n, 1..=n),
            proptest::collection::btree_set(0..n, 0..=n),
            any::<bool>(),
        )
            .prop_map(move |(commits, source_tips, held, stranger)| {
                let mut source_tips: Vec<usize> = source_tips.into_iter().collect();
                if !source_tips.contains(&(n - 1)) {
                    source_tips.push(n - 1);
                }
                Dag {
                    commits,
                    source_tips,
                    held: held.into_iter().collect(),
                    stranger,
                }
            })
    })
}

/// Build `dag` in a fresh bare source with one fast-import stream that
/// writes each commit's whole tree (`deleteall`, then every file it holds),
/// then give a bare destination exactly the held commits.
fn build(scratch: &Scratch, dag: &Dag) -> (PathBuf, PathBuf) {
    use std::fmt::Write as _;
    let source = scratch.bare("source.git");
    let mut trees: Vec<Files> = Vec::with_capacity(dag.commits.len());
    let mut stream = String::new();
    for (index, (parents, edits)) in dag.commits.iter().enumerate() {
        let mut files = parents
            .first()
            .map_or_else(Files::default, |first| trees[*first].clone());
        for edit in edits {
            files.apply(*edit);
        }
        let message = format!("c{index}");
        let _ = write!(
            stream,
            "commit refs/fi/c{index}\nmark :{}\ncommitter T <t@invalid> {} +0000\ndata {}\n{message}\n",
            index + 1,
            1_790_000_000 + index * 60,
            message.len()
        );
        let mut parents = parents.iter();
        if let Some(first) = parents.next() {
            let _ = writeln!(stream, "from :{}", first + 1);
        }
        for other in parents {
            let _ = writeln!(stream, "merge :{}", other + 1);
        }
        stream.push_str("deleteall\n");
        for (file, body) in files.bodies.iter().enumerate() {
            let Some(body) = body else { continue };
            let body = body.concat();
            let _ = write!(
                stream,
                "M 100644 inline {}\ndata {}\n{body}\n",
                path(file, files.renamed),
                body.len()
            );
        }
        stream.push('\n');
        trees.push(files);
    }
    ok(
        feed(args(&source, ["fast-import", "--quiet"]), stream.as_bytes()),
        "fast-import",
    );
    let oid = |index: usize| rev(&source, &format!("refs/fi/c{index}"));
    let destination = scratch.bare("destination.git");
    if !dag.held.is_empty() {
        let mut fetch = git(&destination);
        fetch.args(["fetch", "-q"]).arg(&source);
        for index in &dag.held {
            fetch.arg(format!("refs/fi/c{index}:refs/heads/d{index}"));
        }
        run(fetch, "fetch held");
    }
    for index in &dag.source_tips {
        run(
            args(
                &source,
                ["update-ref", &format!("refs/heads/b{index}"), &oid(*index)],
            ),
            "tip",
        );
    }
    // The fast-import refs go; their commits stay in the source's store, so
    // a held commit no source ref names is still a have.
    for index in 0..dag.commits.len() {
        run(
            args(&source, ["update-ref", "-d", &format!("refs/fi/c{index}")]),
            "drop",
        );
    }
    run(
        args(
            &source,
            [
                "symbolic-ref",
                "HEAD",
                &format!("refs/heads/b{}", dag.commits.len() - 1),
            ],
        ),
        "HEAD",
    );
    if dag.stranger {
        let tree = text_of(feed(args(&destination, ["mktree"]), b""), "mktree");
        let stranger = text_of(
            feed(
                args(&destination, ["commit-tree", &tree, "-m", "stranger"]),
                b"",
            ),
            "commit-tree",
        );
        run(
            args(
                &destination,
                ["update-ref", "refs/heads/stranger", &stranger],
            ),
            "stranger",
        );
    }
    (source, destination)
}

fn check_dag(name: &str, dag: &Dag) -> Encoding {
    let scratch = Scratch::new(name);
    let (source, destination) = build(&scratch, dag);
    check(&scratch, name, &source, &destination)
}

/// One write of file `file` from seed `seed`.
fn edit(file: usize, seed: u64) -> Vec<Edit> {
    vec![Edit::Write(file, seed)]
}

/// A PINNED shape, and the deltas its oracle pack must hold at least:
/// `thin` `REF_DELTA`s on a have and `ofs` `OFS_DELTA`s inside the pack. The
/// estimate's bytes equal that pack's, so a row that requires a thin delta
/// fails if the estimate loses `--thin` or its preferred bases, and one that
/// requires an in-pack delta fails if it loses `--delta-base-offset`.
struct Pinned {
    name: &'static str,
    dag: Dag,
    thin: usize,
    ofs: usize,
}

/// Shapes the generator reaches rarely in 12 cases, each judged by the same
/// [`check`].
#[test]
#[allow(clippy::too_many_lines)] // One data row per shape.
fn pinned_dags_equal_upload_pack() {
    // c0 writes f0.txt, c1 writes a/f1.txt, and each later commit rewrites
    // the file its grandparent wrote.
    let chain = |n: usize| -> Vec<Commit> {
        (0..n)
            .map(|i| {
                let parents = if i == 0 { Vec::new() } else { vec![i - 1] };
                (parents, edit(i % 2, 7 + u64::try_from(i).unwrap()))
            })
            .collect()
    };
    let pinned = [
        // The thin case: the destination holds c2, the tip's parent, whose
        // tree holds the a/f1.txt c1 wrote (56 lines, 1.4 KB). The tip
        // rewrites two of those lines, so the pack is the tip's commit, root
        // tree and tree a whole, and the new blob as a REF_DELTA on the held
        // version (4 objects, 376 bytes with git 2.52 and 2.54).
        Pinned {
            name: "parent-held",
            dag: Dag {
                commits: chain(4),
                source_tips: vec![3],
                held: vec![2],
                stranger: false,
            },
            thin: 1,
            ofs: 0,
        },
        // A want reachable from a have: upload-pack streams an empty pack
        // (zero objects), and the estimate builds none.
        Pinned {
            name: "want-behind-a-have",
            dag: Dag {
                commits: chain(3),
                source_tips: vec![1, 2],
                held: vec![2],
                stranger: false,
            },
            thin: 0,
            ofs: 0,
        },
        // Every source tip held: no want, so no pack at all.
        Pinned {
            name: "all-held",
            dag: Dag {
                commits: chain(3),
                source_tips: vec![2],
                held: vec![2],
                stranger: false,
            },
            thin: 0,
            ofs: 0,
        },
        // Nothing held, one destination-only tip: no have, the whole
        // closure is missing, and the stranger counts as a tip only. Both
        // versions of f0.txt travel, so one is an OFS_DELTA on the other.
        Pinned {
            name: "nothing-held-stranger",
            dag: Dag {
                commits: chain(3),
                source_tips: vec![2],
                held: Vec::new(),
                stranger: true,
            },
            thin: 0,
            ofs: 1,
        },
        // A merge over two held branch tips, offered ancestors first, plus a
        // held commit no source ref names.
        Pinned {
            name: "merge-over-held-branches",
            dag: Dag {
                commits: vec![
                    (Vec::new(), edit(0, 1)),
                    (vec![0], edit(1, 2)),
                    (vec![0], edit(2, 3)),
                    (vec![1, 2], vec![Edit::Write(0, 4), Edit::Write(3, 5)]),
                    (vec![3], edit(1, 6)),
                ],
                source_tips: vec![4],
                held: vec![0, 1, 2],
                stranger: true,
            },
            thin: 1,
            ofs: 0,
        },
        // Three rewrites of a/f1.txt over a held first version: the pack
        // holds deltas both on the have and on each other.
        Pinned {
            name: "rewrites-over-a-have",
            dag: Dag {
                commits: vec![
                    (Vec::new(), edit(1, 21)),
                    (vec![0], edit(1, 22)),
                    (vec![1], edit(1, 23)),
                    (vec![2], edit(1, 24)),
                ],
                source_tips: vec![3],
                held: vec![0],
                stranger: false,
            },
            thin: 1,
            ofs: 1,
        },
        // A directory rename over a have: the tip moves a/ to b/ and
        // rewrites b/f1.txt, while b/sub is the subtree the have holds as
        // a/sub. The walk leaves b/sub and its blob out; sparse edge marking
        // (`pack.useSparse`) would pack them again, and the estimate would
        // refuse on the count.
        Pinned {
            name: "directory-rename-over-a-have",
            dag: Dag {
                commits: vec![
                    (
                        Vec::new(),
                        vec![Edit::Write(0, 31), Edit::Write(1, 32), Edit::Write(2, 33)],
                    ),
                    (vec![0], edit(3, 34)),
                    (vec![1], vec![Edit::Rename, Edit::Write(1, 35)]),
                ],
                source_tips: vec![2],
                held: vec![1],
                stranger: false,
            },
            thin: 0,
            ofs: 0,
        },
    ];
    for row in &pinned {
        let encoding = check_dag(row.name, &row.dag);
        assert!(
            encoding.thin_delta >= row.thin && encoding.ofs_delta >= row.ofs,
            "{}: the oracle pack holds {encoding:?}; the row needs at least {} thin and {} \
             in-pack deltas",
            row.name,
            row.thin,
            row.ofs
        );
    }
}

proptest! {
    #![proptest_config(test_support::prop_config(12))]

    /// On random DAGs, with random source tips, held commits (named or not
    /// by any source ref) and a destination-only tip, the estimate equals
    /// the upload-pack oracle: the thin pack's objects and bytes, and the
    /// missing set's per-type count and disk size.
    #[test]
    fn estimate_equals_upload_pack_over_random_dags(dag in dag()) {
        check_dag("random-dag", &dag);
    }
}
