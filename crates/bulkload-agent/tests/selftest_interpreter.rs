//! M2 item 4 (2026-10-16): the selftest reports the binary's ELF
//! interpreter (whether it exists and lives in `/nix/store`) and, for every
//! `/nix/store` path the binary names, whether a GC root Nix registered
//! covers it. A symlink in `$BULKLOAD_GCROOTS` that Nix does not know of is
//! `unregistered`, never rooted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

/// The selftest's `interpreter` and `gc roots` lines, after their labels.
fn selftest(roots: &std::path::Path) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env(
            bulkload_agent::space::TEST_SPACE_ENV,
            bulkload_agent::space::TEST_SPACE_ROOMY.test_env_value(),
        )
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
    let line = |label: &str| {
        stdout
            .lines()
            .find_map(|line| line.trim_start().strip_prefix(label))
            .unwrap_or_else(|| panic!("no {label} line in {stdout}"))
            .trim()
            .to_owned()
    };
    (line("interpreter"), line("gc roots"))
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split(' ')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {key}= in {line}"))
}

#[test]
fn the_selftest_reports_the_interpreter_and_unregistered_roots() {
    let roots = tempfile::tempdir().unwrap();
    let (interpreter, gc) = selftest(roots.path());
    if !cfg!(target_os = "linux") {
        assert!(interpreter.starts_with("none"), "{interpreter}");
        return;
    }
    let path = interpreter.split(' ').next().unwrap().to_owned();
    assert!(path.starts_with('/'), "{interpreter}");
    assert!(interpreter.contains(" exists=yes "), "{interpreter}");
    let paths: usize = field(&gc, "store-paths").parse().unwrap();
    let Some(rest) = path.strip_prefix("/nix/store/") else {
        assert!(interpreter.ends_with("nix-store=no"), "{interpreter}");
        return;
    };
    assert!(interpreter.ends_with("nix-store=yes"), "{interpreter}");
    assert!(paths >= 1, "{gc}");
    assert_eq!(field(&gc, "rooted"), "0", "{gc}");
    let store = format!("/nix/store/{}", rest.split('/').next().unwrap());
    assert!(field(&gc, "unrooted").contains(&store), "{gc}");
    // A symlink made by hand is not a root Nix registered.
    std::os::unix::fs::symlink(&store, roots.path().join("bulkload-agent-1-root")).unwrap();
    let (_, gc) = selftest(roots.path());
    if field(&gc, "registry") == "read" {
        assert_eq!(field(&gc, "rooted"), "0", "{gc}");
        assert_eq!(field(&gc, "unregistered"), "1", "{gc}");
        assert!(field(&gc, "unrooted").contains(&store), "{gc}");
    }
}
