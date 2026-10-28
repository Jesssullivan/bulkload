//! M2r2 A-103 through the binary: a `copy` into a landing whose store was
//! lost extends a grown byte-prefix file in place, prints one
//! `prefix-extended` line and counts it on the counters line; with
//! `BULKLOAD_PREFIX_EXTEND=0` the same file is refused
//! `DESTINATION_OCCUPIED` and left as it was.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("bulkload-prefix-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("source")).unwrap();
        std::fs::create_dir(base.join("destination")).unwrap();
        Self(base)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn copy(&self, extend: Option<&str>) -> (bool, String, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        command.env(
            bulkload_agent::space::TEST_SPACE_ENV,
            bulkload_agent::space::TEST_SPACE_ROOMY.test_env_value(),
        );
        command.arg("copy").args(
            ["source", "destination", "source-state", "destination-state"]
                .map(|name| self.path(name)),
        );
        command.env_remove("BULKLOAD_PREFIX_EXTEND");
        if let Some(value) = extend {
            command.env("BULKLOAD_PREFIX_EXTEND", value);
        }
        let out = command.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn lines(count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|index| {
            format!("{{\"n\":{index},\"text\":\"line {index} of a transcript\"}}\n").into_bytes()
        })
        .collect()
}

fn counter(output: &str, name: &str) -> u64 {
    output
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in {output}"))
}

fn grown(scratch: &Scratch) -> (Vec<u8>, usize) {
    let full = lines(40_000);
    let prior = full.len() * 3 / 4;
    std::fs::write(scratch.path("source/t.jsonl"), &full[..prior]).unwrap();
    let (ok, stdout, stderr) = scratch.copy(None);
    assert!(ok, "{stdout}{stderr}");
    std::fs::remove_dir_all(scratch.path("destination-state")).unwrap();
    std::fs::write(scratch.path("source/t.jsonl"), &full).unwrap();
    (full, prior)
}

fn settle(source: &Path) {
    bulkload_agent::transfer::settle_racy_window(source).unwrap();
}

#[test]
fn copy_extends_a_grown_prefix_and_says_so() {
    let scratch = Scratch::new("on");
    let (full, prior) = grown(&scratch);
    settle(&scratch.path("source"));
    settle(&scratch.path("destination"));
    let (ok, stdout, stderr) = scratch.copy(None);
    assert!(ok, "{stdout}{stderr}");
    assert_eq!(
        std::fs::read(scratch.path("destination/t.jsonl")).unwrap(),
        full
    );
    let expected = format!(
        "prefix-extended t.jsonl from={prior} to={} prefix_blake3={}",
        full.len(),
        blake3::hash(&full[..prior]).to_hex()
    );
    assert!(stderr.contains(&expected), "{stderr}");
    assert_eq!(counter(&stdout, "dest_prefix_extended"), 1, "{stdout}");
    assert_eq!(
        counter(&stdout, "write_dest_prefix_append_bytes"),
        (full.len() - prior) as u64
    );
}

#[test]
fn copy_with_the_extension_off_refuses_as_before() {
    let scratch = Scratch::new("off");
    let (full, prior) = grown(&scratch);
    settle(&scratch.path("source"));
    settle(&scratch.path("destination"));
    let (ok, stdout, stderr) = scratch.copy(Some("0"));
    assert!(!ok, "{stdout}{stderr}");
    assert!(
        stderr.contains("refused t.jsonl: DESTINATION_OCCUPIED"),
        "{stderr}"
    );
    assert!(!stderr.contains("prefix-extended"), "{stderr}");
    assert_eq!(
        std::fs::read(scratch.path("destination/t.jsonl")).unwrap(),
        &full[..prior]
    );
}
