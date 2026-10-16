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

fn block() -> Vec<u8> {
    let mut inputs = render::sample();
    inputs.workspace = b"/estate/main".to_vec();
    render::render(&inputs, false)
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
        },
    )
    .unwrap();
    settle(&worktree, &ledgers).unwrap();
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
        },
    )
    .unwrap();
    settle(&worktree, &ledgers).unwrap();
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
        held(&target, "AGENTS.md", &meta, Some(&record), fsid).unwrap(),
        Held::Modified
    );
    // Recorded well after the stamp: the print is trusted, nothing read.
    record.recorded_ns += 3 * super::super::super::RACY_GRANULARITY_NS;
    assert!(matches!(
        held(&target, "AGENTS.md", &meta, Some(&record), fsid).unwrap(),
        Held::Intact(_)
    ));
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
