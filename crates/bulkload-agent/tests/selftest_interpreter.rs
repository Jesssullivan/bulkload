//! M2 item 4 (2026-10-16): the selftest reports the binary's ELF
//! interpreter, whether it exists, whether it lives in `/nix/store`, and
//! whether a GC root in `$BULKLOAD_GCROOTS` names its store path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn selftest(roots: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("selftest")
        .env("BULKLOAD_GCROOTS", roots)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    stdout
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("interpreter"))
        .unwrap_or_else(|| panic!("no interpreter line in {stdout}"))
        .trim()
        .to_owned()
}

#[test]
fn the_selftest_reports_the_interpreter_and_its_gc_root() {
    let roots = tempfile::tempdir().unwrap();
    let line = selftest(roots.path());
    if !cfg!(target_os = "linux") {
        assert!(line.starts_with("none"), "{line}");
        return;
    }
    let interpreter = line.split(' ').next().unwrap().to_owned();
    assert!(interpreter.starts_with('/'), "{line}");
    assert!(line.contains(" exists=yes "), "{line}");
    let Some(rest) = interpreter.strip_prefix("/nix/store/") else {
        assert!(line.ends_with("nix-store=no rooted=n/a"), "{line}");
        return;
    };
    assert!(line.ends_with("nix-store=yes rooted=no"), "{line}");
    let store = format!("/nix/store/{}", rest.split('/').next().unwrap());
    std::os::unix::fs::symlink(&store, roots.path().join("bulkload-agent-1-root")).unwrap();
    let line = selftest(roots.path());
    assert!(line.ends_with("nix-store=yes rooted=yes"), "{line}");
}
