//! P64 PACK-MINIMALITY and P65 THIN-DELTA for v1 capture bundles (Q42 lane
//! L1; OI-1003-Q42, OI-1003-Q44, OI-1003-Q45, R-N13).
//!
//! **P64, an object-set law.** For every capture bundle `B` an estate pass
//! publishes, grouped (a shared plan base) or chained (WP2):
//!
//! ```text
//! packed(B) ⊆ reach(refs(B)) \ reach(prerequisites(B))
//! ```
//!
//! `reach` is the full object closure, not the edge trees git's walk marks.
//! `reach(refs(B)) \ reach(prerequisites(B))` is exactly the objects new
//! since `B`'s prerequisites plus this capture's own metadata objects (its
//! staged and worktree snapshots and metadata refs) that its prerequisites
//! do not already provide. So no bundle re-packs an object its prerequisites
//! already hold: an item bundle holds no copy of its group base's blobs.
//!
//! **P65.** A small edit to a large tracked blob, committed or not, packs as
//! a delta against the copy its prerequisites hold, grouped and chained. The
//! pack is then thin, and every row's apply restores it byte for byte through
//! `index-pack --fix-thin` (an import's fetch, or `chain::flatten`).
//!
//! The rows are a fixed table (OI-1003-Q7: no fuzzing), one test per
//! (layout, mutation). Each runs the verb binary end to end: a first pass, the
//! mutation, a second pass, the law over every item bundle of both passes,
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

fn verb(args: &[&std::ffi::OsStr]) {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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
}

struct Fixture {
    _root: Root,
    root: PathBuf,
    source: PathBuf,
    detached: Option<PathBuf>,
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

fn fixture(name: &str, layout: Layout) -> Fixture {
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
    for round in 0..4 {
        std::fs::write(
            source.join("notes.txt"),
            format!("notes {round}\n{}", "line\n".repeat(64 * (round + 1))),
        )
        .unwrap();
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
    let detached = (layout == Layout::Grouped).then(|| {
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
    });
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
        detached,
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

fn capture(fixture: &Fixture) -> BTreeSet<PathBuf> {
    verb(&[
        "estate-capture".as_ref(),
        fixture.plan.as_os_str(),
        fixture.state.as_os_str(),
        fixture.corpus.as_os_str(),
        "1".as_ref(),
    ]);
    std::fs::read_dir(&fixture.corpus)
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
        .collect()
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
        appended,
        reach_refs,
    }
}

/// P64 over one bundle; returns what it inspected for the row's own checks.
fn law(fixture: &Fixture, row: &str, pass: u32, bundle: &Path) -> Inspected {
    let seen = inspect(fixture, bundle);
    let reach_prerequisites = if seen.prerequisites.is_empty() {
        BTreeSet::new()
    } else {
        object_set(&feed(
            git(&fixture.source).args(["rev-list", "--objects", "--stdin"]),
            seen.prerequisites.join("\n").as_bytes(),
        ))
    };
    let count = |kind: &str| seen.packed.values().filter(|p| p.kind == kind).count();
    eprintln!(
        "P64 row={row} pass={pass} bundle={} bytes={} header={} pack={} objects={} \
         commits={} trees={} blobs={} tags={} deltas={} thin_bases={} prerequisites={} tips={}",
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
        .filter(|(value, _)| reach_prerequisites.contains(*value))
        .map(|(value, packed)| format!("{value} {packed:?}"))
        .collect();
    assert!(
        held.is_empty(),
        "P64 {row} pass {pass}: {} of {} packed objects are already reachable from \
         the bundle's {} prerequisites: {held:?}",
        held.len(),
        seen.packed.len(),
        seen.prerequisites.len()
    );
    seen
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

fn row(layout: Layout, mutation: Mutation) {
    let name = format!("{layout:?}-{mutation:?}");
    let fixture = fixture(&name, layout);
    let first = capture(&fixture);
    assert_eq!(first.len(), fixture.items.len(), "{first:?}");
    for bundle in &first {
        let seen = law(&fixture, &name, 1, bundle);
        // Item bundles of a group hold no copy of the base's blobs; a
        // chained first pass is self-contained and carries it once.
        if layout == Layout::Grouped {
            assert!(
                !seen.packed.contains_key(&fixture.large),
                "{name}: an item bundle re-packed the base's large blob"
            );
        }
    }
    if mutation == Mutation::FirstPass {
        apply_and_compare(&fixture);
        return;
    }
    let moved = fixture.detached.as_deref().unwrap_or(&fixture.source);
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
    }
    let second: BTreeSet<PathBuf> = capture(&fixture).difference(&first).cloned().collect();
    assert!(
        !second.is_empty(),
        "{name}: the mutation recaptured nothing"
    );
    let edited = match mutation {
        Mutation::LargeCommitted => Some(fixture.source.as_path()),
        Mutation::LargeWorktree => Some(moved),
        _ => None,
    }
    .map(|checkout| {
        feed(
            git(checkout).args(["hash-object", "--stdin"]),
            &std::fs::read(checkout.join("big.bin")).unwrap(),
        )
    });
    let mut deltas = 0;
    let mut thin = 0;
    for bundle in &second {
        let seen = law(&fixture, &name, 2, bundle);
        assert!(
            !seen.packed.contains_key(&fixture.large),
            "{name}: a second-pass bundle re-packed the large blob"
        );
        // P65: the edited large blob, wherever packed, is a small delta.
        if let Some(packed) = edited.as_ref().and_then(|value| seen.packed.get(value)) {
            assert!(
                packed.delta && packed.in_pack < (LARGE / 64) as u64,
                "P65 {name}: the edited large blob packed whole: {packed:?}"
            );
            deltas += 1;
            thin += usize::from(seen.appended > 0);
        }
    }
    if edited.is_some() {
        assert!(deltas > 0, "P65 {name}: no bundle packed the edited blob");
        // The delta's base is the prerequisites' copy, so the pack is thin
        // and the apply below restores through index-pack --fix-thin.
        assert_eq!(thin, deltas, "P65 {name}: a delta was not against the base");
    }
    apply_and_compare(&fixture);
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
