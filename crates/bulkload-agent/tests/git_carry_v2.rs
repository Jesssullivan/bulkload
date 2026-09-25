//! W6 M1 PR 1: the negotiated thin-pack sender against the upload-pack oracle
//! (R-N60, R-N97, R-N113, R-N116, R-N75; bulkload#48, TIN-4545).
//!
//! Every test builds its repositories under the system temp dir, runs the
//! real `git` on `PATH`, and removes them afterwards. The product code under
//! test is `bulkload_agent::git_carry::carry_v2`; the oracle is `upload-pack
//! --stateless-rpc` (protocol v2) given exactly the sender's have list, in
//! its order, and the destination's shallow lines, under the M1 pins.
//!
//! The gate, asserted on every fixture by [`check`]:
//! - the haves are exactly the held tips (every destination tip the source
//!   holds), ancestors first, with non-commit tips last (spike D1);
//! - the sent object set, the union of every segment, equals upload-pack's;
//! - sent objects <= the estimate's `missing_objects`, and sent bytes <=
//!   1.1 x `missing_thin_pack_bytes`;
//! - each segment's header counts exactly its list lines, the segments are
//!   disjoint, and each indexes alone against the destination's store;
//! - one segment is byte-identical to upload-pack's pack;
//! - the destination ingests every segment and passes `fsck --strict`.
//!
//! Measurements print as `m1 <case> key=value ...` lines (`--nocapture`).

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

use bulkload_agent::git_carry::carry_v2::{
    first_round, FirstRound, ListStore, Offer, PackPlan, Source, DEFAULT_SEGMENT_CAP,
};
use bulkload_agent::git_carry::estimate::{estimate, Destination, StderrStore};
use bulkload_agent::BulkloadRefusal;

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// Fixture Git: hooks off (R-N98 allows it in fixtures this test creates and
/// removes), the M1 pins, a fixed identity and date, no user or system
/// configuration and no redirecting variable from the parent environment.
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
            "bulkload-carry-v2-{name}-{}-{}-{nanos}",
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

    /// A private (0700) directory, as a state dir must be.
    fn state(&self, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = self.path(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
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

// ---------------------------------------------------------------------------
// The oracle and the object-set reader
// ---------------------------------------------------------------------------

fn pkt_line(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(line.as_bytes());
}

/// R-N113's exactness oracle: `upload-pack --stateless-rpc` (protocol v2)
/// given exactly `haves`, in that order, plus the `shallow` lines, under the
/// M1 pins. Returns the pack it streams on sideband 1 (empty when none).
fn upload_pack(source: &Path, wants: &[String], haves: &[String], shallow: &[String]) -> Vec<u8> {
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
    if pack.is_empty() {
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
        .unwrap();
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
    .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    args(repo, ["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .unwrap()
        .success()
}

/// R-N113 and R-N116 checked independently of the product: the haves are
/// exactly the destination tips the source holds; no commit have precedes a
/// have that is its proper ancestor; every non-commit have comes last.
fn assert_haves(name: &str, source: &Path, destination: &Path, round: &FirstRound) {
    let held: BTreeSet<String> = tips_of(destination)
        .into_iter()
        .filter(|tip| has_object(source, tip))
        .collect();
    let offered: BTreeSet<String> = round.haves().iter().cloned().collect();
    assert_eq!(offered, held, "{name}: haves are exactly the held tips");
    assert_eq!(
        offered.len(),
        round.haves().len(),
        "{name}: no duplicate have"
    );
    let peeled: Vec<Option<String>> = round
        .haves()
        .iter()
        .map(|have| peeled_commit(source, have))
        .collect();
    let first_non_commit = peeled.iter().position(Option::is_none);
    if let Some(at) = first_non_commit {
        assert!(
            peeled[at..].iter().all(Option::is_none),
            "{name}: non-commit haves come last"
        );
    }
    for (i, earlier) in peeled.iter().enumerate() {
        for later in peeled.iter().skip(i + 1) {
            if let (Some(earlier), Some(later)) = (earlier, later) {
                assert!(
                    earlier == later || !is_ancestor(source, later, earlier),
                    "{name}: {later} is an ancestor of {earlier} but comes after it"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The M1 sender, judged
// ---------------------------------------------------------------------------

/// What one sent plan measured.
struct Sent {
    plan: PackPlan,
    round: FirstRound,
    segments: Vec<Vec<u8>>,
    set: BTreeSet<String>,
    oracle: Vec<u8>,
    oracle_set: BTreeSet<String>,
}

impl Sent {
    fn bytes(&self) -> usize {
        self.segments.iter().map(Vec::len).sum()
    }

    fn objects(&self) -> u32 {
        self.segments.iter().map(|s| header_count(s)).sum()
    }
}

/// Run M1's sender for `source` -> `destination` with segment cap `cap` and
/// assert the gate (module docs). Ingests every segment into the destination
/// and checks `fsck --strict`.
fn check(scratch: &Scratch, name: &str, source: &Path, destination: &Path, cap: u64) -> Sent {
    let sender = Source::probe(source, None).unwrap();
    let offer = Offer::probe(destination, None).unwrap();
    let wants = sender.wants().unwrap();
    let round = first_round(&sender, &offer, &wants, None).unwrap();
    assert_haves(name, source, destination, &round);
    let plan = PackPlan::build(&sender, &round, cap, None).unwrap();

    let mut segments = Vec::new();
    let mut set = BTreeSet::new();
    for index in 0..plan.segments() {
        let mut pack = Vec::new();
        let receipt = plan.send_segment(&sender, index, &mut pack, None).unwrap();
        assert_eq!(receipt.index, index);
        assert_eq!(receipt.bytes, u64::try_from(pack.len()).unwrap());
        assert_eq!(receipt.blake3, blake3::hash(&pack).to_hex().to_string());
        assert_eq!(
            usize::try_from(receipt.objects).unwrap(),
            plan.segment_objects(index).unwrap(),
            "{name}: segment {index} header counts its list lines"
        );
        let carried = carried(scratch, destination, &pack);
        assert_eq!(
            carried.len(),
            plan.segment_objects(index).unwrap(),
            "{name}: segment {index} indexes alone and carries its lines"
        );
        assert!(set.is_disjoint(&carried), "{name}: segments are disjoint");
        set.extend(carried);
        segments.push(pack);
    }

    let oracle = upload_pack(source, round.wants(), round.haves(), round.shallow());
    let oracle_set = carried(scratch, destination, &oracle);
    assert_eq!(set, oracle_set, "{name}: R-N113 sent set == upload-pack");

    let estimate = estimate(source, &Destination::Local(destination.to_path_buf())).unwrap();
    let measured = Sent {
        plan,
        round,
        segments,
        set,
        oracle,
        oracle_set,
    };
    let objects = u64::from(measured.objects());
    let bytes = u64::try_from(measured.bytes()).unwrap();
    assert!(
        objects <= estimate.missing.objects(),
        "{name}: sent objects {objects} <= estimate {}",
        estimate.missing.objects()
    );
    assert_eq!(
        objects, estimate.thin_pack.objects,
        "{name}: sent == estimate"
    );
    if cap == DEFAULT_SEGMENT_CAP {
        assert!(
            bytes * 10 <= estimate.thin_pack.bytes * 11,
            "{name}: byte gate {bytes} <= 1.1 x {}",
            estimate.thin_pack.bytes
        );
    }
    if measured.segments.len() == 1 {
        assert_eq!(
            measured.segments[0], measured.oracle,
            "{name}: one segment is byte-identical to upload-pack"
        );
    }
    println!(
        "m1 case={name} cap={cap} held={} non_commit_haves={} wants={} shallow={} edges={} \
         segments={} sent_objects={objects} sent_bytes={bytes} oracle_objects={} \
         oracle_bytes={} estimate_objects={} estimate_bytes={} sent_set_eq_oracle=true \
         byte_ratio={:.4}",
        measured.round.haves().len(),
        measured.round.non_commit_haves(),
        measured.round.wants().len(),
        measured.round.shallow().len(),
        measured.plan.edges(),
        measured.segments.len(),
        measured.oracle_set.len(),
        measured.oracle.len(),
        estimate.missing.objects(),
        estimate.thin_pack.bytes,
        ratio(bytes, estimate.thin_pack.bytes),
    );

    ingest(
        destination,
        &measured.segments,
        measured.round.wants(),
        name,
    );
    measured
}

fn ratio(value: u64, base: u64) -> f64 {
    if base == 0 {
        return 1.0;
    }
    let value = f64::from(u32::try_from(value).unwrap());
    let base = f64::from(u32::try_from(base).unwrap());
    value / base
}

/// Index every segment into `destination`, point a test ref at each want,
/// and require `fsck --strict` to pass (the destination-side quarantine and
/// ref transaction are M1 PR 2).
fn ingest(destination: &Path, segments: &[Vec<u8>], wants: &[String], name: &str) {
    for segment in segments {
        ok(
            feed(
                args(destination, ["index-pack", "--stdin", "--fix-thin"]),
                segment,
            ),
            "index-pack",
        );
    }
    let mut transaction = Vec::new();
    for (index, want) in wants.iter().enumerate() {
        let _ = write!(
            transaction,
            "create refs/carry-test/{name}/{index}\0{want}\0"
        );
    }
    ok(
        feed(
            args(destination, ["update-ref", "--stdin", "-z"]),
            &transaction,
        ),
        "update-ref",
    );
    let fsck = args(
        destination,
        ["fsck", "--strict", "--no-progress", "--no-dangling"],
    )
    .output()
    .unwrap();
    assert!(
        fsck.status.success(),
        "{name}: fsck: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

// ---------------------------------------------------------------------------
// Spike Q1 fixtures, now through the product sender
// ---------------------------------------------------------------------------

#[test]
fn delta_heavy() {
    let scratch = Scratch::new("delta");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 7, 4, 12);
    let destination = destination_at(&scratch, &source, &commits[4]);
    check(
        &scratch,
        "delta-heavy",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
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
    check(
        &scratch,
        "rename-heavy",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
}

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
    let sent = check(
        &scratch,
        "tag-only",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert_eq!(sent.set.len(), 2, "exactly the two tag objects");
}

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
    commit_all(&source, "add submodule gitlink");
    let destination = destination_at(&scratch, &source, &base);
    check(
        &scratch,
        "submodule-gitlink",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
}

#[test]
fn matching_shallow_frontier() {
    let scratch = Scratch::new("shallow");
    let origin = scratch.init("origin", false);
    delta_history(&origin, 13, 2, 8);
    let source = clone(&scratch, &origin, "source", Some(3), false);
    let destination = clone(&scratch, &origin, "destination.git", Some(3), true);
    let frontier = fs::read(source.join(".git/shallow")).unwrap();
    assert_eq!(frontier, fs::read(destination.join("shallow")).unwrap());
    let mut rng = Rng(17);
    for i in 0..3 {
        write(
            &source,
            "src/file0.txt",
            lines(&mut rng, 1500).concat().as_bytes(),
        );
        commit_all(&source, &format!("shallow work {i}"));
    }
    let sent = check(
        &scratch,
        "matching-shallow",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert!(!sent.round.shallow().is_empty());
    assert_eq!(frontier, fs::read(destination.join("shallow")).unwrap());
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
    check(
        &scratch,
        "fork-below-have-full",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
}

/// A shallow destination needs aggressive edges, as upload-pack's
/// `--shallow` selects; the plain rule over-counts here (spike Q1 caveat 2).
#[test]
fn fork_below_have_shallow_destination() {
    let scratch = Scratch::new("fork-shallow");
    let (source, destination) = fork_below_have(&scratch, Some(3));
    let sent = check(
        &scratch,
        "fork-below-have-shallow",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    let mut request = String::new();
    for want in sent.round.wants() {
        let _ = writeln!(request, "{want}");
    }
    request.push_str("--not\n");
    for have in sent.round.haves() {
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
        sent.set.len() < plain,
        "aggressive edges send fewer than the plain rule ({} vs {plain})",
        sent.set.len()
    );
}

/// Reviewer fixture B (#55 R3-1): two held tips on a full destination, one
/// of which `git fetch` would never offer. Both are haves (R-N113).
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
    let sent = check(
        &scratch,
        "fixture-b",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert!(sent.round.haves().len() >= 2, "multi-held-tip");
    assert!(
        sent.round.destination_tips() > sent.round.haves().len(),
        "a destination tip unknown to the source is not a have"
    );
}

/// Reviewer fixture P1 (R-N116): a shallow destination holding a commit and
/// its parent. The sender offers the parent first; child first, upload-pack
/// drops it and sends more.
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
        let sent = check(&scratch, &name, &source, &destination, DEFAULT_SEGMENT_CAP);
        assert_eq!(sent.round.haves(), [parent.clone(), child.clone()]);
        let child_first = upload_pack(
            &source,
            sent.round.wants(),
            &[child, parent],
            sent.round.shallow(),
        );
        assert!(
            header_count(&child_first) > sent.objects(),
            "{name}: child-first haves send more"
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
        check(
            &scratch,
            &format!("fixture-m-shallow-{shallow}"),
            &source,
            &destination,
            DEFAULT_SEGMENT_CAP,
        );
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
    let sent = check(
        &scratch,
        "fixture-f",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert_eq!(sent.round.shallow().len(), 2);
}

/// Reviewer fixture A3 (#55 R3): a shallow destination holding a commit and
/// its grandparent, neither advertised by the source.
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
    let sent = check(
        &scratch,
        "fixture-a3",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert_eq!(sent.round.haves().len(), 2);
    assert_eq!(sent.set.len(), 2, "the re-added commit and its tree");
}

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

/// `pack.useBitmaps=false` is pinned: the bitmapped source still sends the
/// whole walk, as upload-pack under the same pin does.
#[test]
fn bitmap_source_full_destination() {
    let scratch = Scratch::new("bitmap");
    let (source, have) = bitmap_source(&scratch);
    let destination = destination_at(&scratch, &source, &have);
    let sent = check(
        &scratch,
        "bitmap-source",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert_eq!(sent.set.len(), 4, "c3, its tree, f.txt and h.txt");
}

/// The bitmapped source with a shallow destination at the have: the needed
/// reverted blob is sent and the destination ingests cleanly.
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
    check(
        &scratch,
        "bitmap-source-shallow-destination",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert!(has_object(&destination, &reverted), "the needed blob came");
}

// ---------------------------------------------------------------------------
// Deferred spike items and M1-specific cases
// ---------------------------------------------------------------------------

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
    let sent = check(
        &scratch,
        "d1-non-commit-tips",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    let tree_tag = rev(&source, "refs/mirror/tags/tree-tag");
    let blob_tag = rev(&source, "refs/mirror/tags/blob-tag");
    let commit_tag = rev(&source, "refs/mirror/tags/commit-tag");
    let haves = sent.round.haves();
    assert_eq!(sent.round.non_commit_haves(), 3);
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

/// Nothing to send: the destination holds every want. No segment, no pack.
#[test]
fn destination_holding_everything_gets_no_pack() {
    let scratch = Scratch::new("nothing");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 41, 2, 3);
    let destination = destination_at(&scratch, &source, commits.last().unwrap());
    let sent = check(
        &scratch,
        "nothing-to-send",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
    assert_eq!(sent.plan.segments(), 0);
    assert!(sent.oracle.is_empty() || header_count(&sent.oracle) == 0);
    assert!(sent.round.wants().is_empty());
}

/// The delta fixture of spike Q2 with a 64 KiB cap: several segments, each
/// self-contained, together exactly upload-pack's set.
fn segmented_fixture(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 29, 8, 30);
    let mut rng = Rng(31);
    for i in 0..4 {
        write(&source, &format!("bin/blob{i}.bin"), &rng.bytes(24 * 1024));
        commit_all(&source, &format!("binary {i}"));
    }
    run(args(&source, ["repack", "-adq"]), "repack");
    let destination = destination_at(scratch, &source, &commits[0]);
    (source, destination)
}

const SMALL_CAP: u64 = 64 * 1024;

#[test]
fn segments_are_self_contained_and_together_equal_upload_pack() {
    let scratch = Scratch::new("segments");
    let (source, destination) = segmented_fixture(&scratch);
    let sent = check(&scratch, "segmented-64k", &source, &destination, SMALL_CAP);
    assert!(sent.plan.segments() >= 3, "several segments");
    let single = sent.oracle.len();
    println!(
        "m1 case=segmented-64k segments={} segmented_bytes={} single_pack_bytes={single} \
         overhead_pct={:.1}",
        sent.plan.segments(),
        sent.bytes(),
        (ratio(
            u64::try_from(sent.bytes()).unwrap(),
            u64::try_from(single).unwrap()
        ) - 1.0)
            * 100.0
    );
}

/// "A crash in segment k re-sends only segments >= k", sender side: the plan
/// is persisted, segments 0..k are sent and indexed, segment k is cut off
/// mid-stream (index-pack refuses it), and after a restart the plan is read
/// back by `pack_id` and only segments k.. are packed again. The union
/// ingests, is exactly upload-pack's set, and passes fsck. Regenerated bytes
/// need not match the first attempt's digests (spike D6).
#[test]
fn resume_from_segment_k_packs_only_later_segments() {
    let scratch = Scratch::new("resume");
    let (source, destination) = segmented_fixture(&scratch);
    let lists = ListStore::open(&scratch.state("state")).unwrap();
    let sender = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, SMALL_CAP, None).unwrap();
    let path = lists.persist(&plan, &sender).unwrap();
    assert!(path.ends_with(format!("git-carry-v2/lists/{}.list", plan.pack_id())));
    let n = plan.segments();
    let k = n / 2;
    assert!(k >= 1 && k < n, "a middle segment fails ({n} segments)");

    // First attempt: 0..k land, k is cut off.
    let mut first = Vec::new();
    for index in 0..=k {
        let mut pack = Vec::new();
        first.push(plan.send_segment(&sender, index, &mut pack, None).unwrap());
        let body = if index == k {
            pack[..pack.len() / 2].to_vec()
        } else {
            pack
        };
        let indexed = feed(
            args(&destination, ["index-pack", "--stdin", "--fix-thin"]),
            &body,
        );
        assert_eq!(indexed.status.success(), index < k, "segment {index}");
    }
    drop(plan);
    drop(sender);

    // Restart: read the plan back by name and send only k..n.
    let sender = Source::probe(&source, None).unwrap();
    let plan = lists.load(first_pack_id(&path)).unwrap();
    assert_eq!(plan.segments(), n);
    let mut resumed = Vec::new();
    let mut regenerated = 0_u64;
    for index in k..plan.segments() {
        let mut pack = Vec::new();
        let receipt = plan.send_segment(&sender, index, &mut pack, None).unwrap();
        regenerated += receipt.bytes;
        ok(
            feed(
                args(&destination, ["index-pack", "--stdin", "--fix-thin"]),
                &pack,
            ),
            "resumed index-pack",
        );
        resumed.push(receipt);
    }
    assert_eq!(resumed.len(), n - k, "only segments >= k were packed again");
    let total: u64 = first[..k].iter().map(|r| r.bytes).sum::<u64>() + regenerated;
    // The destination now holds everything: the refs-only connectivity check
    // passes for the wants, and fsck is clean after the refs are published.
    let mut wants = String::new();
    for want in plan.wants() {
        let _ = writeln!(wants, "{want}");
    }
    ok(
        feed(
            args(
                &destination,
                [
                    "rev-list",
                    "--objects",
                    "--stdin",
                    "--not",
                    "--all",
                    "--quiet",
                ],
            ),
            wants.as_bytes(),
        ),
        "connectivity",
    );
    ingest(&destination, &[], plan.wants(), "resume");
    let oracle = upload_pack(&source, plan.wants(), plan.haves(), &[]);
    let sent_objects: u32 = first[..k].iter().map(|r| r.objects).sum::<u32>()
        + resumed.iter().map(|r| r.objects).sum::<u32>();
    assert_eq!(sent_objects, header_count(&oracle));
    let identical = resumed
        .iter()
        .filter(|r| first.get(r.index).is_some_and(|f| f.blake3 == r.blake3))
        .count();
    println!(
        "m1 case=resume segments={n} failed_at={k} regenerated_segments={} \
         regenerated_bytes={regenerated} total_bytes={total} regenerated_segment_k_identical={identical}/1",
        n - k
    );
}

fn first_pack_id(path: &Path) -> &str {
    path.file_stem().unwrap().to_str().unwrap()
}

/// The list store: private, identity-checked, never replacing, and verified
/// on read.
#[test]
fn list_store_is_private_verified_and_outside_the_source() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Scratch::new("lists");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 43, 2, 2);
    let destination = destination_at(&scratch, &source, &commits[0]);
    let sender = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap();

    // Opening creates nothing; persisting twice is idempotent.
    let state = scratch.state("state");
    let lists = ListStore::open(&state).unwrap();
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);
    let path = lists.persist(&plan, &sender).unwrap();
    assert_eq!(lists.persist(&plan, &sender).unwrap(), path);
    let mode = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&state.join("git-carry-v2")), 0o700);
    assert_eq!(mode(&state.join("git-carry-v2/lists")), 0o700);
    assert_eq!(lists.load(plan.pack_id()).unwrap(), plan);
    let entries = fs::read_dir(state.join("git-carry-v2/lists"))
        .unwrap()
        .count();
    assert_eq!(entries, 1, "no temporary left behind");

    // A tampered list is refused by digest; a widened one by mode.
    let bytes = fs::read(&path).unwrap();
    let mut tampered = bytes.clone();
    let at = tampered.len() - 6;
    tampered[at] ^= 1;
    fs::write(&path, &tampered).unwrap();
    assert_eq!(
        lists.load(plan.pack_id()),
        Err(BulkloadRefusal::DigestMismatch)
    );
    // Persist refuses to accept the planted bytes under the name.
    assert!(lists.persist(&plan, &sender).is_err());
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(lists.load(plan.pack_id()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(lists.load(plan.pack_id()).unwrap(), plan);
    // A symlink at the name is refused, never followed.
    fs::remove_file(&path).unwrap();
    let elsewhere = scratch.path("elsewhere.list");
    fs::write(&elsewhere, &bytes).unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    assert!(lists.load(plan.pack_id()).is_err());
    fs::remove_file(&path).unwrap();
    // Absent and malformed names.
    assert_eq!(
        lists.load(plan.pack_id()),
        Err(BulkloadRefusal::SealedObjectMissing)
    );
    assert_eq!(
        lists.load("../../etc/passwd"),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    assert_eq!(
        ListStore::open(&scratch.state("empty"))
            .unwrap()
            .load(plan.pack_id()),
        Err(BulkloadRefusal::SealedObjectMissing)
    );
    // A world-readable state dir is refused.
    let open = scratch.state("open");
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ListStore::open(&open).is_err());

    // A state dir inside the source is refused before anything is written,
    // under a symlinked spelling too.
    for inside in [source.join("state"), source.join(".git/state")] {
        fs::create_dir(&inside).unwrap();
        fs::set_permissions(&inside, fs::Permissions::from_mode(0o700)).unwrap();
        let lists = ListStore::open(&inside).unwrap();
        let refused = lists.persist(&plan, &sender).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::SnapshotRootsOverlap);
        assert_eq!(refused.reason, Some("state_dir_inside_repository"));
        assert_eq!(fs::read_dir(&inside).unwrap().count(), 0);
    }
    let link = scratch.path("source-link");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    let lists = ListStore::open(&link.join("state")).unwrap();
    assert_eq!(
        lists.persist(&plan, &sender).unwrap_err().refusal,
        BulkloadRefusal::SnapshotRootsOverlap
    );
}

/// R-N75 and malformed offers, refused before anything is listed.
#[test]
fn unprovable_destinations_and_malformed_offers_are_refused() {
    let scratch = Scratch::new("refusals");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 47, 2, 3);
    let sender = Source::probe(&source, None).unwrap();
    let wants = sender.wants().unwrap();
    let held = Offer {
        tips: BTreeSet::from([commits[0].clone()]),
        ..Offer::default()
    };
    assert!(first_round(&sender, &held, &wants, None).is_ok());
    let partial = Offer {
        partial: true,
        ..held.clone()
    };
    let refused = first_round(&sender, &partial, &wants, None).unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    assert_eq!(refused.reason, Some("destination_partial_clone"));
    let shallow = Offer {
        shallow: BTreeSet::from([commits[1].clone()]),
        ..held.clone()
    };
    let refused = first_round(&sender, &shallow, &wants, None).unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    assert_eq!(refused.reason, Some("destination_shallow_frontier_differs"));
    let upper = commits[0].to_uppercase();
    for bad in ["HEAD", "--all", &commits[0][..39], "-n1", &upper] {
        let malformed = Offer {
            tips: BTreeSet::from([bad.to_owned()]),
            ..Offer::default()
        };
        assert_eq!(
            first_round(&sender, &malformed, &wants, None)
                .unwrap_err()
                .refusal,
            BulkloadRefusal::GitInventoryMalformed,
            "{bad}"
        );
        assert_eq!(
            first_round(&sender, &held, &BTreeSet::from([bad.to_owned()]), None)
                .unwrap_err()
                .refusal,
            BulkloadRefusal::GitInventoryMalformed,
            "want {bad}"
        );
    }
    let round = first_round(&sender, &held, &wants, None).unwrap();
    assert_eq!(
        PackPlan::build(&sender, &round, 0, None)
            .unwrap_err()
            .refusal,
        BulkloadRefusal::FieldDomainViolation
    );
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap();
    assert_eq!(
        plan.send_segment(&sender, plan.segments(), &mut Vec::new(), None)
            .unwrap_err()
            .refusal,
        BulkloadRefusal::FieldDomainViolation
    );
}

/// R-N131: a shallow source with a full destination refuses at negotiation,
/// before anything is listed, persisted or sent, whether or not the
/// destination already holds the boundary's parents; the destination gains
/// no shallow file. The estimate refuses the same pair the same way.
#[test]
fn r_n131_shallow_source_full_destination_refuses_before_sending() {
    let scratch = Scratch::new("r-n131");
    let origin = scratch.init("origin", false);
    let commits = delta_history(&origin, 67, 2, 4);
    let source = clone(&scratch, &origin, "source", Some(2), false);
    write(&source, "src/file0.txt", noise(68, 30).as_bytes());
    commit_all(&source, "shallow work");
    let empty = scratch.init("empty.git", true);
    let holding = destination_at(&scratch, &origin, &commits[1]);
    // #73 round-2 D2: the state dir is really in use. A list store and a
    // stderr store are opened on it (opening creates nothing), and the
    // store goes to every probe and to negotiation.
    let state = scratch.state("state");
    let lists = ListStore::open(&state).unwrap();
    let store = StderrStore::open(&state).unwrap();
    let sender = Source::probe(&source, Some(&store)).unwrap();
    assert!(!sender.shallow().is_empty());
    let wants = sender.wants().unwrap();
    for destination in [&empty, &holding] {
        let refs_before = text(args(destination, ["for-each-ref"]), "refs");
        let offer = Offer::probe(destination, Some(&store)).unwrap();
        assert!(offer.shallow.is_empty());
        let refused = first_round(&sender, &offer, &wants, Some(&store)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
        assert_eq!(refused.reason, Some("source_shallow_destination_full"));
        let estimated = estimate(&source, &Destination::Local(destination.clone())).unwrap_err();
        assert_eq!(estimated.refusal, BulkloadRefusal::GitHavesUnprovable);
        assert_eq!(estimated.reason, Some("source_shallow_destination_full"));
        assert!(!destination.join("shallow").exists(), "no shallow file");
        assert_eq!(
            text(args(destination, ["for-each-ref"]), "refs"),
            refs_before
        );
    }
    // No round, so no plan and no list: `git-carry-v2/` was never created.
    // The refusals are the verb's own, so no child failed and the stderr
    // store holds its key and nothing else (each child opened a capture,
    // which made `stderr/` and the key, and discarded it on success).
    let mut names: Vec<_> = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["stderr"]);
    let kept: Vec<_> = fs::read_dir(state.join("stderr"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(kept, ["key"]);
    drop(lists);
    // The same source into a destination shallow at its frontier carries.
    let matching = clone(&scratch, &origin, "matching.git", Some(2), true);
    assert_eq!(
        fs::read(source.join(".git/shallow")).unwrap(),
        fs::read(matching.join("shallow")).unwrap()
    );
    check(
        &scratch,
        "r-n131-matching",
        &source,
        &matching,
        DEFAULT_SEGMENT_CAP,
    );
}

/// #73 round-2 D1: a round only `first_round` can build still meets a source
/// that moved. Negotiated against a full source and a full destination, the
/// build refuses once the source has become shallow (R-N131); a plan built
/// and persisted while both were shallow at one frontier refuses every
/// resumed segment once the source's frontier moves (R-N75), and once the
/// source is unshallowed.
#[test]
fn a_source_whose_frontier_moved_is_refused_at_build_and_at_resume() {
    let scratch = Scratch::new("d1-moved");
    let origin = scratch.init("origin", false);
    let commits = delta_history(&origin, 71, 2, 6);
    // Full source, full destination; then the source takes a frontier.
    let source = clone(&scratch, &origin, "source", None, false);
    let destination = destination_at(&scratch, &origin, &commits[2]);
    let full = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&full, &offer, &full.wants().unwrap(), None).unwrap();
    fs::write(source.join(".git/shallow"), format!("{}\n", commits[4])).unwrap();
    let moved = Source::probe(&source, None).unwrap();
    let refused = PackPlan::build(&moved, &round, DEFAULT_SEGMENT_CAP, None).unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    assert_eq!(refused.reason, Some("source_shallow_destination_full"));

    // Both shallow at one frontier: plan, persist, send segment 0.
    let shallow_source = clone(&scratch, &origin, "shallow-source", Some(2), false);
    write(&shallow_source, "src/file0.txt", noise(72, 40).as_bytes());
    commit_all(&shallow_source, "work");
    let shallow_destination = clone(&scratch, &origin, "shallow-destination.git", Some(2), true);
    let lists = ListStore::open(&scratch.state("state")).unwrap();
    let sender = Source::probe(&shallow_source, None).unwrap();
    let offer = Offer::probe(&shallow_destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap();
    assert_eq!(plan.shallow(), round.shallow());
    lists.persist(&plan, &sender).unwrap();
    assert!(plan.send_segment(&sender, 0, &mut Vec::new(), None).is_ok());
    let frontier = fs::read(shallow_source.join(".git/shallow")).unwrap();
    // The frontier moves (deepened by one commit's worth), then goes away.
    for (contents, reason) in [
        (
            format!("{}\n", commits[3]).into_bytes(),
            "destination_shallow_frontier_differs",
        ),
        (Vec::new(), "destination_shallow_frontier_differs"),
    ] {
        if contents.is_empty() {
            fs::remove_file(shallow_source.join(".git/shallow")).unwrap();
        } else {
            fs::write(shallow_source.join(".git/shallow"), &contents).unwrap();
        }
        let resumed = Source::probe(&shallow_source, None).unwrap();
        let loaded = lists.load(plan.pack_id()).unwrap();
        let refused = loaded
            .send_segment(&resumed, 0, &mut Vec::new(), None)
            .unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
        assert_eq!(refused.reason, Some(reason));
        assert_eq!(
            loaded.check_source(&resumed).unwrap_err().reason,
            Some(reason)
        );
    }
    fs::write(shallow_source.join(".git/shallow"), &frontier).unwrap();
    let restored = Source::probe(&shallow_source, None).unwrap();
    assert!(lists
        .load(plan.pack_id())
        .unwrap()
        .send_segment(&restored, 0, &mut Vec::new(), None)
        .is_ok());
}

/// An object the walk needs but the source lacks refuses the plan; one that
/// vanishes between the list and the pack refuses the segment, with its
/// stderr classified and never echoed (R-N121).
#[test]
fn a_missing_object_refuses_before_or_while_packing() {
    let scratch = Scratch::new("missing");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 53, 2, 3);
    write(&source, "unique.txt", noise(99, 10).as_bytes());
    commit_all(&source, "unique");
    let destination = destination_at(&scratch, &source, &commits[0]);
    let sender = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap();
    // The fixture's objects are loose (never repacked): remove a blob only
    // the unsent commit reaches.
    let blob = rev(&source, "HEAD:unique.txt");
    let loose = objects_dir(&source).join(&blob[..2]).join(&blob[2..]);
    assert!(loose.exists(), "the fixture's objects are loose");
    fs::remove_file(&loose).unwrap();
    let refused = plan
        .send_segment(&sender, 0, &mut Vec::new(), None)
        .unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::GitInventoryMalformed);
    assert_eq!(refused.reason, Some("pack_objects_failed"));
    let receipt = refused.stderr.clone().unwrap();
    assert!(receipt.keyed_blake3.is_none(), "no store, no digest");
    for line in refused.lines() {
        assert!(!line.contains(&blob) && !line.contains("fatal"), "{line}");
    }
    let refused = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::GitInventoryMalformed);
    assert_eq!(refused.reason, Some("source_lacks_reachable_objects"));
}

/// Spike caveat 5: list lines are raw bytes. Tree paths that are not UTF-8,
/// that carry spaces, or a leading `-`, `#` or hex-looking name, survive the
/// list, its encoding and resume, and the sender still equals upload-pack.
#[test]
fn paths_that_are_not_utf8_travel_as_raw_bytes() {
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
    let lists = ListStore::open(&scratch.state("state")).unwrap();
    let sender = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    for cap in [1, DEFAULT_SEGMENT_CAP] {
        let plan = PackPlan::build(&sender, &round, cap, None).unwrap();
        let path = lists.persist(&plan, &sender).unwrap();
        assert!(
            fs::read(&path).unwrap().windows(4).any(|w| w == b"\xffir/"),
            "the list holds the raw bytes"
        );
        assert_eq!(lists.load(plan.pack_id()).unwrap(), plan);
    }
    check(&scratch, "raw-paths-cap1", &source, &destination, 1);
    let destination = {
        let again = scratch.init("destination-2.git", true);
        push(&source, &again, &rev(&source, "main~1"), "refs/heads/main");
        again
    };
    check(
        &scratch,
        "raw-paths",
        &source,
        &destination,
        DEFAULT_SEGMENT_CAP,
    );
}

/// A sink that fails part-way refuses with its own error. The child's stdout
/// is closed, so it ends on its own and is waited for; nothing is signalled
/// and nothing blocks (R-N11).
#[test]
fn a_failing_sink_refuses_and_the_child_ends_on_its_own() {
    struct Failing(usize);
    impl std::io::Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            let take = bytes.len().min(self.0);
            self.0 -= take;
            Ok(take)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let scratch = Scratch::new("sink");
    let source = scratch.init("source", false);
    let mut rng = Rng(59);
    write(&source, "big.bin", &rng.bytes(4 << 20));
    commit_all(&source, "big");
    let destination = scratch.init("destination.git", true);
    let sender = Source::probe(&source, None).unwrap();
    let offer = Offer::probe(&destination, None).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), None).unwrap();
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, None).unwrap();
    let started = std::time::Instant::now();
    let refused = plan
        .send_segment(&sender, 0, &mut Failing(1000), None)
        .unwrap_err();
    assert_eq!(refused.refusal, BulkloadRefusal::Io(Some(28)));
    assert!(started.elapsed() < std::time::Duration::from_mins(1));
    // The same plan still sends in full afterwards.
    let mut pack = Vec::new();
    let receipt = plan.send_segment(&sender, 0, &mut pack, None).unwrap();
    assert_eq!(receipt.objects, 3);
}

/// R-N121 with a store: a refused segment's raw stderr goes to the private
/// store under a keyed digest; the refusal carries only the class, the
/// digest and the file's path.
#[test]
fn a_refused_segment_keeps_its_stderr_privately() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Scratch::new("stderr");
    let source = scratch.init("source", false);
    let commits = delta_history(&source, 61, 2, 2);
    write(&source, "unique.txt", noise(98, 10).as_bytes());
    commit_all(&source, "unique");
    let destination = destination_at(&scratch, &source, &commits[0]);
    let state = scratch.state("state");
    let store = StderrStore::open(&state).unwrap();
    let sender = Source::probe(&source, Some(&store)).unwrap();
    let offer = Offer::probe(&destination, Some(&store)).unwrap();
    let round = first_round(&sender, &offer, &sender.wants().unwrap(), Some(&store)).unwrap();
    let plan = PackPlan::build(&sender, &round, DEFAULT_SEGMENT_CAP, Some(&store)).unwrap();
    // Successful children kept nothing.
    let kept = |state: &Path| {
        fs::read_dir(state.join("stderr")).map_or(0, |entries| {
            entries
                .filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.file_name().to_string_lossy().ends_with(".log"))
                })
                .count()
        })
    };
    assert_eq!(kept(&state), 0);
    let blob = rev(&source, "HEAD:unique.txt");
    fs::remove_file(objects_dir(&source).join(&blob[..2]).join(&blob[2..])).unwrap();
    let refused = plan
        .send_segment(&sender, 0, &mut Vec::new(), Some(&store))
        .unwrap_err();
    let receipt = refused.stderr.clone().unwrap();
    let file = receipt.file.clone().unwrap();
    let raw = fs::read(&file).unwrap();
    assert!(!raw.is_empty());
    let key: [u8; 32] = fs::read(state.join("stderr/key"))
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        receipt.keyed_blake3.unwrap(),
        blake3::keyed_hash(&key, &raw).to_hex().to_string()
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(kept(&state), 1);
    let printed = refused.lines().join("\n");
    let raw_text = String::from_utf8_lossy(&raw);
    for line in raw_text.lines().filter(|line| line.len() > 8) {
        assert!(!printed.contains(line), "stderr echoed: {line}");
    }
    // A store inside the source is refused before any child runs.
    let inside = source.join("state");
    fs::create_dir(&inside).unwrap();
    fs::set_permissions(&inside, fs::Permissions::from_mode(0o700)).unwrap();
    let store = StderrStore::open(&inside).unwrap();
    assert_eq!(
        Source::probe(&source, Some(&store)).unwrap_err().refusal,
        BulkloadRefusal::SnapshotRootsOverlap
    );
    assert_eq!(
        Offer::probe(&source, Some(&store)).unwrap_err().refusal,
        BulkloadRefusal::SnapshotRootsOverlap
    );
    assert_eq!(fs::read_dir(&inside).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// Random DAGs
// ---------------------------------------------------------------------------

use proptest::prelude::*;

/// Parent indexes, and (file, content seed) edits.
type Commit = (Vec<usize>, Vec<(usize, u64)>);

/// A random history: each commit names up to two earlier parents and edits
/// one to three of four files. `source_tips` and `held` index commits;
/// `stranger` adds a destination-only commit the source never saw.
#[derive(Debug, Clone)]
struct Dag {
    commits: Vec<Commit>,
    source_tips: Vec<usize>,
    held: Vec<usize>,
    stranger: bool,
    small_cap: bool,
}

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
                let edits = proptest::collection::vec((0_usize..4, any::<u64>()), 1..=3);
                (parents, edits)
            })
            .collect::<Vec<_>>();
        (
            commits,
            proptest::collection::btree_set(0..n, 1..=n),
            proptest::collection::btree_set(0..n, 0..=n),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(move |(commits, source_tips, held, stranger, small_cap)| {
                let mut source_tips: Vec<usize> = source_tips.into_iter().collect();
                if !source_tips.contains(&(n - 1)) {
                    source_tips.push(n - 1);
                }
                Dag {
                    commits,
                    source_tips,
                    held: held.into_iter().collect(),
                    stranger,
                    small_cap,
                }
            })
    })
}

/// Build `dag` in a fresh source with one fast-import stream, then give a
/// bare destination exactly the held commits.
fn build(scratch: &Scratch, dag: &Dag) -> (PathBuf, PathBuf) {
    let source = scratch.init("source.git", true);
    let mut stream = Vec::new();
    let mut blob_mark = 1_000;
    for (index, (parents, edits)) in dag.commits.iter().enumerate() {
        let mut files = Vec::new();
        for (file, seed) in edits {
            blob_mark += 1;
            let body = noise(*seed, 40 + usize::try_from(seed % 60).unwrap());
            let _ = write!(
                stream,
                "blob\nmark :{blob_mark}\ndata {}\n{body}\n",
                body.len()
            );
            files.push((file, blob_mark));
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
        for (file, mark) in files {
            let _ = writeln!(stream, "M 100644 :{mark} f{file}.txt");
        }
        stream.push(b'\n');
    }
    ok(
        feed(args(&source, ["fast-import", "--quiet"]), &stream),
        "fast-import",
    );
    let oid = |index: usize| rev(&source, &format!("refs/fi/c{index}"));
    let destination = scratch.init("destination.git", true);
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

fn text_of(output: Output, what: &str) -> String {
    String::from_utf8(ok(output, what))
        .unwrap()
        .trim_end()
        .to_owned()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 12,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// On random DAGs, with random source tips, held commits (named or not by
    /// any source ref), a destination-only tip and a small or default cap,
    /// the gate holds and the destination ingests to a clean fsck.
    #[test]
    fn random_dags_equal_upload_pack(dag in dag()) {
        let scratch = Scratch::new("dag");
        let (source, destination) = build(&scratch, &dag);
        let cap = if dag.small_cap { 512 } else { DEFAULT_SEGMENT_CAP };
        let sent = check(&scratch, "random-dag", &source, &destination, cap);
        prop_assert_eq!(&sent.set, &sent.oracle_set);
    }
}
