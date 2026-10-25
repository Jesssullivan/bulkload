//! SPEC rules 5 and 6: source inventory retains nested estates without a
//! full metadata census of every ordinary or rebuildable file.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::estate::moving::{self, Inventory, REBUILDABLE_NEST};

fn git(repo: &Path, args: &[&str]) {
    let ran = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&ran.stderr)
    );
}

fn repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(path.join("f"), b"committed\n").unwrap();
    git(path, &["add", "f"]);
    git(path, &["commit", "--quiet", "-m", "fixture"]);
}

fn sources(found: &Inventory) -> Vec<PathBuf> {
    found
        .items
        .iter()
        .map(|planned| planned.item.source.clone())
        .collect()
}

#[test]
fn inventory_finds_dirty_nests_without_a_file_row_census_and_keeps_rebuildable_notes() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(scratch.path()).unwrap().join("repo");
    repo(&root);
    let nested = root.join("vendor/nested");
    repo(&nested);
    fs::write(nested.join("f"), b"dirty nested work\n").unwrap();
    let rebuilt = root.join("target/dependency");
    repo(&rebuilt);
    fs::create_dir_all(root.join("ordinary/deep")).unwrap();
    for index in 0..2048 {
        fs::write(
            root.join(format!("ordinary/deep/file-{index}")),
            b"content\n",
        )
        .unwrap();
    }
    let before = Counters::snapshot();
    let found = moving::inventory(std::slice::from_ref(&root), &[]).unwrap();
    let after = Counters::snapshot().since(before);
    assert_eq!(
        after.get(Counter::CensusWalks),
        0,
        "inventory must not allocate per-file RowSchema rows"
    );
    assert_eq!(sources(&found), vec![root, nested.clone()]);
    assert!(found
        .notes
        .iter()
        .any(|note| note.path == rebuilt && note.what == REBUILDABLE_NEST && note.count == 1));
    assert_eq!(fs::read(nested.join("f")).unwrap(), b"dirty nested work\n");
    assert!(
        rebuilt.join(".git").is_dir(),
        "inventory never mutates a rebuildable nest"
    );
}

#[test]
fn a_tracked_path_beneath_a_rebuildable_name_keeps_its_nested_repository() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(scratch.path()).unwrap().join("repo");
    repo(&root);
    let nested = root.join("target/dependency");
    repo(&nested);
    fs::write(root.join("target/kept.txt"), b"tracked output contract\n").unwrap();
    git(&root, &["add", "target/kept.txt"]);
    let found = moving::inventory(std::slice::from_ref(&root), &[]).unwrap();
    assert_eq!(sources(&found), vec![root, nested.clone()]);
    assert!(!found
        .notes
        .iter()
        .any(|note| note.path == nested && note.what == REBUILDABLE_NEST));
}

#[test]
fn non_git_dot_directories_missing_roots_maps_and_symlink_notes_are_preserved() {
    let scratch = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let root = base.join("source");
    let nested = root.join(".cache/work/repo");
    repo(&nested);
    fs::write(root.join(".cache/notes.txt"), b"not a repository\n").unwrap();
    std::os::unix::fs::symlink(&nested, root.join("alias")).unwrap();
    let missing = base.join("missing");
    let mapped = base.join("dest-cache");
    let found = moving::inventory(
        &[missing.clone(), root.clone()],
        &[
            (root.clone(), base.join("dest")),
            (root.join(".cache"), mapped.clone()),
        ],
    )
    .unwrap();
    assert_eq!(sources(&found), vec![nested]);
    assert_eq!(
        found.items[0].item.workspace,
        Some(mapped.join("work/repo"))
    );
    assert!(found
        .notes
        .iter()
        .any(|note| note.path == missing && note.code == Some("SOURCE_ROOT_MISSING")));
    assert!(found
        .notes
        .iter()
        .any(|note| note.path == root.join("alias") && note.what == "symlink"));
    assert!(found
        .notes
        .iter()
        .any(|note| note.path == root.join(".cache") && note.what == "not-git" && note.count == 1));
}

#[test]
fn a_stale_nested_pointer_is_named_without_hiding_a_valid_sibling() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(scratch.path()).unwrap().join("repo");
    repo(&root);
    let valid = root.join("vendor/valid");
    repo(&valid);
    let stale = root.join("vendor/stale");
    fs::create_dir_all(&stale).unwrap();
    fs::write(
        stale.join(".git"),
        b"gitdir: /nonexistent-bulkload-fixture-admin\n",
    )
    .unwrap();
    let found = moving::inventory(std::slice::from_ref(&root), &[]).unwrap();
    assert_eq!(sources(&found), vec![root, valid]);
    assert!(found
        .notes
        .iter()
        .any(|note| note.path == stale && note.what == "unreadable" && note.code.is_some()));
}
