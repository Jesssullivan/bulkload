//! Fixtures and the P64 law shared by the grouped-capture tables:
//! `tests/git_group_minimality.rs` (P64, P65) and
//! `tests/git_grouped_chain.rs` (P68). A fixed table's helpers: no fuzzing
//! (OI-1003-Q7). Every helper runs the verb binary or plain `git`.

#![allow(dead_code, clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The large tracked blob: incompressible, so a re-pack shows in bytes.
pub const LARGE: usize = 1024 * 1024 + 17;
/// Where the 64-byte edit lands in it.
pub const EDIT_AT: usize = 512 * 1024;
/// Orphan branch tips whose trees hold no large blob. More than pack-objects'
/// ten preferred bases, so a delta needs the right edge first.
pub const TIPS: usize = 12;
/// `rounds.bin`: incompressible and rewritten by every commit of main's
/// history, so an older round's copy sits only in that round's tree.
pub const ROUND: usize = 64 * 1024;
/// `payload.bin`: an untracked file the payload rows never change.
pub const PAYLOAD: usize = 512 * 1024;

pub fn git(repo: &Path) -> Command {
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

pub fn run(command: &mut Command) -> String {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

pub fn feed(command: &mut Command, input: &[u8]) -> String {
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
pub fn verb(args: &[&std::ffi::OsStr]) -> (String, String) {
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
pub fn noise(len: usize, mut state: u32) -> Vec<u8> {
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
pub fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

pub struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// A main checkout plus a branch and a detached linked worktree: a
    /// 3-item group on one shared plan base.
    Grouped,
    /// The main checkout alone: a group of one, chained on its prior capture.
    Chained,
    /// The main checkout plus the `wt` branch worktree: a 2-item group (P68's
    /// Probe-2 fixture).
    Pair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
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

pub struct Fixture {
    pub _root: Root,
    pub root: PathBuf,
    pub source: PathBuf,
    /// The checkout every worktree mutation lands in: the detached linked
    /// worktree of a group, or the main checkout of a chain.
    pub moved: PathBuf,
    pub plan: PathBuf,
    pub state: PathBuf,
    pub corpus: PathBuf,
    /// (source checkout, restore target), one per item.
    pub items: Vec<(PathBuf, PathBuf)>,
    /// main's first-parent history, oldest first.
    pub history: Vec<String>,
    /// The large blob every first pass carries once, in the base.
    pub large: String,
    /// The existing repository every item restores into.
    pub dest: PathBuf,
    /// The directory that holds every restore target.
    pub restored: PathBuf,
}

pub fn commit_all(repo: &Path, message: &str) -> String {
    run(git(repo).args(["add", "-A"]));
    run(git(repo).args(["commit", "-q", "-m", message]));
    run(git(repo).args(["rev-parse", "HEAD"]))
}

pub fn fixture(name: &str, layout: Layout, mutation: Mutation) -> Fixture {
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
    let moved = if layout == Layout::Chained {
        source.clone()
    } else {
        let branch = root.join("wt-branch");
        run(git(&source)
            .args(["worktree", "add", "-q", "-b", "wt"])
            .arg(&branch)
            .arg(&history[3]));
        std::fs::write(branch.join("notes.txt"), b"notes on wt\n").unwrap();
        commit_all(&branch, "wt");
        std::fs::write(branch.join("scratch.txt"), b"untracked in wt\n").unwrap();
        items.push((branch.clone(), restored.join("branch")));
        if layout == Layout::Grouped {
            let held = root.join("wt-detached");
            run(git(&source)
                .args(["worktree", "add", "-q", "--detach"])
                .arg(&held)
                .arg(&history[2]));
            std::fs::write(held.join("scratch.txt"), b"untracked in detached\n").unwrap();
            items.push((held.clone(), restored.join("detached")));
            held
        } else {
            branch
        }
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
        dest,
        restored,
        root,
        _root: guard,
    }
}

pub fn side_commit(source: &Path, parent: &str, name: &str, bytes: &[u8]) {
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

pub fn overwrite_large(checkout: &Path) {
    let path = checkout.join("big.bin");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[EDIT_AT..EDIT_AT + 64].copy_from_slice(&[b'q'; 64]);
    std::fs::write(&path, bytes).unwrap();
}

// A one-line, uncommitted edit to a small tracked file.
pub fn edit_notes(checkout: &Path) {
    let path = checkout.join("notes.txt");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"one more line\n");
    std::fs::write(&path, bytes).unwrap();
}

/// One item line of an estate verb's receipt.
#[derive(Debug)]
pub struct Receipt {
    pub item: String,
    pub source: PathBuf,
    pub source_bytes_read: u64,
    pub reuse_unavailable: Option<String>,
}

/// What one `estate-capture` run published and printed.
pub struct Pass {
    /// Every item bundle in the corpus after the run.
    pub bundles: BTreeSet<PathBuf>,
    pub receipts: Vec<Receipt>,
    pub counters: BTreeMap<String, u64>,
}

impl Pass {
    /// The bundles this pass published that `before` did not hold.
    pub fn fresh(&self, before: &Self) -> BTreeSet<PathBuf> {
        self.bundles.difference(&before.bundles).cloned().collect()
    }

    /// The receipt of the item whose source is `checkout`.
    pub fn receipt(&self, checkout: &Path) -> &Receipt {
        let wanted = std::fs::canonicalize(checkout).unwrap();
        self.receipts
            .iter()
            .find(|row| std::fs::canonicalize(&row.source).is_ok_and(|path| path == wanted))
            .unwrap_or_else(|| panic!("no receipt for {}: {:?}", checkout.display(), self.receipts))
    }
}

/// Whether `bundle` is a capture of `item` (published as `{item}-{digest}`).
pub fn of_item(bundle: &Path, item: &str) -> bool {
    bundle
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("{item}-"))
}

pub fn receipts(stdout: &str) -> Vec<Receipt> {
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

pub fn capture(fixture: &Fixture) -> Pass {
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
pub struct Packed {
    pub kind: String,
    pub in_pack: u64,
    pub delta: bool,
}

/// What a capture bundle declares and packs.
pub struct Inspected {
    pub header: usize,
    pub pack: usize,
    pub prerequisites: Vec<String>,
    pub tips: Vec<String>,
    pub packed: BTreeMap<String, Packed>,
    /// The parents of every commit the pack holds.
    pub parents: BTreeSet<String>,
    /// Bases `index-pack --fix-thin` had to append: the pack was thin.
    pub appended: usize,
    /// Every object reachable from the bundle's refs.
    pub reach_refs: BTreeSet<String>,
}

pub fn object_set(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| line.split(' ').next())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn inspect(fixture: &Fixture, bundle: &Path) -> Inspected {
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
pub fn law(
    fixture: &Fixture,
    row: &str,
    pass: u32,
    bundle: &Path,
) -> (Inspected, BTreeSet<String>) {
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

pub fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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

pub fn apply_and_compare(fixture: &Fixture) {
    apply_into(fixture, &fixture.root.join("applied"));
    compare(fixture);
}

/// `estate-apply` of the fixture's plan and corpus under the apply state
/// directory `applied`.
pub fn apply_into(fixture: &Fixture, applied: &Path) {
    verb(&[
        "estate-apply".as_ref(),
        fixture.plan.as_os_str(),
        fixture.corpus.as_os_str(),
        applied.as_os_str(),
        "neo".as_ref(),
        "1".as_ref(),
    ]);
}

/// Every restored item equals its source checkout: HEAD, status and bytes.
pub fn compare(fixture: &Fixture) {
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
