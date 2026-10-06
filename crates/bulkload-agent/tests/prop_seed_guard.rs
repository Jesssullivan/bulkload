//! No-fuzz seed guard (OI-1003-Q7; property-test plan §3 "Contract test
//! changes").
//!
//! Property tests run one bounded, fixed-seed corpus in CI and persist
//! nothing between runs. The only place that builds a proptest `Config` is
//! the shared helper `crates/bulkload-agent/src/test_support.rs`
//! (`prop_config`). This test reads every Rust source in the workspace and
//! fails on any line that escapes it:
//!
//! - a `ProptestConfig` or `proptest::test_runner::Config` construction;
//! - any `RngSeed` (so `RngSeed::Random` too);
//! - a local `fn prop_config` that mirrors the helper;
//! - a `proptest!` block whose first item is not
//!   `#![proptest_config(test_support::prop_config(..))]` (a bare
//!   `prop_config(..)` counts only where the file imports
//!   `test_support::prop_config`); a block with no config at all runs
//!   `Config::default()`, a random seed plus a persistence file;
//! - a `#[proptest]` attribute or a `TestRunner::` that does not pass
//!   `prop_config(..)`.
//!
//! Comment lines are skipped. Two files are never scanned: the helper itself
//! and this guard, whose test inputs spell the patterns it refuses.
//!
//! [`EXEMPT`] names the files that still escape, each with its exact finding
//! count. The list only shrinks: a file that is fixed must leave it (or lower
//! its count), and neither the entry ceiling nor the finding ceiling is ever
//! raised.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};

/// The shared helper, compiled here as well so its contract is checked
/// directly (the library module is `#[cfg(test)] pub(crate)`).
#[path = "../src/test_support.rs"]
mod test_support;

/// The one file allowed to build a proptest `Config`.
const HELPER: &str = "crates/bulkload-agent/src/test_support.rs";

/// This guard: its test inputs spell every refused pattern.
const SELF: &str = "crates/bulkload-agent/tests/prop_seed_guard.rs";

/// Files that still escape the helper: (workspace path, exact finding count,
/// why). An entry whose file no longer exists is tolerated so the lane that
/// deletes it does not have to touch this guard; remove the entry then.
const EXEMPT: &[(&str, usize, &str)] = &[
    (
        "crates/bulkload-agent/tests/git_carry_v2.rs",
        3,
        "unseeded `random_dags_equal_upload_pack` (random seed); the file \
         disappears when L5 deletes carry_v2 (WP2 PR 3)",
    ),
    (
        "crates/bulkload-agent/tests/refusal_taxonomy.rs",
        6,
        "its own fixed-seed `prop_config` (seed 0x5733_7265_6675_7365); \
         migrates onto the helper after L5 lands",
    ),
];

/// Never raised: the number of exempt files when the guard landed.
const EXEMPT_CEILING: usize = 2;

/// Never raised: the total exempt findings when the guard landed.
const FINDINGS_CEILING: usize = 9;

/// One escape: 1-based line and what it is.
type Finding = (usize, &'static str);

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Every escape in one Rust source text.
fn findings(text: &str) -> Vec<Finding> {
    let lines: Vec<&str> = text.lines().collect();
    let imports_helper = lines.iter().any(|line| {
        !is_comment(line) && line.contains("use ") && line.contains("test_support::prop_config")
    });
    let routes = |line: &str| {
        line.contains("test_support::prop_config(")
            || (imports_helper && line.contains("prop_config("))
    };
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_comment(line) {
            continue;
        }
        let number = index + 1;
        if line.contains("ProptestConfig") {
            found.push((number, "ProptestConfig outside the helper"));
        }
        if line.contains("test_runner::Config") {
            found.push((number, "proptest Config built outside the helper"));
        }
        if line.contains("RngSeed") {
            found.push((number, "RngSeed outside the helper"));
        }
        if line.contains("fn prop_config") {
            found.push((number, "a local prop_config mirrors the helper"));
        }
        if line.contains("#[proptest") && !routes(line) {
            found.push((number, "#[proptest] without test_support::prop_config"));
        }
        if line.contains("TestRunner::") && !routes(line) {
            found.push((number, "TestRunner without test_support::prop_config"));
        }
        if let Some(at) = line.find("proptest!") {
            if !block_routes(line, at, &lines[index + 1..], &routes) {
                found.push((
                    number,
                    "proptest! block without #![proptest_config(test_support::prop_config(..))]",
                ));
            }
        }
    }
    found
}

/// Whether the `proptest!` block opening at `line[at..]` starts with a
/// `#![proptest_config(..)]` that routes through the helper.
fn block_routes(line: &str, at: usize, rest: &[&str], routes: &dyn Fn(&str) -> bool) -> bool {
    let tail = line[at + "proptest!".len()..].trim_start_matches([' ', '\t', '{', '(']);
    let first = std::iter::once(tail)
        .chain(rest.iter().copied().filter(|next| !is_comment(next)))
        .map(str::trim)
        .find(|candidate| !candidate.is_empty());
    first.is_some_and(|first| first.starts_with("#![proptest_config(") && routes(first))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Every `.rs` file under `dir`, as workspace-relative `/` paths. Symlinks
/// are not followed; hidden directories, `target` and `bazel-*` are skipped.
fn rust_sources(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if kind.is_dir() {
            if name.starts_with('.') || name == "target" || name.starts_with("bazel-") {
                continue;
            }
            rust_sources(root, &path, out);
        } else if kind.is_file() && name.ends_with(".rs") {
            let relative = path.strip_prefix(root).unwrap();
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn every_property_routes_through_the_shared_helper() {
    let root = workspace_root();
    let mut sources = Vec::new();
    rust_sources(&root, &root, &mut sources);
    sources.sort();
    assert!(
        sources.len() > 50 && sources.iter().any(|path| path == HELPER),
        "the scan found {} sources and no {HELPER}; the workspace root is wrong",
        sources.len()
    );

    let mut escapes = Vec::new();
    let mut seen_exempt = Vec::new();
    let mut scanned = 0_usize;
    for path in &sources {
        if path == HELPER || path == SELF {
            continue;
        }
        scanned += 1;
        let text = fs::read_to_string(root.join(path)).unwrap();
        let found = findings(&text);
        if let Some((_, count, _)) = EXEMPT.iter().find(|(exempt, _, _)| exempt == path) {
            seen_exempt.push(path.clone());
            assert_eq!(
                found.len(),
                *count,
                "{path} is exempt with exactly {count} findings but has {}: {found:?}. \
                 Fewer: lower the count or drop the entry. More: route the new \
                 property through test_support::prop_config.",
                found.len()
            );
            continue;
        }
        escapes.extend(
            found
                .into_iter()
                .map(|(line, what)| format!("{path}:{line}: {what}")),
        );
    }
    for (exempt, _, why) in EXEMPT {
        if !seen_exempt.iter().any(|path| path == exempt) {
            eprintln!("prop_seed_guard: exempt {exempt} is gone ({why}); drop its entry");
        }
    }
    eprintln!(
        "prop_seed_guard: scanned={scanned} exempt_present={} escapes={}",
        seen_exempt.len(),
        escapes.len()
    );
    assert!(
        escapes.is_empty(),
        "properties must use test_support::prop_config (OI-1003-Q7, fixed seed, no \
         persistence):\n{}",
        escapes.join("\n")
    );
}

#[test]
fn the_exemption_list_only_shrinks() {
    assert!(
        EXEMPT.len() <= EXEMPT_CEILING,
        "the exemption list only shrinks (was {EXEMPT_CEILING} when the guard landed)"
    );
    let total: usize = EXEMPT.iter().map(|(_, count, _)| count).sum();
    assert!(
        total <= FINDINGS_CEILING,
        "exempt findings only shrink (were {FINDINGS_CEILING} when the guard landed)"
    );
    for (index, (path, count, why)) in EXEMPT.iter().enumerate() {
        assert!(
            *count > 0 && !why.is_empty(),
            "{path}: an entry needs findings and a reason"
        );
        assert!(
            EXEMPT[..index].iter().all(|(earlier, _, _)| earlier < path),
            "keep EXEMPT sorted and unique ({path})"
        );
        assert!(
            *path != HELPER && *path != SELF,
            "{path} is not an exemption"
        );
    }
}

#[test]
fn the_helper_fixes_the_seed_and_persists_nothing() {
    let config = test_support::prop_config(7);
    assert!(config.failure_persistence.is_none());
    let deep = std::env::var_os(test_support::DEEP).is_some_and(|value| value == "1");
    if deep {
        assert_eq!(config.cases, 140);
    } else {
        assert_eq!(config.cases, 7);
        assert_eq!(
            config.rng_seed,
            proptest::test_runner::RngSeed::Fixed(test_support::CI_SEED)
        );
    }
}

#[test]
fn the_guard_refuses_each_escape() {
    let refused = [
        "proptest! {\n    #![proptest_config(ProptestConfig::default())]\n",
        "proptest! {\n    #![proptest_config(ProptestConfig { cases: 9, ..ProptestConfig::default() })]\n",
        "proptest! {\n    #[test]\n    fn p(x in 0_u8..9) {}\n}\n",
        "proptest::proptest! {\n    // a comment\n    #![proptest_config(prop_config(4))]\n",
        "let c = proptest::test_runner::Config::default();\n",
        "let s = proptest::test_runner::RngSeed::Random;\n",
        "fn prop_config(cases: u32) -> Config { todo!() }\n",
        "#[proptest(cases = 4)]\nfn p(x: u8) {}\n",
        "let mut runner = TestRunner::default();\n",
    ];
    for text in refused {
        assert!(!findings(text).is_empty(), "the guard missed:\n{text}");
    }
}

#[test]
fn the_guard_accepts_the_helper() {
    let accepted = [
        "proptest! {\n    #![proptest_config(crate::test_support::prop_config(48))]\n",
        "proptest::proptest! {\n    // P1\n\n    #![proptest_config(test_support::prop_config(if cfg!(miri) { 8 } else { 256 }))]\n",
        "use crate::test_support::prop_config;\nproptest! {\n    #![proptest_config(prop_config(64))]\n",
        "// ProptestConfig::default() in a comment\n/// RngSeed::Random in a doc line\n",
        "let mut runner = TestRunner::new(test_support::prop_config(4));\n",
    ];
    for text in accepted {
        assert_eq!(
            findings(text),
            Vec::<Finding>::new(),
            "the guard refused:\n{text}"
        );
    }
}
