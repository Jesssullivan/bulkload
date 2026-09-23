//! `git-carry-estimate` over several pairs (F13, PR #55 review).
//!
//! Every pair prints one whole block. A refused pair prints a `refused=` line
//! with its stable code instead of silently ending the output, the pairs after
//! it are still measured, and the verb exits nonzero.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?}");
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir(&repo).unwrap();
    run(git(&repo).args(["init", "--quiet", "--template=", "-b", "main"]));
    std::fs::write(repo.join("a.txt"), name).unwrap();
    run(git(&repo).args(["add", "a.txt"]));
    run(git(&repo).args(["commit", "--quiet", "-m", name]));
    repo
}

#[test]
fn a_refused_pair_prints_its_own_line_and_the_rest_are_still_measured() {
    let root = Root(std::env::temp_dir().join(format!(
        "bulkload-estimate-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    let source = repo(&root.0, "source");
    let destination = repo(&root.0, "destination");
    let plain = root.0.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("git-carry-estimate")
        .arg(&source)
        .arg(&destination)
        .arg(&source)
        .arg(&plain)
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    let blocks: Vec<&str> = stdout.trim_end().split("\n\n").collect();
    assert_eq!(blocks.len(), 3, "{stdout}");
    for index in [0, 2] {
        let block = blocks[index];
        assert!(block.contains("\nmissing_objects="), "{block}");
        assert!(block.contains("\nmissing_thin_pack_bytes="), "{block}");
        assert!(!block.contains("refused="), "{block}");
    }
    let refused = blocks[1];
    assert!(
        refused.contains(&format!("destination={}", plain.display())),
        "{refused}"
    );
    assert!(
        refused.contains("\nrefused=GIT_REPOSITORY_NOT_AT_PATH"),
        "{refused}"
    );
    assert!(!refused.contains("missing_objects="), "{refused}");
}
