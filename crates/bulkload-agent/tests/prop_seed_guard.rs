//! No-fuzz seed guard (OI-1003-Q7; property-test plan §3 "Contract test
//! changes").
//!
//! Property tests run one bounded, fixed-seed corpus in CI and persist
//! nothing between runs. The only place that builds a proptest `Config` is
//! the shared helper `crates/bulkload-agent/src/test_support.rs`
//! (`prop_config`). This test reads every Rust source in the workspace and
//! fails on any line that escapes it.
//!
//! **The one accepted config.** The helper call, written on one line, is the
//! whole config expression:
//!
//! - `#![proptest_config(test_support::prop_config(<cases>))]`, with an
//!   optional `crate::`, `self::` or `super::` in front. A bare
//!   `prop_config(<cases>)` counts only in a file that has the plain import
//!   `use <path>::test_support::prop_config;`. Only a `//` comment may follow
//!   the closing `))]`. A struct update over the helper, a wrapper around it
//!   and a method call on it are all refused.
//! - `TestRunner::new(test_support::prop_config(<cases>))`, in the same
//!   spellings. The only other accepted use of the name is the plain import
//!   `use proptest::test_runner::TestRunner;`.
//!
//! **Refused on any line** (a line whose first token is `//` is skipped; a
//! trailing comment is read as code, so a refused word there is a finding):
//!
//! - `ProptestConfig`;
//! - `test_runner::Config`, a braced or glob `test_runner` import
//!   (`test_runner::{`, `test_runner::*`) and a `test_runner as` rename, which
//!   would each bring `Config` in under a spelling the scan cannot follow;
//! - the bare word `Config` in a file that uses proptest;
//! - `RngSeed` or `rng_seed`;
//! - `FileFailurePersistence` or `failure_persistence`;
//! - a local `fn prop_config`, and an `as prop_config` or `as test_support`
//!   rename;
//! - a `proptest_config(` or `#[proptest` that is not the accepted config;
//! - a `TestRunner` that is not one of the two accepted uses;
//! - a `proptest!` block whose first item is not a `#![proptest_config(..)]`:
//!   a block with no config runs `Config::default()`, a random seed plus a
//!   persistence file.
//!
//! The scan is text based and reads one line at a time. It errs towards
//! refusing: helper-routed code in another layout (the config split over two
//! lines, a braced helper import, `TestRunner` as a type in a signature) is a
//! finding and has to be rewritten into the accepted form. It does not see
//! through a macro or an alias that hides every spelling above.
//!
//! Two files are never scanned: the helper itself and this guard, whose test
//! inputs spell the patterns it refuses.
//!
//! [`EXEMPT`] names the files that still escape, each with its exact finding
//! count. The list only shrinks: a file that is fixed must leave it (or lower
//! its count), and neither the entry ceiling nor the finding ceiling is
//! raised once the guard is on main.

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
        4,
        "unseeded `random_dags_equal_upload_pack` (random seed); the file \
         disappears when L5 deletes carry_v2 (WP2 PR 3)",
    ),
    (
        "crates/bulkload-agent/tests/refusal_taxonomy.rs",
        7,
        "its own fixed-seed `prop_config` (seed 0x5733_7265_6675_7365); \
         migrates onto the helper after L5 lands",
    ),
];

/// Never raised once on main: the number of exempt files when the guard
/// landed.
const EXEMPT_CEILING: usize = 2;

/// Never raised once on main: the total exempt findings when the guard
/// landed (4 + 7 under the rules above).
const FINDINGS_CEILING: usize = 11;

/// One escape: 1-based line and what it is.
type Finding = (usize, &'static str);

/// The only accepted opening of a block's config.
const CONFIG_OPEN: &str = "#![proptest_config(";

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

const fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The byte offsets at which `word` stands in `line` as a whole identifier.
fn words(line: &str, word: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    line.match_indices(word)
        .map(|(at, _)| at)
        .filter(|&at| {
            let joined_before = at > 0 && is_ident(bytes[at - 1]);
            let joined_after = bytes
                .get(at + word.len())
                .is_some_and(|&next| is_ident(next));
            !joined_before && !joined_after
        })
        .collect()
}

/// If `text` starts with a call of the shared helper, the text after the
/// call's closing parenthesis. The argument stays on the line and holds no
/// string, character literal, comment or division.
fn after_helper_call(text: &str, imports_helper: bool) -> Option<&str> {
    let qualified = ["crate::", "self::", "super::"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .unwrap_or(text)
        .strip_prefix("test_support::prop_config(");
    let arguments = match qualified {
        Some(arguments) => arguments,
        None if imports_helper => text.strip_prefix("prop_config(")?,
        None => return None,
    };
    let mut depth = 1_usize;
    for (at, character) in arguments.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&arguments[at + 1..]);
                }
            }
            '"' | '\'' | '/' => return None,
            _ => {}
        }
    }
    None
}

/// Whether `text`, trimmed, is `open` + the helper call + `close` and then
/// nothing but a `//` comment.
fn is_helper_only(text: &str, open: &str, close: &str, imports_helper: bool) -> bool {
    text.trim()
        .strip_prefix(open)
        .and_then(|call| after_helper_call(call, imports_helper))
        .and_then(|rest| rest.strip_prefix(close))
        .map(str::trim_start)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("//"))
}

/// Whether the file has the plain import that lets a bare `prop_config(..)`
/// mean the helper.
fn imports_helper(lines: &[&str]) -> bool {
    lines.iter().any(|line| {
        let line = line.trim();
        line.strip_prefix("use ")
            .and_then(|path| path.strip_suffix("test_support::prop_config;"))
            .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with("::"))
    })
}

/// Every escape in one Rust source text.
fn findings(text: &str) -> Vec<Finding> {
    let lines: Vec<&str> = text.lines().collect();
    let imports_helper = imports_helper(&lines);
    let uses_proptest = lines.iter().any(|line| {
        !is_comment(line) && (line.contains("proptest") || line.contains("TestRunner"))
    });
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_comment(line) {
            continue;
        }
        let number = index + 1;
        if line.contains("ProptestConfig") {
            found.push((number, "ProptestConfig outside the helper"));
        }
        if [
            "test_runner::Config",
            "test_runner::{",
            "test_runner::*",
            "test_runner as ",
        ]
        .iter()
        .any(|reach| line.contains(reach))
        {
            found.push((number, "proptest Config reachable outside the helper"));
        }
        if uses_proptest
            && words(line, "Config")
                .iter()
                .any(|&at| !line[..at].ends_with("test_runner::"))
        {
            found.push((number, "a bare Config in a file that uses proptest"));
        }
        if line.contains("RngSeed") || line.contains("rng_seed") {
            found.push((number, "a seed set outside the helper"));
        }
        if line.contains("FileFailurePersistence") || line.contains("failure_persistence") {
            found.push((number, "failure persistence set outside the helper"));
        }
        if line.contains("fn prop_config") {
            found.push((number, "a local prop_config mirrors the helper"));
        }
        if line.contains(" as prop_config") || line.contains(" as test_support") {
            found.push((number, "a rename onto the helper's name"));
        }
        if line.contains("proptest_config(")
            && !is_helper_only(line, CONFIG_OPEN, ")]", imports_helper)
        {
            found.push((
                number,
                "the config is not exactly #![proptest_config(test_support::prop_config(..))]",
            ));
        }
        if line.contains("#[proptest") && !is_helper_only(line, "#[proptest(", ")]", imports_helper)
        {
            found.push((number, "#[proptest] without test_support::prop_config"));
        }
        if !runner_routes(line, imports_helper) {
            found.push((
                number,
                "TestRunner is not exactly TestRunner::new(test_support::prop_config(..))",
            ));
        }
        if let Some(at) = line.find("proptest!") {
            if !block_has_config(line, at, &lines[index + 1..]) {
                found.push((number, "proptest! block without a #![proptest_config(..)]"));
            }
        }
    }
    found
}

/// Whether every `TestRunner` on the line is the plain import or
/// `TestRunner::new(<the helper call>)`.
fn runner_routes(line: &str, imports_helper: bool) -> bool {
    if line.trim() == "use proptest::test_runner::TestRunner;" {
        return true;
    }
    words(line, "TestRunner").into_iter().all(|at| {
        line[at + "TestRunner".len()..]
            .strip_prefix("::new(")
            .and_then(|call| after_helper_call(call, imports_helper))
            .is_some_and(|rest| rest.starts_with(')'))
    })
}

/// Whether the first item of the `proptest!` block opening at `line[at..]`
/// is a `#![proptest_config(..)]`. Its shape is checked on its own line.
fn block_has_config(line: &str, at: usize, rest: &[&str]) -> bool {
    let tail = line[at + "proptest!".len()..].trim_start_matches([' ', '\t', '{', '(']);
    std::iter::once(tail)
        .chain(rest.iter().copied())
        .filter(|item| !is_comment(item))
        .map(str::trim)
        .find(|item| !item.is_empty())
        .is_some_and(|first| first.starts_with(CONFIG_OPEN))
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
        // Imports that bring `Config` in under a spelling the scan cannot follow.
        "use proptest::test_runner::{Config, FileFailurePersistence};\n",
        "use proptest::test_runner::{\n    TestRunner,\n};\n",
        "use proptest::test_runner::*;\n",
        "use proptest::test_runner as runner;\n",
        "use proptest::{prelude::*, test_runner::{TestRunner as R}};\n",
        // The helper call is not the whole config.
        "#![proptest_config(wrap(test_support::prop_config(1)))]\n",
        "#![proptest_config(test_support::prop_config(1).more())]\n",
        "#![proptest_config(test_support::prop_config(1))] fn f() {}\n",
        "#![proptest_config(other::test_support::prop_config(1))]\n",
        "#![proptest_config(test_support::prop_config(\n    1,\n))]\n",
        "#![proptest_config(c())] // test_support::prop_config(64)\n",
        "proptest! { #![proptest_config(c())] /* test_support::prop_config(64) */\n",
        "use crate::test_support::prop_config;\n#![proptest_config(unseeded_prop_config())]\n",
        "use crate::test_support::prop_config as helper;\n#![proptest_config(prop_config(4))]\n",
        "use crate::test_support::{prop_config};\n#![proptest_config(prop_config(4))]\n",
        "use other::unseeded as prop_config;\n",
        "use other as test_support;\n",
        // A config field set anywhere outside the helper.
        "let c = base(); c.failure_persistence = file();\n",
        "let persistence = FileFailurePersistence::Off;\n",
        "let c = Thing { rng_seed: seed(), ..base() };\n",
        // A runner that is not exactly `TestRunner::new(<helper call>)`.
        "let mut runner = TestRunner::new(c()); // test_support::prop_config(4)\n",
        "let mut runner = TestRunner::new(wrap(test_support::prop_config(4)));\n",
        "let mut runner = TestRunner::new(test_support::prop_config(4).more());\n",
        "let mut runner = TestRunner::new_with_rng(test_support::prop_config(4), rng);\n",
        "let runner: TestRunner = Default::default();\n",
        "let mut runner = TestRunner::new(prop_config(4));\n",
    ];
    for text in refused {
        assert!(!findings(text).is_empty(), "the guard missed:\n{text}");
    }
}

/// The review's escapes, each with the lines that must be findings.
#[test]
fn the_guard_refuses_a_config_built_over_the_helper() {
    let refused: [(&str, &[usize]); 6] = [
        (
            "use proptest::test_runner::{Config, FileFailurePersistence};\n\
             proptest! {\n\
             #![proptest_config(Config { rng_seed: Default::default(), failure_persistence: \
             Some(Box::new(FileFailurePersistence::SourceParallel(\"proptest-regressions\"))), \
             ..crate::test_support::prop_config(64) })]\n",
            &[1, 3],
        ),
        (
            "use proptest::test_runner::{Config, FileFailurePersistence};\n\
             proptest! {\n\
             #![proptest_config(Config::default())] // TODO: test_support::prop_config(64)\n",
            &[1, 3],
        ),
        (
            "use proptest::test_runner::*;\n\
             fn c() -> Config { Config::default() }\n\
             let mut runner = TestRunner::new(c()); // test_support::prop_config(\n",
            &[1, 2, 3],
        ),
        (
            "use crate::test_support::prop_config;\n\
             fn unseeded_prop_config() -> Config { Config::default() }\n\
             proptest! {\n\
             #![proptest_config(unseeded_prop_config())]\n",
            &[2, 4],
        ),
        (
            "use proptest::test_runner::{Config, FileFailurePersistence};\n\
             proptest! {\n\
             #![proptest_config(Config { cases: 100_000, ..test_support::prop_config(1) })]\n",
            &[1, 3],
        ),
        (
            "use proptest::test_runner::{Config, TestRunner};\n\
             let mut runner = TestRunner::new(Config { cases: 9, ..test_support::prop_config(4) });\n",
            &[1, 2],
        ),
    ];
    for (text, lines) in refused {
        let found = findings(text);
        for line in lines {
            assert!(
                found.iter().any(|(number, _)| number == line),
                "the guard missed line {line} of:\n{text}\nfound: {found:?}"
            );
        }
    }
    // The config line is refused for its shape alone, with no `Config` word
    // and no import to lean on.
    assert_eq!(
        findings("#![proptest_config(wrap(test_support::prop_config(1)))]\n"),
        vec![(
            1,
            "the config is not exactly #![proptest_config(test_support::prop_config(..))]"
        )]
    );
}

#[test]
fn the_guard_accepts_the_helper() {
    let accepted = [
        "proptest! {\n    #![proptest_config(crate::test_support::prop_config(48))]\n",
        "proptest::proptest! {\n    // P1\n\n    #![proptest_config(test_support::prop_config(if cfg!(miri) { 8 } else { 256 }))]\n",
        "use crate::test_support::prop_config;\nproptest! {\n    #![proptest_config(prop_config(64))]\n",
        "proptest! {\n    #![proptest_config(super::test_support::prop_config(6))] // P9\n",
        "// ProptestConfig::default() in a comment\n/// RngSeed::Random in a doc line\n",
        "let mut runner = TestRunner::new(test_support::prop_config(4));\n",
        "use proptest::test_runner::TestRunner;\nfn f() {\n    let mut runner = TestRunner::new(crate::test_support::prop_config(4));\n}\n",
        "use test_support::prop_config;\nlet mut runner = proptest::test_runner::TestRunner::new(prop_config(4));\n",
        // `Config` is an ordinary name in a file that does not use the crate.
        "struct Config { cases: u32 }\nlet c = Config { cases: 4 };\n",
    ];
    for text in accepted {
        assert_eq!(
            findings(text),
            Vec::<Finding>::new(),
            "the guard refused:\n{text}"
        );
    }
}
