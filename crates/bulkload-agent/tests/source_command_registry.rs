//! P76 SOURCE-COMMAND-REGISTRY (S2: never interrupt the source; OI-1003-Q5,
//! OI-1003-Q9, OI-1003-Q16 typed source access).
//!
//! Every child process the agent's non-test code builds is registered here.
//! The one source-safe Git builder is `git_carry::git`: it runs every Git
//! child with `--no-optional-locks`, the `git_env` table's `-c` overrides
//! (hooks, fsmonitor, automatic gc and maintenance off) and environment
//! (`GIT_OPTIONAL_LOCKS=0`, no lazy fetch, no prompt, no system or global
//! configuration), and a discovery ceiling (WP1, #145). The process enters
//! background CPU and IO priority before any verb runs (WP0(f),
//! OI-1003-Q17/Q25), and every child inherits it.
//!
//! The registry is a source scan, like `refusal_taxonomy.rs`: it reads the
//! agent's `src/` tree as text, drops comments, string and char literals,
//! `tests.rs` files, every module declared behind `#[cfg(test)]` or
//! `#[cfg(all(test, ..))]` and every item behind those attributes, then finds
//! each `Command::new`. A site is named by module, enclosing function and
//! program (`git_carry::estimate::local_probe("bash")`), never by line, so it
//! survives edits around it. Every site must be the sanctioned builder or an
//! entry in [`BYPASS_ALLOWLIST`], which may only shrink (bulkload#188).
//!
//! The static scan is backed by a dynamic one: a `git` wrapper first on
//! `PATH` records every Git child a real `git-export` and a real local
//! `git-carry-estimate` start, and each must carry the contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one source-safe Git builder.
const SANCTIONED: &str = "git_carry::git(\"git\")";

/// Child builders in non-test code that do not go through [`SANCTIONED`]
/// today, each with why it is tolerated. Filed as bulkload#188. This list may
/// only shrink: a new child goes through `git_carry::git`, or the S2 contract
/// is argued here and the ceiling in [`the_bypass_allowlist_only_shrinks`]
/// is not raised.
const BYPASS_ALLOWLIST: &[(&str, &str)] = &[
    (
        "git_carry::estimate::local_probe(\"bash\")",
        "Runs PROBE_SCRIPT on the local source. Every Git call in the script \
         goes through its `g` function, whose flags, exports and unsets are \
         tested entry for entry against git_env \
         (estimate::tests::probe_and_source_git_calls_are_hardened), but the \
         child itself is not built by git_carry::git, and its own env_remove \
         list is a hand copy of 9 of git_env::CLEARED's 11 keys.",
    ),
    (
        "git_carry::estimate::ssh_command(\"ssh\")",
        "Runs PROBE_SCRIPT on a remote source over ssh. The script carries \
         the git_env hardening, but the remote `bash -s` does not enter \
         background CPU or IO priority (WP0(f) covers only the local verb).",
    ),
    (
        "main::pull_command(\"ssh\")",
        "Starts the remote `bulkload-agent serve`, which is the source reader \
         and enters background priority itself (OI-1003-Q25). The ssh child \
         touches no local source.",
    ),
    (
        "provider_sqlite::hydrate::hydrate_one(program)",
        "A caller-named decompressor (`-dc --`) reads a retained compressed \
         rollout. It is not one of WP0(b)'s typed source access kinds (file \
         read, allowlisted git read, SQLite backup), though it only reads \
         its input.",
    ),
];

/// Git subcommands that rewrite or repack a repository's store. No non-test
/// code may name one, for any repository.
const MAINTENANCE: &[&str] = &[
    "gc",
    "maintenance",
    "repack",
    "prune",
    "prune-packed",
    "commit-graph",
    "multi-pack-index",
    "pack-refs",
];

fn agent_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

// ---------------------------------------------------------------- the scan

/// One source file: its text, its text with comments, literals and test items
/// blanked (same byte offsets), and its string literals' content spans.
struct Scanned {
    original: String,
    code: String,
    literals: Vec<Range<usize>>,
    test_modules: Vec<String>,
}

impl Scanned {
    fn new(original: String) -> Self {
        let (masked, literals) = mask(&original);
        let spans = test_spans(&masked);
        let mut test_modules = Vec::new();
        for span in &spans {
            test_modules.extend(declared_modules(&masked[span.clone()]));
        }
        let mut code = masked.into_bytes();
        for span in &spans {
            for byte in &mut code[span.clone()] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
        let code = String::from_utf8(code).unwrap();
        let literals = literals
            .into_iter()
            .filter(|literal| !spans.iter().any(|span| span.contains(&literal.start)))
            .collect();
        Self {
            original,
            code,
            literals,
            test_modules,
        }
    }

    fn literal(&self, span: &Range<usize>) -> &str {
        &self.original[span.clone()]
    }
}

/// `source` with comments and string/char literal contents blanked to spaces
/// (newlines kept, byte offsets unchanged), and the content span of every
/// string literal.
fn mask(source: &str) -> (String, Vec<Range<usize>>) {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut literals = Vec::new();
    let blank = |out: &mut Vec<u8>, range: Range<usize>| {
        for byte in &mut out[range] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        let ident_before = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if b == b'/' && next == Some(b'/') {
            let end = bytes[i..]
                .iter()
                .position(|c| *c == b'\n')
                .map_or(bytes.len(), |at| i + at);
            blank(&mut out, i..end);
            i = end;
        } else if b == b'/' && next == Some(b'*') {
            let start = i;
            let mut depth = 0_usize;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start..i.min(bytes.len()));
        } else if (b == b'r' || b == b'b') && !ident_before && raw_open(&bytes[i..]).is_some() {
            let (open, hashes) = raw_open(&bytes[i..]).unwrap();
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            let start = i;
            let content = i + open;
            let end = bytes[content..]
                .windows(close.len())
                .position(|window| window == close.as_slice())
                .map_or(bytes.len(), |at| content + at);
            literals.push(content..end);
            i = (end + close.len()).min(bytes.len());
            blank(&mut out, start..i);
        } else if b == b'"' || (b == b'b' && next == Some(b'"') && !ident_before) {
            let start = i;
            let content = if b == b'b' { i + 2 } else { i + 1 };
            let mut j = content;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(bytes.len());
            literals.push(content..end);
            i = (end + 1).min(bytes.len());
            blank(&mut out, start..i);
        } else if b == b'\'' && char_literal_len(&bytes[i..]).is_some() {
            let len = char_literal_len(&bytes[i..]).unwrap();
            blank(&mut out, i..i + len);
            i += len;
        } else {
            i += 1;
        }
    }
    (String::from_utf8(out).unwrap(), literals)
}

/// For `r"`, `r#"`, `br"`, `br#"`: the opening's length and its `#` count.
fn raw_open(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = usize::from(bytes.first() == Some(&b'b'));
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = bytes[i..].iter().take_while(|b| **b == b'#').count();
    i += hashes;
    (bytes.get(i) == Some(&b'"')).then_some((i + 1, hashes))
}

/// The length of a char literal at the start of `bytes`, or `None` for a
/// lifetime.
fn char_literal_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) == Some(&b'\\') {
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

/// The end (exclusive) of the brace block opening at `code[open]`.
fn block_end(code: &str, open: usize) -> usize {
    let mut depth = 0_usize;
    for (offset, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + offset + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// The spans of every item behind `#[cfg(test)]` or `#[cfg(all(test, ..))]`,
/// from the attribute to the end of the item's block, or to its `;`.
fn test_spans(code: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut from = 0;
    loop {
        let Some(at) = ["#[cfg(test)]", "#[cfg(all(test"]
            .iter()
            .filter_map(|marker| code[from..].find(marker).map(|at| from + at))
            .min()
        else {
            return spans;
        };
        let after_attr = at + code[at..].find(']').unwrap() + 1;
        let brace = code[after_attr..].find('{').map(|x| after_attr + x);
        let semi = code[after_attr..].find(';').map(|x| after_attr + x);
        let end = match (brace, semi) {
            (Some(brace), Some(semi)) if semi < brace => semi + 1,
            (Some(brace), _) => block_end(code, brace),
            (None, Some(semi)) => semi + 1,
            (None, None) => code.len(),
        };
        spans.push(at..end);
        from = end;
    }
}

/// Whether `code[at]` starts a token (no identifier character before it).
fn token_start(code: &str, at: usize) -> bool {
    at == 0 || {
        let before = code.as_bytes()[at - 1];
        !(before.is_ascii_alphanumeric() || before == b'_')
    }
}

fn identifier(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

/// The names of `mod NAME;` declarations in `code`.
fn declared_modules(code: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("mod ").map(|at| from + at) {
        from = at + 4;
        if !token_start(code, at) {
            continue;
        }
        let rest = code[from..].trim_start();
        let name = identifier(rest);
        if !name.is_empty() && rest[name.len()..].trim_start().starts_with(';') {
            names.push(name.to_owned());
        }
    }
    names
}

/// Every `.rs` file under `root`.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.sort();
    out
}

/// The directory holding `file`'s child modules.
fn module_dir(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap();
    if stem == "mod" || stem == "lib" || stem == "main" {
        file.parent().unwrap().to_path_buf()
    } else {
        file.with_extension("")
    }
}

/// The module path of `file` under `root`: `git_carry/estimate.rs` is
/// `git_carry::estimate`, `io/mod.rs` is `io`, `main.rs` is `main`.
fn module_path(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap().with_extension("");
    let mut parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.len() > 1 && parts.last().is_some_and(|last| last == "mod") {
        parts.pop();
    }
    parts.join("::")
}

/// Every non-test source file under `root`, scanned. A file is test code when
/// it is a `tests.rs`, when its parent declares it behind `#[cfg(test)]`, or
/// when it lies under such a module's directory.
fn non_test_sources(root: &Path) -> BTreeMap<PathBuf, Scanned> {
    let mut scanned: BTreeMap<PathBuf, Scanned> = rust_files(root)
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            (path, Scanned::new(text))
        })
        .collect();
    let mut test_dirs = Vec::new();
    for (path, file) in &scanned {
        let dir = module_dir(path);
        for name in &file.test_modules {
            test_dirs.push(dir.join(name));
            test_dirs.push(dir.join(format!("{name}.rs")));
        }
    }
    scanned.retain(|path, _| {
        path.file_name().is_none_or(|name| name != "tests.rs")
            && !test_dirs.iter().any(|dir| path.starts_with(dir))
    });
    scanned
}

/// A child builder in non-test code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    id: String,
    shown: String,
}

/// The innermost `fn` whose body holds `at`.
fn enclosing_fn(code: &str, at: usize) -> String {
    let mut best: Option<(usize, String)> = None;
    let mut from = 0;
    while let Some(found) = code[from..].find("fn ").map(|x| from + x) {
        from = found + 3;
        if !token_start(code, found) {
            continue;
        }
        let name = identifier(code[from..].trim_start()).to_owned();
        if name.is_empty() || found > at {
            continue;
        }
        let brace = code[from..].find('{').map(|x| from + x);
        let semi = code[from..].find(';').map(|x| from + x);
        let Some(open) = brace.filter(|open| semi.is_none_or(|semi| *open < semi)) else {
            continue;
        };
        if open < at
            && at < block_end(code, open)
            && best.as_ref().is_none_or(|(start, _)| found > *start)
        {
            best = Some((found, name));
        }
    }
    best.map_or_else(|| "<module>".to_owned(), |(_, name)| name)
}

/// Every `Command::new` in `file`'s non-test code, named
/// `module::function(program)`.
fn sites_in(module: &str, shown: &str, file: &Scanned) -> Vec<Site> {
    let code = &file.code;
    let mut sites = Vec::new();
    let needle = "Command::new";
    let mut from = 0;
    while let Some(at) = code[from..].find(needle).map(|x| from + x) {
        from = at + needle.len();
        let after = code[from..].trim_start();
        let program = if after.starts_with('(') {
            let open = from + code[from..].find('(').unwrap();
            let mut depth = 0_i32;
            let mut close = code.len();
            for (offset, c) in code[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + offset;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            file.original[open + 1..close].trim().to_owned()
        } else {
            "<fn pointer>".to_owned()
        };
        let line = code[..at].matches('\n').count() + 1;
        sites.push(Site {
            id: format!("{module}::{}({program})", enclosing_fn(code, at)),
            shown: format!("{shown}:{line}"),
        });
    }
    sites
}

fn all_sites() -> Vec<Site> {
    let root = agent_src();
    let mut sites = Vec::new();
    for (path, file) in non_test_sources(&root) {
        let module = module_path(&root, &path);
        let shown = path.strip_prefix(&root).unwrap().display().to_string();
        sites.extend(sites_in(&module, &shown, &file));
    }
    sites.sort();
    sites
}

// --------------------------------------------------------------- the tests

/// The scan finds a child in non-test code, names it by function and
/// program, and ignores prose, literals and test items. This is also the
/// mutation the registry must catch: a Git child built without `git_env`.
#[test]
fn the_scan_finds_a_new_child_and_skips_test_code() {
    let code = "use std::process::Command;\n\
        fn git(repo: &Path) -> Command { let mut c = Command::new(\"git\"); c }\n\
        pub fn sneaky(repo: &Path) -> bool {\n\
            let probe = |x: u8| x + 1;\n\
            Command::new(\"git\").arg(\"status\").status().is_ok()\n\
        }\n\
        // Command::new(\"in-a-comment\")\n\
        const S: &str = \"Command::new(\\\"in-a-literal\\\")\";\n\
        const R: &str = r#\"Command::new(\"in-a-raw-literal\")\"#;\n\
        #[cfg(test)]\nmod tests { fn t() { Command::new(\"in-a-test\"); } }\n\
        #[cfg(all(test, feature = \"io-trace\"))]\nfn traced() { Command::new(\"traced\"); }\n\
        #[cfg(test)]\n#[allow(clippy::panic)]\nmod more_tests;\n\
        fn later() { let _ = ['{', '}']; Command::new(program); }\n";
    let file = Scanned::new(code.to_owned());
    let ids: Vec<String> = sites_in("m", "m.rs", &file)
        .into_iter()
        .map(|site| site.id)
        .collect();
    assert_eq!(
        ids,
        ["m::git(\"git\")", "m::sneaky(\"git\")", "m::later(program)"]
    );
    assert_eq!(file.test_modules, ["more_tests"]);
    let literals: Vec<&str> = file
        .literals
        .iter()
        .map(|span| file.literal(span))
        .collect();
    assert!(literals.contains(&"status"), "{literals:?}");
    assert!(!literals.contains(&"in-a-test"), "{literals:?}");
}

/// The scan reaches the whole agent and drops its test files: it sees the
/// sanctioned builder, and none of the many fixture children in tests.
#[test]
fn the_scan_reads_the_agent_and_drops_test_files() {
    let root = agent_src();
    let sources = non_test_sources(&root);
    let names: BTreeSet<String> = sources
        .keys()
        .map(|path| module_path(&root, path))
        .collect();
    for module in [
        "main",
        "lib",
        "git_carry",
        "git_carry::estimate",
        "transfer",
        "walk",
    ] {
        assert!(names.contains(module), "{module} not scanned: {names:?}");
    }
    for module in [
        "git_carry::source_inert_tests",
        "git_carry::refs_scale_tests",
        "io::tests",
        "transfer::tests",
        "materialize::adoption_power_loss",
    ] {
        assert!(!names.contains(module), "test module {module} scanned");
    }
    assert!(all_sites().iter().any(|site| site.id == SANCTIONED));
}

/// P76: every child the agent builds outside tests is the sanctioned Git
/// builder or a named, argued bypass. A new `Command::new("git")` anywhere
/// in non-test code fails here.
#[test]
fn every_child_goes_through_the_source_safe_builder() {
    let sites = all_sites();
    let allowed: BTreeSet<&str> = BYPASS_ALLOWLIST.iter().map(|(id, _)| *id).collect();
    let sanctioned: Vec<&Site> = sites.iter().filter(|site| site.id == SANCTIONED).collect();
    assert_eq!(
        sanctioned.len(),
        1,
        "exactly one sanctioned Git builder: {sanctioned:?}"
    );
    let unregistered: Vec<&Site> = sites
        .iter()
        .filter(|site| site.id != SANCTIONED && !allowed.contains(site.id.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "child processes built outside git_carry::git (S2, OI-1003-Q16): build \
         Git children with git_carry::git, or argue the bypass in \
         BYPASS_ALLOWLIST without raising its ceiling: {unregistered:#?}"
    );
    let found: BTreeSet<&str> = sites.iter().map(|site| site.id.as_str()).collect();
    let stale: Vec<&&str> = allowed.iter().filter(|id| !found.contains(**id)).collect();
    assert!(
        stale.is_empty(),
        "bypasses that no longer exist -- remove them from BYPASS_ALLOWLIST \
         (it only shrinks): {stale:?}"
    );
}

#[test]
fn the_bypass_allowlist_only_shrinks() {
    let ids: BTreeSet<&str> = BYPASS_ALLOWLIST.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), BYPASS_ALLOWLIST.len(), "duplicate allowlist ids");
    assert!(
        BYPASS_ALLOWLIST.len() <= 4,
        "the bypass allowlist only shrinks (4 at P76, bulkload#188)"
    );
    for (id, why) in BYPASS_ALLOWLIST {
        assert!(why.len() > 40, "{id}: say why the bypass is tolerated");
    }
}

/// The literals inside `code[range]` of `file`.
fn literals_in<'a>(file: &'a Scanned, range: &Range<usize>) -> Vec<&'a str> {
    file.literals
        .iter()
        .filter(|span| range.contains(&span.start))
        .map(|span| file.literal(span))
        .collect()
}

/// The span of the block opened by the first `opener` in `file`'s code.
fn block_of(file: &Scanned, opener: &str) -> Range<usize> {
    let at = file
        .code
        .find(opener)
        .unwrap_or_else(|| panic!("`{opener}` not found"));
    let open = at + file.code[at..].find('{').unwrap();
    at..block_end(&file.code, open)
}

/// The span from `opener` to the end of its item (`;`).
fn item_of(file: &Scanned, within: &Range<usize>, opener: &str) -> Range<usize> {
    let at = within.start
        + file.code[within.clone()]
            .find(opener)
            .unwrap_or_else(|| panic!("`{opener}` not found"));
    at..at + file.code[at..].find("];").unwrap()
}

/// The sanctioned builder holds the S2 contract (#145): optional locks off
/// twice over, hooks, fsmonitor, automatic gc and maintenance off, no lazy
/// fetch, no prompt, no system or global config, the redirecting variables
/// cleared, and a discovery ceiling.
#[test]
fn the_source_safe_builder_holds_the_s2_contract() {
    let root = agent_src();
    let sources = non_test_sources(&root);
    let file = &sources[&root.join("git_carry.rs")];

    let table = block_of(file, "mod git_env {");
    let config = literals_in(file, &item_of(file, &table, "const CONFIG"));
    for required in [
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "gc.auto=0",
        "maintenance.auto=false",
    ] {
        assert!(
            config.contains(&required),
            "git_env::CONFIG lacks {required}"
        );
    }
    let set = literals_in(file, &item_of(file, &table, "const SET"));
    let pairs: BTreeMap<&str, &str> = set.chunks_exact(2).map(|pair| (pair[0], pair[1])).collect();
    for (key, value) in [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
    ] {
        assert_eq!(pairs.get(key), Some(&value), "git_env::SET {key}");
    }
    let cleared = literals_in(file, &item_of(file, &table, "const CLEARED"));
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(cleared.contains(&key), "git_env::CLEARED lacks {key}");
    }

    let builder = block_of(file, "fn git(repo: &Path) -> Command {");
    let body = &file.code[builder.clone()];
    for uses in [
        "git_env::CLEARED",
        "env_remove",
        "git_env::CONFIG",
        "git_env::SET",
    ] {
        assert!(body.contains(uses), "git_carry::git does not use {uses}");
    }
    let literals = literals_in(file, &builder);
    for required in [
        "git",
        "--no-optional-locks",
        "-c",
        "GIT_CEILING_DIRECTORIES",
    ] {
        assert!(
            literals.contains(&required),
            "git_carry::git lacks {required:?}"
        );
    }
}

/// No non-test code names a store-rewriting Git subcommand, and nothing but
/// the `git_env` table names `GIT_OPTIONAL_LOCKS` (so nothing unsets or
/// overrides it).
#[test]
fn no_child_names_maintenance_or_overrides_optional_locks() {
    let root = agent_src();
    let mut found = Vec::new();
    for (path, file) in non_test_sources(&root) {
        let shown = path.strip_prefix(&root).unwrap().display().to_string();
        let table = (path == root.join("git_carry.rs")).then(|| block_of(&file, "mod git_env {"));
        for span in &file.literals {
            let literal = file.literal(span);
            let in_table = table
                .as_ref()
                .is_some_and(|table| table.contains(&span.start));
            if MAINTENANCE.contains(&literal)
                || (literal == "GIT_OPTIONAL_LOCKS" && !in_table)
                || literal.starts_with("--no-no-optional-locks")
            {
                let line = file.code[..span.start].matches('\n').count() + 1;
                found.push(format!("{shown}:{line}: {literal:?}"));
            }
        }
    }
    assert!(found.is_empty(), "S2 contract breaches: {found:#?}");
}

// ------------------------------------------------------- the dynamic check

/// A private scratch root, removed on drop.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Root {
    let root = std::env::temp_dir().join(format!(
        "bulkload-p76-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    Root(root.canonicalize().unwrap())
}

/// A fixture Git child (test identity, no user configuration).
fn fixture_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Test", "-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false", "-C"])
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The first executable `git` on `PATH`.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .expect("git on PATH")
}

/// One recorded Git child: its arguments and the S2 environment it saw.
#[derive(Debug)]
struct Recorded {
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

const RECORDED_ENV: &[&str] = &[
    "GIT_OPTIONAL_LOCKS",
    "GIT_NO_LAZY_FETCH",
    "GIT_TERMINAL_PROMPT",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
];

/// Install a `git` wrapper in `bin` that appends one line per call to `log`
/// (tab-separated: the recorded environment, `--`, then the arguments) and
/// then runs the real Git with the same arguments.
fn install_wrapper(bin: &Path, log: &Path) {
    fs::create_dir_all(bin).unwrap();
    let mut env = String::new();
    for key in RECORDED_ENV {
        let _ = write!(env, "{key}=${{{key}-<unset>}}\t");
    }
    let script = format!(
        "#!/bin/sh\n\
         line=\"{env}--\"\n\
         for arg in \"$@\"; do line=\"$line\t$arg\"; done\n\
         printf '%s\\n' \"$line\" >> '{log}'\n\
         exec '{git}' \"$@\"\n",
        env = env,
        log = log.display(),
        git = real_git().display(),
    );
    let wrapper = bin.join("git");
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
}

fn read_log(log: &Path) -> Vec<Recorded> {
    let text = fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let mut env = BTreeMap::new();
            for field in fields.by_ref() {
                if field == "--" {
                    break;
                }
                let (key, value) = field.split_once('=').unwrap();
                env.insert(key.to_owned(), value.to_owned());
            }
            Recorded {
                args: fields.map(str::to_owned).collect(),
                env,
            }
        })
        .collect()
}

/// The subcommand of a recorded call: the first argument that is neither an
/// option nor an option's value.
fn subcommand(args: &[String]) -> Option<&str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "-C" => {
                iter.next();
            }
            option if option.starts_with('-') => {}
            command => return Some(command),
        }
    }
    None
}

/// Every breach of the S2 contract in one recorded call.
fn breaches(call: &Recorded) -> Vec<String> {
    let mut out = Vec::new();
    let sub = subcommand(&call.args);
    for (key, value) in [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ] {
        if call.env.get(key).map(String::as_str) != Some(value) {
            out.push(format!("{key}={:?}", call.env.get(key)));
        }
    }
    if let Some(sub) = sub.filter(|sub| MAINTENANCE.contains(sub)) {
        out.push(format!("maintenance subcommand {sub}"));
    }
    // `git version` reads no repository; every other call carries the flags.
    if sub != Some("version") {
        let configs: BTreeSet<&str> = call
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].as_str())
            .collect();
        for required in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "gc.auto=0",
            "maintenance.auto=false",
        ] {
            if !configs.contains(required) {
                out.push(format!("no -c {required}"));
            }
        }
        if call.args.first().map(String::as_str) != Some("--no-optional-locks") {
            out.push("no leading --no-optional-locks".to_owned());
        }
    }
    out
}

/// P76, dynamic half: every Git child a real `git-export` and a real local
/// `git-carry-estimate` start carries the S2 contract. A Git child built
/// without `git_env` fails here once a verb runs it.
#[test]
fn every_git_child_of_a_capture_and_an_estimate_is_hardened() {
    let root = scratch("children");
    let source = root.0.join("source");
    let destination = root.0.join("destination");
    for repo in [&source, &destination] {
        fs::create_dir(repo).unwrap();
        fixture_git(repo, &["init", "--quiet", "--template=", "-b", "main"]);
    }
    fs::write(source.join("tracked"), b"tracked\n").unwrap();
    fixture_git(&source, &["add", "tracked"]);
    fixture_git(&source, &["commit", "--quiet", "-m", "one"]);
    fs::write(source.join("tracked"), b"staged\n").unwrap();
    fixture_git(&source, &["add", "tracked"]);
    fs::write(source.join("tracked"), b"worktree\n").unwrap();
    fs::write(source.join("untracked"), b"untracked\n").unwrap();

    let bin = root.0.join("bin");
    let log = root.0.join("git.log");
    install_wrapper(&bin, &log);
    let mut path = std::ffi::OsString::from(bin.as_os_str());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap());

    let capture = root.0.join("capture");
    let export = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env("PATH", &path)
        .arg("git-export")
        .arg(&source)
        .arg(&capture)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "git-export: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let exported = read_log(&log).len();
    assert!(
        exported >= 5,
        "the wrapper saw only {exported} Git children"
    );

    let estimate = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env("PATH", &path)
        .arg("git-carry-estimate")
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        estimate.status.success(),
        "git-carry-estimate: {}",
        String::from_utf8_lossy(&estimate.stderr)
    );
    let calls = read_log(&log);
    assert!(calls.len() > exported, "the estimate started no Git child");

    let breached: Vec<(Vec<String>, Vec<String>)> = calls
        .iter()
        .map(|call| (call.args.clone(), breaches(call)))
        .filter(|(_, found)| !found.is_empty())
        .collect();
    assert!(
        breached.is_empty(),
        "Git children without the S2 contract: {breached:#?}"
    );
}

/// The dynamic check's oracle refuses each breach on its own.
#[test]
fn the_dynamic_oracle_names_each_breach() {
    let good_env: BTreeMap<String, String> = [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let good_args: Vec<String> = [
        "--no-optional-locks",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "-C",
        "/r",
        "rev-parse",
        "HEAD",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let good = Recorded {
        args: good_args.clone(),
        env: good_env.clone(),
    };
    assert!(breaches(&good).is_empty(), "{:?}", breaches(&good));

    let mut env = good_env.clone();
    env.insert("GIT_OPTIONAL_LOCKS".to_owned(), "<unset>".to_owned());
    assert_eq!(
        breaches(&Recorded {
            args: good_args.clone(),
            env
        })
        .len(),
        1
    );

    let bare = Recorded {
        args: vec!["-C".to_owned(), "/r".to_owned(), "status".to_owned()],
        env: good_env.clone(),
    };
    assert_eq!(breaches(&bare).len(), 5, "{:?}", breaches(&bare));

    let mut args = good_args;
    *args.last_mut().unwrap() = "--auto".to_owned();
    *args.iter_mut().rev().nth(1).unwrap() = "gc".to_owned();
    let gc = Recorded {
        args,
        env: good_env.clone(),
    };
    assert_eq!(breaches(&gc), ["maintenance subcommand gc"]);

    let version = Recorded {
        args: vec!["version".to_owned()],
        env: good_env,
    };
    assert!(breaches(&version).is_empty());
}
