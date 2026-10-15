//! S4 refusal sweep (2026-10-08): each refusal names the fact its site knows.
//!
//! - #219: apply of a never-captured item refuses `CAPTURE_ABSENT`, not
//!   `SEALED_OBJECT_MISSING`; alternates a capture can follow are followed
//!   (the landing borrows nothing and `fsck --full` is clean), and a chain it
//!   cannot follow refuses `GIT_SOURCE_ALTERNATES` at capture.
//! - #220: a full TMPDIR under a capture refuses `SPACE_EXHAUSTED` naming it.
//! - #162: a bare repository whose HEAD is unborn captures as ref custody.
//! - #183: a refs import into a missing repository, or a directory that
//!   holds none, refuses `GIT_REPOSITORY_NOT_AT_PATH` item by item, not
//!   `GIT_INVENTORY_MALFORMED` (nor `GIT_DESTINATION_OCCUPIED` on a plan
//!   base), and never aborts the apply.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use super::*;
use std::os::unix::ffi::OsStrExt as _;
use std::process::Command;

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("bulkload-s4-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git_out(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "user.name=Bulkload test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

// A non-bare repository at `path` with one commit of `content`.
fn committed(path: &Path, content: &[u8]) -> String {
    fs::create_dir_all(path).unwrap();
    git_out(path, &["init", "-q", "--template="]);
    fs::write(path.join("file"), content).unwrap();
    git_out(path, &["add", "file"]);
    git_out(path, &["commit", "-q", "-m", "commit"]);
    git_out(path, &["rev-parse", "HEAD"])
}

fn bare_destination(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git_out(path, &["init", "-q", "--bare", "--template="]);
}

type Rows = Vec<(&'static str, Option<String>)>;

fn capture_rows(plan: &Path, state: &Path, corpus: &Path) -> (bool, Rows) {
    let rows = Mutex::new(Vec::new());
    let ok = capture(plan, state, corpus, 1, &|row| {
        rows.lock().unwrap().push((row.outcome, row.reason.clone()));
        Ok(())
    })
    .is_ok();
    (ok, rows.into_inner().unwrap())
}

fn apply_rows(plan: &Path, corpus: &Path, state: &Path) -> (bool, Rows) {
    let rows = Mutex::new(Vec::new());
    let ok = apply(plan, corpus, state, "neo", 1, &|row| {
        rows.lock().unwrap().push((row.outcome, row.reason.clone()));
        Ok(())
    })
    .is_ok();
    (ok, rows.into_inner().unwrap())
}

// Every regular file and directory under `root` with its mtime and ctime at
// nanosecond resolution: the S2 census a capture must leave unchanged.
fn stat_census(root: &Path) -> Vec<(PathBuf, i64, i64, i64, i64)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        out.push((
            path.clone(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ));
        if meta.is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
    }
    out.sort();
    out
}

fn fsck_clean(repository: &Path) {
    git_out(repository, &["fsck", "--full", "--strict", "--no-progress"]);
    let objects = PathBuf::from(git_out(
        repository,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    ));
    assert!(
        !objects.join("info/alternates").exists(),
        "{}: the landing borrows nothing",
        repository.display()
    );
}

// #219, part 1: an item whose capture refused has no record; apply names
// that (`CAPTURE_ABSENT`) before it reads anything else, and writes nothing.
// Mutation checked: apply_item refusing `SEALED_OBJECT_MISSING` again fails
// this test.
#[test]
fn apply_of_a_never_captured_item_refuses_capture_absent() {
    let root = Root::new("never-captured");
    let origin = root.join("origin");
    committed(&origin, b"origin");
    git_out(&origin, &["config", "uploadpack.allowFilter", "true"]);
    let clone = root.join("clone");
    git_out(
        &root.0,
        &[
            "clone",
            "-q",
            "--template=",
            "--no-checkout",
            "--filter=blob:none",
            &format!("file://{}", origin.display()),
            clone.to_str().unwrap(),
        ],
    );
    let destination = root.join("destination.git");
    bare_destination(&destination);
    let plan = root.join("plan");
    add(&plan, &clone, &destination, None).unwrap();
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(!ok);
    assert_eq!(
        rows,
        vec![("refused", Some("GIT_SOURCE_PARTIAL_CLONE".to_owned()))]
    );
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(!ok);
    assert_eq!(rows, vec![("refused", Some("CAPTURE_ABSENT".to_owned()))]);
    assert!(
        git_out(&destination, &["for-each-ref"]).is_empty(),
        "nothing was imported"
    );
}

// #219, part 2: a source whose objects sit wholly or partly in an alternate,
// through a chain and through a relative entry, captures; its landing (a
// refs import, and a standalone checkout) is `fsck --full` clean and has no
// alternates file. The bundle follows the alternates: it packs every
// reachable object wherever the source stores it. Mutation checked: packing
// the capture with `pack-objects --local` (objects in alternates skipped)
// fails this test at capture.
#[test]
fn alternates_are_followed_and_the_landing_borrows_nothing() {
    let root = Root::new("alternates");
    let origin = root.join("origin");
    let base = committed(&origin, b"borrowed");
    // Wholly borrowed: a bare shared clone holds no object of its own.
    let wholly = root.join("wholly.git");
    git_out(
        &root.0,
        &[
            "clone",
            "-q",
            "--bare",
            "--shared",
            "--template=",
            origin.to_str().unwrap(),
            wholly.to_str().unwrap(),
        ],
    );
    // Partly borrowed: a branch of its own, fetched into its own store.
    let other = root.join("other");
    let own = committed(&other, b"own objects");
    git_out(
        &wholly,
        &[
            "fetch",
            "-q",
            other.to_str().unwrap(),
            "main:refs/heads/own",
        ],
    );
    // Two levels, the second through a relative entry.
    let chained = root.join("chained.git");
    git_out(
        &root.0,
        &[
            "init",
            "-q",
            "--bare",
            "--template=",
            chained.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(chained.join("objects/info")).unwrap();
    fs::write(
        chained.join("objects/info/alternates"),
        b"../../wholly.git/objects\n",
    )
    .unwrap();
    git_out(&chained, &["update-ref", "refs/heads/main", &base]);
    git_out(&chained, &["update-ref", "refs/heads/own", &own]);
    // A checkout whose objects are all borrowed.
    let work = root.join("work");
    git_out(
        &root.0,
        &[
            "clone",
            "-q",
            "--shared",
            "--template=",
            wholly.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    let plan = root.join("plan");
    let mut destinations = Vec::new();
    for (name, source) in [("wholly", &wholly), ("chained", &chained)] {
        let destination = root.join(&format!("{name}-destination.git"));
        bare_destination(&destination);
        add(&plan, source, &destination, None).unwrap();
        destinations.push(destination);
    }
    let landed = root.join("work-landed");
    add(&plan, &work, &landed, Some(&landed)).unwrap();
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(ok, "{rows:?}");
    for destination in &destinations {
        fsck_clean(destination);
        let refs = git_out(
            destination,
            &["for-each-ref", "--format=%(objectname)", "refs/carry/v1/"],
        );
        for tip in [&base, &own] {
            assert!(refs.lines().any(|line| line == tip), "{refs}");
        }
    }
    fsck_clean(&landed);
    assert_eq!(git_out(&landed, &["rev-parse", "HEAD"]), base);
    assert_eq!(fs::read(landed.join("file")).unwrap(), b"borrowed");
}

// `count` bare repositories, each borrowing the previous one's store by an
// absolute alternates entry; the first holds the commit. Returns them in
// order, so the last borrows through the longest chain.
fn alternates_chain(root: &Root, count: usize) -> (Vec<PathBuf>, String) {
    let seed = root.join("seed");
    let head = committed(&seed, b"deep");
    let mut chain = Vec::new();
    let mut previous = PathBuf::from(git_out(
        &seed,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    ));
    for level in 0..count {
        let store = root.join(&format!("level-{level}.git"));
        git_out(
            &root.0,
            &[
                "init",
                "-q",
                "--bare",
                "--template=",
                store.to_str().unwrap(),
            ],
        );
        fs::create_dir_all(store.join("objects/info")).unwrap();
        let mut entry = previous.as_os_str().as_bytes().to_vec();
        entry.push(b'\n');
        fs::write(store.join("objects/info/alternates"), entry).unwrap();
        previous = store.join("objects");
        chain.push(store);
    }
    (chain, head)
}

// #219, part 2: Git reads alternates files at most six levels down, and the
// capture's private repository borrows the source's store, one level more
// than the source. A source whose commit lies six stores down (it reads it)
// refuses `GIT_SOURCE_ALTERNATES` at capture, naming the alternates file the
// capture cannot follow; the source one level up (five stores down, the
// deepest the capture follows) captures, and its landing is fsck clean.
// Mutations checked: the depth bound at 6 (the deep source then fails at
// export, untyped `GIT_CHILD_FAILED`) and at 4 (the boundary source refuses)
// each fail this test; so does the old `GIT_INVENTORY_MALFORMED`.
#[test]
fn an_alternates_chain_deeper_than_capture_reads_refuses_typed() {
    let root = Root::new("deep-alternates");
    let (chain, head) = alternates_chain(&root, 6);
    let (deep, boundary) = (&chain[5], &chain[4]);
    for source in [deep, boundary] {
        git_out(source, &["update-ref", "refs/heads/main", &head]);
    }
    let corpus = root.join("corpus");
    let deep_plan = root.join("deep-plan");
    let deep_destination = root.join("deep-destination.git");
    bare_destination(&deep_destination);
    add(&deep_plan, deep, &deep_destination, None).unwrap();
    let (ok, rows) = capture_rows(&deep_plan, &root.join("deep-state"), &corpus);
    assert!(!ok);
    let unfollowable = chain[0].join("objects/info/alternates");
    assert_eq!(
        rows,
        vec![(
            "refused",
            Some(
                BulkloadRefusal::GitSourceAlternates(unfollowable.as_os_str().as_bytes().to_vec())
                    .to_string()
            )
        )]
    );
    let item = id(inspect(&deep_plan).unwrap().first().unwrap()).unwrap();
    assert!(!corpus.join(format!("{item}.capture")).exists());

    let plan = root.join("boundary-plan");
    let destination = root.join("boundary-destination.git");
    bare_destination(&destination);
    add(&plan, boundary, &destination, None).unwrap();
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(ok, "{rows:?}");
    fsck_clean(&destination);
}

// #162 (and #219's landing): a bare repository whose HEAD names a branch
// that does not exist is valid. It captures as ref custody, with no
// `refs/carry-export/head`, imports its refs, and leaves the source's stat
// census unchanged (S2). An empty bare repository (no ref at all) does too.
// Mutation checked: `read_authority` resolving HEAD with `rev-parse --verify
// HEAD` again refuses both items `GIT_CHILD_FAILED stderr_class=other` and
// fails this test.
#[test]
fn a_bare_repository_with_an_unborn_head_captures_refs_only() {
    let root = Root::new("unborn");
    let work = root.join("work");
    let tip = committed(&work, b"branch");
    let unborn = root.join("unborn.git");
    git_out(
        &root.0,
        &[
            "init",
            "-q",
            "--bare",
            "--template=",
            unborn.to_str().unwrap(),
        ],
    );
    git_out(
        &work,
        &[
            "push",
            "-q",
            unborn.to_str().unwrap(),
            "HEAD:refs/heads/other",
        ],
    );
    git_out(&unborn, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let empty = root.join("empty.git");
    git_out(
        &root.0,
        &[
            "init",
            "-q",
            "--bare",
            "--template=",
            empty.to_str().unwrap(),
        ],
    );
    let before = (stat_census(&unborn), stat_census(&empty));
    let plan = root.join("plan");
    let mut destinations = Vec::new();
    for (name, source) in [("unborn", &unborn), ("empty", &empty)] {
        let destination = root.join(&format!("{name}-destination.git"));
        bare_destination(&destination);
        add(&plan, source, &destination, None).unwrap();
        destinations.push(destination);
    }
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    assert_eq!(rows, vec![("captured", None), ("captured", None)]);
    assert_eq!(
        (stat_census(&unborn), stat_census(&empty)),
        before,
        "the capture wrote nothing under either source (S2)"
    );
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(ok, "{rows:?}");
    assert_eq!(rows, vec![("refs-imported", None), ("refs-imported", None)]);
    let imported = git_out(
        &destinations[0],
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/carry/v1/",
        ],
    );
    assert!(
        imported
            .lines()
            .any(|line| line.starts_with(&tip) && line.ends_with("/refs/heads/other")),
        "{imported}"
    );
    assert!(
        !imported.lines().any(|line| line.ends_with("/head")),
        "an unborn HEAD exports no head ref: {imported}"
    );
    for destination in &destinations {
        fsck_clean(destination);
    }
}

// #220: a capture whose bulkload temporary under TMPDIR meets ENOSPC (as a
// full TMPDIR fails it) refuses `SPACE_EXHAUSTED` naming TMPDIR, as its own
// receipt, with no capture record. Before the fix it refused a bare `IO`
// with neither errno nor path. Mutation checked: `PrivateDir::create`
// answering `Io(None)` again fails this test; so does dropping the
// ENOSPC/EDQUOT arm of `refuse::io_in` (it then refuses `IO (errno 28)`).
#[test]
fn a_full_tmpdir_refuses_space_exhausted_naming_it() {
    let root = Root::new("full-tmpdir");
    let source = root.join("source");
    committed(&source, b"payload");
    git_carry::mid_pass::arm_at(&source, git_carry::mid_pass::Stage::IndexRead, || {
        git_carry::scratch_fault::arm(libc::ENOSPC);
    });
    let destination = root.join("destination");
    let plan = root.join("plan");
    add(&plan, &source, &destination, Some(&destination)).unwrap();
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(!ok);
    let full =
        BulkloadRefusal::SpaceExhausted(Some(std::env::temp_dir().as_os_str().as_bytes().to_vec()));
    assert_eq!(rows, vec![("refused", Some(full.to_string()))]);
    let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
    assert!(!corpus.join(format!("{item}.capture")).exists());
    let record =
        crate::outcome::read_record(&root.join("state").join(format!("{item}.outcome"))).unwrap();
    assert_eq!(
        record.outcome.refusal().map(crate::outcome::Refusal::code),
        Some("SPACE_EXHAUSTED"),
        "the durable record is typed"
    );
}

// #220: the private copy of the source index, written under TMPDIR, meets
// ENOSPC after its directory was made (a full disk still makes directories):
// the capture refuses `SPACE_EXHAUSTED` naming TMPDIR, as its own receipt.
// Mutation checked: that write through `refuse_at` again refuses
// `IO (errno 28)` and fails this test.
#[test]
fn a_full_tmpdir_at_the_index_copy_refuses_space_exhausted_naming_it() {
    let root = Root::new("full-tmpdir-index");
    let source = root.join("source");
    committed(&source, b"payload");
    git_carry::mid_pass::arm_at(&source, git_carry::mid_pass::Stage::IndexRead, || {
        git_carry::scratch_fault::arm_at(git_carry::scratch_fault::Site::IndexWrite, libc::EDQUOT);
    });
    let destination = root.join("destination");
    let plan = root.join("plan");
    add(&plan, &source, &destination, Some(&destination)).unwrap();
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(!ok);
    let full =
        BulkloadRefusal::SpaceExhausted(Some(std::env::temp_dir().as_os_str().as_bytes().to_vec()));
    assert_eq!(rows, vec![("refused", Some(full.to_string()))]);
}

// #220: a bundle stage that fills its filesystem, at its create (inodes,
// directory blocks) or at a write, names the directory its private
// directory was made in (the bundle's own, the corpus for an apply), which
// outlives the refusal; never the private directory, which the refusal
// removes. A private directory no parent accepts names the parent that ran
// out of space even when a later parent then fails otherwise. Any other
// errno stays `IO` with it. Mutations checked: naming
// `destination.parent()` again, `refuse_at` on the stage's create, and the
// last parent's refusal winning in `PrivateDir::create` each fail this test.
#[test]
fn a_full_stage_names_the_directory_that_outlives_it() {
    use git_carry::scratch_fault::{arm_at, Site};
    let root = Root::new("full-stage");
    let bundle = root.join("capture.bundle");
    fs::write(&bundle, b"not read past the copy").unwrap();
    let named = BulkloadRefusal::SpaceExhausted(Some(root.0.as_os_str().as_bytes().to_vec()));
    for site in [Site::StageCreate, Site::StageWrite] {
        arm_at(site, libc::ENOSPC);
        assert_eq!(
            git_carry::stage_bundle(&bundle).err(),
            Some(named.clone()),
            "{site:?}"
        );
        arm_at(site, libc::EACCES);
        assert_eq!(
            git_carry::stage_bundle(&bundle).err(),
            Some(BulkloadRefusal::Io(Some(libc::EACCES))),
            "{site:?}"
        );
    }
    // The corpus is full; TMPDIR, tried next, refuses access.
    arm_at(Site::Mkdir, libc::ENOSPC);
    arm_at(Site::Mkdir, libc::EACCES);
    assert_eq!(git_carry::stage_bundle(&bundle).err(), Some(named));
    let left: Vec<_> = fs::read_dir(&root.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, vec![std::ffi::OsString::from("capture.bundle")]);
}

// #183, item 1: a refs import whose destination repository does not exist,
// or is a directory that holds no repository (an empty directory, a partial
// cleanup), refuses `GIT_REPOSITORY_NOT_AT_PATH` (it used to read the failed
// `bundle verify` as `GIT_INVENTORY_MALFORMED`), item by item, and creates
// nothing there. An existing non-repository used to abort the whole apply
// from its grouping loop (GIT_CHILD_FAILED, no item record at all).
// Mutations checked: dropping the guard in `import_verified`, reducing it to
// a directory check, or `?` again in apply's grouping loop each fail this
// test.
#[test]
fn a_refs_import_into_a_missing_repository_refuses_by_name() {
    let root = Root::new("missing-destination");
    let plan = root.join("plan");
    let missing = root.join("missing.git");
    let empty = root.join("empty.git");
    fs::create_dir(&empty).unwrap();
    let cleaned = root.join("cleaned.git");
    fs::create_dir_all(cleaned.join("objects")).unwrap();
    fs::write(cleaned.join("config"), b"left behind\n").unwrap();
    for (name, destination) in [("a", &missing), ("b", &empty), ("c", &cleaned)] {
        let source = root.join(&format!("source-{name}"));
        committed(&source, name.as_bytes());
        add(&plan, &source, destination, None).unwrap();
    }
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(!ok);
    assert_eq!(
        rows,
        vec![("refused", Some("GIT_REPOSITORY_NOT_AT_PATH".to_owned())); 3]
    );
    assert!(!missing.exists(), "nothing is created");
    assert_eq!(
        fs::read_dir(&empty).unwrap().count(),
        0,
        "nothing is created"
    );
    assert_eq!(
        fs::read_dir(&cleaned).unwrap().count(),
        2,
        "nothing is created"
    );
}

// Two checkouts of one source repository (main, and a `wt` worktree), so a
// capture pass puts both on one plan base. Each item restores into its own
// target; `standalone` makes main's item its own repository.
fn grouped_on_a_base(root: &Root, standalone: bool) -> (PathBuf, PathBuf, Vec<PathBuf>) {
    let source = root.join("source");
    committed(&source, b"base");
    let wt = root.join("wt");
    git_out(
        &source,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    let repository = root.join("repository");
    fs::create_dir(&repository).unwrap();
    git_out(&repository, &["init", "-q", "--template="]);
    let plan = root.join("plan");
    let main_target = root.join("target-main");
    let wt_target = root.join("target-wt");
    let main_repository = if standalone {
        &main_target
    } else {
        &repository
    };
    add(&plan, &source, main_repository, Some(&main_target)).unwrap();
    add(&plan, &wt, &repository, Some(&wt_target)).unwrap();
    (plan, repository, vec![main_target, wt_target])
}

// The staged bundle of each planned item's capture requires the plan base.
fn on_a_base(plan: &Path, corpus: &Path) {
    for item in inspect(plan).unwrap() {
        let record: Capture =
            read(&corpus.join(format!("{}.capture", id(&item).unwrap()))).unwrap();
        let staged = git_carry::stage_bundle(&corpus.join(&record.bundle)).unwrap();
        assert!(
            git_carry::shared::requires_base(staged.path()).unwrap(),
            "{} is captured on the plan base",
            item.source.display()
        );
    }
}

// #183, item 1, on a plan base (`import_base`): a capture whose bundle
// requires the plan base, applied into a destination repository that does
// not exist, refuses `GIT_REPOSITORY_NOT_AT_PATH` (it used to refuse
// `GIT_DESTINATION_OCCUPIED`), and creates nothing. A standalone item on the
// base (its workspace is its repository) still refuses
// `GIT_DESTINATION_OCCUPIED`: a standalone destination is never preseeded.
// Mutation checked: the old combined guard (missing or standalone, one
// `GIT_DESTINATION_OCCUPIED`) fails this test.
#[test]
fn a_plan_base_import_into_a_missing_repository_refuses_by_name() {
    let root = Root::new("missing-base-destination");
    let (plan, repository, targets) = grouped_on_a_base(&root, false);
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    on_a_base(&plan, &corpus);
    fs::remove_dir_all(&repository).unwrap();
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(!ok);
    assert_eq!(
        rows,
        vec![("refused", Some("GIT_REPOSITORY_NOT_AT_PATH".to_owned())); 2]
    );
    assert!(!repository.exists(), "nothing is created");
    for target in &targets {
        assert!(!target.exists(), "nothing is restored");
    }

    let root = Root::new("standalone-on-a-base");
    let (plan, _, targets) = grouped_on_a_base(&root, true);
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    on_a_base(&plan, &corpus);
    let (_, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(
        rows.contains(&("refused", Some("GIT_DESTINATION_OCCUPIED".to_owned()))),
        "{rows:?}"
    );
    assert!(!targets[0].exists(), "the standalone is not laid down");
}

// #162: the unborn-HEAD exemption is bare-only. A non-bare checkout whose
// symbolic HEAD names a branch that does not exist still refuses
// `GIT_CHILD_FAILED` and records no capture: a repository with a working
// tree and no HEAD commit has nothing its workspace could be restored from.
// Mutation checked: dropping `bare_root` from `carried_head`'s exemption
// captures it with no head and fails this test.
#[test]
fn a_non_bare_repository_with_an_unborn_head_still_refuses() {
    let root = Root::new("unborn-non-bare");
    let source = root.join("source");
    committed(&source, b"payload");
    git_out(&source, &["symbolic-ref", "HEAD", "refs/heads/gone"]);
    let destination = root.join("destination.git");
    bare_destination(&destination);
    let plan = root.join("plan");
    add(&plan, &source, &destination, None).unwrap();
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(!ok);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "refused");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_some_and(|reason| reason.starts_with("GIT_CHILD_FAILED")),
        "{rows:?}"
    );
    let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
    assert!(!corpus.join(format!("{item}.capture")).exists());
}

// A bare repository at `path` holding `tip` as `main`, whose alternates file
// lists `entries`, one per line.
fn borrowing(path: &Path, tip: Option<&str>, entries: &[&Path]) {
    git_out(
        path.parent().unwrap(),
        &[
            "init",
            "-q",
            "--bare",
            "--template=",
            path.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(path.join("objects/info")).unwrap();
    let mut file = Vec::new();
    for entry in entries {
        file.extend_from_slice(entry.as_os_str().as_bytes());
        file.push(b'\n');
    }
    fs::write(path.join("objects/info/alternates"), file).unwrap();
    if let Some(tip) = tip {
        git_out(path, &["update-ref", "refs/heads/main", tip]);
    }
}

// #219: Git links each store once (by its real path), so an alternates entry
// naming the source's own store, a cycle between two stores, and a diamond
// whose long arm reaches an already-linked store five levels down all cost
// no depth: Git reads them, and so does the capture. Each captures, and its
// landing is self-contained and fsck clean. Mutation checked: the walk
// without its linked set refuses all three `GIT_SOURCE_ALTERNATES`.
#[test]
fn self_cyclic_and_diamond_alternates_capture_as_git_reads_them() {
    let root = Root::new("cyclic-alternates");
    let seed = root.join("seed");
    let tip = committed(&seed, b"shared");
    let seed_objects = seed.join(".git/objects");
    // Self: the store lists itself, then the seed.
    let own = root.join("self.git");
    borrowing(&own, None, &[]);
    fs::write(
        own.join("objects/info/alternates"),
        [
            own.join("objects").as_os_str().as_bytes(),
            b"\n",
            seed_objects.as_os_str().as_bytes(),
            b"\n",
        ]
        .concat(),
    )
    .unwrap();
    git_out(&own, &["update-ref", "refs/heads/main", &tip]);
    // Cycle: A borrows B, B borrows A and the seed.
    let a = root.join("a.git");
    let b = root.join("b.git");
    borrowing(&a, None, &[&b.join("objects")]);
    borrowing(&b, None, &[&a.join("objects"), &seed_objects]);
    git_out(&a, &["update-ref", "refs/heads/main", &tip]);
    // Diamond: S borrows [B2, A1]; B2 borrows X, X borrows the seed; A1..A4
    // chain down to X, which the long arm reaches five stores below S.
    let x = root.join("x.git");
    borrowing(&x, None, &[&seed_objects]);
    let b2 = root.join("b2.git");
    borrowing(&b2, None, &[&x.join("objects")]);
    let mut below = x.join("objects");
    for level in (1..=4).rev() {
        let arm = root.join(&format!("a{level}.git"));
        borrowing(&arm, None, &[&below]);
        below = arm.join("objects");
    }
    let diamond = root.join("diamond.git");
    borrowing(&diamond, Some(&tip), &[&b2.join("objects"), &below]);
    let plan = root.join("plan");
    let mut destinations = Vec::new();
    for (name, source) in [("self", &own), ("cycle", &a), ("diamond", &diamond)] {
        assert!(!git_carry::partial_clone(source).unwrap(), "{name}");
        let destination = root.join(&format!("{name}-destination.git"));
        bare_destination(&destination);
        add(&plan, source, &destination, None).unwrap();
        destinations.push(destination);
    }
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(ok, "{rows:?}");
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(ok, "{rows:?}");
    for destination in &destinations {
        fsck_clean(destination);
    }
}

// #219: a `clone --shared` whose lender was then moved away. Git skips the
// missing store and the export misses the objects it held; the capture
// names the alternates file whose entry it cannot open
// (`GIT_SOURCE_ALTERNATES`), not `GIT_CHILD_FAILED stderr_class=other` (all
// Git says is "unable to normalize alternate object path").
// A stale entry for objects nobody needs still captures. Mutation checked:
// `capture_refusal` without the missing-alternate attribution fails this
// test.
#[test]
fn a_missing_lender_refuses_naming_its_alternates_file() {
    let root = Root::new("missing-lender");
    let origin = root.join("origin");
    committed(&origin, b"lent");
    let work = root.join("work.git");
    git_out(
        &root.0,
        &[
            "clone",
            "-q",
            "--bare",
            "--shared",
            "--template=",
            origin.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    fs::rename(&origin, root.join("origin-moved")).unwrap();
    // Stale: a lender that is gone, while every object is its own.
    let stale_seed = root.join("stale-seed");
    let stale_tip = committed(&stale_seed, b"own");
    let stale = root.join("stale.git");
    borrowing(&stale, None, &[&root.join("gone/objects")]);
    git_out(
        &stale,
        &[
            "fetch",
            "-q",
            stale_seed.to_str().unwrap(),
            "main:refs/heads/main",
        ],
    );
    assert_eq!(git_out(&stale, &["rev-parse", "main"]), stale_tip);
    let plan = root.join("plan");
    for (name, source) in [("work", &work), ("stale", &stale)] {
        let destination = root.join(&format!("{name}-destination.git"));
        bare_destination(&destination);
        add(&plan, source, &destination, None).unwrap();
    }
    let corpus = root.join("corpus");
    let (ok, rows) = capture_rows(&plan, &root.join("state"), &corpus);
    assert!(!ok);
    let unopenable = work.join("objects/info/alternates");
    assert_eq!(
        rows,
        vec![
            (
                "refused",
                Some(
                    BulkloadRefusal::GitSourceAlternates(
                        unopenable.as_os_str().as_bytes().to_vec()
                    )
                    .to_string()
                )
            ),
            ("captured", None),
        ]
    );
}

// #219 (S5): with alternates followed, a lender's `gc` under the pass is a
// rewrite of what the capture reads. A `clone --shared` whose lender drops
// and prunes an object the source still names makes the export's child fail
// while the lender's pack listing moves: drift custody, as for a rewrite of
// the source's own store, and the next pass (with the source's ref fixed)
// captures. Mutation checked: the pack listing of the source's own store
// alone refuses `GIT_CHILD_FAILED stderr_class=bad_object`.
#[test]
fn a_lender_rewrite_under_the_pass_is_drift_custody() {
    let root = Root::new("lender-rewrite");
    let lender = root.join("lender");
    committed(&lender, b"lent");
    git_out(&lender, &["checkout", "-q", "-b", "doomed"]);
    fs::write(lender.join("doomed-only"), b"reachable only from doomed").unwrap();
    git_out(&lender, &["add", "doomed-only"]);
    git_out(&lender, &["commit", "-q", "-m", "doomed"]);
    git_out(&lender, &["checkout", "-q", "main"]);
    let source = root.join("source.git");
    git_out(
        &root.0,
        &[
            "clone",
            "-q",
            "--bare",
            "--shared",
            "--template=",
            lender.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    let destination = root.join("destination.git");
    bare_destination(&destination);
    let plan = root.join("plan");
    add(&plan, &source, &destination, None).unwrap();
    let inside = fs::canonicalize(&lender).unwrap();
    git_carry::mid_pass::arm(&source, move || {
        git_out(&inside, &["branch", "-q", "-D", "doomed"]);
        git_out(&inside, &["reflog", "expire", "--expire=now", "--all"]);
        git_out(&inside, &["repack", "-a", "-d", "-q"]);
        git_out(&inside, &["prune", "--expire=now"]);
    });
    let corpus = root.join("corpus");
    let state = root.join("state");
    let (ok, rows) = capture_rows(&plan, &state, &corpus);
    assert!(ok, "{rows:?}");
    assert_eq!(
        rows,
        vec![("deferred-with-drift", Some("drift=1".to_owned()))]
    );
    let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
    assert!(!corpus.join(format!("{item}.capture")).exists());
    git_out(&source, &["update-ref", "-d", "refs/heads/doomed"]);
    let (ok, rows) = capture_rows(&plan, &state, &corpus);
    assert!(ok, "{rows:?}");
    assert_eq!(rows, vec![("captured", None)]);
}

// #220: a Git child out of space names no directory (its stderr is never
// carried, R-N121), but its verb knows where its children write. Apply's
// children write into the destination and its corpus stages, which apply's
// preflight charges: `DESTINATION_SPACE_INSUFFICIENT`. A capture's write
// only under PRIVATE_STATE, which nothing charges: `SPACE_EXHAUSTED` naming
// it. Each verb is driven end to end, with its next Git child failing as one
// out of space (`scratch_fault::Site::Child`, armed from a mid-pass hook on
// the item's own thread). A refusal that already names its directory keeps
// it, and every other refusal passes through. Mutation checked: either verb
// passing the unnamed refusal through fails this test.
#[test]
fn a_child_out_of_space_is_named_by_its_verb() {
    use git_carry::scratch_fault::{arm_at, Site};
    let root = Root::new("child-space");
    let source = root.join("source");
    committed(&source, b"payload");
    let plan = root.join("plan");
    let destination = root.join("destination.git");
    bare_destination(&destination);
    add(&plan, &source, &destination, None).unwrap();
    let state = root.join("state");
    let corpus = root.join("corpus");
    git_carry::mid_pass::arm_at(&source, git_carry::mid_pass::Stage::IndexRead, || {
        arm_at(Site::Child, 0);
    });
    let (ok, rows) = capture_rows(&plan, &state, &corpus);
    assert!(!ok);
    let named = BulkloadRefusal::SpaceExhausted(Some(state.as_os_str().as_bytes().to_vec()));
    assert_eq!(rows, vec![("refused", Some(named.to_string()))]);
    let (ok, rows) = capture_rows(&plan, &state, &corpus);
    assert!(ok, "{rows:?}");
    let record: Capture = read(&corpus.join(format!(
        "{}.capture",
        id(inspect(&plan).unwrap().first().unwrap()).unwrap()
    )))
    .unwrap();
    git_carry::mid_pass::arm_at(
        &corpus.join(&record.bundle),
        git_carry::mid_pass::Stage::BundleChecked,
        || arm_at(Site::Child, 0),
    );
    let (ok, rows) = apply_rows(&plan, &corpus, &root.join("applied"));
    assert!(!ok);
    assert_eq!(
        rows,
        vec![("refused", Some("DESTINATION_SPACE_INSUFFICIENT".to_owned()))]
    );
    let item = inspect(&plan).unwrap().remove(0);
    let corpus_named = BulkloadRefusal::SpaceExhausted(Some(b"/corpus".to_vec()));
    let other = BulkloadRefusal::GitChildFailed(git_carry::estimate::StderrClass::Other);
    for refusal in [corpus_named, other] {
        assert_eq!(charged_space(refusal.clone()), refusal);
        assert_eq!(capture_refusal(refusal.clone(), &item, &state), refusal);
    }
}
