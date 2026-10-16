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

//! - H20, H21, H33: a block over a user's edit below it never vouches for
//!   the edit (render, plain rerun, re-taken print), and a rerun settles
//!   to writing nothing.
//! - H24, H25: a block another landing rendered is carried content
//!   (OI-1003-Q185), kept below this landing's block.
//! - H26, H27: the privacy guard counts a preserved inert origin and URL
//!   rewrites. H28: commit detection reads branch history.
//! - H29: `ready` follows consolidation. H30: options persist.
//! - H31, H34: `AGENTS.override.md`. H32: an edited created file is never
//!   lifted. H35: a new destination ref makes the block stale.
//! - Re-review of 2026-10-16: H36, a symlinked global configuration's
//!   rewrite rules are read; H37, a block of this landing's identity that
//!   no record names (a round trip) is carried content; H38, a live sibling
//!   strictly ahead of the landing keeps the hand-off open; H39, commit
//!   detection reads every target name's history, whatever this run's
//!   targets are.
//! - Re-review 2 of 2026-10-16: H40 and H41, a round trip of this
//!   landing's current or an older block while the record stands is
//!   carried content (the whole file is the landed blob); H42, a retired
//!   block put back byte for byte is this landing's again; H43, a created
//!   file stays created across re-renders.
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
    agent_with(args, |command| {
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    })
}

/// [`agent`] with the child's environment set by `environ`.
fn agent_with(args: &[OsString], environ: impl FnOnce(&mut Command)) -> Ran {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
    command.args(args);
    environ(&mut command);
    let out = command.output().unwrap();
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
        agent(&handoff_args(self, extra))
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
    assert!(
        !world.dst().join("AGENTS.override.md").exists(),
        "an override is never created"
    );
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

// ---- review of 2026-10-16 (converge safety; privacy and claims) ---------------
//
// Each row below reproduced a refuter's finding against 9ec8d36 and failed
// there; the design note's section 18 has the per-finding table. The two
// crash findings are fault-harness rows (`estate_handoff_rerender_*`,
// `estate_handoff_clear_noop_apply`), which need the fault points.

impl World {
    /// The `estate-diff` item line of the main.
    fn diff_main(&self) -> String {
        let diff = self.diff();
        diff.stdout
            .lines()
            .find(|line| {
                line.contains("\"kind\":\"item\"")
                    && line.contains(&format!("\"source\":\"{}\"", self.src().display()))
            })
            .unwrap()
            .to_owned()
    }

    /// The source edits `AGENTS.md`, captured; then an apply.
    fn source_edits_agents(&self) -> Ran {
        fs::write(self.src().join("AGENTS.md"), b"# Agents, v2\n").unwrap();
        git(&self.src(), &["commit", "--quiet", "-am", "agents v2"]);
        settle();
        self.capture();
        self.apply()
    }
}

/// H21 (converge safety, BLOCKER): a plain rerun after a user edit below the
/// block keeps the block (same bytes) but re-takes the record's print. That
/// print must not vouch for the user's bytes: estate-diff must not call the
/// item identical, and a converge that replaces the target refuses MODIFIED
/// with the user's edit kept (S5, `NoLocalWorkLost`).
#[test]
fn h21_a_plain_rerun_never_vouches_for_an_edit_below_the_block() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    settle();
    let agents = world.dst().join("AGENTS.md");
    let mut bytes = fs::read(&agents).unwrap();
    bytes.extend_from_slice(b"A local note.\n");
    fs::write(&agents, &bytes).unwrap();
    settle();
    for _ in 0..2 {
        let again = world.handoff(&[]);
        assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
        settle();
    }
    let kept = remainder_of(&agents);
    assert!(kept.ends_with(b"A local note.\n"));
    let line = world.diff_main();
    assert!(!line.contains("\"class\":\"identical\""), "{line}");
    let applied = world.source_edits_agents();
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

/// The text of a hand-off block another landing rendered (its BEGIN line
/// names that landing's identity, not this one's).
fn foreign_block() -> Vec<u8> {
    let id = "0123456789abcdef";
    format!(
        "<!-- bulkload-handoff:v1 BEGIN id={id} for=fedcba9876543210 {TOKEN} -->\n> an earlier landing's hand-off, on another host\n<!-- bulkload-handoff:v1 END id={id} -->\n\n"
    )
    .into_bytes()
}

/// H24 (converge safety, minor; OI-1003-Q185): a hand-off block carried in
/// with the source's bytes (here the source is itself a handed-off
/// checkout: the block uncommitted in its working tree) is carried content.
/// The hand-off renders its own block above it and keeps it byte for byte;
/// `--check` names it; `--clear` removes only this landing's block;
/// converge proves the target.
#[test]
fn h24_a_block_carried_from_the_source_is_kept_as_content() {
    let world = World::new();
    let mut carried = foreign_block();
    carried.extend_from_slice(AGENTS);
    fs::write(world.src().join("AGENTS.md"), &carried).unwrap();
    settle();
    world.capture();
    let landed = world.apply();
    assert!(landed.ok, "{}\n{}", landed.stdout, landed.stderr);
    let agents = world.dst().join("AGENTS.md");
    assert_eq!(fs::read(&agents).unwrap(), carried);
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"carried\""),
        "{}",
        check.stdout
    );
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        remainder_of(&agents),
        carried,
        "the carried block was dropped"
    );
    let block = String::from_utf8(block_of(&agents)).unwrap();
    assert!(block.contains("Carried hand-off blocks"), "{block}");
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"present\""),
        "{}",
        check.stdout
    );
    fs::write(world.src().join("a.txt"), b"alpha 2\n").unwrap();
    git(
        &world.src(),
        &["commit", "--quiet", "-m", "a change", "--", "a.txt"],
    );
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert_eq!(remainder_of(&agents), carried);
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), carried);
}

/// H25 (privacy and claims, MAJOR; OI-1003-Q185): a handed-off landing
/// carried on to a third place (another SOURCE label, another path) arrives
/// with the first landing's block, which names the first landing's paths.
/// Nothing is dropped from the carry; the third landing's hand-off renders
/// its own block above the carried one, and `--clear` there removes only
/// its own.
#[test]
fn h25_a_recarried_landing_keeps_the_earlier_block_below_its_own() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    settle();
    let first = fs::read(world.dst().join("AGENTS.md")).unwrap();
    let r = &world.root;
    let plan = r.join("p2-plan");
    let third = r.join("third");
    let added = agent(&os(&[&"estate-add", &plan, &world.dst(), &third, &third]));
    assert!(added.ok, "{}", added.stderr);
    let corpus = r.join("p2-corpus");
    let cap = agent(&os(&[
        &"estate-capture",
        &plan,
        &r.join("p2-private"),
        &corpus,
        &"1",
    ]));
    assert!(cap.ok, "{}\n{}", cap.stdout, cap.stderr);
    let state = r.join("p2-state");
    let ap = agent(&os(&[
        &"estate-apply",
        &plan,
        &corpus,
        &state,
        &"sting",
        &"1",
    ]));
    assert!(ap.ok, "{}\n{}", ap.stdout, ap.stderr);
    let agents = third.join("AGENTS.md");
    assert_eq!(fs::read(&agents).unwrap(), first, "carried as it was");
    let handoff = |extra: &[&str]| {
        let mut list: Vec<OsString> = vec!["estate-handoff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(os(&[&plan, &corpus, &state, &"sting"]));
        agent(&list)
    };
    let check = handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(
        row(&check.stdout, &third).contains("\"state\":\"carried\""),
        "{}",
        check.stdout
    );
    let ran = handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        remainder_of(&agents),
        first,
        "the carried block was dropped"
    );
    let block = String::from_utf8(block_of(&agents)).unwrap();
    assert!(
        block.contains(&format!("| This landing | `{}` |", third.display())),
        "{block}"
    );
    let cleared = handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), first);
}

/// H26 (privacy and claims, BLOCKER): a landing keeps the source's origin
/// inert (preserved in `.git/carry-config`, no remote configured). That is
/// still the repository's remote: the guard refuses `HANDOFF_NOT_PRIVATE`
/// unless the operator names its prefix, and never prints the URL.
#[test]
fn h26_a_preserved_inert_origin_is_still_a_remote() {
    let url = "git@github.com:Jesssullivan/public-repo.git";
    let world = World::with(|src| {
        git(src, &["remote", "add", "origin", url]);
    });
    assert_eq!(git(&world.dst(), &["remote"]), "", "the origin is inert");
    let frozen = freeze_all(&world.root.join("dst"));
    let refused = world.handoff(&[]);
    assert!(!refused.ok, "{}", refused.stdout);
    assert!(
        row(&refused.stdout, &world.dst()).contains("HANDOFF_NOT_PRIVATE"),
        "{}",
        refused.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), frozen);
    let ran = world.handoff(&["--private-remote", "git@github.com:Jesssullivan/"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(row(&ran.stdout, &world.dst()).contains("\"private\":\"private-remote\""));
    for text in [&refused.stdout, &refused.stderr, &ran.stdout, &ran.stderr] {
        assert!(!text.contains(url), "{text}");
    }
}

/// H27 (privacy and claims, minor): the prefix check reads the URLs Git
/// would use: a remote under a private prefix that `url.<base>.insteadOf`
/// or `pushInsteadOf` rewrites to a public one is not private.
#[test]
fn h27_insteadof_rewrites_are_checked() {
    let world = World::new();
    git(
        &world.dst(),
        &["remote", "add", "origin", "priv:team/repo.git"],
    );
    let private = ["--private-remote", "priv:"];
    let baseline = world.handoff(&private);
    assert!(baseline.ok, "{}\n{}", baseline.stdout, baseline.stderr);
    assert!(world.handoff(&["--clear"]).ok);
    for key in ["insteadOf", "pushInsteadOf"] {
        let name = format!("url.https://github.com/.{key}");
        git(&world.dst(), &["config", &name, "priv:"]);
        let refused = world.handoff(&private);
        assert!(!refused.ok, "{key}: {}", refused.stdout);
        assert!(
            row(&refused.stdout, &world.dst()).contains("HANDOFF_NOT_PRIVATE"),
            "{key}: {}",
            refused.stdout
        );
        let both = world.handoff(&[
            "--private-remote",
            "priv:",
            "--private-remote",
            "https://github.com/",
        ]);
        assert!(both.ok, "{key}: {}\n{}", both.stdout, both.stderr);
        assert!(world.handoff(&["--clear"]).ok);
        git(&world.dst(), &["config", "--unset", &name]);
    }
}

/// H28 (privacy and claims, MAJOR): a block committed and removed again by a
/// later commit is still in the branch's history, and a push would publish
/// it: `--check` reports `committed`.
#[test]
fn h28_a_block_in_branch_history_is_reported() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let agents = world.dst().join("AGENTS.md");
    git(&world.dst(), &["add", "AGENTS.md"]);
    git(&world.dst(), &["commit", "--quiet", "-m", "oops"]);
    let rest = remainder_of(&agents);
    fs::write(&agents, &rest).unwrap();
    git(
        &world.dst(),
        &["commit", "--quiet", "-m", "fix", "--", "AGENTS.md"],
    );
    let check = world.handoff(&["--check"]);
    assert!(!check.ok, "{}", check.stdout);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"committed\""),
        "{}",
        check.stdout
    );
    assert!(
        check.stderr.contains("HANDOFF_MARKER_COMMITTED"),
        "{}",
        check.stderr
    );
}

fn ready(stdout: &str, workspace: &Path) -> bool {
    row(stdout, workspace).contains("\"ready\":true")
}

/// H29 (privacy and claims, MAJOR): "done" is reachable and honest.
/// Consolidating at the landing (a commit on the landed branch) closes the
/// hand-off rather than opening rows that can never close; a live sibling
/// holding a commit the landing lacks keeps it open until that commit is
/// brought in.
#[test]
fn h29_ready_follows_consolidation() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    settle();
    fs::write(world.dst().join("a.txt"), b"alpha merged\n").unwrap();
    git(
        &world.dst(),
        &["commit", "--quiet", "-m", "consolidate", "--", "a.txt"],
    );
    settle();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(ready(&check.stdout, &world.dst()), "{}", check.stdout);
    // A live sibling one commit ahead of the landing.
    let live = world.root.join("live");
    fs::create_dir(&live).unwrap();
    let out = Command::new("cp")
        .arg("-a")
        .arg(world.src())
        .arg(live.join("main"))
        .output()
        .unwrap();
    assert!(out.status.success());
    fs::write(live.join("main/live.txt"), b"live work\n").unwrap();
    git(&live.join("main"), &["add", "live.txt"]);
    git(
        &live.join("main"),
        &["commit", "--quiet", "-m", "live work"],
    );
    let sibling = format!("{}={}", world.root.join("dst").display(), live.display());
    assert!(world.handoff(&["--sibling", &sibling]).ok);
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(!ready(&check.stdout, &world.dst()), "{}", check.stdout);
    // The agent brings the sibling's commit into the landing.
    git(
        &world.dst(),
        &[
            "fetch",
            "--quiet",
            live.join("main").to_str().unwrap(),
            "main",
        ],
    );
    git(
        &world.dst(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "merge",
            "--quiet",
            "--no-edit",
            "FETCH_HEAD",
        ],
    );
    settle();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(ready(&check.stdout, &world.dst()), "{}", check.stdout);
}

/// H30 (privacy and claims, minor): a plain rerun keeps the options the
/// record was rendered with (`--sibling`), and the `--check` the block
/// prints, which has no flags, reads a flagged render as `present`.
#[test]
fn h30_a_plain_rerun_and_the_printed_check_keep_the_records_options() {
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
    let named = format!("`{}`", live.join("main").display());
    let sibling = format!("{}={}", world.root.join("dst").display(), live.display());
    assert!(world.handoff(&["--sibling", &sibling]).ok);
    settle();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}", check.stderr);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"present\""),
        "{}",
        check.stdout
    );
    let again = world.handoff(&[]);
    assert!(again.ok, "{}", again.stderr);
    assert!(
        row(&again.stdout, &world.dst()).contains("\"outcome\":\"unchanged\""),
        "{}",
        again.stdout
    );
    let block = String::from_utf8(block_of(&world.dst().join("AGENTS.md"))).unwrap();
    assert!(block.contains(&named), "the sibling was dropped: {block}");
}

/// H31 (privacy and claims, minor): Codex reads `AGENTS.override.md` instead
/// of `AGENTS.md` in a directory that has one, so an existing override
/// carries the block too (never created).
#[test]
fn h31_an_agents_override_carries_the_block() {
    const OVERRIDE: &[u8] = b"# Override\n";
    let world = World::with(|src| fs::write(src.join("AGENTS.override.md"), OVERRIDE).unwrap());
    let path = world.dst().join("AGENTS.override.md");
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert!(split(&fs::read(&path).unwrap()).is_some(), "no block");
    assert_eq!(remainder_of(&path), OVERRIDE);
    let linked = world.dst_wt().join("AGENTS.override.md");
    assert_eq!(remainder_of(&linked), OVERRIDE);
    assert!(split(&fs::read(&linked).unwrap()).is_some(), "no block");
    world.source_change(b"alpha 2\n");
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(world.handoff(&["--clear"]).ok);
    assert_eq!(fs::read(&path).unwrap(), OVERRIDE);
}

/// H32 (mutation row, review of 2026-10-16): a created file the user edited
/// inside its block, at the same length, is the user's: converge's lift
/// leaves it, the source's add of the path refuses MODIFIED, and the user's
/// bytes are kept.
#[test]
fn h32_an_edited_created_file_is_never_lifted() {
    let world = World::new();
    git(&world.src(), &["rm", "--quiet", "CLAUDE.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "no claude"]);
    settle();
    world.capture();
    assert!(world.apply().ok);
    assert!(world.handoff(&[]).ok);
    let claude = world.dst().join("CLAUDE.md");
    let edited = String::from_utf8(fs::read(&claude).unwrap())
        .unwrap()
        .replacen("converge and consolidate", "CONVERGE and consolidate", 1)
        .into_bytes();
    assert_eq!(edited.len() as u64, fs::metadata(&claude).unwrap().len());
    fs::write(&claude, &edited).unwrap();
    fs::write(world.src().join("CLAUDE.md"), b"Back again.\n").unwrap();
    git(&world.src(), &["add", "CLAUDE.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "claude back"]);
    settle();
    world.capture();
    let applied = world.apply();
    assert!(
        applied.stdout.contains("GIT_DESTINATION_MODIFIED"),
        "{}",
        applied.stdout
    );
    assert_eq!(
        fs::read(&claude).unwrap(),
        edited,
        "the user's file was lifted"
    );
}

/// H33 (mutation row, review of 2026-10-16): after a user edit below the
/// block, a rerun re-takes the record's print once, checking the bytes
/// under the block first; from then on a rerun reads nothing and writes
/// nothing (S3), and the re-taken print still never vouches for the edit.
#[test]
fn h33_a_rerun_after_an_edit_below_settles_to_writing_nothing() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    settle();
    let agents = world.dst().join("AGENTS.md");
    let mut bytes = fs::read(&agents).unwrap();
    bytes.extend_from_slice(b"A local note.\n");
    fs::write(&agents, &bytes).unwrap();
    settle();
    for _ in 0..2 {
        let ran = world.handoff(&[]);
        assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        settle();
    }
    let frozen = freeze_all(&world.root.join("dst"));
    let again = world.handoff(&[]);
    assert!(again.ok, "{}", again.stderr);
    assert!(
        row(&again.stdout, &world.dst()).contains("\"outcome\":\"unchanged\""),
        "{}",
        again.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), frozen, "a rerun wrote");
    assert_eq!(
        again.counters.get("read_dest_verify_existing_bytes"),
        Some(&0),
        "{}",
        again.stderr
    );
    let applied = world.source_edits_agents();
    assert!(
        applied.stdout.contains("GIT_DESTINATION_MODIFIED"),
        "{}",
        applied.stdout
    );
    assert!(remainder_of(&agents).ends_with(b"A local note.\n"));
}

/// H34 (mutation row): an override the record names that the source then
/// deletes is gone after the converge; the H step never creates one.
#[test]
fn h34_an_override_the_source_deletes_is_never_created() {
    let world =
        World::with(|src| fs::write(src.join("AGENTS.override.md"), b"# Override\n").unwrap());
    assert!(world.handoff(&[]).ok);
    git(&world.src(), &["rm", "--quiet", "AGENTS.override.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "no override"]);
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        !world.dst().join("AGENTS.override.md").exists(),
        "{}",
        applied.stdout
    );
    assert!(world.handoff(&[]).ok);
    assert!(!world.dst().join("AGENTS.override.md").exists());
}

/// H35 (mutation row): a ref created at the destination is one of the
/// things the block says (a destination-only ref), so `--check` reads the
/// block as `stale` and a rerun renders it again.
#[test]
fn h35_a_new_destination_ref_makes_the_block_stale() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    git(&world.dst(), &["branch", "extra"]);
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}", check.stderr);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"stale\""),
        "{}",
        check.stdout
    );
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}", ran.stderr);
    assert!(
        row(&ran.stdout, &world.dst()).contains("\"outcome\":\"written\""),
        "{}",
        ran.stdout
    );
    assert!(String::from_utf8(block_of(&world.dst().join("AGENTS.md")))
        .unwrap()
        .contains("`refs/heads/extra`"));
}

/// The arguments of an `estate-handoff` run of `world` with `extra` flags.
fn handoff_args(world: &World, extra: &[&str]) -> Vec<OsString> {
    let mut list: Vec<OsString> = vec!["estate-handoff".into()];
    list.extend(extra.iter().map(OsString::from));
    list.extend(os(&[
        &world.plan(),
        &world.corpus(),
        &world.state(),
        &SOURCE,
    ]));
    list
}

/// H36 (re-review of 2026-10-16, finding 3): the user's global
/// configuration is read as Git reads it, through a symlink (a
/// home-manager `~/.config/git/config`): its `insteadOf` rule that rewrites
/// a private remote to a public one refuses `HANDOFF_NOT_PRIVATE`. A
/// dangling link is no file, as for Git.
#[test]
fn h36_a_symlinked_global_configuration_is_read() {
    let world = World::new();
    git(
        &world.dst(),
        &["remote", "add", "origin", "priv:team/repo.git"],
    );
    let home = world.root.join("home");
    let xdg = home.join(".config");
    fs::create_dir_all(xdg.join("git")).unwrap();
    let store = world.root.join("store-config");
    fs::write(
        &store,
        "[url \"https://github.com/\"]\n\tinsteadOf = priv:\n",
    )
    .unwrap();
    let args = handoff_args(&world, &["--private-remote", "priv:"]);
    let run = |args: &[OsString]| {
        agent_with(args, |command| {
            command
                .env_remove("GIT_CONFIG_GLOBAL")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &xdg);
        })
    };
    let link = xdg.join("git/config");
    // A dangling link: no rule, the remote is under the prefix.
    std::os::unix::fs::symlink(world.root.join("no-such-config"), &link).unwrap();
    let dangling = run(&args);
    assert!(dangling.ok, "{}\n{}", dangling.stdout, dangling.stderr);
    assert!(run(&handoff_args(&world, &["--clear"])).ok);
    // The rule through the link.
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&store, &link).unwrap();
    let frozen = freeze_all(&world.root.join("dst"));
    let linked = run(&args);
    assert!(!linked.ok, "{}", linked.stdout);
    assert!(
        row(&linked.stdout, &world.dst()).contains("HANDOFF_NOT_PRIVATE"),
        "{}",
        linked.stdout
    );
    assert_eq!(freeze_all(&world.root.join("dst")), frozen, "wrote");
}

/// H37 (re-review of 2026-10-16, finding 4; OI-1003-Q185): a block this
/// landing rendered, carried back to the source and landed again at the
/// same path with no record (a round trip) names this landing's identity,
/// but no record vouches for it: it is the file's content. A render keeps
/// it below the new block and names it as carried; `--clear` removes the
/// new block only, leaving the landed bytes.
#[test]
fn h37_a_round_tripped_block_of_this_landing_is_carried_content() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let with_block = fs::read(world.dst().join("AGENTS.md")).unwrap();
    fs::write(world.src().join("AGENTS.md"), &with_block).unwrap();
    git(
        &world.src(),
        &[
            "commit",
            "--quiet",
            "-am",
            "round trip carried the block back",
        ],
    );
    fs::remove_dir_all(world.root.join("dst")).unwrap();
    fs::create_dir_all(world.root.join("dst")).unwrap();
    fs::remove_dir_all(world.state()).unwrap();
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    let agents = world.dst().join("AGENTS.md");
    assert_eq!(fs::read(&agents).unwrap(), with_block);
    let check = world.handoff(&["--check"]);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"action\":\"carried\""),
        "{}",
        check.stdout
    );
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        remainder_of(&agents),
        with_block,
        "the carried block was dropped"
    );
    let block = String::from_utf8(block_of(&agents)).unwrap();
    assert!(block.contains("Carried hand-off blocks"), "{block}");
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), with_block);
}

/// H38 (re-review of 2026-10-16, finding 2): a live sibling strictly ahead
/// of the landing (a commit the landing lacks, the landing unchanged)
/// keeps the hand-off open; bringing the commit in closes it.
#[test]
fn h38_a_sibling_ahead_of_the_landing_keeps_it_open() {
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
    fs::write(live.join("main/live.txt"), b"live work\n").unwrap();
    git(&live.join("main"), &["add", "live.txt"]);
    git(
        &live.join("main"),
        &["commit", "--quiet", "-m", "live work"],
    );
    let sibling = format!("{}={}", world.root.join("dst").display(), live.display());
    assert!(world.handoff(&["--sibling", &sibling]).ok);
    settle();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(!ready(&check.stdout, &world.dst()), "{}", check.stdout);
    git(
        &world.dst(),
        &[
            "fetch",
            "--quiet",
            live.join("main").to_str().unwrap(),
            "main",
        ],
    );
    git(
        &world.dst(),
        &["merge", "--quiet", "--ff-only", "FETCH_HEAD"],
    );
    settle();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(ready(&check.stdout, &world.dst()), "{}", check.stdout);
}

/// H39 (re-review of 2026-10-16, finding 5): a README block committed while
/// `--readme` was on and fixed by a later commit is still in the branch's
/// history after `--clear` drops the record (and with it the README as a
/// target): `--check` reports `committed`.
#[test]
fn h39_commit_detection_reads_every_target_names_history() {
    let world = World::new();
    assert!(world.handoff(&["--readme"]).ok);
    let readme = world.dst().join("README.md");
    assert!(fs::read(&readme).unwrap().starts_with(BEGIN));
    git(&world.dst(), &["add", "README.md"]);
    git(&world.dst(), &["commit", "--quiet", "-m", "oops"]);
    let rest = remainder_of(&readme);
    fs::write(&readme, &rest).unwrap();
    git(
        &world.dst(),
        &["commit", "--quiet", "-m", "fix", "--", "README.md"],
    );
    assert!(world.handoff(&["--clear"]).ok);
    let check = world.handoff(&["--check"]);
    assert!(!check.ok, "{}", check.stdout);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"committed\""),
        "{}",
        check.stdout
    );
}

/// H40 (re-review 2 of 2026-10-16, finding 2; OI-1003-Q185): the source
/// carries this landing's current block back (byte-identical to the
/// record's, above an edited body) while the record stands. The landed blob
/// now starts with that block, so the block is the file's content, not
/// bulkload's overlay: the H step keeps it below a new block, a later
/// converge proves the seat, and `--clear` leaves the landed bytes.
#[test]
fn h40_a_round_trip_of_the_recorded_block_is_carried_content() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    settle();
    let agents = world.dst().join("AGENTS.md");
    let mut carried = block_of(&agents);
    carried.extend_from_slice(b"# Agents v2\n");
    fs::write(world.src().join("AGENTS.md"), &carried).unwrap();
    git(
        &world.src(),
        &["commit", "--quiet", "-am", "round trip, the same block"],
    );
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        applied.stdout.contains("handoff=written"),
        "{}",
        applied.stdout
    );
    assert_eq!(
        remainder_of(&agents),
        carried,
        "the carried block was dropped"
    );
    let block = String::from_utf8(block_of(&agents)).unwrap();
    assert!(block.contains("Carried hand-off blocks"), "{block}");
    world.source_change(b"alpha after the round trip\n");
    let again = world.apply();
    assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
    assert_eq!(remainder_of(&agents), carried);
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), carried);
}

/// H41 (re-review 2 of 2026-10-16, finding 2): the source carries back an
/// older block of this landing's identity (not the record's) while the
/// record stands. The whole file is the landed blob, so the block is
/// carried content, never an edit of this landing's block: the H step,
/// a rerun and `--clear` neither refuse nor drop it.
#[test]
fn h41_a_round_trip_of_an_older_block_is_carried_content() {
    let world = World::new();
    assert!(world.handoff(&["--as-of", "1790000000000000000"]).ok);
    let agents = world.dst().join("AGENTS.md");
    let older = block_of(&agents);
    assert!(
        world
            .handoff(&["--renew", "--as-of", "1790000001000000000"])
            .ok
    );
    assert_ne!(block_of(&agents), older, "no newer block");
    settle();
    let mut carried = older;
    carried.extend_from_slice(b"# Agents v2\n");
    fs::write(world.src().join("AGENTS.md"), &carried).unwrap();
    git(
        &world.src(),
        &["commit", "--quiet", "-am", "round trip, an older block"],
    );
    settle();
    world.capture();
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        !applied.stdout.contains("HANDOFF_BLOCK_MODIFIED"),
        "{}",
        applied.stdout
    );
    assert_eq!(remainder_of(&agents), carried);
    let ran = world.handoff(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), carried);
}

/// H42 (re-review 2 of 2026-10-16, finding 3): a block removed by hand
/// (retired) and then put back byte for byte (an editor's undo, a `git
/// stash pop`) is this landing's block again, never carried content: a
/// converge proves the seat, the H step takes it back into the record and
/// renders it current, `--check` reads it as this landing's, and
/// `--clear` removes it.
#[test]
fn h42_a_retired_block_put_back_is_this_landings_again() {
    let world = World::new();
    assert!(world.handoff(&[]).ok);
    let agents = world.dst().join("AGENTS.md");
    let with_block = fs::read(&agents).unwrap();
    fs::write(&agents, AGENTS).unwrap();
    let retired = world.handoff(&[]);
    assert!(retired.ok, "{}\n{}", retired.stdout, retired.stderr);
    assert!(
        row(&retired.stdout, &world.dst()).contains("\"action\":\"removed-by-hand\""),
        "{}",
        retired.stdout
    );
    fs::write(&agents, &with_block).unwrap();
    let check = world.handoff(&["--check"]);
    assert!(check.ok, "{}\n{}", check.stdout, check.stderr);
    assert!(
        !row(&check.stdout, &world.dst()).contains("carried"),
        "{}",
        check.stdout
    );
    world.source_change(b"alpha 2\n");
    let applied = world.apply();
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    assert!(
        applied.stdout.contains("handoff=written"),
        "{}",
        applied.stdout
    );
    assert_eq!(remainder_of(&agents), AGENTS);
    let block = String::from_utf8(block_of(&agents)).unwrap();
    assert!(!block.contains("Carried hand-off blocks"), "{block}");
    let check = world.handoff(&["--check"]);
    assert!(
        row(&check.stdout, &world.dst()).contains("\"state\":\"present\""),
        "{}",
        check.stdout
    );
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert_eq!(fs::read(&agents).unwrap(), AGENTS);
}

/// H43 (found while porting the re-review 2 probes): a file the hand-off
/// created stays `created` when a later render replaces its block, so
/// `--clear` removes it (never leaves an empty file) and converge lifts it
/// before the source adds the path.
#[test]
fn h43_a_created_file_stays_created_across_re_renders() {
    let world = World::new();
    git(&world.src(), &["rm", "--quiet", "CLAUDE.md"]);
    git(&world.src(), &["commit", "--quiet", "-m", "no claude"]);
    settle();
    world.capture();
    assert!(world.apply().ok);
    let claude = world.dst().join("CLAUDE.md");
    let rerendered = |world: &World, content: &[u8]| {
        world.source_change(content);
        let applied = world.apply();
        assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
        assert!(
            applied.stdout.contains("handoff=written"),
            "{}",
            applied.stdout
        );
        let check = world.handoff(&["--check"]);
        assert!(
            row(&check.stdout, &world.dst())
                .contains("{\"path\":\"CLAUDE.md\",\"kind\":\"created\""),
            "{}",
            check.stdout
        );
    };
    assert!(world.handoff(&[]).ok);
    rerendered(&world, b"alpha 2\n");
    let cleared = world.handoff(&["--clear"]);
    assert!(cleared.ok, "{}\n{}", cleared.stdout, cleared.stderr);
    assert!(!claude.exists(), "--clear left a created file behind");
    assert!(world.handoff(&[]).ok);
    rerendered(&world, b"alpha 3\n");
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
    assert_eq!(remainder_of(&claude), b"Back again.\n");
}
