//! WP3: the refusal taxonomy carries no dead vocabulary.
//!
//! Every [`BulkloadRefusal`] variant must be raised by at least one piece of
//! non-test code in the workspace. A code that nothing raises tells an
//! operator about a failure that cannot happen and hides the one that did,
//! so a variant with no constructor is deleted, not kept "for later"
//! (architecture review 2026-10-03, WP3 PR 1; OI-1003-Q15..Q21).
//!
//! The scan reads the workspace's `src/` trees as text. It drops comments,
//! string and char literals, `tests.rs` files, `tests/` directories and every
//! item behind `#[cfg(test)]` or `#[cfg(all(test, ..))]`, then looks for
//! `BulkloadRefusal::<Variant>` in a value position: not a match arm, an
//! or-pattern, a `let`/`if let` pattern, a comparison operand or a
//! `matches!` argument.
//!
//! The same scan holds the count of bare `Io(None)` refusals per file to an
//! allowlist that may only shrink (WP3 PR 2).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use bulkload_agent::BulkloadRefusal;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn refusal_source() -> PathBuf {
    workspace().join("crates/bulkload-proto/src/refusal.rs")
}

/// The variant names, in declaration order, read from the enum's source.
fn variants() -> Vec<String> {
    let source = fs::read_to_string(refusal_source()).unwrap();
    let start = source.find("pub enum BulkloadRefusal {").unwrap();
    let body = &source[start..];
    let end = body.find("\n}\n").unwrap();
    body[..end]
        .lines()
        .skip(1)
        .filter_map(|line| {
            let rest = line.strip_prefix("    ")?;
            let first = rest.chars().next()?;
            if !first.is_ascii_uppercase() {
                return None;
            }
            let name: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            Some(name)
        })
        .collect()
}

/// Every non-test `.rs` file under the workspace's crates' `src/` trees,
/// except the taxonomy itself.
fn sources() -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tests" {
                    visit(&path, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for krate in fs::read_dir(workspace().join("crates")).unwrap() {
        let src = krate.unwrap().path().join("src");
        if src.is_dir() {
            visit(&src, &mut out);
        }
    }
    let taxonomy = refusal_source();
    out.retain(|path| *path != taxonomy);
    out.sort();
    out
}

/// `source` with comments and string/char literals blanked to spaces
/// (newlines kept), so neither prose nor a literal can count as code.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, b: u8| out.push(if b == b'\n' { b'\n' } else { b' ' });
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if b == b'/' && next == Some(b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                blank(&mut out, bytes[i]);
                i += 1;
            }
        } else if b == b'/' && next == Some(b'*') {
            let mut depth = 0_usize;
            while i < bytes.len() {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, bytes[i]);
                    i += 1;
                }
            }
        } else if (b == b'r' || b == b'b')
            && !bytes
                .get(i.wrapping_sub(1))
                .is_some_and(|p| p.is_ascii_alphanumeric() || *p == b'_')
            && raw_string_hashes(&bytes[i..]).is_some()
        {
            let (prefix, hashes) = raw_string_hashes(&bytes[i..]).unwrap();
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            out.extend(std::iter::repeat_n(b' ', prefix));
            i += prefix;
            while i < bytes.len() && !bytes[i..].starts_with(&close) {
                blank(&mut out, bytes[i]);
                i += 1;
            }
            out.extend(std::iter::repeat_n(b' ', close.len().min(bytes.len() - i)));
            i += close.len();
        } else if b == b'"' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    blank(&mut out, bytes[i]);
                    i += 1;
                }
                if i < bytes.len() {
                    blank(&mut out, bytes[i]);
                    i += 1;
                }
            }
            out.push(b' ');
            i += 1;
        } else if b == b'\'' && char_literal_len(&bytes[i..]).is_some() {
            let len = char_literal_len(&bytes[i..]).unwrap();
            out.extend(std::iter::repeat_n(b' ', len));
            i += len;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// For `r"`, `r#"`, `br"`, `br#"`: the length of the opening and the number
/// of `#`s.
fn raw_string_hashes(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    if bytes.first() == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = bytes[i..].iter().take_while(|b| **b == b'#').count();
    i += hashes;
    (bytes.get(i) == Some(&b'"')).then_some((i + 1, hashes))
}

/// The length of a char literal at the start of `bytes` (`'a'`, `'\n'`,
/// `'\u{1F600}'`, a multi-byte char), or `None` for a lifetime.
fn char_literal_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) == Some(&b'\\') {
        // `'\n'`, `'\''`, `'\u{..}'`: the escaped char, then the close.
        let end = bytes.iter().skip(3).position(|b| *b == b'\'')?;
        return Some(end + 4);
    }
    let width = match *bytes.get(1)? {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    };
    (bytes.get(1 + width) == Some(&b'\'')).then_some(width + 2)
}

/// `code` with every item behind `#[cfg(test)]` or `#[cfg(all(test, ..))]`
/// removed: from the attribute to the end of the item's brace block, or to
/// its `;` when it has none.
fn without_test_items(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    loop {
        let at = ["#[cfg(test)]", "#[cfg(all(test"]
            .iter()
            .filter_map(|marker| rest.find(marker))
            .min();
        let Some(at) = at else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        let item = &rest[at..];
        // Skip the attribute itself, then the item.
        let after_attr = item.find(']').unwrap() + 1;
        let body = &item[after_attr..];
        let brace = body.find('{');
        let semi = body.find(';');
        let end = match (brace, semi) {
            (Some(brace), Some(semi)) if semi < brace => after_attr + semi + 1,
            (Some(brace), _) => {
                let mut depth = 0_usize;
                let mut end = item.len();
                for (offset, c) in body[brace..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = after_attr + brace + offset + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end
            }
            (None, Some(semi)) => after_attr + semi + 1,
            (None, None) => item.len(),
        };
        rest = &item[end..];
    }
}

/// Whether the occurrence of a variant path at `start..end` of `code` is a
/// value (a constructor) rather than a pattern or a comparison operand.
fn is_constructor(code: &str, start: usize, end: usize) -> bool {
    let before = code[..start].trim_end();
    if before.ends_with("==") || before.ends_with("!=") {
        return false;
    }
    // Inside `matches!( .. )`?
    if let Some(open) = before.rfind("matches!(") {
        let span = &code[open + "matches!(".len()..start];
        let mut depth = 1_i32;
        let closed = span.chars().any(|c| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth == 0
        });
        if !closed {
            return false;
        }
    }
    // Skip a payload group, then any closing parens of an enclosing pattern.
    let mut after = &code[end..];
    if after.starts_with('(') || after.starts_with('{') {
        let (open, close) = if after.starts_with('(') {
            ('(', ')')
        } else {
            ('{', '}')
        };
        let mut depth = 0_i32;
        for (offset, c) in after.char_indices() {
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    after = &after[offset + 1..];
                    break;
                }
            }
        }
    }
    let after = after.trim_start_matches(|c: char| c == ')' || c.is_whitespace());
    let pattern = after.starts_with("=>")
        || after.starts_with("==")
        || after.starts_with("!=")
        || (after.starts_with('|') && !after.starts_with("||"))
        || after.starts_with("if ")
        || (after.starts_with('=') && !after.starts_with("=="));
    !pattern
}

/// Non-test constructor sites per variant, as `path:line`.
fn constructors() -> BTreeMap<String, Vec<String>> {
    let names = variants();
    let mut found: BTreeMap<String, Vec<String>> = names
        .iter()
        .map(|name| (name.clone(), Vec::new()))
        .collect();
    let root = workspace();
    for path in sources() {
        let code = without_test_items(&code_only(&fs::read_to_string(&path).unwrap()));
        let shown = path.strip_prefix(&root).unwrap().display().to_string();
        let needle = "BulkloadRefusal::";
        let mut from = 0;
        while let Some(found_at) = code[from..].find(needle) {
            let start = from + found_at;
            let name_start = start + needle.len();
            let name: String = code[name_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let end = name_start + name.len();
            from = end.max(start + 1);
            if let Some(sites) = found.get_mut(&name) {
                if is_constructor(&code, start, end) {
                    let line = code[..start].matches('\n').count() + 1;
                    sites.push(format!("{shown}:{line}"));
                }
            }
        }
    }
    found
}

#[test]
fn the_scan_reads_the_whole_taxonomy() {
    let names = variants();
    assert!(names.len() >= 40, "parsed only {} variants", names.len());
    // Each parsed name is a real variant: its code is listed.
    assert_eq!(names.len(), BulkloadRefusal::CODES.len());
    assert!(names.iter().any(|name| name == "Io"));
    assert!(names.iter().any(|name| name == "GitChildFailed"));
}

#[test]
fn the_scan_tells_constructors_from_patterns() {
    let code = "fn f(r: X) -> R {\n\
        if r == BulkloadRefusal::A { }\n\
        match r { BulkloadRefusal::B(_) => {}, BulkloadRefusal::C | BulkloadRefusal::D => {} }\n\
        if let Err(BulkloadRefusal::E) = r {}\n\
        let _ = matches!(r, BulkloadRefusal::F(..));\n\
        return Err(BulkloadRefusal::G(Some(1)));\n\
        x.ok_or(BulkloadRefusal::H)?;\n\
        }\n\
        #[cfg(test)]\nmod tests { fn t() { Err(BulkloadRefusal::I) } }\n\
        // BulkloadRefusal::J\n\
        const S: &str = \"BulkloadRefusal::K\";\n";
    let code = without_test_items(&code_only(code));
    let mut constructed = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("BulkloadRefusal::") {
        let start = from + at;
        let end = start + "BulkloadRefusal::".len() + 1;
        if is_constructor(&code, start, end) {
            constructed.push(&code[end - 1..end]);
        }
        from = end;
    }
    assert_eq!(constructed, ["G", "H"]);
}

#[test]
fn every_refusal_variant_has_a_non_test_constructor() {
    let unconstructed: Vec<String> = constructors()
        .into_iter()
        .filter(|(_, sites)| sites.is_empty())
        .map(|(name, _)| name)
        .collect();
    assert!(
        unconstructed.is_empty(),
        "refusal variants nothing outside test code raises -- delete them, or \
         raise them where the failure happens: {unconstructed:?}"
    );
}

/// Non-test `BulkloadRefusal::Io(None)` constructor sites, counted per file.
fn io_none_sites() -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let root = workspace();
    for path in sources() {
        let code = without_test_items(&code_only(&fs::read_to_string(&path).unwrap()));
        let needle = "BulkloadRefusal::Io(None)";
        let mut from = 0;
        while let Some(at) = code[from..].find(needle) {
            let start = from + at;
            let end = start + "BulkloadRefusal::Io".len();
            from = start + needle.len();
            if is_constructor(&code, start, end) {
                let shown = path.strip_prefix(&root).unwrap().display().to_string();
                *counts.entry(shown).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Every non-test `IO` refusal with no errno, per file (WP3). A bare
/// `Io(None)` names no cause and no errno, so it cannot close an item
/// (docs/design.md). This list may only shrink: a new site raises a typed
/// refusal or goes through `.refuse_at(site)` with the OS error; a removed
/// site lowers its count here in the same change.
const IO_NONE_ALLOWLIST: &[(&str, usize)] = &[
    ("crates/bulkload-agent/src/freshness.rs", 2),
    ("crates/bulkload-agent/src/git_carry.rs", 1),
    ("crates/bulkload-agent/src/git_carry/batch_objects.rs", 3),
    ("crates/bulkload-agent/src/git_carry/estimate.rs", 5),
    (
        "crates/bulkload-agent/src/git_carry/estimate/stderr_store.rs",
        3,
    ),
    ("crates/bulkload-agent/src/git_carry/raw_tree.rs", 2),
    ("crates/bulkload-agent/src/git_carry/shallow.rs", 1),
    ("crates/bulkload-agent/src/hash.rs", 1),
    ("crates/bulkload-agent/src/io/durable.rs", 6),
    ("crates/bulkload-agent/src/main.rs", 3),
    ("crates/bulkload-agent/src/materialize.rs", 1),
    ("crates/bulkload-agent/src/provider_sqlite.rs", 5),
    ("crates/bulkload-agent/src/provider_sqlite/hydrate.rs", 15),
    ("crates/bulkload-agent/src/provider_sqlite/online.rs", 8),
    ("crates/bulkload-agent/src/transfer.rs", 1),
];

#[test]
fn bare_io_none_sites_only_shrink() {
    let found = io_none_sites();
    let allowed: BTreeMap<String, usize> = IO_NONE_ALLOWLIST
        .iter()
        .map(|(file, count)| ((*file).to_owned(), *count))
        .collect();
    let grown: Vec<_> = found
        .iter()
        .filter(|(file, count)| **count > allowed.get(*file).copied().unwrap_or(0))
        .collect();
    assert!(
        grown.is_empty(),
        "new bare Io(None) refusals -- raise a typed refusal, or use \
         .refuse_at(site) with the OS error: {grown:?}"
    );
    let shrunk: Vec<_> = allowed
        .iter()
        .filter(|(file, count)| found.get(*file).copied().unwrap_or(0) < **count)
        .collect();
    assert!(
        shrunk.is_empty(),
        "Io(None) sites went away -- lower IO_NONE_ALLOWLIST to the new counts \
         (found {found:?}): {shrunk:?}"
    );
    let total: usize = IO_NONE_ALLOWLIST.iter().map(|(_, count)| count).sum();
    assert!(
        total <= 70,
        "the allowlist only shrinks (was 70 at WP3 PR 2)"
    );
}

/// Fixed-seed proptest config (OI-1003-Q7): CI replays one bounded corpus.
/// Fold into the shared `test_support::prop_config` once it lands on main.
fn prop_config(cases: u32) -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        cases,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5733_7265_6675_7365),
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    }
}

proptest::proptest! {
    #![proptest_config(prop_config(256))]

    /// R-N121 as a property: whatever a Git child prints, its refusal shows
    /// only a code and a class from the closed set -- never a stderr byte.
    #[test]
    fn a_git_child_refusal_never_echoes_stderr(
        noise in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        phrase in proptest::sample::select(vec![
            "", "fatal: not a git repository", "ssh: connect to host x: Connection timed out",
            "Permission denied (publickey).", "fatal: bad object deadbeef",
            "ssh: Could not resolve hostname x", "secret-token-1234",
        ]),
    ) {
        let mut raw = noise;
        raw.extend_from_slice(phrase.as_bytes());
        let class = bulkload_agent::git_carry::estimate::StderrClass::of(&raw);
        let shown = BulkloadRefusal::GitChildFailed(class).to_string();
        let allowed = [
            "not_a_repository", "auth_failed", "host_unreachable", "timeout", "bad_object", "other",
        ]
        .map(|class| format!("GIT_CHILD_FAILED stderr_class={class}"));
        proptest::prop_assert!(allowed.contains(&shown), "{shown:?}");
        proptest::prop_assert_eq!(BulkloadRefusal::GitChildFailed(class).code(), "GIT_CHILD_FAILED");
    }
}
