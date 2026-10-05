//! P64 PACK-MINIMALITY and P65 THIN-DELTA for v1 capture bundles (Q42 lane
//! L1; OI-1003-Q42, OI-1003-Q44, OI-1003-Q45, R-N13).
//!
//! **P64, what the writer guarantees.** For every capture bundle `B` an
//! estate pass publishes, grouped (a shared plan base) or chained (WP2):
//!
//! ```text
//! packed(B) ⊆ reach(refs(B))
//! packed(B) ∩ (C(B) ∪ under(T(B))) = ∅
//! ```
//!
//! `reach` is the full object closure. `C(B)` is the commits `B`'s
//! prerequisites reach. `T(B)` is the commits of `C(B)` whose trees git's
//! `--objects-edge-aggressive` walk marks: every prerequisite, every ref of
//! `B` whose commit is in `C(B)` (HEAD's, through `--all`), and every parent
//! in `C(B)` of a commit `B` packs (an edge). `under` is every tree and blob
//! beneath those trees. So an item bundle holds no copy of a blob in its
//! base's tip trees or in its own HEAD's tree, and an edited blob deltas
//! against that copy (P65). Only commits are prerequisites, so an annotated
//! tag object, or a ref to a tree or blob, is outside `C(B)`.
//!
//! **What P64 does not bound** (OI-1003-Q42 reading (a): the law is what
//! the walk guarantees, not the full closure of the prerequisites). Each
//! row reports `packed(B) ∩ reach(prerequisites(B))`, the full-closure
//! overlap, and pins it: empty, except where a row below pins its cost.
//! - Content the prerequisites hold only deeper in history, outside every
//!   `T(B)` tree: a blob a `checkout <old> -- path`, `restore --source`,
//!   `revert` or stash brings back. It is packed again, whole unless a
//!   preferred base is similar. The `revert_to_older_content` rows pin one
//!   such blob, packed whole, as the only overlap.
//! - The capture's snapshot payload. Untracked and ignored files sit only
//!   in the staged and worktree snapshot trees, which no prerequisite holds
//!   (a prior capture's worktree commit is never one, and a shared base
//!   carries none), so they are outside `reach(prerequisites(B))` and P64
//!   allows them. An unchanged untracked file is therefore packed again,
//!   whole, on every pass that recaptures its item. The
//!   `unchanged_untracked_payload` rows pin that cost (#174 tracks
//!   deltaing or excluding it against the prior capture's worktree tree).
//!
//! **P65.** A small edit to a large tracked blob, committed or not, packs as
//! a delta against the copy its prerequisites hold, grouped and chained. The
//! pack is then thin, and every row's apply restores it byte for byte through
//! `index-pack --fix-thin` (an import's fetch, or `chain::flatten`).
//!
//! **Thin reuse.** The `thin_reuse_third_pass` rows run a third pass whose
//! retained capture is a thin bundle, so its blob-reuse fetch completes the
//! pack from the source store. That pass reuses every unchanged seat (no
//! `reuse_unavailable`; `source_bytes_read` is the one edited seat), counts
//! the completed base in `read_source_capture_reuse_bytes` (R25), and, when
//! chained, restores through a flatten of two thin links.
//!
//! The rows are a fixed table (OI-1003-Q7: no fuzzing), one test per
//! (layout, mutation). Each runs the verb binary end to end: a first pass, the
//! mutation, a later pass, the law over every item bundle of every pass,
//! then `estate-apply` and a byte comparison of every restored workspace.
//! Each row prints one `P64 ...` line per bundle with its bytes and object
//! counts (`--nocapture`), the lane note's measurements.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The large tracked blob: incompressible, so a re-pack shows in bytes.
const LARGE: usize = 1024 * 1024 + 17;
/// Where the 64-byte edit lands in it.
const EDIT_AT: usize = 512 * 1024;
/// Orphan branch tips whose trees hold no large blob. More than pack-objects'
/// ten preferred bases, so a delta needs the right edge first.
const TIPS: usize = 12;
/// `rounds.bin`: incompressible and rewritten by every commit of main's
/// history, so an older round's copy sits only in that round's tree.
const ROUND: usize = 64 * 1024;
/// `payload.bin`: an untracked file the payload rows never change.
const PAYLOAD: usize = 512 * 1024;

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(key);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-10-04T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-04T00:00:00Z")
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

fn run(command: &mut Command) -> String {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

fn feed(command: &mut Command, input: &[u8]) -> String {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let bytes = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        out.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Run one verb; returns its stdout (receipts) and stderr (counters).
fn verb(args: &[&std::ffi::OsStr]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        out.status.success(),
        "{args:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

// Incompressible bytes.
fn noise(len: usize, mut state: u32) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

// Seats must be older than one timestamp tick before a pass whose capture a
// later pass reuses, or that later pass reads them again as racy (R-N76).
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// A main checkout plus a branch and a detached linked worktree: a
    /// 3-item group on one shared plan base.
    Grouped,
    /// The main checkout alone: a group of one, chained on its prior capture.
    Chained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// No second pass: the law over the first pass alone.
    FirstPass,
    /// `checkout --detach` to an older first-parent commit.
    HeadMove,
    /// A plumbing commit on the unmerged `side` branch.
    SideCommit,
    /// A committed directory rename on main (the pack.useSparse case).
    DirRename,
    /// A committed 64-byte overwrite inside the large tracked blob.
    LargeCommitted,
    /// The same overwrite, left uncommitted in a worktree.
    LargeWorktree,
    /// `checkout <c1> -- rounds.bin`: content the prerequisites hold only in
    /// an older commit's tree comes back (P64's first unbounded cost).
    RevertOlder,
    /// A one-line edit beside an unchanged untracked `payload.bin` present
    /// since the first pass (P64's second unbounded cost).
    UntrackedPayload,
    /// The uncommitted large edit, then a third pass after a one-line edit:
    /// that pass reuses blobs from a thin retained capture.
    ThinReuse,
}

struct Fixture {
    _root: Root,
    root: PathBuf,
    source: PathBuf,
    /// The checkout every worktree mutation lands in: the detached linked
    /// worktree of a group, or the main checkout of a chain.
    moved: PathBuf,
    plan: PathBuf,
    state: PathBuf,
    corpus: PathBuf,
    /// (source checkout, restore target), one per item.
    items: Vec<(PathBuf, PathBuf)>,
    /// main's first-parent history, oldest first.
    history: Vec<String>,
    /// The large blob every first pass carries once, in the base.
    large: String,
}

fn commit_all(repo: &Path, message: &str) -> String {
    run(git(repo).args(["add", "-A"]));
    run(git(repo).args(["commit", "-q", "-m", message]));
    run(git(repo).args(["rev-parse", "HEAD"]))
}

fn fixture(name: &str, layout: Layout, mutation: Mutation) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "bulkload-p64-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let guard = Root(root.clone());
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    run(git(&source).args(["init", "-q", "--template=", "-b", "main"]));
    std::fs::write(source.join("big.bin"), noise(LARGE, 7)).unwrap();
    std::fs::create_dir_all(source.join("tree/a")).unwrap();
    std::fs::create_dir_all(source.join("tree/b")).unwrap();
    std::fs::write(source.join("tree/a/one.bin"), noise(4096, 11)).unwrap();
    std::fs::write(source.join("tree/a/two.bin"), noise(4096, 13)).unwrap();
    std::fs::write(source.join("tree/b/three.bin"), noise(4096, 17)).unwrap();
    let mut history = Vec::new();
    for round in 0..4u32 {
        std::fs::write(
            source.join("notes.txt"),
            format!(
                "notes {round}\n{}",
                "line\n".repeat(64 * (round as usize + 1))
            ),
        )
        .unwrap();
        std::fs::write(source.join("rounds.bin"), noise(ROUND, 101 + round)).unwrap();
        history.push(commit_all(&source, &format!("c{round}")));
    }
    let large = run(git(&source).args(["rev-parse", "HEAD:big.bin"]));
    // side: one commit on c0, through plumbing, checked out nowhere.
    side_commit(&source, &history[0], "side.txt", b"side 0\n");
    // Orphan tips with small trees: base or prior tips that hold no copy
    // of the large blob.
    for tip in 0..TIPS {
        let blob = feed(
            git(&source).args(["hash-object", "-w", "--stdin"]),
            format!("tip {tip}\n").as_bytes(),
        );
        let tree = feed(
            git(&source).arg("mktree"),
            format!("100644 blob {blob}\ttip.txt\n").as_bytes(),
        );
        let commit = run(git(&source).args(["commit-tree", &tree, "-m", &format!("tip {tip}")]));
        run(git(&source).args(["update-ref", &format!("refs/heads/tip/{tip:02}"), &commit]));
    }
    std::fs::write(source.join("scratch.txt"), b"untracked in main\n").unwrap();
    let dest = root.join("dest");
    std::fs::create_dir(&dest).unwrap();
    run(git(&dest).args(["init", "-q", "--template="]));
    let restored = root.join("restored");
    std::fs::create_dir(&restored).unwrap();
    let mut items = vec![(source.clone(), restored.join("main"))];
    let moved = if layout == Layout::Grouped {
        let branch = root.join("wt-branch");
        run(git(&source)
            .args(["worktree", "add", "-q", "-b", "wt"])
            .arg(&branch)
            .arg(&history[3]));
        std::fs::write(branch.join("notes.txt"), b"notes on wt\n").unwrap();
        commit_all(&branch, "wt");
        std::fs::write(branch.join("scratch.txt"), b"untracked in wt\n").unwrap();
        let held = root.join("wt-detached");
        run(git(&source)
            .args(["worktree", "add", "-q", "--detach"])
            .arg(&held)
            .arg(&history[2]));
        std::fs::write(held.join("scratch.txt"), b"untracked in detached\n").unwrap();
        items.push((branch, restored.join("branch")));
        items.push((held.clone(), restored.join("detached")));
        held
    } else {
        source.clone()
    };
    if mutation == Mutation::UntrackedPayload {
        std::fs::write(moved.join("payload.bin"), noise(PAYLOAD, 29)).unwrap();
    }
    let plan = root.join("plan");
    for (checkout, target) in &items {
        verb(&[
            "estate-add".as_ref(),
            plan.as_os_str(),
            checkout.as_os_str(),
            dest.as_os_str(),
            target.as_os_str(),
        ]);
    }
    Fixture {
        state: root.join("state"),
        corpus: root.join("corpus"),
        plan,
        source,
        moved,
        items,
        history,
        large,
        root,
        _root: guard,
    }
}

fn side_commit(source: &Path, parent: &str, name: &str, bytes: &[u8]) {
    let index = source.with_file_name(format!("side-{name}.index"));
    let indexed = |args: &[&str]| {
        let mut command = git(source);
        command.env("GIT_INDEX_FILE", &index).args(args);
        command
    };
    run(&mut indexed(&["read-tree", parent]));
    let blob = feed(git(source).args(["hash-object", "-w", "--stdin"]), bytes);
    run(&mut indexed(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},{name}"),
    ]));
    let tree = run(&mut indexed(&["write-tree"]));
    let commit = run(git(source).args(["commit-tree", &tree, "-p", parent, "-m", name]));
    run(git(source).args(["update-ref", "refs/heads/side", &commit]));
    std::fs::remove_file(index).unwrap();
}

fn overwrite_large(checkout: &Path) {
    let path = checkout.join("big.bin");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[EDIT_AT..EDIT_AT + 64].copy_from_slice(&[b'q'; 64]);
    std::fs::write(&path, bytes).unwrap();
}

// A one-line, uncommitted edit to a small tracked file.
fn edit_notes(checkout: &Path) {
    let path = checkout.join("notes.txt");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"one more line\n");
    std::fs::write(&path, bytes).unwrap();
}

/// One item line of an estate verb's receipt.
#[derive(Debug)]
struct Receipt {
    item: String,
    source: PathBuf,
    source_bytes_read: u64,
    reuse_unavailable: Option<String>,
}

/// What one `estate-capture` run published and printed.
struct Pass {
    /// Every item bundle in the corpus after the run.
    bundles: BTreeSet<PathBuf>,
    receipts: Vec<Receipt>,
    counters: BTreeMap<String, u64>,
}

impl Pass {
    /// The bundles this pass published that `before` did not hold.
    fn fresh(&self, before: &Self) -> BTreeSet<PathBuf> {
        self.bundles.difference(&before.bundles).cloned().collect()
    }

    /// The receipt of the item whose source is `checkout`.
    fn receipt(&self, checkout: &Path) -> &Receipt {
        let wanted = std::fs::canonicalize(checkout).unwrap();
        self.receipts
            .iter()
            .find(|row| std::fs::canonicalize(&row.source).is_ok_and(|path| path == wanted))
            .unwrap_or_else(|| panic!("no receipt for {}: {:?}", checkout.display(), self.receipts))
    }
}

/// Whether `bundle` is a capture of `item` (published as `{item}-{digest}`).
fn of_item(bundle: &Path, item: &str) -> bool {
    bundle
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("{item}-"))
}

fn receipts(stdout: &str) -> Vec<Receipt> {
    stdout
        .lines()
        .filter(|line| line.starts_with("item=") && line.contains(" source_bytes_read="))
        .map(|line| {
            let field = |key: &str| {
                line.split(' ')
                    .find_map(|pair| pair.strip_prefix(key))
                    .map(str::to_owned)
            };
            let source = line
                .split_once(" source=\"")
                .and_then(|(_, rest)| rest.split_once("\" outcome="))
                .unwrap()
                .0;
            Receipt {
                item: field("item=").unwrap(),
                source: PathBuf::from(source),
                source_bytes_read: field("source_bytes_read=").unwrap().parse().unwrap(),
                reuse_unavailable: field("reuse_unavailable="),
            }
        })
        .collect()
}

fn capture(fixture: &Fixture) -> Pass {
    let (stdout, stderr) = verb(&[
        "estate-capture".as_ref(),
        fixture.plan.as_os_str(),
        fixture.state.as_os_str(),
        fixture.corpus.as_os_str(),
        "1".as_ref(),
    ]);
    let counters = stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .unwrap_or_else(|| panic!("no counters line in {stderr}"))
        .split(' ')
        .filter_map(|pair| pair.split_once('='))
        .filter_map(|(key, value)| Some((key.to_owned(), value.parse().ok()?)))
        .collect();
    let bundles = std::fs::read_dir(&fixture.corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bundle"))
        // The shared plan base is the self-contained prerequisite, not an
        // item capture.
        .filter(|path| {
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shared-")
        })
        .collect();
    Pass {
        bundles,
        receipts: receipts(&stdout),
        counters,
    }
}

/// One object a bundle's pack holds.
#[derive(Debug)]
struct Packed {
    kind: String,
    in_pack: u64,
    delta: bool,
}

/// What a capture bundle declares and packs.
struct Inspected {
    header: usize,
    pack: usize,
    prerequisites: Vec<String>,
    tips: Vec<String>,
    packed: BTreeMap<String, Packed>,
    /// The parents of every commit the pack holds.
    parents: BTreeSet<String>,
    /// Bases `index-pack --fix-thin` had to append: the pack was thin.
    appended: usize,
    /// Every object reachable from the bundle's refs.
    reach_refs: BTreeSet<String>,
}

fn object_set(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| line.split(' ').next())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn inspect(fixture: &Fixture, bundle: &Path) -> Inspected {
    let bytes = std::fs::read(bundle).unwrap();
    let split = bytes.windows(2).position(|pair| pair == b"\n\n").unwrap() + 2;
    let (header, pack) = bytes.split_at(split);
    let mut prerequisites = Vec::new();
    let mut tips = Vec::new();
    for line in std::str::from_utf8(header).unwrap().lines().skip(1) {
        if let Some(rest) = line.strip_prefix('-') {
            prerequisites.push(rest.split(' ').next().unwrap().to_owned());
        } else if !line.is_empty() && !line.starts_with('@') {
            tips.push(line.split(' ').next().unwrap().to_owned());
        }
    }
    assert_eq!(&pack[..4], b"PACK", "{}", bundle.display());
    let declared = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as usize;
    // A scratch store that reads the source's objects, as a destination
    // holding the prerequisites would.
    let scratch = fixture.root.join(format!(
        "inspect-{}",
        bundle.file_name().unwrap().to_string_lossy()
    ));
    run(git(&fixture.root)
        .args(["init", "-q", "--bare", "--template="])
        .arg(&scratch));
    std::fs::write(
        scratch.join("objects/info/alternates"),
        format!("{}\n", fixture.source.join(".git/objects").display()),
    )
    .unwrap();
    let indexed = feed(
        git(&scratch).args(["index-pack", "--fix-thin", "--stdin"]),
        pack,
    );
    let hash = indexed.split('\t').nth(1).unwrap();
    let idx = scratch.join(format!("objects/pack/pack-{hash}.idx"));
    // The original objects sit before the original trailer; fix-thin
    // appends any missing delta base after them.
    let end = (pack.len() - 20) as u64;
    let mut packed = BTreeMap::new();
    let mut appended = 0;
    for line in run(git(&scratch).arg("verify-pack").arg("-v").arg(&idx)).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 || fields[0].len() != 40 {
            continue;
        }
        let offset: u64 = fields[4].parse().unwrap();
        if offset >= end {
            appended += 1;
            continue;
        }
        packed.insert(
            fields[0].to_owned(),
            Packed {
                kind: fields[1].to_owned(),
                in_pack: fields[3].parse().unwrap(),
                delta: fields.len() >= 7,
            },
        );
    }
    assert_eq!(packed.len(), declared, "{}", bundle.display());
    let commits: Vec<&str> = packed
        .iter()
        .filter(|(_, object)| object.kind == "commit")
        .map(|(value, _)| value.as_str())
        .collect();
    let parents = if commits.is_empty() {
        BTreeSet::new()
    } else {
        feed(
            git(&scratch).args(["rev-list", "--no-walk", "--parents", "--stdin"]),
            commits.join("\n").as_bytes(),
        )
        .lines()
        .flat_map(|line| line.split(' ').skip(1))
        .map(str::to_owned)
        .collect()
    };
    let reach_refs = object_set(&feed(
        git(&scratch).args(["rev-list", "--objects", "--stdin"]),
        tips.join("\n").as_bytes(),
    ));
    std::fs::remove_dir_all(&scratch).unwrap();
    Inspected {
        header: header.len(),
        pack: pack.len(),
        prerequisites,
        tips,
        packed,
        parents,
        appended,
        reach_refs,
    }
}

/// P64 over one bundle. Returns what it inspected, for the row's own checks,
/// and the full-closure overlap `packed(B) ∩ reach(prerequisites(B))`,
/// which P64 does not bound and every row pins.
fn law(fixture: &Fixture, row: &str, pass: u32, bundle: &Path) -> (Inspected, BTreeSet<String>) {
    let seen = inspect(fixture, bundle);
    // Every prerequisite and every commit they reach is in the source.
    let listed = |args: &[&str], values: &[&String]| {
        if values.is_empty() {
            return BTreeSet::new();
        }
        let request = values
            .iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        object_set(&feed(
            git(&fixture.source).args(args).arg("--stdin"),
            request.as_bytes(),
        ))
    };
    let prerequisites: Vec<&String> = seen.prerequisites.iter().collect();
    // C(B), then T(B): its commits the walk marks, and everything under
    // their trees.
    let reached = listed(&["rev-list"], &prerequisites);
    let marked: BTreeSet<&String> = seen
        .prerequisites
        .iter()
        .chain(&seen.tips)
        .chain(&seen.parents)
        .filter(|value| reached.contains(*value))
        .collect();
    let under = listed(
        &["rev-list", "--objects", "--no-walk"],
        &marked.into_iter().collect::<Vec<_>>(),
    );
    let full = listed(&["rev-list", "--objects"], &prerequisites);
    let overlap: BTreeSet<String> = seen
        .packed
        .keys()
        .filter(|value| full.contains(*value))
        .cloned()
        .collect();
    let count = |kind: &str| seen.packed.values().filter(|p| p.kind == kind).count();
    eprintln!(
        "P64 row={row} pass={pass} bundle={} bytes={} header={} pack={} objects={} \
         commits={} trees={} blobs={} tags={} deltas={} thin_bases={} prerequisites={} tips={} \
         overlap={}",
        &bundle.file_name().unwrap().to_string_lossy()[..16],
        seen.header + seen.pack,
        seen.header,
        seen.pack,
        seen.packed.len(),
        count("commit"),
        count("tree"),
        count("blob"),
        count("tag"),
        seen.packed.values().filter(|p| p.delta).count(),
        seen.appended,
        seen.prerequisites.len(),
        seen.tips.len(),
        overlap.len(),
    );
    let stray: Vec<String> = seen
        .packed
        .iter()
        .filter(|(value, _)| !seen.reach_refs.contains(*value))
        .map(|(value, packed)| format!("{value} {packed:?}"))
        .collect();
    assert!(
        stray.is_empty(),
        "P64 {row} pass {pass}: packed objects no ref of the bundle reaches: {stray:?}"
    );
    let held: Vec<String> = seen
        .packed
        .iter()
        .filter(|(value, _)| reached.contains(*value) || under.contains(*value))
        .map(|(value, packed)| format!("{value} {packed:?}"))
        .collect();
    assert!(
        held.is_empty(),
        "P64 {row} pass {pass}: {} of {} packed objects are commits the bundle's {} \
         prerequisites reach, or sit under a tip or edge tree they reach: {held:?}",
        held.len(),
        seen.packed.len(),
        seen.prerequisites.len()
    );
    (seen, overlap)
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.parent() == Some(root) && entry.file_name() == ".git" {
                continue;
            }
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_owned();
                let bytes = if kind.is_symlink() {
                    std::fs::read_link(&path)
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes()
                } else {
                    std::fs::read(&path).unwrap()
                };
                found.insert(rel, bytes);
            }
        }
    }
    found
}

fn apply_and_compare(fixture: &Fixture) {
    let applied = fixture.root.join("applied");
    verb(&[
        "estate-apply".as_ref(),
        fixture.plan.as_os_str(),
        fixture.corpus.as_os_str(),
        applied.as_os_str(),
        "neo".as_ref(),
        "1".as_ref(),
    ]);
    for (checkout, target) in &fixture.items {
        assert_eq!(
            run(git(target).args(["rev-parse", "HEAD"])),
            run(git(checkout).args(["rev-parse", "HEAD"])),
            "{}",
            target.display()
        );
        assert_eq!(
            run(git(target).args(["status", "--porcelain=v1"])),
            run(git(checkout).args(["status", "--porcelain=v1"])),
            "{}",
            target.display()
        );
        assert!(
            files(target) == files(checkout),
            "restored bytes differ: {}",
            target.display()
        );
    }
}

/// What a row expects of the bundles one later pass published.
struct Expect<'a> {
    name: &'a str,
    pass: u32,
    /// The moved checkout's item.
    item: &'a str,
    /// The edited large blob (P65), if the row edits it.
    edited: Option<&'a str>,
    /// The one object the moved item's bundle may share with the full
    /// closure of its prerequisites (P64's pinned leftover).
    leftover: Option<&'a str>,
    /// An unchanged payload blob the moved item's bundle packs whole again.
    payload: Option<&'a str>,
}

/// The law and every row pin over one later pass. Returns the moved item's
/// bundle from it, if that item was recaptured.
fn later_pass(
    fixture: &Fixture,
    fresh: &BTreeSet<PathBuf>,
    expect: &Expect<'_>,
) -> Option<PathBuf> {
    let (name, pass) = (expect.name, expect.pass);
    assert!(!fresh.is_empty(), "{name}: pass {pass} recaptured nothing");
    let mut deltas = 0;
    let mut thin = 0;
    let mut moved = Vec::new();
    for bundle in fresh {
        let (seen, overlap) = law(fixture, name, pass, bundle);
        let mine = of_item(bundle, expect.item);
        if mine {
            moved.push(bundle.clone());
        }
        assert!(
            !seen.packed.contains_key(&fixture.large),
            "{name}: a pass {pass} bundle re-packed the large blob"
        );
        match expect.leftover.filter(|_| mine) {
            Some(value) => {
                // Pinned: one blob the prerequisites hold only in an older
                // commit's tree, packed again whole.
                assert_eq!(
                    overlap,
                    BTreeSet::from([value.to_owned()]),
                    "P64 {name} pass {pass}: the full-closure overlap"
                );
                let packed = &seen.packed[value];
                assert!(
                    !packed.delta && packed.in_pack >= ROUND as u64,
                    "P64 {name}: the reverted blob's cost moved: {packed:?}"
                );
            }
            None => assert!(
                overlap.is_empty(),
                "P64 {name} pass {pass}: full-closure overlap {overlap:?}"
            ),
        }
        if let Some(value) = expect.payload.filter(|_| mine) {
            let packed = &seen.packed[value];
            assert!(
                !packed.delta && packed.in_pack >= PAYLOAD as u64,
                "P64 {name}: the unchanged payload's cost moved: {packed:?}"
            );
        }
        // P65: the edited large blob, wherever packed, is a small delta.
        if let Some(packed) = expect.edited.and_then(|value| seen.packed.get(value)) {
            assert!(
                packed.delta && packed.in_pack < (LARGE / 64) as u64,
                "P65 {name}: the edited large blob packed whole: {packed:?}"
            );
            deltas += 1;
            thin += usize::from(seen.appended > 0);
        }
    }
    if expect.edited.is_some() {
        assert!(deltas > 0, "P65 {name}: no bundle packed the edited blob");
        // The delta's base is the prerequisites' copy, so the pack is thin
        // and the apply below restores through index-pack --fix-thin.
        assert_eq!(thin, deltas, "P65 {name}: a delta was not against the base");
    }
    assert!(moved.len() <= 1, "{name}: pass {pass}: {moved:?}");
    moved.pop()
}

fn blob_at(fixture: &Fixture, revision: &str) -> String {
    run(git(&fixture.source).args(["rev-parse", revision]))
}

fn hashed(checkout: &Path, path: &str) -> String {
    feed(
        git(checkout).args(["hash-object", "--stdin"]),
        &std::fs::read(checkout.join(path)).unwrap(),
    )
}

fn row(layout: Layout, mutation: Mutation) {
    let name = format!("{layout:?}-{mutation:?}");
    let fixture = fixture(&name, layout, mutation);
    let moved = fixture.moved.as_path();
    let first = capture(&fixture);
    assert_eq!(
        first.bundles.len(),
        fixture.items.len(),
        "{:?}",
        first.bundles
    );
    let item = first.receipt(moved).item.clone();
    let payload = (mutation == Mutation::UntrackedPayload).then(|| hashed(moved, "payload.bin"));
    for bundle in &first.bundles {
        let (seen, overlap) = law(&fixture, &name, 1, bundle);
        assert!(
            overlap.is_empty(),
            "P64 {name} pass 1: full-closure overlap {overlap:?}"
        );
        // Item bundles of a group hold no copy of the base's blobs; a
        // chained first pass is self-contained and carries it once.
        if layout == Layout::Grouped {
            assert!(
                !seen.packed.contains_key(&fixture.large),
                "{name}: an item bundle re-packed the base's large blob"
            );
        }
        if let Some(value) = payload.as_deref().filter(|_| of_item(bundle, &item)) {
            let packed = &seen.packed[value];
            assert!(
                !packed.delta && packed.in_pack >= PAYLOAD as u64,
                "P64 {name}: the untracked payload's first cost: {packed:?}"
            );
        }
    }
    if mutation == Mutation::FirstPass {
        apply_and_compare(&fixture);
        return;
    }
    let mut leftover = None;
    match mutation {
        Mutation::FirstPass => unreachable!(),
        Mutation::HeadMove => {
            run(git(moved).args(["checkout", "-q", "--detach", &fixture.history[1]]));
        }
        Mutation::SideCommit => {
            let tip = run(git(&fixture.source).args(["rev-parse", "refs/heads/side"]));
            side_commit(&fixture.source, &tip, "side2.txt", b"side 1\n");
        }
        Mutation::DirRename => {
            run(git(&fixture.source).args(["mv", "tree/a", "tree/moved"]));
            run(git(&fixture.source).args(["commit", "-q", "-m", "rename"]));
        }
        Mutation::LargeCommitted => {
            overwrite_large(&fixture.source);
            run(git(&fixture.source).args(["commit", "-q", "-am", "large edit"]));
        }
        Mutation::LargeWorktree => overwrite_large(moved),
        Mutation::RevertOlder => {
            // c1's copy: in no prerequisite's tip tree, no edge tree and not
            // HEAD's (c2 grouped, c3 chained), yet reachable from them all.
            run(git(moved).args(["checkout", &fixture.history[1], "--", "rounds.bin"]));
            leftover = Some(blob_at(
                &fixture,
                &format!("{}:rounds.bin", fixture.history[1]),
            ));
        }
        Mutation::UntrackedPayload => edit_notes(moved),
        Mutation::ThinReuse => {
            overwrite_large(moved);
            // The third pass reuses this pass's capture: no seat may be racy
            // against its start.
            settle();
        }
    }
    let second = capture(&fixture);
    let edited = match mutation {
        Mutation::LargeCommitted => Some(fixture.source.as_path()),
        Mutation::LargeWorktree | Mutation::ThinReuse => Some(moved),
        _ => None,
    }
    .map(|checkout| hashed(checkout, "big.bin"));
    let mut expect = Expect {
        name: &name,
        pass: 2,
        item: &item,
        edited: edited.as_deref(),
        leftover: leftover.as_deref(),
        payload: payload.as_deref(),
    };
    let link = later_pass(&fixture, &second.fresh(&first), &expect);
    // A mutation in the moved checkout recaptures its item, so every pin on
    // that item's bundle ran.
    let touched = matches!(
        mutation,
        Mutation::HeadMove
            | Mutation::LargeWorktree
            | Mutation::RevertOlder
            | Mutation::UntrackedPayload
            | Mutation::ThinReuse
    );
    assert!(
        !touched || link.is_some(),
        "{name}: the moved item was not recaptured"
    );
    if let Some(link) = link.filter(|_| mutation == Mutation::ThinReuse) {
        thin_reuse(&fixture, &second, &link, &mut expect);
    }
    apply_and_compare(&fixture);
}

// The third pass of a thin-reuse row: its retained capture, `link`, is thin.
fn thin_reuse(fixture: &Fixture, second: &Pass, link: &Path, expect: &mut Expect<'_>) {
    let name = expect.name;
    let moved = fixture.moved.as_path();
    edit_notes(moved);
    let third = capture(fixture);
    let receipt = third.receipt(moved);
    // Every unchanged seat, the thin-delta'd large blob among them, comes
    // from the retained capture: only the edited file is read (R25).
    assert_eq!(receipt.reuse_unavailable, None, "{name}: {receipt:?}");
    assert_eq!(
        receipt.source_bytes_read,
        std::fs::metadata(moved.join("notes.txt")).unwrap().len(),
        "{name}: {receipt:?}"
    );
    // The reuse fetch completed `link` from the source store: the base it
    // appended (the whole large blob) is counted beside the bundle.
    let reuse_read = third.counters["read_source_capture_reuse_bytes"];
    eprintln!(
        "P64 row={name} pass=3 reuse_read={reuse_read} retained={} readback={} \
         source_bytes_read={}",
        std::fs::metadata(link).unwrap().len(),
        third.counters["read_source_pack_readback_bytes"],
        receipt.source_bytes_read,
    );
    assert!(
        reuse_read >= std::fs::metadata(link).unwrap().len() + LARGE as u64,
        "{name}: the reuse fetch's completed base went uncounted: {reuse_read}"
    );
    expect.pass = 3;
    let head = later_pass(fixture, &third.fresh(second), expect)
        .unwrap_or_else(|| panic!("{name}: pass 3 did not recapture the moved item"));
    // Both links after the first are thin, and the head names its
    // predecessor: a chained apply flattens two thin links.
    for bundle in [link, head.as_path()] {
        let seen = inspect(fixture, bundle);
        assert!(
            seen.appended > 0,
            "{name}: {} is not thin",
            bundle.display()
        );
        assert!(!seen.prerequisites.is_empty(), "{}", bundle.display());
        if fixture.items.len() == 1 {
            let mut prior = bundle.as_os_str().to_owned();
            prior.push(".prior");
            assert!(
                Path::new(&prior).exists(),
                "{}: not chained",
                bundle.display()
            );
        }
    }
}

#[test]
fn p64_grouped_first_pass() {
    row(Layout::Grouped, Mutation::FirstPass);
}

#[test]
fn p64_grouped_head_move() {
    row(Layout::Grouped, Mutation::HeadMove);
}

#[test]
fn p64_grouped_side_commit() {
    row(Layout::Grouped, Mutation::SideCommit);
}

#[test]
fn p64_grouped_directory_rename() {
    row(Layout::Grouped, Mutation::DirRename);
}

#[test]
fn p64_p65_grouped_committed_large_edit() {
    row(Layout::Grouped, Mutation::LargeCommitted);
}

#[test]
fn p64_p65_grouped_worktree_large_edit() {
    row(Layout::Grouped, Mutation::LargeWorktree);
}

#[test]
fn p64_grouped_revert_to_older_content() {
    row(Layout::Grouped, Mutation::RevertOlder);
}

#[test]
fn p64_grouped_unchanged_untracked_payload() {
    row(Layout::Grouped, Mutation::UntrackedPayload);
}

#[test]
fn p64_p65_grouped_thin_reuse_third_pass() {
    row(Layout::Grouped, Mutation::ThinReuse);
}

#[test]
fn p64_chained_first_pass() {
    row(Layout::Chained, Mutation::FirstPass);
}

#[test]
fn p64_chained_head_move() {
    row(Layout::Chained, Mutation::HeadMove);
}

#[test]
fn p64_chained_side_commit() {
    row(Layout::Chained, Mutation::SideCommit);
}

#[test]
fn p64_chained_directory_rename() {
    row(Layout::Chained, Mutation::DirRename);
}

#[test]
fn p64_p65_chained_committed_large_edit() {
    row(Layout::Chained, Mutation::LargeCommitted);
}

#[test]
fn p64_p65_chained_worktree_large_edit() {
    row(Layout::Chained, Mutation::LargeWorktree);
}

#[test]
fn p64_chained_revert_to_older_content() {
    row(Layout::Chained, Mutation::RevertOlder);
}

#[test]
fn p64_chained_unchanged_untracked_payload() {
    row(Layout::Chained, Mutation::UntrackedPayload);
}

#[test]
fn p64_p65_chained_thin_reuse_third_pass() {
    row(Layout::Chained, Mutation::ThinReuse);
}
