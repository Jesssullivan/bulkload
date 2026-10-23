//! Unit rows of the hand-off overlay (TIN-6569, OI-1003-Q183; design
//! section 12): the one predicate converge gained, the delta's
//! indifference to it, the settle of a crashed step, the exchange undone
//! for a writer that raced it, and render-then-clear as the identity.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use super::super::super::batch_objects::BatchObjects;
use super::super::{delta, holds, Side};
use super::*;
use crate::test_support::prop_config;
use proptest::prelude::*;

fn git_run(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

const LANDED: &[u8] = b"# Agents\n\nRepository rules.\n";

/// A repository holding `LANDED` as `AGENTS.md`'s blob, and that blob.
fn repo() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&repo).unwrap();
    git_run(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(repo.join("AGENTS.md"), LANDED).unwrap();
    git_run(&repo, &["add", "AGENTS.md"]);
    git_run(&repo, &["commit", "--quiet", "-m", "c0"]);
    let blob = git_run(&repo, &["rev-parse", "HEAD:AGENTS.md"]);
    (dir, repo, blob)
}

fn inputs() -> render::Inputs {
    let mut inputs = render::sample();
    inputs.workspace = b"/estate/main".to_vec();
    inputs
}

fn block() -> Vec<u8> {
    render::render(&inputs(), false)
}

/// The identity of the landing [`block`] is rendered for.
fn ours() -> String {
    inputs().owner()
}

fn side(blob: &str, prefix: Vec<Prefix>) -> Side {
    Side {
        kind: Kind::Regular,
        mode: 0o644,
        blob: Some(blob.to_owned()),
        link: None,
        prefix,
    }
}

fn prefix_of(block: &[u8]) -> Prefix {
    Prefix {
        len: len64(block),
        digest: digest(block),
        trusted: None,
        strip: 0,
    }
}

/// No workspace ledger: nothing is landed content.
fn unlanded() -> Landed<'static> {
    Landed {
        source: "neo",
        instance: "",
        sides: None,
        stripped: Vec::new(),
    }
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn holds_accepts_the_recorded_block_then_the_landed_blob_only() {
    let (_dir, repo, blob) = repo();
    let block = block();
    let mut other = block.clone();
    let at = other.len() / 2;
    other[at] ^= 1;
    let with = side(&blob, vec![prefix_of(&block)]);
    let without = side(&blob, Vec::new());
    let path = repo.join("AGENTS.md");
    let mut objects = BatchObjects::new(&repo).unwrap();
    for (bytes, overlaid, plain, why) in [
        (LANDED.to_vec(), true, true, "the landed blob alone"),
        (
            cat(&[&block, LANDED]),
            true,
            false,
            "the block, then the landed blob",
        ),
        (
            cat(&[&other, LANDED]),
            false,
            false,
            "another block, then the blob",
        ),
        (
            cat(&[&block, b"edited\n"]),
            false,
            false,
            "the block, then other bytes",
        ),
        (block.clone(), false, false, "the block alone"),
        (
            cat(&[&block, LANDED, b"x"]),
            false,
            false,
            "the blob and more",
        ),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            holds(&mut objects, &path, &with).unwrap(),
            overlaid,
            "{why}"
        );
        assert_eq!(
            holds(&mut objects, &path, &without).unwrap(),
            plain,
            "{why}"
        );
    }
    objects.finish().unwrap();
}

/// OI-1003-Q190: a landed blob whose head is a carried block, and a side
/// that records its removal (`handoff.stripped`): the seat holds the blob
/// without it, alone or under this landing's block, and still the whole
/// blob; any other bytes do not hold it. Without the record only the whole
/// blob holds.
#[test]
fn holds_accepts_a_recorded_removal_of_carried_blocks_only() {
    let (_dir, repo, _) = repo();
    let carried = b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef for=fedcba9876543210 -->\n> carried\n<!-- bulkload-handoff:v1 END id=0123456789abcdef -->\n\n".to_vec();
    assert_eq!(
        render::marker(&carried),
        render::Marker::Block(carried.len())
    );
    let path = repo.join("AGENTS.md");
    fs::write(&path, cat(&[&carried, LANDED])).unwrap();
    git_run(&repo, &["commit", "--quiet", "-am", "a carried block"]);
    let blob = git_run(&repo, &["rev-parse", "HEAD:AGENTS.md"]);
    let block = block();
    let strip = len64(&carried);
    let with = side(
        &blob,
        vec![
            prefix_of(&block),
            Prefix {
                strip,
                ..prefix_of(&block)
            },
            Prefix {
                len: 0,
                digest: digest(b""),
                trusted: None,
                strip,
            },
        ],
    );
    let without = side(&blob, vec![prefix_of(&block)]);
    let mut objects = BatchObjects::new(&repo).unwrap();
    for (bytes, stripped, plain, why) in [
        (
            cat(&[&carried, LANDED]),
            true,
            true,
            "the whole landed blob",
        ),
        (
            LANDED.to_vec(),
            true,
            false,
            "the blob without the carried block",
        ),
        (
            cat(&[&block, LANDED]),
            true,
            false,
            "this landing's block on the blob without it",
        ),
        (
            cat(&[&block, &carried, LANDED]),
            true,
            true,
            "this landing's block on the whole blob",
        ),
        (
            cat(&[&carried[1..], LANDED]),
            false,
            false,
            "a part of the carried block removed",
        ),
        (
            cat(&[LANDED, b"x"]),
            false,
            false,
            "the blob without it, and more",
        ),
        (Vec::new(), false, false, "nothing"),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            holds(&mut objects, &path, &with).unwrap(),
            stripped,
            "{why}"
        );
        assert_eq!(
            holds(&mut objects, &path, &without).unwrap(),
            plain,
            "{why}"
        );
    }
    objects.finish().unwrap();
}

#[test]
fn only_the_old_side_carries_a_prefix_and_it_plans_no_write() {
    let old = BTreeMap::from([(
        b"AGENTS.md".to_vec(),
        side(&"a".repeat(40), vec![prefix_of(&block())]),
    )]);
    let new = BTreeMap::from([(b"AGENTS.md".to_vec(), side(&"a".repeat(40), Vec::new()))]);
    assert!(delta(&old, &new).is_empty(), "a prefix is not a change");
    let changed = BTreeMap::from([(b"AGENTS.md".to_vec(), side(&"b".repeat(40), Vec::new()))]);
    let ops = delta(&old, &changed);
    assert_eq!(ops.len(), 1);
    assert!(!ops[0].old.as_ref().unwrap().prefix.is_empty());
    assert!(ops[0].new.as_ref().unwrap().prefix.is_empty());
}

fn record(block: &[u8]) -> HandoffLanded {
    HandoffLanded {
        item: "i".to_owned(),
        capture: [1; 32],
        inputs: [2; 32],
        open: 0,
        as_of_ns: 1,
        recorded_ns: 1,
        fsid: 0,
        options: Stored::default(),
        files: vec![HandoffFile {
            path: b"AGENTS.md".to_vec(),
            kind: FileKind::Prefixed,
            block_len: len64(block),
            block: digest(block),
            print: None,
            base: None,
            carried: false,
        }],
        retired: Vec::new(),
    }
}

#[test]
fn settle_removes_its_own_displaced_file_and_records_the_block() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = fs::canonicalize(dir.path()).unwrap();
    let ledgers = worktree.join("ledgers");
    fs::create_dir(&ledgers).unwrap();
    let block = block();
    let new = cat(&[&block, LANDED]);
    // A crash after the exchange: the target new, the old file displaced.
    fs::write(worktree.join("AGENTS.md"), &new).unwrap();
    fs::write(worktree.join(temp_name("t", 0)), LANDED).unwrap();
    landed::write(
        &ledgers.join(HANDOFF_INTENT),
        &HandoffIntent {
            op: Op::Render,
            tag: "t".to_owned(),
            steps: vec![Step {
                path: b"AGENTS.md".to_vec(),
                old_block: None,
                new_block: Some(block.clone()),
                before: Some(digest(LANDED)),
                after: Some(digest(&new)),
            }],
            next: Some(record(&block)),
            stripped: Vec::new(),
            carried: false,
        },
    )
    .unwrap();
    settle(&worktree, &ledgers, "neo", &ours()).unwrap();
    assert!(
        !worktree.join(temp_name("t", 0)).exists(),
        "ours is removed"
    );
    assert_eq!(fs::read(worktree.join("AGENTS.md")).unwrap(), new);
    assert!(!ledgers.join(HANDOFF_INTENT).exists());
    let written: HandoffLanded = landed::read(&ledgers.join(HANDOFF_LANDED))
        .unwrap()
        .unwrap();
    assert_eq!(written.files.len(), 1);
    assert_eq!(written.files[0].block, digest(&block));
    assert_eq!(written.inputs, [2; 32], "the run's record, complete");
}

#[test]
fn settle_puts_back_a_third_partys_file_a_crash_displaced() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = fs::canonicalize(dir.path()).unwrap();
    let ledgers = worktree.join("ledgers");
    fs::create_dir(&ledgers).unwrap();
    let block = block();
    let new = cat(&[&block, LANDED]);
    let theirs = b"an editor saved this between the read and the exchange\n";
    fs::write(worktree.join("AGENTS.md"), &new).unwrap();
    fs::write(worktree.join(temp_name("t", 0)), theirs).unwrap();
    landed::write(
        &ledgers.join(HANDOFF_INTENT),
        &HandoffIntent {
            op: Op::Render,
            tag: "t".to_owned(),
            steps: vec![Step {
                path: b"AGENTS.md".to_vec(),
                old_block: None,
                new_block: Some(block.clone()),
                before: Some(digest(LANDED)),
                after: Some(digest(&new)),
            }],
            next: Some(record(&block)),
            stripped: Vec::new(),
            carried: false,
        },
    )
    .unwrap();
    settle(&worktree, &ledgers, "neo", &ours()).unwrap();
    assert_eq!(fs::read(worktree.join("AGENTS.md")).unwrap(), theirs);
    assert!(!worktree.join(temp_name("t", 0)).exists());
    assert!(!ledgers.join(HANDOFF_INTENT).exists());
    // No block is in place, so the record names none.
    assert!(!ledgers.join(HANDOFF_LANDED).exists());
}

#[test]
fn the_exchange_is_undone_when_the_target_moved_after_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = fs::canonicalize(dir.path()).unwrap();
    let target = worktree.join("AGENTS.md");
    fs::write(&target, LANDED).unwrap();
    let read = read_whole(&target, "AGENTS.md").unwrap();
    // A writer replaces the file after the plan read it.
    let theirs = b"replaced\n";
    fs::write(worktree.join("other"), theirs).unwrap();
    fs::rename(worktree.join("other"), &target).unwrap();
    let new = cat(&[&block(), LANDED]);
    let refused = exchange(
        &worktree,
        "AGENTS.md",
        &read,
        &new,
        &temp_name("t", 0),
        Op::Render,
    );
    assert_eq!(refused, Err(BulkloadRefusal::HandoffBlockModified));
    assert_eq!(fs::read(&target).unwrap(), theirs);
    assert!(!worktree.join(temp_name("t", 0)).exists());
}

/// A record's print vouches for a block only outside the racy window of
/// the record's `recorded_ns` (as the workspace ledger's does in `prove`):
/// an edit inside the block in the same timestamp tick as the hand-off's
/// write leaves the print unchanged, so a racy print is proved by reading.
#[test]
fn a_racy_record_print_is_proved_by_reading() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = fs::canonicalize(dir.path()).unwrap();
    let target = worktree.join("AGENTS.md");
    let block = block();
    // The bytes on disk: the block with one byte changed inside it, at the
    // same length (what the print cannot see within one tick).
    let mut edited = block.clone();
    let at = edited.iter().position(|byte| *byte == b'>').unwrap();
    edited[at] = b'<';
    fs::write(&target, cat(&[&edited, LANDED])).unwrap();
    let meta = fs::symlink_metadata(&target).unwrap();
    let fsid = landed::fsid(&worktree).unwrap();
    let mut record = record(&block);
    record.fsid = fsid;
    record.files[0].print = SeatPrint::of(b"AGENTS.md", &meta);
    // Recorded in the same tick as the file's stamp: racy, so read.
    record.recorded_ns = record.files[0].print.as_ref().unwrap().mtime_ns + 1;
    assert_eq!(
        held(
            &worktree,
            "AGENTS.md",
            &meta,
            Some(&record),
            fsid,
            &ours(),
            &mut unlanded()
        )
        .unwrap(),
        Held::Modified
    );
    // Recorded well after the stamp: the print is trusted, nothing read.
    record.recorded_ns += 3 * super::super::super::RACY_GRANULARITY_NS;
    assert!(matches!(
        held(
            &worktree,
            "AGENTS.md",
            &meta,
            Some(&record),
            fsid,
            &ours(),
            &mut unlanded()
        )
        .unwrap(),
        Held::Intact(_)
    ));
}

/// How the rerun after a render whose print caught a same-tick edit
/// reaches the record (review of 2026-10-16 and its re-review).
#[derive(Clone, Copy)]
enum Rerun {
    /// An unchanged rerun: the record is current and its racy print is
    /// re-dated (the re-dating path).
    Unchanged,
    /// The created `CLAUDE.md` removed by hand first: the rerun retires it
    /// and keeps the intact `AGENTS.md` block whose print is unchanged (the
    /// keep path).
    Retire,
    /// `--renew` over a current record: the same block, kept (the keep
    /// path).
    Renew,
}

/// How the user's edit inside the render is stamped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    /// As written: in the render's own timestamp window.
    InWindow,
    /// Stamped now explicitly, then the render takes longer than the racy
    /// allowance (C03).
    SlowRender,
    /// Its mtime set an hour back (as `cp -p`, `rsync -t`, `tar x` or
    /// `touch -d` leave it); only its ctime says when it was written (#292
    /// review F1).
    OldMtime,
}

/// A user edit made after the render's exchange and before its print is
/// taken (the test hook) leaves a racy record print of the user's bytes
/// over a `base` the render read. Whatever path the rerun takes past the
/// racy window (`rerun`), it checks the bytes under the block before the
/// print is re-dated, so the print never vouches for the edit, and a
/// converge that replaces the target refuses MODIFIED with the edit kept.
/// `edit`: how the edit is stamped ([`Edit`]). The print stays racy because
/// the record is dated at the render's start, before its write, not after
/// its prints (C03), and is judged by its ctime as well as its mtime (F1).
fn an_edit_after_the_exchange_survives(rerun: Rerun, edit: Edit) {
    use crate::estate::{self, HandoffMode, HandoffOptions};
    // The prefix machinery's fixture: the legacy tracked targets (debug
    // builds only; every hand-off test in this module asks for them).
    std::env::set_var(super::TRACKED_TARGETS_ENV, "1");
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let source = root.join("src");
    fs::create_dir(&source).unwrap();
    git_run(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("AGENTS.md"), LANDED).unwrap();
    fs::write(source.join("a.txt"), b"alpha\n").unwrap();
    git_run(&source, &["add", "-A"]);
    git_run(&source, &["commit", "--quiet", "-m", "c0"]);
    let target = root.join("dst");
    let (plan, corpus, state) = (root.join("plan"), root.join("corpus"), root.join("state"));
    estate::add(&plan, &source, &target, Some(&target)).unwrap();
    let tick = || std::thread::sleep(std::time::Duration::from_millis(3_100));
    let capture = || {
        estate::capture(&plan, &root.join("private"), &corpus, 1, &|_| Ok(())).unwrap();
    };
    let apply = || {
        let rows = std::sync::Mutex::new(Vec::new());
        let _ = estate::apply(&plan, &corpus, &state, "neo", 1, &|row| {
            rows.lock()
                .unwrap()
                .push(row.reason.clone().unwrap_or_default());
            Ok(())
        });
        rows.into_inner().unwrap()
    };
    let handoff = |renew: bool| {
        estate::handoff(
            &plan,
            &corpus,
            &state,
            "neo",
            &HandoffOptions {
                mode: HandoffMode::Render,
                renew,
                ..HandoffOptions::default()
            },
            &mut |_| Ok(()),
        )
        .unwrap();
    };
    tick();
    capture();
    apply();
    let agents = target.join("AGENTS.md");
    let edited = agents.clone();
    super::super::super::mid_pass::arm_at(
        &target,
        super::super::super::mid_pass::Stage::HandoffWritten,
        move || {
            let mut bytes = fs::read(&edited).unwrap();
            bytes.extend_from_slice(b"A local note.\n");
            fs::write(&edited, bytes).unwrap();
            let now = std::time::SystemTime::now();
            match edit {
                Edit::InWindow => {}
                Edit::SlowRender => {
                    fs::File::open(&edited).unwrap().set_modified(now).unwrap();
                    std::thread::sleep(std::time::Duration::from_nanos(
                        u64::try_from(super::super::super::RACY_GRANULARITY_NS).unwrap()
                            + 100_000_000,
                    ));
                }
                Edit::OldMtime => {
                    fs::File::open(&edited)
                        .unwrap()
                        .set_modified(now - std::time::Duration::from_hours(1))
                        .unwrap();
                }
            }
        },
    );
    handoff(false);
    assert!(fs::read(&agents).unwrap().ends_with(b"A local note.\n"));
    tick();
    match rerun {
        Rerun::Unchanged => handoff(false),
        Rerun::Retire => {
            fs::remove_file(target.join("CLAUDE.md")).unwrap();
            handoff(false);
        }
        Rerun::Renew => handoff(true),
    }
    fs::write(source.join("AGENTS.md"), b"# Agents, v2\n").unwrap();
    git_run(&source, &["commit", "--quiet", "-am", "agents v2"]);
    tick();
    capture();
    let rows = apply();
    assert!(
        rows.iter()
            .any(|reason| reason.contains("GIT_DESTINATION_MODIFIED")),
        "{rows:?}"
    );
    assert!(
        fs::read(&agents).unwrap().ends_with(b"A local note.\n"),
        "the user's edit was landed over"
    );
}

/// Review of 2026-10-16 (mutation row for the re-dating path).
#[test]
fn a_redated_print_never_vouches_for_an_edit_made_after_the_exchange() {
    an_edit_after_the_exchange_survives(Rerun::Unchanged, Edit::InWindow);
}

/// C03: the same edit inside a render slower than the racy allowance. The
/// record was dated after the render's prints, so the edit's print was
/// trusted at once and the converge landed over it.
#[test]
fn an_edit_inside_a_slow_render_is_never_vouched_for() {
    an_edit_after_the_exchange_survives(Rerun::Unchanged, Edit::SlowRender);
}

/// #292 review F1 (the reviewer's hand-off probe): the same edit with its
/// mtime an hour back. Judged by mtime alone its print was trusted at once,
/// and the converge landed over the edit (`seats-replaced=1`).
#[test]
fn an_edit_with_an_old_mtime_is_never_vouched_for() {
    an_edit_after_the_exchange_survives(Rerun::Unchanged, Edit::OldMtime);
}

/// Re-review of 2026-10-16, finding 1: the keep path re-dated a racy
/// record print equal to the target's without checking the bytes under
/// the block (a rerun that retires another target).
#[test]
fn a_kept_block_never_redates_a_racy_print_unchecked_on_a_retire() {
    an_edit_after_the_exchange_survives(Rerun::Retire, Edit::InWindow);
}

/// Re-review of 2026-10-16, finding 1: the same through `--renew`.
#[test]
fn a_kept_block_never_redates_a_racy_print_unchecked_on_a_renew() {
    an_edit_after_the_exchange_survives(Rerun::Renew, Edit::InWindow);
}

/// Mutation row (review of 2026-10-16): `--clear` of a created file that a
/// writer replaced after the read puts the writer's file back and refuses.
#[test]
fn the_removal_is_undone_when_the_target_moved_after_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = fs::canonicalize(dir.path()).unwrap();
    let target = worktree.join("CLAUDE.md");
    fs::write(&target, block()).unwrap();
    let read = read_whole(&target, "CLAUDE.md").unwrap();
    let theirs = cat(&[&block(), b"theirs\n"]);
    fs::write(worktree.join("other"), &theirs).unwrap();
    fs::rename(worktree.join("other"), &target).unwrap();
    let refused = remove(&worktree, "CLAUDE.md", &read, &temp_name("t", 0));
    assert_eq!(refused, Err(BulkloadRefusal::HandoffBlockModified));
    assert_eq!(fs::read(&target).unwrap(), theirs);
    assert!(!worktree.join(temp_name("t", 0)).exists());
}

fn body() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(Vec::new()),
        Just(b"no final newline".to_vec()),
        Just(b"crlf\r\nlines\r\n".to_vec()),
        Just(b"\xef\xbb\xbfa BOM\n".to_vec()),
        Just(b"---\nfront: matter\n---\n".to_vec()),
        Just(b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef -->\n".to_vec()),
        proptest::collection::vec(any::<u8>(), 0..512),
    ]
}

proptest! {
    #![proptest_config(prop_config(32))]

    /// Render then clear is the identity on any body: the block is
    /// recognised at byte 0 above it, and stripping it gives the body's
    /// bytes back, byte for byte.
    #[test]
    fn render_then_clear_is_identity(body in body()) {
        let dir = tempfile::tempdir().unwrap();
        let worktree = fs::canonicalize(dir.path()).unwrap();
        let target = worktree.join("AGENTS.md");
        fs::write(&target, &body).unwrap();
        let block = block();
        let read = read_whole(&target, "AGENTS.md").unwrap();
        exchange(&worktree, "AGENTS.md", &read, &cat(&[&block, &body]), &temp_name("r", 0), Op::Render).unwrap();
        let rendered = fs::read(&target).unwrap();
        prop_assert_eq!(render::marker(&rendered), render::Marker::Block(block.len()));
        let read = read_whole(&target, "AGENTS.md").unwrap();
        exchange(&worktree, "AGENTS.md", &read, &rendered[block.len()..], &temp_name("c", 0), Op::Clear).unwrap();
        prop_assert_eq!(fs::read(&target).unwrap(), body);
        prop_assert_eq!(fs::read_dir(&worktree).unwrap().count(), 1);
    }
}

// ---- `--clear --orphaned` (2026-10-22) ---------------------------------------------

const ORPHAN_SOURCE: &str = "neo-inplace";

/// A block of source `source` rendered on `host` for the landing at
/// `workspace`.
fn block_for_landing(host: &str, workspace: &Path, source: &str) -> Vec<u8> {
    let mut inputs = render::sample();
    inputs.dest_host = host.to_owned();
    inputs.workspace = workspace.as_os_str().as_bytes().to_vec();
    inputs.source = source.to_owned();
    render::render(&inputs, false)
}

#[test]
fn source_blocks_finds_the_sources_blocks_in_the_head_chain_only() {
    let at = Path::new("/estate/main");
    let ours = block_for_landing("sting", at, ORPHAN_SOURCE);
    let other = block_for_landing("sting", at, "neo");
    let body = b"# Agents\n";
    // Another source's block above ours, the body below: ours alone.
    let bytes = cat(&[&other, &ours, body]);
    assert_eq!(
        source_blocks(&bytes, ORPHAN_SOURCE).unwrap(),
        vec![other.len()..other.len() + ours.len()]
    );
    assert_eq!(source_blocks(&bytes, "neo").unwrap(), vec![0..other.len()]);
    // A block below text is not in the chain.
    let later = cat(&[body, &ours]);
    assert!(source_blocks(&later, ORPHAN_SOURCE).unwrap().is_empty());
    // A source's block cut (a BEGIN with no END) refuses; another
    // source's cut block above nothing of ours is no block of ours.
    let cut = &ours[..ours.len() - 40];
    assert_eq!(
        source_blocks(cut, ORPHAN_SOURCE),
        Err(BulkloadRefusal::HandoffBlockModified)
    );
    assert!(source_blocks(&other[..other.len() - 40], ORPHAN_SOURCE)
        .unwrap()
        .is_empty());
    // A block holding another block's markers (nested) refuses.
    let text = String::from_utf8(ours.clone()).unwrap();
    let nested = text.replacen(
        "\n**Do this first**",
        &format!("\n{}\n**Do this first**", String::from_utf8(other).unwrap()),
        1,
    );
    assert!(matches!(
        render::marker(nested.as_bytes()),
        render::Marker::Block(_)
    ));
    assert_eq!(
        source_blocks(nested.as_bytes(), ORPHAN_SOURCE),
        Err(BulkloadRefusal::HandoffBlockModified)
    );
}

/// A checkout (`git init`) to clear in, canonical.
fn checkout() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&repo).unwrap();
    git_run(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    (dir, repo)
}

/// An orphaned clear with no plan and no record: each block of the source
/// this host rendered (also for the path the workspace had before it
/// moved) is removed byte for byte, text outside it untouched, a carried
/// block kept; a file left empty is kept (only a record proves bulkload
/// created it); the clear is recorded (no intent left) and a rerun is a
/// no-op.
#[test]
fn an_orphaned_clear_removes_this_hosts_blocks_byte_for_byte() {
    let (_dir, repo) = checkout();
    let host = host();
    let ours = block_for_landing(&host, &repo, ORPHAN_SOURCE);
    let moved = block_for_landing(&host, Path::new("/srv/old/name"), ORPHAN_SOURCE);
    let neo = block_for_landing(&host, &repo, "neo");
    let carried = block_for_landing("another-host", &repo, ORPHAN_SOURCE);
    let agents_body = b"# Agents\n\nRepository rules.\n\n<!-- a trailing note -->\n";
    fs::write(repo.join("AGENTS.md"), cat(&[&neo, &ours, agents_body])).unwrap();
    fs::write(repo.join("CLAUDE.md"), cat(&[&moved, b"See AGENTS.md.\n"])).unwrap();
    fs::write(repo.join(README), cat(&[&carried, b"# Readme\n"])).unwrap();
    fs::write(repo.join(LOCAL), &ours).unwrap();
    let row = clear_orphaned(&repo, ORPHAN_SOURCE).unwrap();
    assert_eq!(row.outcome, "cleared");
    assert_eq!(
        fs::read(repo.join("AGENTS.md")).unwrap(),
        cat(&[&neo, agents_body]),
        "another source's block and the body stay byte for byte"
    );
    assert_eq!(
        fs::read(repo.join("CLAUDE.md")).unwrap(),
        b"See AGENTS.md.\n"
    );
    assert_eq!(
        fs::read(repo.join(README)).unwrap(),
        cat(&[&carried, b"# Readme\n"]),
        "a block another host rendered is carried content"
    );
    assert_eq!(fs::read(repo.join(LOCAL)).unwrap(), b"");
    let actions: Vec<(String, &str, &str)> = row
        .files
        .iter()
        .map(|file| {
            (
                String::from_utf8(file.path.clone()).unwrap(),
                file.kind,
                file.action,
            )
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            ("AGENTS.md".to_owned(), "unrecorded", "cleared"),
            (
                "CLAUDE.local.md".to_owned(),
                "unrecorded",
                "cleared-empty-kept"
            ),
            ("CLAUDE.md".to_owned(), "unrecorded", "cleared"),
            ("README.md".to_owned(), "unrecorded", "carried-kept"),
        ]
    );
    let dir = landed::ledger_dir(&repo.join(".git"), ORPHAN_SOURCE);
    assert!(!dir.join(HANDOFF_INTENT).exists());
    assert!(!dir.join(HANDOFF_LANDED).exists());
    let names: Vec<_> = fs::read_dir(&repo)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.as_bytes().starts_with(TEMP_PREFIX.as_bytes()))
        .collect();
    assert!(names.is_empty(), "{names:?}");
    let again = clear_orphaned(&repo, ORPHAN_SOURCE).unwrap();
    assert_eq!(again.outcome, "unchanged");
    assert_eq!(
        fs::read(repo.join("AGENTS.md")).unwrap(),
        cat(&[&neo, agents_body])
    );
}

/// With its record: the recorded block is cleared (also unsealed bytes
/// would not be needed), a file the record names as created is removed,
/// and the record goes as `--clear` removes it.
#[test]
fn an_orphaned_clear_follows_the_record_for_a_created_file() {
    let (_dir, repo) = checkout();
    let ours = block_for_landing(&host(), &repo, ORPHAN_SOURCE);
    fs::write(repo.join(LOCAL), &ours).unwrap();
    let dir = landed::ensure_dir(&repo.join(".git"), ORPHAN_SOURCE).unwrap();
    let mut record = record(&ours);
    record.files[0].path = LOCAL.as_bytes().to_vec();
    record.files[0].kind = FileKind::Created;
    landed::write(&dir.join(HANDOFF_LANDED), &record).unwrap();
    let row = clear_orphaned(&repo, ORPHAN_SOURCE).unwrap();
    assert_eq!(row.outcome, "cleared");
    assert_eq!(row.item.as_deref(), Some("i"));
    assert_eq!(row.files.len(), 1);
    assert_eq!(
        (row.files[0].kind, row.files[0].action),
        ("created", "cleared")
    );
    assert!(!repo.join(LOCAL).exists(), "bulkload created it: removed");
    assert!(!dir.join(HANDOFF_LANDED).exists());
    assert!(!dir.join(HANDOFF_INTENT).exists());
}

/// Refusals, each with nothing written: a relative path, a symbolic link
/// out of the worktree, a malformed or an edited block of the source.
#[test]
fn an_orphaned_clear_refuses_typed_and_writes_nothing() {
    assert_eq!(
        clear_orphaned(Path::new("relative/repo"), ORPHAN_SOURCE),
        Err(BulkloadRefusal::PathNotAbsolute)
    );
    let (_dir, repo) = checkout();
    let ours = block_for_landing(&host(), &repo, ORPHAN_SOURCE);
    let agents = cat(&[&ours, LANDED]);
    fs::write(repo.join("AGENTS.md"), &agents).unwrap();
    // A malformed block of the source in a later target: nothing written,
    // AGENTS.md's good block included.
    let cut = cat(&[&ours[..ours.len() - 30], b"\nbody\n"]);
    fs::write(repo.join("CLAUDE.md"), &cut).unwrap();
    assert_eq!(
        clear_orphaned(&repo, ORPHAN_SOURCE),
        Err(BulkloadRefusal::HandoffBlockModified)
    );
    assert_eq!(fs::read(repo.join("AGENTS.md")).unwrap(), agents);
    assert_eq!(fs::read(repo.join("CLAUDE.md")).unwrap(), cut);
    // An edit inside a block no record names.
    let edited = String::from_utf8(ours.clone())
        .unwrap()
        .replacen("converge", "converg3", 1);
    fs::write(repo.join("CLAUDE.md"), edited.as_bytes()).unwrap();
    assert_eq!(
        clear_orphaned(&repo, ORPHAN_SOURCE),
        Err(BulkloadRefusal::HandoffBlockModified)
    );
    assert_eq!(fs::read(repo.join("AGENTS.md")).unwrap(), agents);
    // A target that is a link out of the worktree.
    fs::remove_file(repo.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink("../elsewhere.md", repo.join("CLAUDE.md")).unwrap();
    assert_eq!(
        clear_orphaned(&repo, ORPHAN_SOURCE),
        Err(BulkloadRefusal::HandoffTargetUnsupported)
    );
    assert_eq!(fs::read(repo.join("AGENTS.md")).unwrap(), agents);
    // A link to the other target is never followed: covered by its scan.
    fs::remove_file(repo.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink("AGENTS.md", repo.join("CLAUDE.md")).unwrap();
    assert_eq!(
        clear_orphaned(&repo, ORPHAN_SOURCE).unwrap().outcome,
        "cleared"
    );
    assert_eq!(fs::read(repo.join("AGENTS.md")).unwrap(), LANDED);
    assert!(fs::symlink_metadata(repo.join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());
}
