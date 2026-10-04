//! P34 SOURCE-INERT for the v1 capture's object store (S2), and bare sources
//! carried as ref custody (S4): bulkload#162.
//!
//! Git freshens (re-stamps with `utime`) any existing copy of an object it is
//! asked to write, in its own store or in any alternate. The capture's private
//! repository borrows the source's store, so before #162 a changed capture
//! re-stamped source loose objects and packs: a write to the source. These
//! properties hold every `lstat` field of every source node fixed across a
//! capture and two incremental passes that reuse and chain on the one before,
//! and prove the objects the private writers create still reach the bundles.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::{
    attach_matching_payload, bare_marked, capture_census, capture_key_parts, commit_object,
    common_repository, estimate, export_repository, export_repository_with_custody, git, git_env,
    git_writer, import_bundle, input, registered, repair_missing_index, restore_bundle,
    restore_linked, text, CapturePolicy, Export, ExportOptions, Exported, RetainedCapture,
};
use crate::BulkloadRefusal;

fn fresh(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "bulkload-p34-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir(&root).unwrap();
    fs::canonicalize(root).unwrap()
}

// A fixture Git child through the hardened builder (no hooks, no global or
// system configuration), with a test identity.
fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
    let result = git(repo)
        .args(["-c", "user.name=Bulkload test"])
        .args(["-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

type Row = (PathBuf, [i64; 8]);

/// Every node under `root`, itself included, with every `lstat` field a
/// write, a freshen, a create, a rename, a chmod or a link would move (the
/// `transfer::tests` census pattern). Access time is left out: reading the
/// source is the point of a capture.
fn lstat_census(root: &Path) -> Vec<Row> {
    fn visit(root: &Path, relative: &Path, rows: &mut Vec<Row>) {
        let meta = fs::symlink_metadata(root.join(relative)).unwrap();
        rows.push((
            relative.to_path_buf(),
            [
                i64::from(meta.mode()),
                i64::try_from(meta.size()).unwrap(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
                i64::try_from(meta.ino()).unwrap(),
                i64::try_from(meta.nlink()).unwrap(),
            ],
        ));
        if meta.is_dir() {
            for entry in fs::read_dir(root.join(relative)).unwrap() {
                visit(root, &relative.join(entry.unwrap().file_name()), rows);
            }
        }
    }
    let mut rows = Vec::new();
    visit(root, Path::new(""), &mut rows);
    rows.sort();
    rows
}

// The nodes whose census row moved, appeared or vanished, named for the
// failure message rather than printing two whole censuses.
fn moved(before: &[Row], after: &[Row]) -> Vec<String> {
    let old: std::collections::BTreeMap<_, _> = before.iter().cloned().collect();
    let new: std::collections::BTreeMap<_, _> = after.iter().cloned().collect();
    let mut rows = Vec::new();
    for (path, fields) in &old {
        match new.get(path) {
            Some(now) if now == fields => {}
            Some(now) => rows.push(format!(
                "~ {} [mode size mtime mtime_ns ctime ctime_ns ino nlink] {fields:?} -> {now:?}",
                path.display()
            )),
            None => rows.push(format!("- {}", path.display())),
        }
    }
    for path in new.keys().filter(|path| !old.contains_key(*path)) {
        rows.push(format!("+ {}", path.display()));
    }
    rows
}

/// Stamp every object file at a fixed past instant before the census, so a
/// freshen (`utime(NULL)`: now) moves its mtime whatever the filesystem's
/// timestamp granularity.
fn backdate(objects: &Path) {
    // 2001-01-01T00:00:00Z: 11,323 days after the epoch.
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_hours(11_323 * 24);
    let times = fs::FileTimes::new().set_modified(past);
    let mut pending = vec![objects.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            } else {
                fs::File::open(&path).unwrap().set_times(times).unwrap();
            }
        }
    }
}

/// One generated source repository.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Generated toggles, one per source state.
struct Shape {
    /// The first commit's files: (directory 0..3, content seed, length). A
    /// zero length is the empty blob, which the capture's own metadata writes.
    first: Vec<(u8, u8, u16)>,
    /// Pack the first commit (`repack -a -d`); everything later stays loose.
    packed: bool,
    /// Files a second, loose commit adds, beside a rewrite of `f0`.
    second: Vec<(u8, u8, u16)>,
    /// Stashes pushed; the last carries untracked files when set.
    stashes: u8,
    untracked_stash: bool,
    /// A staged change reverted in the index: `write-tree` rebuilds a tree
    /// the source already holds.
    staged_revert: bool,
    /// A staged edit with an unstaged edit on top.
    staged_edit: bool,
    /// Untracked files; the first is a copy of a tracked file's bytes.
    untracked: u8,
}

fn shape() -> impl proptest::strategy::Strategy<Value = Shape> {
    use proptest::prelude::*;
    let file = || (0_u8..3, any::<u8>(), 0_u16..3_000);
    (
        proptest::collection::vec(file(), 1..6),
        any::<bool>(),
        proptest::collection::vec(file(), 0..4),
        0_u8..3,
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        0_u8..3,
    )
        .prop_map(
            |(
                first,
                packed,
                second,
                stashes,
                untracked_stash,
                staged_revert,
                staged_edit,
                untracked,
            )| Shape {
                first,
                packed,
                second,
                stashes,
                untracked_stash,
                staged_revert,
                staged_edit,
                untracked,
            },
        )
}

fn bytes(seed: u8, length: u16) -> Vec<u8> {
    (0..length)
        .map(|index| seed.wrapping_add(u8::try_from(index % 251).unwrap().wrapping_mul(31)))
        .collect()
}

fn place(repo: &Path, directory: u8, name: &str, content: &[u8]) {
    let parent = match directory {
        0 => repo.to_path_buf(),
        1 => repo.join("a"),
        _ => repo.join("a/b"),
    };
    fs::create_dir_all(&parent).unwrap();
    fs::write(parent.join(name), content).unwrap();
}

fn build(root: &Path, shape: &Shape) -> PathBuf {
    let repo = root.join("source");
    fs::create_dir(&repo).unwrap();
    g(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    for (index, (directory, seed, length)) in shape.first.iter().enumerate() {
        place(
            &repo,
            *directory,
            &format!("f{index}"),
            &bytes(*seed, *length),
        );
    }
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "--quiet", "-m", "one"]);
    if shape.packed {
        g(&repo, &["repack", "--quiet", "-a", "-d"]);
    }
    for (index, (directory, seed, length)) in shape.second.iter().enumerate() {
        place(
            &repo,
            *directory,
            &format!("s{index}"),
            &bytes(*seed, *length),
        );
    }
    fs::write(repo.join("f0"), b"second revision").unwrap();
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "--quiet", "-m", "two"]);
    for stash in 0..shape.stashes {
        fs::write(repo.join("f0"), format!("stash {stash}")).unwrap();
        let mut push = vec!["stash", "push", "--quiet"];
        if shape.untracked_stash && stash + 1 == shape.stashes {
            fs::write(repo.join(format!("stash-untracked-{stash}")), b"u").unwrap();
            push.push("--include-untracked");
        }
        g(&repo, &push);
    }
    if shape.staged_revert {
        let original = fs::read(repo.join("f0")).unwrap();
        fs::write(repo.join("f0"), b"staged, then reverted").unwrap();
        g(&repo, &["add", "f0"]);
        fs::write(repo.join("f0"), original).unwrap();
        g(&repo, &["add", "f0"]);
    }
    if shape.staged_edit {
        fs::write(repo.join("f0"), b"staged edit").unwrap();
        g(&repo, &["add", "f0"]);
        fs::write(repo.join("f0"), b"unstaged edit on top").unwrap();
    }
    for index in 0..shape.untracked {
        let content = if index == 0 {
            let (_, seed, length) = shape.first.first().copied().unwrap_or((0, 0, 0));
            bytes(seed, length)
        } else {
            format!("untracked {index}").into_bytes()
        };
        fs::write(repo.join(format!("u{index}")), content).unwrap();
    }
    repo
}

// A capture of `source` into `capture`; with `prior`, an incremental pass that
// reuses and chains on it, as estate-capture runs one. The prior pass is
// presented as having started 10 s later than it did, as if every seat had
// then been still for a while: unchanged seats are not racy, so the pass
// emits them by object name (fast-import's `M <oid>`, often a blob only the
// source holds) without the test sleeping.
fn captured(source: &Path, capture: &Path, prior: Option<&Export>) -> Export {
    let options = ExportOptions {
        prerequisite: None,
        policy: CapturePolicy::default(),
        reuse: prior.map(|prior| RetainedCapture {
            bundle: &prior.bundle,
            started_ns: prior.started_ns + 10_000_000_000,
        }),
        planned: &[],
        chain: prior.map(|prior| prior.bundle.as_path()),
    };
    match export_repository_with_custody(source, capture, &options).unwrap() {
        Exported::Captured(export) => {
            assert!(export.drift.is_empty(), "{:?}", export.drift);
            *export
        }
        Exported::ObjectStoreRewritten(drift) => panic!("{drift:?}"),
    }
}

/// Fetch `bundles`, oldest first, into a new repository that borrows no
/// store, and check every ref's closure is complete there: each object the
/// private writers created reached its bundle or one before it. Returns that
/// repository.
fn carried(root: &Path, bundles: &[&Path]) -> PathBuf {
    let repository = root.join("carried.git");
    g(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            repository.to_str().unwrap(),
        ],
    );
    for (index, bundle) in bundles.iter().enumerate() {
        let refspec = format!("+refs/*:refs/link-{index}/*");
        g(
            &repository,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                bundle.to_str().unwrap(),
                &refspec,
            ],
        );
    }
    g(
        &repository,
        &["fsck", "--connectivity-only", "--no-dangling"],
    );
    repository
}

// The staged tree a bundle carries, as `ls-tree -r` rows, against the
// source's index, as `ls-files --stage` rows: (mode, object, path).
fn staged_equals_index(carried: &Path, link: usize, source: &Path) -> bool {
    let tree = g(
        carried,
        &[
            "ls-tree",
            "-r",
            "-z",
            &format!("refs/link-{link}/carry-export/staged"),
        ],
    );
    let index = g(source, &["ls-files", "--stage", "-z"]);
    let rows = |bytes: &[u8], object_field: usize| -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        bytes
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
            .map(|row| {
                let tab = row.iter().position(|byte| *byte == b'\t').unwrap();
                let (header, path) = row.split_at(tab);
                let fields: Vec<&[u8]> = header.split(|byte| *byte == b' ').collect();
                (
                    fields[0].to_vec(),
                    fields[object_field].to_vec(),
                    path[1..].to_vec(),
                )
            })
            .collect()
    };
    rows(&tree, 2) == rows(&index, 1)
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(6))]

    /// P34 SOURCE-INERT, object store (S2, #162): over generated
    /// repositories with packed and loose objects, stashes, a dirty or
    /// reverted index and untracked files, a capture and two incremental
    /// passes that reuse and chain on the one before leave every `lstat`
    /// field (mode, size, mtime, ctime, inode, link count) of every source
    /// node, its `.git` object store included, exactly as it was. Every
    /// object the capture's private writers create still reaches the chain
    /// of bundles, and the staged tree is the source's index.
    #[test]
    fn p34_a_capture_leaves_the_source_lstat_census_unchanged(shape in shape()) {
        let root = fresh("census");
        let source = build(&root, &shape);
        backdate(&source.join(".git/objects"));
        let before = lstat_census(&source);
        let first = captured(&source, &root.join("capture-1"), None);
        let changed = moved(&before, &lstat_census(&source));
        proptest::prop_assert!(changed.is_empty(), "first capture:\n{}", changed.join("\n"));
        // A worktree-only change, so the next passes are changed captures.
        fs::write(source.join("written-between-passes"), b"new seat").unwrap();
        let before = lstat_census(&source);
        let second = captured(&source, &root.join("capture-2"), Some(&first));
        let changed = moved(&before, &lstat_census(&source));
        proptest::prop_assert!(changed.is_empty(), "second capture:\n{}", changed.join("\n"));
        let third = captured(&source, &root.join("capture-3"), Some(&second));
        let changed = moved(&before, &lstat_census(&source));
        proptest::prop_assert!(changed.is_empty(), "third capture:\n{}", changed.join("\n"));
        // R25 still holds across the split stores: each incremental pass read
        // only the seat written since the pass before, and reused the rest.
        proptest::prop_assert_eq!(
            (second.reuse_unavailable, third.reuse_unavailable),
            (None, None)
        );
        proptest::prop_assert_eq!(
            (second.bytes_read, third.bytes_read),
            (b"new seat".len() as u64, 0)
        );
        let repository = carried(&root, &[&first.bundle, &second.bundle, &third.bundle]);
        proptest::prop_assert!(staged_equals_index(&repository, 2, &source));
        fs::remove_dir_all(root).unwrap();
    }
}

/// A private writer is the one hardened child plus exactly one variable: its
/// object store, the private write store (#162). It inherits no alternate,
/// so it cannot see, and therefore cannot freshen, the source's store.
#[test]
fn a_private_writer_is_the_hardened_child_in_the_write_store() {
    use std::ffi::OsStr;
    let private = Path::new("/estate/capture/repository.git");
    let writer = git_writer(private);
    let reader = git(private);
    let mut expected: std::collections::BTreeMap<_, _> = reader.get_envs().collect();
    let store = private.join(git_env::WRITE_STORE);
    expected.insert(OsStr::new("GIT_OBJECT_DIRECTORY"), Some(store.as_os_str()));
    let envs: std::collections::BTreeMap<_, _> = writer.get_envs().collect();
    assert_eq!(envs, expected);
    assert_eq!(
        envs.get(OsStr::new("GIT_ALTERNATE_OBJECT_DIRECTORIES")),
        Some(&None)
    );
    assert!(writer.get_args().eq(reader.get_args()));
}

/// The private repository's commits are written by `hash-object` into its
/// write store (#162). They are byte for byte what `git commit-tree -m`
/// writes under the archival identity, in both object formats, so every
/// bundle ref and capture keeps its name.
#[test]
fn private_commits_are_exactly_what_commit_tree_writes() {
    let root = fresh("commit-pin");
    for format in ["sha1", "sha256"] {
        let repository = root.join(format!("{format}.git"));
        g(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
                repository.to_str().unwrap(),
            ],
        );
        let blob = input(
            git(&repository).args(["hash-object", "-w", "--stdin"]),
            b"value",
        )
        .unwrap();
        let blob = std::str::from_utf8(&blob).unwrap().trim();
        let tree = input(
            git(&repository).args(["mktree", "-z"]),
            format!("100644 blob {blob}\tvalue\0").as_bytes(),
        )
        .unwrap();
        let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
        for label in [
            "bulkload staged tree",
            "bulkload worktree including untracked and ignored files",
            "bulkload pre-prune worktree",
            "bulkload explicit shallow graph custody",
            "filesystem-v1",
        ] {
            let expected = text(
                git(&repository)
                    .env("GIT_AUTHOR_NAME", "Bulkload archival capture")
                    .env("GIT_AUTHOR_EMAIL", "bulkload@localhost")
                    .env("GIT_COMMITTER_NAME", "Bulkload archival capture")
                    .env("GIT_COMMITTER_EMAIL", "bulkload@localhost")
                    .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
                    .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
                    .args(["commit-tree", &tree, "-m", label]),
            )
            .unwrap();
            assert_eq!(
                commit_object(&mut git(&repository), &tree, label).unwrap(),
                expected,
                "{format} {label}"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

/// S4 (#162): a bare repository is carried as ref custody, never refused
/// with a bare IO (errno 2). It has no index and no worktree, so its staged
/// and worktree trees are empty; its refs and HEAD are carried, an
/// incremental pass chains on the first, the mirror's own `lstat` census
/// (S2) is unchanged by either, and an import names every carried ref.
#[test]
#[allow(clippy::too_many_lines)] // One fixture, two passes, the import.
fn s4_a_bare_repository_is_carried_as_ref_custody() {
    let root = fresh("bare");
    let origin = build(
        &root,
        &Shape {
            first: vec![(0, 1, 40), (1, 2, 0), (2, 3, 900)],
            packed: true,
            second: vec![(1, 4, 70)],
            stashes: 0,
            untracked_stash: false,
            staged_revert: false,
            staged_edit: false,
            untracked: 0,
        },
    );
    g(&origin, &["tag", "-a", "-m", "tag", "v1"]);
    let mirror = root.join("mirror.git");
    g(
        &root,
        &[
            "clone",
            "--quiet",
            "--bare",
            "--no-local",
            origin.to_str().unwrap(),
            mirror.to_str().unwrap(),
        ],
    );
    // A small push leaves loose objects beside the clone's pack.
    fs::write(origin.join("pushed"), b"pushed after the clone").unwrap();
    g(&origin, &["add", "pushed"]);
    g(&origin, &["commit", "--quiet", "-m", "pushed"]);
    g(
        &origin,
        &["push", "--quiet", mirror.to_str().unwrap(), "main"],
    );
    capture_key_parts(&mirror).unwrap();
    backdate(&mirror.join("objects"));
    let before = lstat_census(&mirror);
    let first = captured(&mirror, &root.join("capture-1"), None);
    let changed = moved(&before, &lstat_census(&mirror));
    assert!(changed.is_empty(), "{}", changed.join("\n"));
    let heads = text(
        git(&mirror)
            .args(["bundle", "list-heads"])
            .arg(&first.bundle),
    )
    .unwrap();
    let main = text(git(&mirror).args(["rev-parse", "refs/heads/main"])).unwrap();
    assert!(
        heads.contains(&format!("{main} refs/carry-export/refs/heads/main")),
        "{heads}"
    );
    assert!(heads.contains(" refs/carry-export/refs/tags/v1"), "{heads}");
    assert!(
        heads.contains(&format!("{main} refs/carry-export/head")),
        "{heads}"
    );
    assert!(bare_marked(&heads), "{heads}");
    // A second push, then an incremental pass that chains on the first.
    fs::write(origin.join("pushed"), b"pushed again").unwrap();
    g(&origin, &["commit", "--quiet", "-am", "again"]);
    g(
        &origin,
        &["push", "--quiet", mirror.to_str().unwrap(), "main"],
    );
    backdate(&mirror.join("objects"));
    let before = lstat_census(&mirror);
    let second = captured(&mirror, &root.join("capture-2"), Some(&first));
    let changed = moved(&before, &lstat_census(&mirror));
    assert!(changed.is_empty(), "{}", changed.join("\n"));
    let repository = carried(&root, &[&first.bundle, &second.bundle]);
    let empty = text(git(&repository).args(["hash-object", "-t", "tree", "/dev/null"])).unwrap();
    for name in ["staged", "worktree"] {
        assert_eq!(
            text(git(&repository).args([
                "rev-parse",
                &format!("refs/link-1/carry-export/{name}^{{tree}}")
            ]))
            .unwrap(),
            empty,
            "{name}"
        );
    }
    let destination = root.join("destination");
    fs::create_dir(&destination).unwrap();
    g(
        &destination,
        &["init", "--quiet", "--template=", "-b", "main"],
    );
    assert!(import_bundle(&destination, &first.bundle, "neo").unwrap() > 0);
    let imported = text(git(&destination).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/carry/v1/neo/",
    ]))
    .unwrap();
    assert!(
        imported
            .lines()
            .any(|line| line.starts_with(&main) && line.ends_with("/refs/heads/main")),
        "{imported}"
    );
    fs::remove_dir_all(root).unwrap();
}

// A small committed origin for the S4 layouts below: two commits, a nested
// directory, and one file rewritten by the second.
fn plain_origin(root: &Path) -> PathBuf {
    build(
        root,
        &Shape {
            first: vec![(0, 1, 40), (1, 2, 30), (2, 3, 900)],
            packed: false,
            second: vec![(1, 4, 70)],
            stashes: 0,
            untracked_stash: false,
            staged_revert: false,
            staged_edit: false,
            untracked: 0,
        },
    )
}

// `rendered`'s refusal, if any: the `Ok` shape is not compared.
fn refusal<T>(rendered: crate::Result<T>) -> Option<BulkloadRefusal> {
    rendered.err()
}

/// S4 (#162): a `.git` gitfile naming a bare git dir (the bare-plus-worktrees
/// layout: `clone --bare origin project/.bare`, `gitdir: ./.bare`) is not a
/// repository root. Capture refuses `GIT_REPOSITORY_NOT_AT_PATH`, exactly as
/// the estimate probe does, instead of carrying the bare administration
/// (config, hooks, objects, worktree indexes) as worktree seats under an
/// empty staged tree. The bare git dir itself and its linked worktree are
/// roots, and each captures.
#[test]
fn s4_a_gitfile_naming_a_bare_git_dir_is_not_a_repository_root() {
    let root = fresh("gitfile-bare");
    let origin = plain_origin(&root);
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    g(
        &root,
        &[
            "clone",
            "--quiet",
            "--bare",
            "--no-local",
            origin.to_str().unwrap(),
            project.join(".bare").to_str().unwrap(),
        ],
    );
    fs::write(project.join(".git"), b"gitdir: ./.bare\n").unwrap();
    g(&project, &["worktree", "add", "--quiet", "wt"]);
    fs::write(project.join("notes.txt"), b"beside the bare git dir").unwrap();
    let not_at_path = Some(BulkloadRefusal::GitRepositoryNotAtPath);
    assert_eq!(refusal(capture_key_parts(&project)), not_at_path);
    assert_eq!(
        refusal(export_repository(&project, &root.join("capture"))),
        not_at_path
    );
    let destination = root.join("destination");
    fs::create_dir(&destination).unwrap();
    g(&destination, &["init", "--quiet", "--template="]);
    assert_eq!(
        estimate::estimate(&project, &estimate::Destination::Local(destination))
            .unwrap_err()
            .refusal,
        BulkloadRefusal::GitRepositoryNotAtPath
    );
    capture_key_parts(&project.join(".bare")).unwrap();
    capture_key_parts(&project.join("wt")).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// S4 (#162): a repository's own administration below its root, reached
/// through a `.git` gitfile (`git init --separate-git-dir`), is not a seat,
/// exactly as a root `.git` directory is not: its config, hooks and objects
/// are never carried as worktree bytes.
#[test]
fn s4_a_separate_git_dir_below_the_root_is_not_a_seat() {
    let root = fresh("separate-git-dir");
    let checkout = root.join("checkout");
    g(
        &root,
        &[
            "init",
            "--quiet",
            "--template=",
            "-b",
            "main",
            "--separate-git-dir",
            checkout.join(".gitdata").to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    fs::write(checkout.join("tracked"), b"tracked").unwrap();
    g(&checkout, &["add", "tracked"]);
    g(&checkout, &["commit", "--quiet", "-m", "one"]);
    fs::write(checkout.join("untracked"), b"untracked").unwrap();
    let common = common_repository(&checkout).unwrap();
    assert_eq!(common, checkout.join(".gitdata"));
    let census = capture_census(&checkout, &common, CapturePolicy::default()).unwrap();
    let mut seats: Vec<&[u8]> = census
        .rows
        .iter()
        .map(|row| row.rel_path.as_slice())
        .collect();
    seats.sort_unstable();
    assert_eq!(seats, [&b"tracked"[..], b"untracked"]);
    export_repository(&checkout, &root.join("capture")).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// S4 (#162): a non-bare repository with no index file (a `--no-checkout`
/// clone or linked worktree) refuses `GIT_INVENTORY_INDEX_ABSENT`, never a
/// bare IO (errno 2). Git reads the absent file as an unborn index, which a
/// plain `git checkout` populates; an empty index file does not, so no
/// carried index can restore it. The source's index stays absent.
#[test]
fn s4_a_non_bare_repository_without_an_index_refuses_typed() {
    let root = fresh("no-index");
    let origin = plain_origin(&root);
    let clone = root.join("clone");
    g(
        &root,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            "--no-local",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let linked = root.join("linked");
    g(
        &origin,
        &[
            "worktree",
            "add",
            "--quiet",
            "--no-checkout",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let absent = Some(BulkloadRefusal::GitInventoryIndexAbsent);
    for checkout in [&clone, &linked] {
        let index = text(git(checkout).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index",
        ]))
        .unwrap();
        assert!(!Path::new(&index).exists(), "{index}");
        assert_eq!(refusal(capture_key_parts(checkout)), absent);
        assert_eq!(
            refusal(export_repository(
                checkout,
                &root.join(format!(
                    "capture-{}",
                    checkout.file_name().unwrap().to_str().unwrap()
                ))
            )),
            absent
        );
        assert!(!Path::new(&index).exists(), "{index}");
    }
    fs::remove_dir_all(root).unwrap();
}

/// S4 (#162): a bare capture, plain or a shallow envelope, says so in its
/// bundle headers, and every verb that lays down a workspace, an index or a
/// payload attachment from a capture refuses it `GIT_BARE_CAPTURE_WORKSPACE`
/// before writing anything. An import still carries its refs.
#[test]
fn s4_a_bare_capture_never_lays_down_a_workspace() {
    let root = fresh("bare-workspace");
    let origin = plain_origin(&root);
    let workspace = Some(BulkloadRefusal::GitBareCaptureWorkspace);
    for (name, depth) in [("mirror", None), ("shallow", Some("1"))] {
        let source = root.join(format!("{name}.git"));
        let url = format!("file://{}", origin.display());
        let mut clone = vec!["clone", "--quiet", "--bare"];
        if let Some(depth) = depth {
            clone.extend(["--depth", depth]);
        }
        clone.extend([url.as_str(), source.to_str().unwrap()]);
        g(&root, &clone);
        let bundle = export_repository(&source, &root.join(format!("capture-{name}"))).unwrap();
        let heads = text(git(&root).args(["bundle", "list-heads"]).arg(&bundle)).unwrap();
        // A shallow capture is an envelope: the marker is lifted into its
        // headers beside the custody ref.
        let marker = if depth.is_some() {
            super::shallow::BARE_MARKER.to_owned()
        } else {
            format!("refs/carry-export/{}", super::BARE_METADATA)
        };
        assert!(
            heads
                .lines()
                .any(|line| line.ends_with(&format!(" {marker}"))),
            "{name}: {heads}"
        );
        assert!(bare_marked(&heads), "{name}: {heads}");
        let restored = root.join(format!("restored-{name}"));
        assert_eq!(
            refusal(restore_bundle(&bundle, &restored, "neo")),
            workspace
        );
        assert!(!restored.exists(), "{name}");
        let repository = root.join(format!("repository-{name}"));
        fs::create_dir(&repository).unwrap();
        g(&repository, &["init", "--quiet", "--template="]);
        let linked = root.join(format!("linked-{name}"));
        assert_eq!(
            refusal(restore_linked(&bundle, &repository, &linked, "neo")),
            workspace
        );
        assert!(!linked.exists(), "{name}");
        let receipt = root.join(format!("receipt-{name}"));
        assert_eq!(
            refusal(repair_missing_index(&bundle, &repository, "neo", &receipt)),
            workspace
        );
        let payload = root.join(format!("payload-{name}"));
        fs::create_dir(&payload).unwrap();
        assert_eq!(
            refusal(attach_matching_payload(
                &bundle,
                &repository,
                &payload,
                "neo",
                &receipt
            )),
            workspace
        );
        assert_eq!(
            refusal(registered::restore(
                &bundle,
                &repository,
                &linked,
                &repository.join(".git/worktrees/absent"),
                "neo",
                &receipt
            )),
            workspace
        );
        assert!(
            !receipt.exists() && !payload.join(".git").exists(),
            "{name}"
        );
        assert!(
            text(git(&repository).args(["for-each-ref"]))
                .unwrap()
                .is_empty(),
            "{name}: nothing was imported"
        );
        let imported = root.join(format!("imported-{name}"));
        fs::create_dir(&imported).unwrap();
        g(&imported, &["init", "--quiet", "--template=", "-b", "main"]);
        assert!(
            import_bundle(&imported, &bundle, "neo").unwrap() > 0,
            "{name}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
