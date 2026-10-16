//! Estate hand-off block (Linear TIN-6569, operator ruling OI-1003-Q183,
//! 2026-10-16; design `docs/agent-notes/2026-10-16-estate-handoff-design.md`):
//! `estate-handoff` writes a dated, delimited block at byte 0 of `AGENTS.md`
//! and `CLAUDE.md` in each landed workspace, recorded in its own ledger, and
//! converge keeps an intact block instead of refusing the seat.
//!
//! Rows (the design's section 12):
//!
//! - H0 (control): a naive prepend with no record refuses
//!   `GIT_DESTINATION_MODIFIED` at the next converge. Passes before the
//!   feature: it documents the problem.
//! - H1: the block at byte 0 of both files in a main and a linked worktree,
//!   remainders byte-identical, records written, everything else frozen.
//! - H2: an unchanged rerun reads 0 destination bytes and writes nothing,
//!   once one rerun has re-dated a record whose prints were racy; a no-op
//!   `estate-apply` in between is no reason to re-render.
//! - H3: a source change elsewhere converges with the block kept, and the
//!   H step re-renders it in the same apply.
//! - H4: a source edit of `AGENTS.md` converges; the H step puts the block
//!   back on the new body in the same apply.
//! - H5: the source deletes `CLAUDE.md`, the hand-off creates it, the source
//!   adds it back: the created file is lifted, the add lands, and the H
//!   step prefixes it.
//! - H6: a user edit below the block refuses MODIFIED with both kept; a
//!   hand-off rerun re-renders the block alone.
//! - H7: a user edit inside the block refuses `HANDOFF_BLOCK_MODIFIED`
//!   (hand-off and `--clear`), MODIFIED (converge); removed by hand it is
//!   retired, `--renew` puts it back.
//! - H8: `--clear` restores the bytes and `git status`; a second clear is a
//!   no-op.
//! - H9: `CLAUDE.md -> AGENTS.md` is covered; a link outside the worktree
//!   and a directory refuse `HANDOFF_TARGET_UNSUPPORTED` with nothing
//!   written.
//! - H10: missing files are created untracked (0644) and cleared away.
//! - H11: privacy: a remote not under `--private-remote` refuses
//!   `HANDOFF_NOT_PRIVATE`; a token in a URL never appears anywhere.
//! - H12: `--check` reports `staged` and `committed` and exits with
//!   `HANDOFF_MARKER_COMMITTED`.
//! - H13: `--check` and `--dry-run` write nothing.
//! - H14: `estate-diff` over a handed-off item says `identical`, reading no
//!   destination byte but the targets' (whose record print is racy against
//!   the record, so `prove` reads their content once, as it reads a seat
//!   right after a landing).
//! - H15: `--sibling` names the live copy and its HEAD, and never writes it.
//! - H16: `--readme` is off by default and never creates a README.
//! - H17: a handed-off landing moved to its live name and re-keyed (A11b)
//!   proves with its block on, and the H step re-renders it for the new
//!   item and path.
//! - H18: the render is deterministic under `--as-of`.
//! - H19: the block's commands run as printed from `/`, and every path in
//!   it is absolute even when the hand-off was given relative ones.
//!
//! Fixture repositories are created and destroyed here; their Git runs with
//! no global configuration and no hooks (R-N98).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

#[allow(dead_code)]
#[path = "estate_adopt/gftb.rs"]
mod gftb;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use gftb::{freeze_all, git, settle};

const SOURCE: &str = "neo";
const BEGIN: &[u8] = b"<!-- bulkload-handoff:v1 BEGIN ";
const END: &[u8] = b"<!-- bulkload-handoff:v1 END ";
const TOKEN: &str = "BULKLOAD-HANDOFF-DO-NOT-COMMIT";

const AGENTS: &[u8] = b"# Agents\n\nRepository rules.\n";
const CLAUDE: &[u8] = b"See AGENTS.md.\n";

/// One verb's run: success, stdout, stderr and its `counters` line.
struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
    counters: BTreeMap<String, u64>,
}

fn agent(args: &[OsString]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let counters = stderr
        .lines()
        .find(|line| line.starts_with("counters "))
        .map(|line| {
            line.split(' ')
                .filter_map(|pair| pair.split_once('='))
                .filter_map(|(key, value)| Some((key.to_owned(), value.parse().ok()?)))
                .collect()
        })
        .unwrap_or_default();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
        counters,
    }
}

fn os(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

/// A source repository with a linked worktree, planned as two items (the
/// main and the linked worktree) into one destination, captured and
/// landed.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// A world whose source `shape` adjusts before the first commit.
    fn with(shape: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("bulkload-handoff-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let world = Self { _dir: dir, root };
        let src = world.src();
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(src.join("AGENTS.md"), AGENTS).unwrap();
        fs::write(src.join("CLAUDE.md"), CLAUDE).unwrap();
        fs::write(src.join("README.md"), b"# Readme\n").unwrap();
        fs::write(src.join("a.txt"), b"alpha\n").unwrap();
        shape(&src);
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "--quiet", "-m", "c0"]);
        git(&src, &["branch", "side"]);
        git(
            &src,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "wt",
                world.src_wt().to_str().unwrap(),
            ],
        );
        fs::write(src.join("notes.txt"), b"untracked\n").unwrap();
        fs::create_dir_all(world.root.join("dst")).unwrap();
        fs::create_dir_all(world.root.join("carry")).unwrap();
        let added = agent(&os(&[
            &"estate-add-batch",
            &world.plan(),
            &world.src(),
            &world.dst(),
            &world.dst(),
            &world.src_wt(),
            &world.dst(),
            &world.dst_wt(),
        ]));
        assert!(added.ok, "{}", added.stderr);
        settle();
        world.capture();
        let applied = world.apply();
        assert!(
            applied.ok,
            "first apply: {}\n{}",
            applied.stdout, applied.stderr
        );
        world
    }

    fn src(&self) -> PathBuf {
        self.root.join("src")
    }
    fn src_wt(&self) -> PathBuf {
        self.root.join("src-wt")
    }
    fn dst(&self) -> PathBuf {
        self.root.join("dst/main")
    }
    fn dst_wt(&self) -> PathBuf {
        self.root.join("dst/wt")
    }
    fn plan(&self) -> PathBuf {
        self.root.join("carry/plan.postcard")
    }
    fn corpus(&self) -> PathBuf {
        self.root.join("carry/corpus")
    }
    fn state(&self) -> PathBuf {
        self.root.join("apply-state")
    }

    fn capture(&self) -> Ran {
        let ran = agent(&os(&[
            &"estate-capture",
            &self.plan(),
            &self.root.join("carry/private"),
            &self.corpus(),
            &"1",
        ]));
        assert!(ran.ok, "capture: {}\n{}", ran.stdout, ran.stderr);
        ran
    }

    fn apply(&self) -> Ran {
        agent(&os(&[
            &"estate-apply",
            &self.plan(),
            &self.corpus(),
            &self.state(),
            &SOURCE,
            &"1",
        ]))
    }

    fn handoff(&self, extra: &[&str]) -> Ran {
        let mut list: Vec<OsString> = vec!["estate-handoff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(os(&[&self.plan(), &self.corpus(), &self.state(), &SOURCE]));
        agent(&list)
    }

    fn diff(&self) -> Ran {
        let ran = agent(&os(&[
            &"estate-diff",
            &self.plan(),
            &self.corpus(),
            &SOURCE,
            &self.state(),
        ]));
        assert!(ran.ok, "diff: {}\n{}", ran.stdout, ran.stderr);
        ran
    }

    /// A source change elsewhere (`a.txt`), captured after a tick.
    fn source_change(&self, content: &[u8]) {
        fs::write(self.src().join("a.txt"), content).unwrap();
        git(&self.src(), &["commit", "--quiet", "-am", "a change"]);
        settle();
        self.capture();
    }
}

/// The block at byte 0 of `bytes`, and the remainder after it.
fn split(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if !bytes.starts_with(BEGIN) {
        return None;
    }
    let at = bytes.windows(END.len()).position(|w| w == END)?;
    let line_end = at + bytes[at..].iter().position(|b| *b == b'\n')? + 1;
    if bytes.get(line_end) != Some(&b'\n') {
        return None;
    }
    Some(bytes.split_at(line_end + 1))
}

fn block_of(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).unwrap();
    split(&bytes)
        .unwrap_or_else(|| panic!("no block at byte 0 of {}", path.display()))
        .0
        .to_vec()
}

fn remainder_of(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).unwrap();
    split(&bytes).map_or_else(|| bytes.clone(), |(_, rest)| rest.to_vec())
}

/// The JSON row of `workspace` in a hand-off's stdout.
fn row<'a>(stdout: &'a str, workspace: &Path) -> &'a str {
    let needle = format!("\"workspace\":\"{}\"", workspace.display());
    stdout
        .lines()
        .find(|line| line.contains("\"kind\":\"workspace\"") && line.contains(&needle))
        .unwrap_or_else(|| panic!("no hand-off row for {needle} in {stdout}"))
}

/// Every entry under `root` with its `lstat` identity, except the targets,
/// the worktree roots and bulkload's own ledger directories: what a
/// hand-off must leave exactly as it was.
fn frozen_but_targets(
    root: &Path,
    roots: &[PathBuf],
) -> Vec<(PathBuf, u64, i64, i64, i64, u64, u32)> {
    freeze_all(root)
        .into_iter()
        .filter(|entry| {
            let path = &entry.0;
            let name = path.file_name().unwrap().to_string_lossy();
            !(name == "AGENTS.md"
                || name == "CLAUDE.md"
                || roots.contains(path)
                || path.components().any(|part| part.as_os_str() == "bulkload"))
        })
        .collect()
}

fn status(worktree: &Path) -> String {
    git(worktree, &["status", "--porcelain=v1", "-uall"])
}

#[test]
fn h0_a_naive_prepend_without_a_record_refuses_modified() {
    let world = World::new();
    let agents = world.dst().join("AGENTS.md");
    let mut bytes = b"<!-- a hand-written hand-off note -->\n\n".to_vec();
    bytes.extend_from_slice(AGENTS);
    fs::write(&agents, &bytes).unwrap();
    world.source_change(b"alpha 2\n");
    let applied = world.apply();
    assert!(!applied.ok);
    assert!(
        applied.stdout.contains("GIT_DESTINATION_MODIFIED"),
        "{}",
        applied.stdout
    );
    assert_eq!(
        fs::read(&agents).unwrap(),
        bytes,
        "the operator's edit is kept"
    );
}

#[test]
fn h1_the_block_is_prepended_at_byte_zero_and_recorded() {
    let world = World::new();
    let roots = vec![world.dst(), world.dst_wt()];
    let frozen = frozen_but_targets(&world.root.join("dst"), &roots);
    let heads: Vec<String> = roots
        .iter()
        .map(|w| git(w, &["rev-parse", "HEAD"]) + &git(w, &["ls-files", "--stage"]))
        .collect();
    let refs = git(&world.dst(), &["for-each-ref"]);
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    for worktree in &roots {
        let line = row(&ran.stdout, worktree);
        assert!(line.contains("\"outcome\":\"written\""), "{line}");
        for (name, body) in [("AGENTS.md", AGENTS), ("CLAUDE.md", CLAUDE)] {
            let path = worktree.join(name);
            let block = String::from_utf8(block_of(&path)).unwrap();
            assert_eq!(remainder_of(&path), body, "{}", path.display());
            assert!(block.contains(TOKEN), "{block}");
            assert!(block.contains("OI-1003-Q183"), "{block}");
            assert!(block.contains("TIN-6569"), "{block}");
            assert!(
                block.contains(&format!("`{}`", worktree.display())),
                "the landing's absolute path: {block}"
            );
            assert!(block.contains("estate-handoff --clear"), "{block}");
            assert!(block.contains("estate-diff"), "{block}");
            assert!(
                block.contains(&world.plan().display().to_string()),
                "the plan's absolute path: {block}"
            );
        }
        // The landing's source path on the origin host.
        let block = String::from_utf8(block_of(&worktree.join("AGENTS.md"))).unwrap();
        let source = if worktree == &world.dst() {
            world.src()
        } else {
            world.src_wt()
        };
        assert!(block.contains(&source.display().to_string()), "{block}");
        // Both files carry the same block.
        assert_eq!(
            block_of(&worktree.join("AGENTS.md")),
            block_of(&worktree.join("CLAUDE.md"))
        );
    }
    assert!(world
        .dst()
        .join(".git/bulkload/v1/neo/handoff.landed")
        .is_file());
    assert!(world
        .dst()
        .join(".git/worktrees/wt/bulkload/v1/neo/handoff.landed")
        .is_file());
    assert!(!world
        .dst()
        .join(".git/bulkload/v1/neo/handoff.intent")
        .exists());
    // Nothing but the targets and bulkload's ledger directories moved.
    assert_eq!(frozen_but_targets(&world.root.join("dst"), &roots), frozen);
    let after: Vec<String> = roots
        .iter()
        .map(|w| git(w, &["rev-parse", "HEAD"]) + &git(w, &["ls-files", "--stage"]))
        .collect();
    assert_eq!(after, heads);
    assert_eq!(git(&world.dst(), &["for-each-ref"]), refs);
    // The source is never written.
    assert!(split(&fs::read(world.src().join("AGENTS.md")).unwrap()).is_none());
}

#[test]
fn h2_an_unchanged_rerun_reads_nothing_and_writes_nothing() {
    let world = World::new();
    let first = world.handoff(&[]);
    assert!(first.ok, "{}", first.stderr);
    settle();
    // A no-op apply in between rewrites STATE's outcome records; the block
    // says nothing that moved, so it is no reason to re-render.
    let noop = world.apply();
    assert!(noop.ok, "{}\n{}", noop.stdout, noop.stderr);
    // The record's prints were taken in the racy window of its own
    // `recorded_ns`: the first rerun proves the blocks by reading them and
    // re-dates the record (and nothing else), as Git re-dates a racily
    // clean index.
    let worktrees = |world: &World| frozen_but_targets(&world.root.join("dst"), &[]);
    let files = worktrees(&world);
    let targets =
        [world.dst(), world.dst_wt()].map(|root| fs::read(root.join("AGENTS.md")).unwrap());
    let settling = world.handoff(&[]);
    assert!(settling.ok, "{}", settling.stderr);
    assert!(
        row(&settling.stdout, &world.dst()).contains("\"outcome\":\"unchanged\""),
        "{}",
        settling.stdout
    );
    assert_eq!(
        worktrees(&world),
        files,
        "a settling rerun wrote a worktree"
    );
    assert_eq!(
        [world.dst(), world.dst_wt()].map(|root| fs::read(root.join("AGENTS.md")).unwrap()),
        targets
    );
    let frozen = freeze_all(&world.root.join("dst"));
    let state = freeze_all(&world.state());
    let again = world.handoff(&[]);
    assert!(again.ok, "{}", again.stderr);
    assert!(
        row(&again.stdout, &world.dst()).contains("\"outcome\":\"unchanged\""),
        "{}",
        again.stdout
    );
    // The open rows come from the record: nothing is read again for them.
    assert!(
        row(&again.stdout, &world.dst()).contains("\"open\":0,\"ready\":true"),
        "{}",
        again.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), frozen, "a rerun wrote");
    assert_eq!(freeze_all(&world.state()), state, "a rerun wrote STATE");
    assert_eq!(
        again.counters.get("read_dest_verify_existing_bytes"),
        Some(&0),
        "{}",
        again.stderr
    );
}

#[test]
fn h3_a_converge_keeps_an_intact_block() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    world.source_change(b"alpha 2\n");
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        applied.stdout.contains("outcome=workspace-restored"),
        "{}",
        applied.stdout
    );
    assert_eq!(fs::read(world.dst().join("a.txt")).unwrap(), b"alpha 2\n");
    // The H step re-rendered the block on the kept remainder: the landing
    // moved, so its facts did.
    assert!(
        applied.stdout.contains("handoff=written"),
        "{}",
        applied.stdout
    );
    let block = block_of(&world.dst().join("AGENTS.md"));
    assert_eq!(remainder_of(&world.dst().join("AGENTS.md")), AGENTS);
    let again = world.apply();
    assert!(again.ok);
    assert!(
        again
            .stdout
            .contains("previous-workspace-restoration-not-revalidated"),
        "{}",
        again.stdout
    );
    assert!(!again.stdout.contains("handoff="), "{}", again.stdout);
    assert_eq!(block_of(&world.dst().join("AGENTS.md")), block);
}

#[test]
fn h4_a_source_edit_of_the_target_lands_and_the_block_comes_back() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    fs::write(world.src().join("AGENTS.md"), b"# Agents, v2\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "agents v2"]);
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert_eq!(
        remainder_of(&world.dst().join("AGENTS.md")),
        b"# Agents, v2\n"
    );
    // The H step put a block back on the new body, in the same apply.
    assert!(
        applied.stdout.contains("handoff=written"),
        "{}",
        applied.stdout
    );
    assert!(split(&fs::read(world.dst().join("AGENTS.md")).unwrap()).is_some());
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}", ran.stderr);
    assert!(
        row(&ran.stdout, &world.dst()).contains("\"outcome\":\"unchanged\""),
        "{}",
        ran.stdout
    );
    assert_eq!(
        remainder_of(&world.dst().join("AGENTS.md")),
        b"# Agents, v2\n"
    );
}

#[test]
fn h5_a_created_file_is_lifted_before_the_source_adds_it() {
    let world = World::new();
    git(&world.src(), &["rm", "--quiet", "CLAUDE.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "no claude"]);
    settle();
    world.capture();
    assert!(world.apply().ok);
    assert!(!world.dst().join("CLAUDE.md").exists());
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}", ran.stderr);
    let claude = world.dst().join("CLAUDE.md");
    let (_, rest) = split(&fs::read(&claude).unwrap())
        .map(|(b, r)| (b.to_vec(), r.to_vec()))
        .unwrap();
    assert!(rest.is_empty(), "a created file holds the block alone");
    assert!(status(&world.dst()).contains("?? CLAUDE.md"));
    assert!(row(&ran.stdout, &world.dst()).contains("\"kind\":\"created\""));
    fs::write(world.src().join("CLAUDE.md"), b"Back again.\n").unwrap();
    git(&world.src(), &["add", "CLAUDE.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "claude back"]);
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        applied.stdout.contains("handoff-lifted=1"),
        "{}",
        applied.stdout
    );
    assert!(
        applied.stdout.contains("handoff=written"),
        "{}",
        applied.stdout
    );
    assert_eq!(remainder_of(&claude), b"Back again.\n");
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}", ran.stderr);
    assert_eq!(remainder_of(&claude), b"Back again.\n");
    assert!(row(&ran.stdout, &world.dst()).contains("\"kind\":\"prefixed\""));
}

#[test]
fn h6_a_user_edit_below_the_block_is_the_users() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let agents = world.dst().join("AGENTS.md");
    let mut bytes = fs::read(&agents).unwrap();
    bytes.extend_from_slice(b"A local note.\n");
    fs::write(&agents, &bytes).unwrap();
    world.source_change(b"alpha 2\n");
    let applied = world.apply();
    assert!(!applied.ok);
    assert!(applied.stdout.contains("GIT_DESTINATION_MODIFIED"));
    assert_eq!(fs::read(&agents).unwrap(), bytes);
    let mut remainder = AGENTS.to_vec();
    remainder.extend_from_slice(b"A local note.\n");
    let ran = world.handoff(&["--as-of", "1790000000000000000"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        remainder_of(&agents),
        remainder,
        "the user's bytes are kept"
    );
}

#[test]
fn h7_an_edit_inside_the_block_makes_the_file_the_users() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let agents = world.dst().join("AGENTS.md");
    let bytes = fs::read(&agents).unwrap();
    let edited = String::from_utf8(bytes.clone())
        .unwrap()
        .replacen("converge and consolidate", "CONVERGE and consolidate", 1)
        .into_bytes();
    assert_ne!(edited, bytes);
    fs::write(&agents, &edited).unwrap();
    let frozen = freeze_all(&world.root.join("dst"));
    let main = world.dst().display().to_string();
    for extra in [
        &["--workspace", &main][..],
        &["--clear", "--workspace", &main][..],
    ] {
        let ran = world.handoff(extra);
        assert!(!ran.ok, "{extra:?}");
        assert!(
            row(&ran.stdout, &world.dst()).contains("HANDOFF_BLOCK_MODIFIED"),
            "{}",
            ran.stdout
        );
        assert_eq!(
            freeze_all(&world.root.join("dst")),
            frozen,
            "{extra:?} wrote"
        );
    }
    // Removed by hand: retired, not re-added without --renew.
    let (_, rest) = split(&bytes).unwrap();
    fs::write(&agents, rest).unwrap();
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        row(&ran.stdout, &world.dst()).contains("\"action\":\"removed-by-hand\""),
        "{}",
        ran.stdout
    );
    assert_eq!(fs::read(&agents).unwrap(), AGENTS);
    let ran = world.handoff(&["--renew"]);
    assert!(ran.ok, "{}", ran.stderr);
    assert_eq!(remainder_of(&agents), AGENTS);
    assert!(split(&fs::read(&agents).unwrap()).is_some());
}

#[test]
fn h8_clear_restores_the_bytes_and_git_status() {
    let world = World::new();
    let before: Vec<(Vec<u8>, Vec<u8>, String)> = [world.dst(), world.dst_wt()]
        .iter()
        .map(|w| {
            (
                fs::read(w.join("AGENTS.md")).unwrap(),
                fs::read(w.join("CLAUDE.md")).unwrap(),
                status(w),
            )
        })
        .collect();
    assert!(world.handoff(&[]).ok);
    assert!(status(&world.dst()).contains(" M AGENTS.md"));
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert!(row(&cleared.stdout, &world.dst()).contains("\"outcome\":\"cleared\""));
    let after: Vec<(Vec<u8>, Vec<u8>, String)> = [world.dst(), world.dst_wt()]
        .iter()
        .map(|w| {
            (
                fs::read(w.join("AGENTS.md")).unwrap(),
                fs::read(w.join("CLAUDE.md")).unwrap(),
                status(w),
            )
        })
        .collect();
    assert_eq!(after, before);
    assert!(!world
        .dst()
        .join(".git/bulkload/v1/neo/handoff.landed")
        .exists());
    let frozen = freeze_all(&world.root.join("dst"));
    let again = world.handoff(&["--clear"]);
    assert!(again.ok, "{}", again.stderr);
    assert_eq!(freeze_all(&world.root.join("dst")), frozen);
    // A cleared landing converges on content.
    world.source_change(b"alpha 3\n");
    assert!(world.apply().ok);
}

#[test]
fn h9_symlinked_targets() {
    // CLAUDE.md -> AGENTS.md in the same worktree: covered by AGENTS.md's block.
    let world = World::with(|src| {
        fs::remove_file(src.join("CLAUDE.md")).unwrap();
        std::os::unix::fs::symlink("AGENTS.md", src.join("CLAUDE.md")).unwrap();
    });
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let line = row(&ran.stdout, &world.dst());
    assert!(line.contains("\"kind\":\"via-symlink\""), "{line}");
    assert!(fs::symlink_metadata(world.dst().join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(remainder_of(&world.dst().join("AGENTS.md")), AGENTS);
    assert!(world.handoff(&["--clear"]).ok);
    assert_eq!(fs::read(world.dst().join("AGENTS.md")).unwrap(), AGENTS);

    // A link out of the worktree, and a directory: refused, nothing written.
    let outside = World::with(|src| {
        fs::remove_file(src.join("CLAUDE.md")).unwrap();
        std::os::unix::fs::symlink("../elsewhere.md", src.join("CLAUDE.md")).unwrap();
    });
    let frozen = freeze_all(&outside.root.join("dst"));
    let ran = outside.handoff(&[]);
    assert!(!ran.ok);
    assert!(
        row(&ran.stdout, &outside.dst()).contains("HANDOFF_TARGET_UNSUPPORTED"),
        "{}",
        ran.stdout
    );
    assert_eq!(freeze_all(&outside.root.join("dst")), frozen);
    let dir = World::with(|src| {
        fs::remove_file(src.join("AGENTS.md")).unwrap();
        fs::create_dir(src.join("AGENTS.md")).unwrap();
        fs::write(src.join("AGENTS.md/inner.txt"), b"x\n").unwrap();
    });
    let frozen = freeze_all(&dir.root.join("dst"));
    let ran = dir.handoff(&[]);
    assert!(!ran.ok);
    assert!(row(&ran.stdout, &dir.dst()).contains("HANDOFF_TARGET_UNSUPPORTED"));
    assert_eq!(freeze_all(&dir.root.join("dst")), frozen);
}

#[test]
fn h10_missing_targets_are_created_untracked_and_cleared_away() {
    let world = World::with(|src| {
        fs::remove_file(src.join("AGENTS.md")).unwrap();
        fs::remove_file(src.join("CLAUDE.md")).unwrap();
    });
    let before = status(&world.dst());
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}", ran.stderr);
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = world.dst().join(name);
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o644);
        assert!(remainder_of(&path).is_empty());
    }
    assert!(status(&world.dst()).contains("?? AGENTS.md"));
    assert!(world.handoff(&["--clear"]).ok);
    assert!(!world.dst().join("AGENTS.md").exists());
    assert!(!world.dst().join("CLAUDE.md").exists());
    assert_eq!(status(&world.dst()), before);
}

#[test]
fn h11_privacy_is_deny_by_default_and_urls_are_never_printed() {
    let world = World::new();
    let secret = "s3cr3t-t0ken-value";
    git(
        &world.dst(),
        &[
            "remote",
            "add",
            "origin",
            &format!("https://user:{secret}@forge.example/team/repo.git"),
        ],
    );
    let frozen = freeze_all(&world.root.join("dst"));
    let refused = world.handoff(&[]);
    assert!(!refused.ok);
    assert!(
        row(&refused.stdout, &world.dst()).contains("HANDOFF_NOT_PRIVATE"),
        "{}",
        refused.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), frozen);
    let ran = world.handoff(&["--private-remote", "https://user:"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(row(&ran.stdout, &world.dst()).contains("\"private\":\"private-remote\""));
    assert!(world.handoff(&["--clear"]).ok);
    // The remote is the repository's: the linked worktree needs it too.
    let ran = world.handoff(&[
        "--allow-workspace",
        world.dst().to_str().unwrap(),
        "--allow-workspace",
        world.dst_wt().to_str().unwrap(),
    ]);
    assert!(ran.ok, "{}", ran.stderr);
    assert!(row(&ran.stdout, &world.dst()).contains("\"private\":\"allowed\""));
    // The token is in no output and no byte the hand-off wrote.
    for text in [&refused.stdout, &refused.stderr, &ran.stdout, &ran.stderr] {
        assert!(!text.contains(secret), "{text}");
    }
    for path in [
        world.dst().join("AGENTS.md"),
        world.dst().join("CLAUDE.md"),
        world.dst().join(".git/bulkload/v1/neo/handoff.landed"),
    ] {
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{}",
            path.display()
        );
    }
}

#[test]
fn h12_check_detects_a_staged_or_committed_block() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let clean = world.handoff(&["--check"]);
    assert!(clean.ok, "{}\n{}", clean.stdout, clean.stderr);
    assert!(row(&clean.stdout, &world.dst()).contains("\"state\":\"present\""));
    git(&world.dst(), &["add", "AGENTS.md"]);
    let staged = world.handoff(&["--check"]);
    assert!(!staged.ok);
    assert!(
        row(&staged.stdout, &world.dst()).contains("\"state\":\"staged\""),
        "{}",
        staged.stdout
    );
    assert!(
        staged.stderr.contains("HANDOFF_MARKER_COMMITTED"),
        "{}",
        staged.stderr
    );
    git(&world.dst(), &["commit", "--quiet", "-m", "oops"]);
    let committed = world.handoff(&["--check"]);
    assert!(!committed.ok);
    assert!(
        row(&committed.stdout, &world.dst()).contains("\"state\":\"committed\""),
        "{}",
        committed.stdout
    );
}

#[test]
fn h13_check_and_dry_run_write_nothing() {
    let world = World::new();
    let dst = freeze_all(&world.root.join("dst"));
    let state = freeze_all(&world.state());
    let dry = world.handoff(&["--dry-run"]);
    assert!(dry.ok, "{}\n{}", dry.stdout, dry.stderr);
    assert!(dry.stdout.contains(TOKEN), "the dry run shows the block");
    assert!(row(&dry.stdout, &world.dst()).contains("\"outcome\":\"planned\""));
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}", check.stderr);
    assert!(row(&check.stdout, &world.dst()).contains("\"state\":\"absent\""));
    assert_eq!(freeze_all(&world.root.join("dst")), dst);
    assert_eq!(freeze_all(&world.state()), state);
    assert!(world.handoff(&[]).ok);
    settle();
    let dst = freeze_all(&world.root.join("dst"));
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}", check.stderr);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"ready\":true"),
        "{}",
        check.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), dst);
}

#[test]
fn h14_estate_diff_reads_a_handed_off_item_as_identical() {
    let world = World::new();
    let target_bytes = |world: &World| -> u64 {
        [world.dst(), world.dst_wt()]
            .iter()
            .flat_map(|w| [w.join("AGENTS.md"), w.join("CLAUDE.md")])
            .map(|path| fs::metadata(path).unwrap().len())
            .sum()
    };
    // The landing's seats are racy against its ledger until a converge
    // records them again: the diff reads them by content either way.
    let before = *world
        .diff()
        .counters
        .get("read_dest_verify_existing_bytes")
        .unwrap();
    let landed = target_bytes(&world);
    assert!(world.handoff(&[]).ok);
    settle();
    let diff = world.diff();
    let line = diff
        .stdout
        .lines()
        .find(|line| {
            line.contains("\"kind\":\"item\"")
                && line.contains(&format!("\"source\":\"{}\"", world.src().display()))
        })
        .unwrap();
    assert!(line.contains("\"class\":\"identical\""), "{line}");
    assert!(line.contains("\"adopt\":\"ledgered\""), "{line}");
    // Nothing more is read than the blocks the targets now carry.
    let read = *diff
        .counters
        .get("read_dest_verify_existing_bytes")
        .unwrap();
    assert_eq!(
        read,
        before + target_bytes(&world) - landed,
        "{}",
        diff.stderr
    );
}

#[test]
fn h15_a_sibling_is_named_and_never_written() {
    let world = World::new();
    let live = world.root.join("live");
    fs::create_dir(&live).unwrap();
    let out = Command::new("cp")
        .arg("-a")
        .arg(world.src())
        .arg(live.join("main"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let frozen = freeze_all(&live);
    let sibling = format!("{}={}", world.root.join("dst").display(), live.display());
    let ran = world.handoff(&["--sibling", &sibling]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let block = String::from_utf8(block_of(&world.dst().join("AGENTS.md"))).unwrap();
    assert!(
        block.contains(&format!("`{}`", live.join("main").display())),
        "{block}"
    );
    assert!(block.contains("HEAD `main`"), "{block}");
    let wt = String::from_utf8(block_of(&world.dst_wt().join("AGENTS.md"))).unwrap();
    assert!(
        wt.contains("absent"),
        "the linked worktree's sibling is absent: {wt}"
    );
    assert_eq!(freeze_all(&live), frozen, "the sibling was written");
}

#[test]
fn h16_readme_is_opt_in_and_never_created() {
    let world = World::new();
    let readme = world.dst().join("README.md");
    assert!(world.handoff(&[]).ok);
    assert_eq!(fs::read(&readme).unwrap(), b"# Readme\n");
    assert!(world.handoff(&["--clear"]).ok);
    assert!(world.handoff(&["--readme"]).ok);
    assert_eq!(remainder_of(&readme), b"# Readme\n");
    assert!(split(&fs::read(&readme).unwrap()).is_some());
    assert!(world.handoff(&["--clear"]).ok);
    assert_eq!(fs::read(&readme).unwrap(), b"# Readme\n");
    fs::remove_file(&readme).unwrap();
    assert!(world.handoff(&["--readme"]).ok);
    assert!(!readme.exists(), "a README is never created");
}

#[test]
fn h18_the_render_is_deterministic() {
    let world = World::new();
    let at = "1790000000000000000";
    assert!(world.handoff(&["--as-of", at]).ok);
    let first = fs::read(world.dst().join("AGENTS.md")).unwrap();
    assert!(
        String::from_utf8_lossy(&first).contains("2026-09-21T"),
        "as of: {}",
        String::from_utf8_lossy(&first)
    );
    assert!(world.handoff(&["--clear"]).ok);
    assert!(world.handoff(&["--as-of", at]).ok);
    assert_eq!(fs::read(world.dst().join("AGENTS.md")).unwrap(), first);
}

/// A standalone source landed at an estate path under `plan`, as the GFTB
/// carry did, and the paths of a second, live-path carry of it.
struct Moved {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Moved {
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
    fn carry(&self, name: &str, workspace: &Path) -> [PathBuf; 3] {
        let plan = self.path(&format!("{name}-plan"));
        let added = agent(&os(&[
            &"estate-add",
            &plan,
            &self.path("src"),
            &workspace,
            &workspace,
        ]));
        assert!(added.ok, "{}", added.stderr);
        let corpus = self.path(&format!("{name}-corpus"));
        let ran = agent(&os(&[
            &"estate-capture",
            &plan,
            &self.path(&format!("{name}-private")),
            &corpus,
            &"1",
        ]));
        assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        let state = self.path(&format!("{name}-state"));
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        [plan, corpus, state]
    }
}

fn apply_with(carry: &[PathBuf; 3], extra: &[&str]) -> Ran {
    let mut list: Vec<OsString> = vec!["estate-apply".into()];
    list.extend(extra.iter().map(OsString::from));
    list.extend(os(&[&carry[0], &carry[1], &carry[2], &SOURCE, &"1"]));
    agent(&list)
}

/// H17: a handed-off landing moved to its live name and re-keyed under a
/// live-path plan (A11b) proves with its block on, and the H step
/// re-renders the block for the new item and path.
#[test]
fn h17_a_rekeyed_landing_keeps_its_block_and_is_re_rendered() {
    let dir = tempfile::Builder::new()
        .prefix("bulkload-handoff-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let moved = Moved { _dir: dir, root };
    let src = moved.path("src");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(src.join("AGENTS.md"), AGENTS).unwrap();
    fs::write(src.join("a.txt"), b"alpha\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "--quiet", "-m", "c0"]);
    fs::create_dir(moved.path("estate")).unwrap();
    let landing = moved.path("estate/lamesh");
    settle();
    let first = moved.carry("estate", &landing);
    let applied = apply_with(&first, &[]);
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    let handoff = |carry: &[PathBuf; 3], extra: &[&str]| {
        let mut list: Vec<OsString> = vec!["estate-handoff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(os(&[&carry[0], &carry[1], &carry[2], &SOURCE]));
        agent(&list)
    };
    let ran = handoff(&first, &[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let live = moved.path("lamesh");
    fs::rename(&landing, &live).unwrap();
    settle();
    let second = moved.carry("live", &live);
    let rekeyed = apply_with(&second, &["--adopt"]);
    assert!(rekeyed.ok, "{}\n{}", rekeyed.stdout, rekeyed.stderr);
    assert!(rekeyed.stdout.contains("rekeyed=1"), "{}", rekeyed.stdout);
    assert!(
        rekeyed.stdout.contains("handoff=written"),
        "{}",
        rekeyed.stdout
    );
    let block = String::from_utf8(block_of(&live.join("AGENTS.md"))).unwrap();
    assert!(
        block.contains(&format!("| This landing | `{}` |", live.display())),
        "{block}"
    );
    assert!(block.contains(&second[0].display().to_string()), "{block}");
    assert_eq!(remainder_of(&live.join("AGENTS.md")), AGENTS);
    let check = handoff(&second, &["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(
        row(&check.stdout, &live).contains("\"state\":\"present\""),
        "{}",
        check.stdout
    );
}

/// H19: the commands a block gives run as printed, from any directory,
/// and every path in the block is absolute even when the hand-off was
/// given relative ones.
#[test]
fn h19_the_blocks_commands_run_as_printed() {
    let world = World::new();
    let relative = |path: PathBuf| path.strip_prefix(&world.root).unwrap().to_path_buf();
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .current_dir(&world.root)
        .arg("estate-handoff")
        .args([
            relative(world.plan()),
            relative(world.corpus()),
            relative(world.state()),
        ])
        .arg(SOURCE)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = String::from_utf8(block_of(&world.dst().join("AGENTS.md"))).unwrap();
    for path in [world.plan(), world.corpus(), world.state()] {
        assert!(
            block.contains(&format!("`{}`", path.display())),
            "{path:?} not absolute in {block}"
        );
    }
    // Each command is the backticked text after its cue, run from `/`.
    let command = |cue: &str| -> String {
        let at = block
            .find(cue)
            .unwrap_or_else(|| panic!("no {cue} in {block}"));
        let rest = &block[at..];
        let start = rest.find('`').unwrap() + 1;
        let end = start + rest[start..].find('`').unwrap();
        rest[start..end].to_owned()
    };
    let run = |line: &str| {
        Command::new("sh")
            .arg("-c")
            .arg(line)
            .current_dir("/")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    };
    let diff = run(&command("See what differs, read-only:"));
    assert!(
        diff.status.success(),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    assert!(String::from_utf8_lossy(&diff.stdout).contains("bulkload.estate-diff.v1"));
    let check = run(&command("**When it is done**"));
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let check = String::from_utf8_lossy(&check.stdout).into_owned();
    assert!(
        row(&check, &world.dst()).contains("\"state\":\"present\""),
        "{check}"
    );
    let clear = run(&command("Then remove this block with"));
    assert!(
        clear.status.success(),
        "{}",
        String::from_utf8_lossy(&clear.stderr)
    );
    assert_eq!(fs::read(world.dst().join("AGENTS.md")).unwrap(), AGENTS);
    // The clear named this workspace only.
    assert!(split(&fs::read(world.dst_wt().join("AGENTS.md")).unwrap()).is_some());
}

/// H20: a block re-rendered over a user's edit below it never vouches for
/// that edit. Its record's print, once re-dated past the racy window, is
/// trusted by converge's proof only when the bytes under the block were
/// the landed seat's; otherwise the proof reads them, and the edit is the
/// user's (MODIFIED), never landed over.
#[test]
fn h20_a_block_over_a_users_edit_never_vouches_for_it() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let agents = world.dst().join("AGENTS.md");
    let mut bytes = fs::read(&agents).unwrap();
    bytes.extend_from_slice(b"A local note.\n");
    fs::write(&agents, &bytes).unwrap();
    // The hand-off re-renders the block alone over the user's bytes.
    let ran = world.handoff(&["--as-of", "1790000000000000000"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let kept = remainder_of(&agents);
    assert!(kept.ends_with(b"A local note.\n"));
    // A rerun past the racy window re-dates the record.
    settle();
    assert!(world.handoff(&["--as-of", "1790000000000000000"]).ok);
    fs::write(world.src().join("AGENTS.md"), b"# Agents, v2\n").unwrap();
    git(&world.src(), &["commit", "--quiet", "-am", "agents v2"]);
    settle();
    world.capture();
    let applied = world.apply();
    assert!(!applied.ok, "{}", applied.stdout);
    assert!(
        applied.stdout.contains("GIT_DESTINATION_MODIFIED"),
        "{}",
        applied.stdout
    );
    assert_eq!(
        remainder_of(&agents),
        kept,
        "the user's edit was landed over"
    );
}
