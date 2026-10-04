//! WP10 (OI-1003-Q14): the agent is the source-side engine binary, and it
//! never runs a credential or agent tool. The handoff probes that did live in
//! the `bulkload-handoff` crate now.
//!
//! The source scan is structural: no agent source file names one of these
//! tools as a program to run. If this fails, move the caller out of the agent,
//! do not widen the list.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

/// Programs the agent must never spawn.
const TOOLS: &[&str] = &["sops", "kubectl", "gpg", "gh", "claude", "codex"];

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_agent_source_spawns_a_credential_or_agent_tool() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty());
    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for tool in TOOLS {
            if text.contains(&format!("Command::new(\"{tool}\")")) {
                hits.push(format!("{} spawns {tool}", file.display()));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}

#[test]
fn the_agent_has_no_handoff_verb() {
    let output = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("handoff-verify")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown subcommand"), "{stderr}");
    // The only mention is the echoed unknown verb; USAGE lists no handoff.
    assert_eq!(stderr.matches("handoff").count(), 1, "{stderr}");
}
