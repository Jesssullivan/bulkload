//! Estate adopt (#226; OI-1003-Q171/Q172, Linear TIN-4543; design
//! `docs/agent-notes/2026-10-15-estate-adopt-design.md`): bulkload lands
//! into the live tree, natively, with no hand script.
//!
//! - P90 ADOPT-GFTB (the acceptance row): the GFTB inventory's shapes
//!   ([`gftb`]) go from "live tree + source" to a converged live tree with
//!   bulkload verbs only (`estate-add-batch`, `estate-capture`,
//!   `estate-diff`, `estate-apply --adopt --activate-origin`,
//!   `closure-report`): every adopted checkout's seats keep every `lstat`
//!   field, the destination's own worktree, branch and build output are
//!   untouched, the source-only worktree, the ssh-origin repository and the
//!   bare preservation repository land; an unchanged rerun reads 0 bytes;
//!   a source change then converges in place.
//! - P91 ADOPT-NO-CLOBBER (F4): a destination that differs from the capture
//!   (a seat, a staged hunk, a switched HEAD) refuses
//!   `GIT_DESTINATION_DIVERGED` with nothing written, worktree or `.git`.
//! - P92 ADOPT-FOREIGN (F11): an unrelated repository at a planned path
//!   refuses `GIT_DESTINATION_OCCUPIED` with nothing written.
//! - P93 ADOPT-REKEY (F12): a ledgered landing moved to its live name and
//!   repaired is re-keyed under a live-path plan.
//! - P94 DIFF-READ-ONLY (F14): `estate-diff` changes nothing at the
//!   destination or in STATE, and its classes agree with what `--adopt`
//!   then does.
//! - P95: without `--adopt` an identical copy stays occupied, untouched.
//! - P96 ADOPT-BARE (F7, A11d): a pre-made empty bare takes the marker,
//!   the native refs and the captured symbolic HEAD under `--adopt`.
//! - P108/P109 DIFF-ONE-IMPORT (M2r2 item 3): `estate-diff` imports a
//!   repository's plan base once, however many of its worktree items it
//!   answers, and its rows are byte-identical to an import per item.
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

#[path = "estate_adopt/gftb.rs"]
mod gftb;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gftb::{copy_tree, freeze, freeze_all, git, refs, settle, view, Gftb};

const SOURCE: &str = "neo";

/// One verb's run: success, stdout, and its `counters` line.
struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
    counters: BTreeMap<String, u64>,
}

fn agent(args: &[OsString]) -> Ran {
    agent_with(args, &[])
}

fn agent_with(args: &[OsString], envs: &[(&str, &std::ffi::OsStr)]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(envs.iter().copied())
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

fn args(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

/// Each receipt line's (source, outcome, reason), as printed: the source
/// and reason `Debug`-quoted.
fn receipts(stdout: &str) -> Vec<(String, String, String)> {
    stdout
        .lines()
        .filter(|line| line.starts_with("item=") && line.contains(" outcome="))
        .map(|line| {
            let between = |from: &str, to: &str| {
                let start = line.find(from).unwrap() + from.len();
                let end = line[start..].find(to).map_or(line.len(), |end| start + end);
                line[start..end].to_owned()
            };
            (
                between(" source=", " outcome="),
                between(" outcome=", " reason="),
                between(" reason=", " source_bytes_read="),
            )
        })
        .collect()
}

fn receipt<'a>(
    rows: &'a [(String, String, String)],
    source: &Path,
) -> &'a (String, String, String) {
    let quoted = format!("\"{}\"", source.display().to_string().escape_debug());
    rows.iter()
        .find(|row| row.0 == quoted)
        .unwrap_or_else(|| panic!("no receipt for {quoted} in {rows:?}"))
}

struct Run<'a> {
    world: &'a Gftb,
    plan: PathBuf,
    corpus: PathBuf,
    cstate: PathBuf,
    applied: PathBuf,
}

impl<'a> Run<'a> {
    fn new(world: &'a Gftb, rows: &[(PathBuf, PathBuf, Option<PathBuf>)]) -> Self {
        let run = Self {
            world,
            plan: world.path("carry/plan.postcard"),
            corpus: world.path("carry/corpus"),
            cstate: world.path("carry/private"),
            applied: world.path("apply-state"),
        };
        fs::create_dir_all(world.path("carry")).unwrap();
        let mut list: Vec<OsString> = vec!["estate-add-batch".into(), run.plan.clone().into()];
        for (source, repository, workspace) in rows {
            list.push(source.clone().into());
            list.push(repository.clone().into());
            list.push(
                workspace
                    .clone()
                    .map_or_else(|| OsString::from("-"), PathBuf::into_os_string),
            );
        }
        let added = agent(&list);
        assert!(added.ok, "{}", added.stderr);
        run
    }

    fn capture(&self) -> Ran {
        let ran = agent(&args(&[
            &"estate-capture",
            &self.plan,
            &self.cstate,
            &self.corpus,
            &"1",
        ]));
        assert!(ran.ok, "capture: {}\n{}", ran.stdout, ran.stderr);
        ran
    }

    fn apply(&self, extra: &[&str]) -> Ran {
        let mut list: Vec<OsString> = vec!["estate-apply".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(args(&[
            &self.plan,
            &self.corpus,
            &self.applied,
            &SOURCE,
            &"1",
        ]));
        agent(&list)
    }

    fn diff(&self, extra: &[&str]) -> Ran {
        self.diff_with(extra, &[])
    }

    fn diff_with(&self, extra: &[&str], envs: &[(&str, &std::ffi::OsStr)]) -> Ran {
        let mut list: Vec<OsString> = vec!["estate-diff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(args(&[&self.plan, &self.corpus, &SOURCE, &self.applied]));
        let ran = agent_with(&list, envs);
        assert!(ran.ok, "diff: {}\n{}", ran.stdout, ran.stderr);
        ran
    }

    fn closure(&self) -> Ran {
        agent(&args(&[
            &"closure-report",
            &self.plan,
            &self.corpus,
            &SOURCE,
            &self.applied,
        ]))
    }

    const fn world(&self) -> &Gftb {
        self.world
    }
}

/// A STATE directory as estate-apply wants it: the user's, mode 0700.
fn private_dir(path: &Path) {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

/// The diff line of the item whose source is `source`.
fn diff_line<'a>(stdout: &'a str, source: &Path) -> &'a str {
    let needle = format!("\"source\":\"{}\"", source.display());
    stdout
        .lines()
        .find(|line| line.contains("\"kind\":\"item\"") && line.contains(&needle))
        .unwrap_or_else(|| panic!("no diff row for {needle} in {stdout}"))
}

#[test]
fn p90_the_gftb_shapes_adopt_and_converge_with_bulkload_verbs_only() {
    let world = Gftb::new();
    let run = Run::new(&world, &world.plan_rows());
    settle();
    run.capture();
    private_dir(&run.applied);
    let infra = world.sting("infra");
    let nest = world.sting("infra/.worktrees/nest");
    let feat = world.sting("infra.worktrees/feat");
    let dest_only = world.sting("infra.worktrees/dest-only");
    let lamesh = world.sting("lamesh");

    // estate-diff: the classes the inventory had, read-only.
    let frozen = freeze_all(&world.sting(""));
    let diff = run.diff(&["--scan", world.sting("").to_str().unwrap()]);
    assert_eq!(freeze_all(&world.sting("")), frozen, "estate-diff wrote");
    for (source, class, adopt) in [
        (world.neo("infra"), "identical", "adoptable"),
        (world.neo("infra/.worktrees/nest"), "identical", "adoptable"),
        (world.neo("infra.worktrees/feat"), "identical", "adoptable"),
        (
            world.neo("infra.worktrees/only-src"),
            "source-only",
            "landing",
        ),
        (world.neo("lamesh"), "identical", "adoptable"),
        (world.neo("sshrepo"), "source-only", "landing"),
        (world.neo("preservation.git"), "source-only", "bare-landing"),
    ] {
        let line = diff_line(&diff.stdout, &source);
        assert!(
            line.contains(&format!("\"class\":\"{class}\""))
                && line.contains(&format!("\"adopt\":\"{adopt}\"")),
            "{line}"
        );
    }
    let infra_row = diff_line(&diff.stdout, &world.neo("infra"));
    assert!(
        infra_row.contains(
            "{\"name\":\"refs/heads/diverge\",\"class\":\"diverged\",\"row\":\"ref-kept-destination\"}"
        ),
        "{infra_row}"
    );
    assert!(
        infra_row.contains(
            "{\"name\":\"refs/heads/dest-work\",\"class\":\"dest-only\",\"row\":\"ref-checked-out-elsewhere\"}"
        ),
        "{infra_row}"
    );
    assert!(
        infra_row.contains("\"rebuildable\":{\"roots\":1,"),
        "{infra_row}"
    );
    let lamesh_row = diff_line(&diff.stdout, &world.neo("lamesh"));
    assert!(
        lamesh_row.contains(
            "{\"name\":\"refs/heads/chore/one\",\"class\":\"source-only\",\"row\":\"ref-created\"}"
        ),
        "{lamesh_row}"
    );
    assert!(
        diff_line(&diff.stdout, &world.neo("sshrepo")).contains("\"origin\":\"inert\""),
        "without --activate-origin the ssh origin stays inert"
    );
    assert!(
        diff.stdout
            .lines()
            .any(|line| line.contains("\"kind\":\"worktree\"")
                && line.contains(&format!("\"path\":\"{}\"", dest_only.display()))
                && line.contains("\"class\":\"dest-only\"")),
        "{}",
        diff.stdout
    );

    let before = (
        freeze(&infra),
        freeze(&nest),
        freeze(&feat),
        freeze(&lamesh),
        view(&dest_only),
        freeze(&dest_only),
    );
    // estate-apply --adopt --activate-origin: the live tree, in place.
    let applied = run.apply(&["--adopt", "--activate-origin", "yoga:git/"]);
    assert!(applied.ok, "{}\n{}", applied.stdout, applied.stderr);
    let rows = receipts(&applied.stdout);
    for source in [
        "infra",
        "infra/.worktrees/nest",
        "infra.worktrees/feat",
        "lamesh",
    ] {
        let row = receipt(&rows, &world.neo(source));
        assert_eq!(row.1, "workspace-restored", "{row:?}");
        assert!(row.2.contains("adopted=1"), "{row:?}");
    }
    assert!(receipt(&rows, &world.neo("infra"))
        .2
        .contains("repository-adopted=1"));
    assert!(receipt(&rows, &world.neo("infra"))
        .2
        .contains("stash-adopted=1"));
    assert!(receipt(&rows, &world.neo("lamesh"))
        .2
        .contains("ref-created=3"));
    assert!(receipt(&rows, &world.neo("infra"))
        .2
        .contains("ref-kept-destination=1"));
    assert_eq!(
        receipt(&rows, &world.neo("infra.worktrees/only-src")).1,
        "workspace-restored"
    );
    let ssh = receipt(&rows, &world.neo("sshrepo"));
    assert!(ssh.2.contains("origin-activated-by-prefix=1"), "{ssh:?}");
    let bare = receipt(&rows, &world.neo("preservation.git"));
    assert_eq!(bare.1, "refs-imported", "{bare:?}");
    assert!(bare.2.contains("bare-landed=1"), "{bare:?}");

    // Adopt wrote no seat of any adopted checkout, and left the
    // destination's own worktree, branch, build output and dirt alone.
    let after = (
        freeze(&infra),
        freeze(&nest),
        freeze(&feat),
        freeze(&lamesh),
        view(&dest_only),
        freeze(&dest_only),
    );
    assert_eq!(after, before, "adopt changed a seat");
    assert_eq!(
        fs::read(infra.join("node_modules/pkg/sting-build.js")).unwrap(),
        b"built on sting\n"
    );
    let branch = |repo: &Path, name: &str| git(repo, &["rev-parse", name]).trim().to_owned();
    assert_eq!(branch(&infra, "diverge"), branch(&infra, "dest-work"));
    assert_ne!(
        branch(&infra, "diverge"),
        branch(&world.neo("infra"), "diverge")
    );
    // Every source ref is native at the destination: created, adopted, or
    // (diverge) kept with the source's value at provenance.
    let have = refs(&infra);
    for (name, value) in refs(&world.neo("infra")) {
        if name == "refs/heads/diverge" || name == "refs/stash" {
            continue;
        }
        assert!(have.contains(&(name.clone(), value)), "{name} missing");
    }
    for branch in ["chore/one", "chore/two", "chore/three"] {
        assert_eq!(
            git(&lamesh, &["rev-parse", branch]),
            git(&world.neo("lamesh"), &["rev-parse", branch])
        );
    }
    assert_eq!(
        git(&lamesh, &["config", "remote.origin.url"]).trim(),
        "yoga:git/LA-Mesh"
    );
    // The source-only worktree landed attached, equal to the source.
    let only = world.sting("infra.worktrees/only-src");
    assert_eq!(view(&only).0, "refs/heads/src-only");
    assert_eq!(
        fs::read(only.join("only-src.txt")).unwrap(),
        fs::read(world.neo("infra.worktrees/only-src/only-src.txt")).unwrap()
    );
    // The ssh origin is active, by the named prefix.
    let sshrepo = world.sting("sshrepo");
    assert_eq!(
        git(&sshrepo, &["config", "remote.origin.url"]).trim(),
        "yoga:git/sshrepo"
    );
    assert_eq!(
        fs::read_to_string(sshrepo.join(".git/carry-config/origin-activated-by-prefix")).unwrap(),
        "yoga:git/\n"
    );
    // The preservation bare: native refs and symbolic HEAD as at the
    // source, borrowing nothing.
    let preservation = world.sting("preservation.git");
    assert_eq!(refs(&preservation), refs(&world.neo("preservation.git")));
    assert_eq!(
        git(&preservation, &["symbolic-ref", "HEAD"]),
        git(&world.neo("preservation.git"), &["symbolic-ref", "HEAD"])
    );
    assert!(!preservation.join("objects/info/alternates").exists());
    assert_eq!(
        git(&preservation, &["rev-parse", "--is-bare-repository"]).trim(),
        "true"
    );

    // Closure: every item applied or referenced-only; the rebuildable root
    // counted exactly.
    let closure = run.closure();
    assert!(closure.ok, "{}", closure.stdout);
    assert!(
        closure
            .stdout
            .contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{}",
        closure.stdout
    );
    assert!(
        closure.stdout.contains(&format!(
            "\"omissions\":{{\"items_known\":7,\"items_unknown\":0,\"roots\":1,\"entries\":3,\"bytes\":{}",
            4096 + "module.exports = 1;\n".len()
        )),
        "{}",
        closure.stdout
    );

    // An unchanged rerun reads nothing: the capture reuses, the apply is a
    // journal no-op.
    settle();
    let recapture = run.capture();
    assert!(
        receipts(&recapture.stdout)
            .iter()
            .all(|row| row.1 == "capture-reused-after-census"),
        "{}",
        recapture.stdout
    );
    assert_eq!(
        recapture.counters.get("read_source_file_bytes"),
        Some(&0),
        "{:?}",
        recapture.counters
    );
    let rerun = run.apply(&[]);
    assert!(rerun.ok, "{}", rerun.stderr);
    assert!(receipts(&rerun.stdout)
        .iter()
        .all(|row| row.1.starts_with("previous-")));
    assert_eq!(
        rerun.counters.get("read_dest_verify_existing_bytes"),
        Some(&0)
    );
    assert!(rerun.stdout.contains("source_bytes_read=0"));

    // A source change converges in place, with no flag.
    fs::write(world.neo("infra/a.txt"), b"alpha, second day\n").unwrap();
    git(&world.neo("infra"), &["commit", "--quiet", "-am", "c2"]);
    settle();
    run.capture();
    let untouched = fs::symlink_metadata(infra.join("run.sh")).unwrap();
    let converged = run.apply(&[]);
    assert!(converged.ok, "{}\n{}", converged.stdout, converged.stderr);
    let row = receipt(&receipts(&converged.stdout), &world.neo("infra")).clone();
    assert!(row.2.contains("converged"), "{row:?}");
    assert_eq!(
        fs::read(infra.join("a.txt")).unwrap(),
        b"alpha, second day\n"
    );
    assert_eq!(
        git(&infra, &["rev-parse", "HEAD"]),
        git(&world.neo("infra"), &["rev-parse", "HEAD"])
    );
    {
        use std::os::unix::fs::MetadataExt as _;
        let now = fs::symlink_metadata(infra.join("run.sh")).unwrap();
        assert_eq!(
            (now.ino(), now.mtime_nsec()),
            (untouched.ino(), untouched.mtime_nsec())
        );
    }
    // The adopted prints held, so the adopted checkouts proved by print
    // alone: the only destination bytes read are the seats of the
    // worktree this apply first landed, all racy against its own ledger
    // (written in the timestamp tick of their checkout).
    let landed_racy: u64 = freeze(&only)
        .iter()
        .filter(|row| row.7 & 0o170_000 == 0o100_000)
        .map(|row| row.6)
        .sum();
    assert_eq!(
        converged.counters.get("read_dest_verify_existing_bytes"),
        Some(&landed_racy),
        "{:?}",
        converged.counters
    );
    assert_eq!(view(&dest_only), before.4);
    let closure = run.closure();
    assert!(
        closure
            .stdout
            .contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{}",
        closure.stdout
    );
}

// A one-checkout estate for the refusal rows: the source, and its copy at
// the destination.
fn small() -> (Gftb, PathBuf, PathBuf) {
    let world = Gftb::new();
    let source = world.neo("lamesh");
    let dest = world.sting("lamesh");
    (world, source, dest)
}

#[test]
fn p91_a_destination_that_differs_refuses_diverged_and_nothing_is_written() {
    for mutate in ["seat", "staged", "head"] {
        let (world, source, dest) = small();
        match mutate {
            "seat" => fs::write(dest.join("README"), b"edited on sting\n").unwrap(),
            "staged" => {
                fs::write(dest.join("new.txt"), b"staged on sting\n").unwrap();
                git(&dest, &["add", "new.txt"]);
            }
            _ => {
                git(&dest, &["switch", "--quiet", "-c", "elsewhere"]);
            }
        }
        let run = Run::new(
            &world,
            &[(source.clone(), dest.clone(), Some(dest.clone()))],
        );
        settle();
        run.capture();
        private_dir(&run.applied);
        let frozen = freeze_all(&dest);
        let ran = run.apply(&["--adopt"]);
        assert!(!ran.ok, "{mutate}");
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refused", "{mutate}: {row:?}");
        assert!(
            row.2.contains("GIT_DESTINATION_DIVERGED"),
            "{mutate}: {row:?}"
        );
        assert_eq!(freeze_all(&dest), frozen, "{mutate}: the refusal wrote");
        let closure = run.closure();
        assert!(
            closure.stdout.contains("\"refused_pending_review\":1"),
            "{mutate}: {}",
            closure.stdout
        );
        let diff = run.diff(&[]);
        let line = diff_line(&diff.stdout, &source);
        assert!(line.contains("\"adopt\":\"diverged\""), "{mutate}: {line}");
        let component = match mutate {
            "seat" => "\"seats=1\"",
            "staged" => "\"index\"",
            _ => "\"head\"",
        };
        assert!(line.contains(component), "{mutate}: {line}");
        let _ = run.world();
    }
}

#[test]
fn p92_an_unrelated_repository_stays_occupied_and_gains_nothing() {
    let (world, source, dest) = small();
    fs::remove_dir_all(&dest).unwrap();
    fs::create_dir(&dest).unwrap();
    git(&dest, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(dest.join("README"), b"LA-Mesh\n").unwrap();
    git(&dest, &["add", "."]);
    git(&dest, &["commit", "--quiet", "-m", "another history"]);
    let run = Run::new(
        &world,
        &[(source.clone(), dest.clone(), Some(dest.clone()))],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let frozen = freeze_all(&dest);
    let ran = run.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(freeze_all(&dest), frozen);
    let diff = run.diff(&[]);
    assert!(diff_line(&diff.stdout, &source).contains("\"adopt\":\"occupied-foreign\""));
}

#[test]
fn p93_a_moved_landing_is_rekeyed_under_a_live_path_plan() {
    let (world, source, live) = small();
    fs::remove_dir_all(&live).unwrap();
    // Land into an estate path, as the GFTB carry did.
    let estate = world.path("estate");
    fs::create_dir(&estate).unwrap();
    let landing = estate.join("lamesh");
    let first = Run {
        world: &world,
        plan: world.path("estate-plan"),
        corpus: world.path("estate-corpus"),
        cstate: world.path("estate-cstate"),
        applied: world.path("estate-applied"),
    };
    let added = agent(&args(&[
        &"estate-add",
        &first.plan,
        &source,
        &landing,
        &landing,
    ]));
    assert!(added.ok, "{}", added.stderr);
    first.capture();
    private_dir(&first.applied);
    assert!(first.apply(&[]).ok);
    // Move it to its live name (one filesystem).
    fs::rename(&landing, &live).unwrap();
    // A live-path plan names another item: without --adopt it is occupied.
    let run = Run::new(
        &world,
        &[(source.clone(), live.clone(), Some(live.clone()))],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let refused = run.apply(&[]);
    assert!(receipt(&receipts(&refused.stdout), &source)
        .2
        .contains("GIT_DESTINATION_OCCUPIED"));
    let frozen = freeze(&live);
    let ran = run.apply(&["--adopt"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-restored");
    assert!(row.2.contains("rekeyed=1"), "{row:?}");
    assert_eq!(freeze(&live), frozen);
    let closure = run.closure();
    assert!(
        closure
            .stdout
            .contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{}",
        closure.stdout
    );
    let rerun = run.apply(&[]);
    assert!(rerun.ok);
    assert!(receipts(&rerun.stdout)
        .iter()
        .all(|row| row.1.starts_with("previous-")));
}

#[test]
fn p94_estate_diff_is_read_only_and_agrees_with_adopt() {
    let world = Gftb::new();
    let run = Run::new(&world, &world.plan_rows());
    settle();
    run.capture();
    private_dir(&run.applied);
    // Some destination drift: one item diverges.
    fs::write(
        world.sting("infra.worktrees/feat/feat.txt"),
        b"edited on sting\n",
    )
    .unwrap();
    let dest = freeze_all(&world.sting(""));
    let state = freeze_all(&run.applied);
    let diff = run.diff(&["--since", "0"]);
    assert_eq!(freeze_all(&world.sting("")), dest);
    assert_eq!(freeze_all(&run.applied), state);
    let ran = run.apply(&["--adopt"]);
    let rows = receipts(&ran.stdout);
    for (source, _, _) in world.plan_rows() {
        let line = diff_line(&diff.stdout, &source);
        let outcome = &receipt(&rows, &source).1;
        let adopted = receipt(&rows, &source).2.contains("adopted=1");
        if line.contains("\"adopt\":\"adoptable\"") {
            assert!(adopted, "{line} vs {outcome}");
        } else if line.contains("\"adopt\":\"diverged\"") {
            assert_eq!(outcome, "refused", "{line}");
            assert!(receipt(&rows, &source)
                .2
                .contains("GIT_DESTINATION_DIVERGED"));
        } else {
            assert!(!adopted, "{line} vs {outcome}");
        }
    }
    let feat = diff_line(&diff.stdout, &world.neo("infra.worktrees/feat"));
    assert!(feat.contains("\"seat=feat.txt\""), "{feat}");
    // The copy at sting is untouched by the refusal.
    assert_eq!(
        fs::read(world.sting("infra.worktrees/feat/feat.txt")).unwrap(),
        b"edited on sting\n"
    );
    let _ = copy_tree;
}

#[test]
fn p95_without_adopt_an_identical_copy_stays_occupied() {
    let (world, source, dest) = small();
    let run = Run::new(
        &world,
        &[(source.clone(), dest.clone(), Some(dest.clone()))],
    );
    settle();
    run.capture();
    let frozen = freeze_all(&dest);
    let ran = run.apply(&[]);
    assert!(!ran.ok);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(freeze_all(&dest), frozen);
    let adopted = run.apply(&["--adopt"]);
    assert!(adopted.ok, "{}\n{}", adopted.stdout, adopted.stderr);
    assert!(receipt(&receipts(&adopted.stdout), &source)
        .2
        .contains("adopted=1"));
}

/// F7 (A11d): the GFTB lane's pre-made empty bare (`git init --bare`, its
/// HEAD unborn), a refs-only item into it. Without `--adopt` it gets
/// provenance only; with `--adopt` it takes the marker, every native ref
/// and the captured symbolic HEAD, with no hand step.
#[test]
fn p96_a_premade_empty_bare_is_adopted_and_takes_native_refs() {
    let world = Gftb::new();
    let source = world.neo("preservation.git");
    for adopt in [false, true] {
        let dest = world.sting(&format!("premade-{adopt}.git"));
        git(
            &world.sting(""),
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=master",
                dest.to_str().unwrap(),
            ],
        );
        let run = Run {
            world: &world,
            plan: world.path(&format!("plan-{adopt}")),
            corpus: world.path(&format!("corpus-{adopt}")),
            cstate: world.path(&format!("cstate-{adopt}")),
            applied: world.path(&format!("applied-{adopt}")),
        };
        let added = agent(&args(&[&"estate-add", &run.plan, &source, &dest]));
        assert!(added.ok, "{}", added.stderr);
        run.capture();
        let ran = run.apply(if adopt { &["--adopt"] } else { &[] });
        assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refs-imported", "{row:?}");
        if adopt {
            assert!(row.2.contains("repository-adopted=1"), "{row:?}");
            assert!(row.2.contains("head-symbolic-set=1"), "{row:?}");
            assert_eq!(refs(&dest), refs(&source));
            assert_eq!(
                git(&dest, &["symbolic-ref", "HEAD"]),
                git(&source, &["symbolic-ref", "HEAD"])
            );
            assert!(dest.join("bulkload/v1/neo/adopted").is_file());
        } else {
            assert!(refs(&dest).is_empty(), "provenance only: {:?}", refs(&dest));
        }
        let closure = run.closure();
        assert!(
            closure
                .stdout
                .contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
            "{}",
            closure.stdout
        );
    }
}

// ---- review of 2026-10-16: custody of an adopted or re-keyed path ----------

/// A source repository with its own root commit (no history shared with
/// the fixture's), for the custody rows below.
fn unrelated_source(world: &Gftb, name: &str) -> PathBuf {
    let repo = world.neo(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(repo.join("other.txt"), format!("{name}, another project\n")).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "x c1"]);
    repo
}

/// [`freeze_all`] of a checkout bulkload landed, less its destination
/// lock (`bulkload/v1/neo/lock` and the directory holding it), which any
/// apply of a ledgered checkout takes before it reads the ledger.
fn freeze_landed(root: &Path) -> Vec<(PathBuf, u64, i64, i64, i64, u64, u32)> {
    freeze_all(root)
        .into_iter()
        .filter(|row| {
            !row.0.ends_with("bulkload/v1/neo/lock") && !row.0.ends_with("bulkload/v1/neo")
        })
        .collect()
}

/// The workspace ledger of a checkout, as bytes.
fn ledger_bytes(worktree: &Path) -> Vec<u8> {
    fs::read(worktree.join(".git/bulkload/v1/neo/workspace.landed")).unwrap()
}

/// A plan of its own (plan, corpus, capture state, apply state) under
/// `name`, holding `rows`.
fn named_run<'a>(world: &'a Gftb, name: &str, rows: &[(&Path, &Path, Option<&Path>)]) -> Run<'a> {
    let run = Run {
        world,
        plan: world.path(&format!("{name}-plan")),
        corpus: world.path(&format!("{name}-corpus")),
        cstate: world.path(&format!("{name}-cstate")),
        applied: world.path(&format!("{name}-applied")),
    };
    let mut list: Vec<OsString> = vec!["estate-add-batch".into(), run.plan.clone().into()];
    for (source, repository, workspace) in rows {
        list.push(source.as_os_str().to_owned());
        list.push(repository.as_os_str().to_owned());
        list.push(
            workspace.map_or_else(|| OsString::from("-"), |path| path.as_os_str().to_owned()),
        );
    }
    let added = agent(&list);
    assert!(added.ok, "{}", added.stderr);
    run
}

/// P97 ADOPT-CANONICAL (review finding 1, repro 2): an unlanded copy
/// reached through a symlink is never adopted under the symlink's
/// spelling: `GIT_DESTINATION_OCCUPIED`, nothing written, and
/// `estate-diff` says `symlinked-path`. The same tree under its real path
/// adopts.
#[test]
fn p97_adopt_refuses_a_destination_spelled_through_a_symlink() {
    let (world, source, live) = small();
    let real = world.sting("real");
    fs::create_dir(&real).unwrap();
    let tree = real.join("lamesh");
    fs::rename(&live, &tree).unwrap();
    std::os::unix::fs::symlink("real/lamesh", &live).unwrap();
    let run = named_run(&world, "link", &[(&source, &live, Some(&live))]);
    settle();
    run.capture();
    private_dir(&run.applied);
    let frozen = freeze_all(&tree);
    let ran = run.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(freeze_all(&tree), frozen, "the refusal wrote");
    let diff = run.diff(&[]);
    let row_line = diff_line(&diff.stdout, &source);
    assert!(
        row_line.contains("\"adopt\":\"symlinked-path\""),
        "{row_line}"
    );
    // The control: the real path adopts.
    let control = named_run(&world, "real", &[(&source, &tree, Some(&tree))]);
    control.capture();
    private_dir(&control.applied);
    let adopted = control.apply(&["--adopt"]);
    assert!(adopted.ok, "{}\n{}", adopted.stdout, adopted.stderr);
    assert!(receipt(&receipts(&adopted.stdout), &source)
        .2
        .contains("adopted=1"));
}

/// P98 REKEY-CANONICAL (review finding 1, repro 1, the production GFTB
/// shape): plan P1 lands at an estate path, a live name is a symlink into
/// it, and a live-path plan P2 under `--adopt` never re-keys P1's ledger:
/// `GIT_DESTINATION_OCCUPIED`, the ledger's bytes unchanged, and P1 still
/// owns its landing.
#[test]
fn p98_a_symlinked_live_path_never_rekeys_the_estate_path_ledger() {
    let (world, source, live) = small();
    fs::remove_dir_all(&live).unwrap();
    let estate = world.sting("estate");
    fs::create_dir(&estate).unwrap();
    let landing = estate.join("lamesh");
    let first = named_run(&world, "p1", &[(&source, &landing, Some(&landing))]);
    first.capture();
    private_dir(&first.applied);
    assert!(first.apply(&[]).ok);
    std::os::unix::fs::symlink("estate/lamesh", &live).unwrap();
    let ledger = ledger_bytes(&landing);
    let second = named_run(&world, "p2", &[(&source, &live, Some(&live))]);
    settle();
    second.capture();
    private_dir(&second.applied);
    let frozen = freeze_landed(&landing);
    let ran = second.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(ledger_bytes(&landing), ledger, "the ledger was re-keyed");
    assert_eq!(freeze_landed(&landing), frozen, "the refusal wrote");
    let diff = second.diff(&[]);
    let row_line = diff_line(&diff.stdout, &source);
    assert!(
        row_line.contains("\"adopt\":\"symlinked-path\""),
        "{row_line}"
    );
    // P1 still owns it: its rerun is the journal no-op.
    let rerun = first.apply(&[]);
    assert!(rerun.ok, "{}\n{}", rerun.stdout, rerun.stderr);
}

/// P99 REKEY-IDENTITY (review findings 2 and 8): a re-key is a relabel of
/// the same tree, never a hand-over. A plan typo naming another source at
/// a landed path refuses `GIT_DESTINATION_OCCUPIED` under `--adopt` with
/// nothing written, whether the other source is unrelated or shares the
/// history but not the landed HEAD; the landing's own plan keeps it.
#[test]
fn p99_a_rekey_refuses_a_source_that_is_not_the_landed_tree() {
    for shape in ["unrelated", "related"] {
        let (world, source, _) = small();
        let landing = world.sting("y");
        let first = named_run(&world, "p1", &[(&source, &landing, Some(&landing))]);
        first.capture();
        private_dir(&first.applied);
        assert!(first.apply(&[]).ok);
        let other = if shape == "unrelated" {
            unrelated_source(&world, "x")
        } else {
            // The same history, its HEAD on an orphan branch.
            let other = world.neo("x");
            git(
                &world.neo(""),
                &["clone", "--quiet", source.to_str().unwrap(), "x"],
            );
            git(&other, &["switch", "--quiet", "--orphan", "elsewhere"]);
            fs::write(other.join("other.txt"), b"elsewhere\n").unwrap();
            git(&other, &["add", "other.txt"]);
            git(&other, &["commit", "--quiet", "-m", "x c1"]);
            other
        };
        let ledger = ledger_bytes(&landing);
        let typo = named_run(&world, "typo", &[(&other, &landing, Some(&landing))]);
        settle();
        typo.capture();
        private_dir(&typo.applied);
        let frozen = freeze_landed(&landing);
        let ran = typo.apply(&["--adopt"]);
        let row = receipt(&receipts(&ran.stdout), &other).clone();
        assert!(
            row.2.contains("GIT_DESTINATION_OCCUPIED"),
            "{shape}: {row:?}"
        );
        assert_eq!(ledger_bytes(&landing), ledger, "{shape}: re-keyed");
        assert_eq!(
            freeze_landed(&landing),
            frozen,
            "{shape}: the refusal wrote"
        );
        let diff = typo.diff(&[]);
        let line = diff_line(&diff.stdout, &other);
        assert!(
            line.contains("\"adopt\":\"occupied-foreign\""),
            "{shape}: {line}"
        );
        let rerun = first.apply(&[]);
        assert!(rerun.ok, "{shape}: {}\n{}", rerun.stdout, rerun.stderr);
    }
}

/// P100 ADOPT-LINK (review finding 3): a linked worktree moved without
/// `git worktree repair` (its admin directory's backlink names its old
/// path; `git worktree prune` would remove the admin directory and the
/// ledger with it) is never adopted: `GIT_DESTINATION_OCCUPIED` with
/// nothing in its admin directory, and `estate-diff` says
/// `occupied-shape` with the component `worktree-link`.
#[test]
fn p100_a_linked_worktree_whose_backlink_is_stale_is_not_adopted() {
    let world = Gftb::new();
    let feat = world.sting("infra.worktrees/feat");
    let old = world.sting("infra.worktrees/feat-old");
    fs::rename(&feat, &old).unwrap();
    git(&old, &["worktree", "repair"]);
    fs::rename(&old, &feat).unwrap();
    let admin = PathBuf::from(
        fs::read_to_string(feat.join(".git"))
            .unwrap()
            .trim()
            .strip_prefix("gitdir: ")
            .unwrap(),
    );
    let run = Run::new(&world, &world.plan_rows());
    settle();
    run.capture();
    private_dir(&run.applied);
    let frozen = freeze_all(&admin);
    let ran = run.apply(&["--adopt"]);
    let rows = receipts(&ran.stdout);
    let row = receipt(&rows, &world.neo("infra.worktrees/feat"));
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(freeze_all(&admin), frozen, "the refusal wrote");
    assert!(!admin.join("bulkload").exists());
    // The main beside it still adopts.
    assert!(receipt(&rows, &world.neo("infra")).2.contains("adopted=1"));
    let diff = run.diff(&[]);
    let line = diff_line(&diff.stdout, &world.neo("infra.worktrees/feat"));
    assert!(
        line.contains("\"adopt\":\"occupied-shape\"") && line.contains("\"worktree-link\""),
        "{line}"
    );
}

/// P101 ADOPT-FOREIGN-REFS (review finding 7): with `--adopt`, a refs-only
/// item into an unrelated repository refuses `GIT_DESTINATION_OCCUPIED`
/// after the provenance import (as without the flag): no marker, no
/// `adopted` record, its native refs unchanged, and closure shows the
/// typed refusal, never an applied item.
#[test]
fn p101_adopt_into_an_unrelated_refs_only_repository_refuses_occupied() {
    let world = Gftb::new();
    let source = world.neo("preservation.git");
    let work = unrelated_source(&world, "foreign-work");
    let dest = world.sting("foreign.git");
    git(
        &world.sting(""),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
    );
    let before = refs(&dest);
    let run = named_run(&world, "foreign", &[(&source, &dest, None)]);
    run.capture();
    private_dir(&run.applied);
    let ran = run.apply(&["--adopt"]);
    assert!(!ran.ok, "{}", ran.stdout);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(refs(&dest), before);
    assert!(!dest.join("bulkload/v1/neo/adopted").exists());
    assert!(!dest.join("bulkload/v1/neo/refs.landed").exists());
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"refused_pending_review\":1"),
        "{}",
        closure.stdout
    );
    let diff = run.diff(&[]);
    assert!(diff_line(&diff.stdout, &source).contains("\"adopt\":\"occupied-foreign\""));
}

/// P102 DIFF-SHAPE (review finding 11): where the apply refuses
/// `GIT_DESTINATION_OCCUPIED` before any proof (a checkout of another
/// repository than the plan's), `estate-diff` never says `adoptable`.
#[test]
fn p102_estate_diff_reports_the_shape_the_apply_refuses() {
    let world = Gftb::new();
    let source = world.neo("infra.worktrees/feat");
    let feat = world.sting("infra.worktrees/feat");
    // The plan names lamesh as the repository of infra's worktree.
    let lamesh = world.sting("lamesh");
    let run = named_run(&world, "shape", &[(&source, &lamesh, Some(&feat))]);
    settle();
    run.capture();
    private_dir(&run.applied);
    let ran = run.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    let diff = run.diff(&[]);
    let line = diff_line(&diff.stdout, &source);
    assert!(
        line.contains("\"adopt\":\"occupied-shape\"") && line.contains("\"other-repository\""),
        "{line}"
    );
}

/// The `GloriousFlywheel` shape of the 2026-10-16 dogfood (findings F1 and
/// F2): the destination's main checkout carries an `info/exclude` the
/// source lacks, so its adoption refuses `GIT_DESTINATION_DIVERGED`; a
/// source-only linked worktree then meets a common exclude that is not the
/// capture's (`GIT_IGNORE_POLICY_CONFLICT`), and another is planned under
/// a parent the destination cannot have (`GIT_DESTINATION_PARENT_MISSING`:
/// since M2 finding 1 an absent parent is made, so the path runs through a
/// regular file, which never is).
fn linked_refusals(world: &Gftb) -> (Run<'_>, PathBuf, PathBuf) {
    let infra = world.sting("infra");
    fs::create_dir_all(infra.join(".git/info")).unwrap();
    fs::write(infra.join(".git/info/exclude"), b"sting-only/\n").unwrap();
    fs::write(world.sting("missing"), b"a file, not a directory\n").unwrap();
    let conflict = world.neo("infra.worktrees/only-src");
    let orphan = world.neo("infra/.worktrees/nest");
    let main = world.neo("infra");
    let run = named_run(
        world,
        "linked",
        &[
            (&main, &infra, Some(&infra)),
            (
                &conflict,
                &infra,
                Some(&world.sting("infra.worktrees/only-src")),
            ),
            (&orphan, &infra, Some(&world.sting("missing/deeper/nest"))),
        ],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    (run, conflict, orphan)
}

/// Every ref of `repo`, bulkload's namespaces included.
fn all_refs(repo: &Path) -> String {
    git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// P103 LINKED-REFUSAL-WRITES-NOTHING (dogfood F2): a first linked landing
/// that refuses `GIT_IGNORE_POLICY_CONFLICT` or
/// `GIT_DESTINATION_PARENT_MISSING` decides it before any write: the
/// destination repository's refs (provenance included) are unchanged, and
/// so is every entry of its `.git`. Before the fix the provenance import
/// ran before the exclude check and left `refs/carry/v1/neo/...` behind.
#[test]
fn p103_a_refused_linked_landing_leaves_the_repository_untouched() {
    let world = Gftb::new();
    let (run, conflict, orphan) = linked_refusals(&world);
    let infra = world.sting("infra");
    let refs_before = all_refs(&infra);
    let frozen = freeze_all(&infra.join(".git"));
    let ran = run.apply(&["--adopt"]);
    assert!(!ran.ok, "{}", ran.stdout);
    let rows = receipts(&ran.stdout);
    let main = receipt(&rows, &world.neo("infra"));
    assert!(main.2.contains("GIT_DESTINATION_DIVERGED"), "{main:?}");
    let row = receipt(&rows, &conflict);
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_IGNORE_POLICY_CONFLICT"), "{row:?}");
    let row = receipt(&rows, &orphan);
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_PARENT_MISSING"), "{row:?}");
    assert_eq!(
        all_refs(&infra),
        refs_before,
        "a refused linked landing left refs in the destination repository"
    );
    assert_eq!(
        freeze_all(&infra.join(".git")),
        frozen,
        "a refused linked landing wrote"
    );
    assert!(!world.sting("infra.worktrees/only-src").exists());
    assert_eq!(
        fs::read(world.sting("missing")).unwrap(),
        b"a file, not a directory\n"
    );
}

/// P104 DIFF-LINKED (dogfood F1): `estate-diff` runs a first linked
/// landing's pre-landing checks (the apply's own function) and reports
/// `ignore-policy-conflict` and `parent-missing`, never `landing`, where
/// the apply then refuses `GIT_IGNORE_POLICY_CONFLICT` and
/// `GIT_DESTINATION_PARENT_MISSING`.
#[test]
fn p104_estate_diff_reports_the_linked_refusals_the_apply_makes() {
    let world = Gftb::new();
    let (run, conflict, orphan) = linked_refusals(&world);
    let diff = run.diff(&[]);
    let line = diff_line(&diff.stdout, &conflict);
    assert!(
        line.contains("\"class\":\"source-only\"")
            && line.contains("\"adopt\":\"ignore-policy-conflict\""),
        "{line}"
    );
    let line = diff_line(&diff.stdout, &orphan);
    assert!(
        line.contains("\"class\":\"source-only\"") && line.contains("\"adopt\":\"parent-missing\""),
        "{line}"
    );
    let ran = run.apply(&["--adopt"]);
    let rows = receipts(&ran.stdout);
    assert!(receipt(&rows, &conflict)
        .2
        .contains("GIT_IGNORE_POLICY_CONFLICT"));
    assert!(receipt(&rows, &orphan)
        .2
        .contains("GIT_DESTINATION_PARENT_MISSING"));
}

/// Every `*.parents` record in `state`, as raw bytes.
fn parents_records(state: &Path) -> Vec<Vec<u8>> {
    let mut found: Vec<Vec<u8>> = fs::read_dir(state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "parents"))
        .map(|path| fs::read(path).unwrap())
        .collect();
    found.sort();
    found
}

fn contains(haystack: &[u8], needle: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let needle = needle.as_os_str().as_bytes();
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// P105 PARENTS-MADE (M2 finding 1, 2026-10-16: 42 of 104 in-place items
/// refused `GIT_DESTINATION_PARENT_MISSING`): a standalone workspace, a
/// linked worktree and a bare repository planned under absent parents
/// land. `estate-diff` says `landing` (`bare-landing`) with `parents=2`
/// and writes nothing; the apply makes each absent level 0755, counts it
/// in the receipt (`parent-created=2`) and records it in STATE
/// (`{item}-{source}.parents`); the rerun is a journal no-op that makes
/// nothing.
#[test]
fn p105_a_first_landing_makes_its_absent_parents() {
    let world = Gftb::new();
    let infra = world.sting("infra");
    let standalone = world.sting("made/deep/lamesh");
    let linked = world.sting("infra.made/deep/only-src");
    let bare = world.sting("bare-made/x/preservation.git");
    let run = named_run(
        &world,
        "parents",
        &[
            (&world.neo("infra"), &infra, Some(&infra)),
            (
                &world.neo("infra.worktrees/only-src"),
                &infra,
                Some(&linked),
            ),
            (&world.neo("lamesh"), &standalone, Some(&standalone)),
            (&world.neo("preservation.git"), &bare, None),
        ],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let frozen = freeze_all(&world.sting(""));
    let diff = run.diff(&[]);
    assert_eq!(freeze_all(&world.sting("")), frozen, "estate-diff wrote");
    for (source, adopt) in [
        ("lamesh", "landing"),
        ("infra.worktrees/only-src", "landing"),
        ("preservation.git", "bare-landing"),
    ] {
        let line = diff_line(&diff.stdout, &world.neo(source));
        assert!(
            line.contains("\"class\":\"source-only\"")
                && line.contains(&format!("\"adopt\":\"{adopt}\""))
                && line.contains("\"parents=2\""),
            "{line}"
        );
    }
    let ran = run.apply(&["--adopt"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let rows = receipts(&ran.stdout);
    for (source, outcome) in [
        ("lamesh", "workspace-restored"),
        ("infra.worktrees/only-src", "workspace-restored"),
        ("preservation.git", "refs-imported"),
    ] {
        let row = receipt(&rows, &world.neo(source));
        assert_eq!(row.1, outcome, "{row:?}");
        assert!(row.2.contains("parent-created=2"), "{row:?}");
    }
    assert_eq!(
        git(&standalone, &["rev-parse", "HEAD"]),
        git(&world.neo("lamesh"), &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&linked, &["rev-parse", "--git-common-dir"]).trim(),
        infra.join(".git").to_str().unwrap()
    );
    assert_eq!(
        git(&bare, &["rev-parse", "--is-bare-repository"]).trim(),
        "true"
    );
    let made = [
        "made",
        "made/deep",
        "infra.made",
        "infra.made/deep",
        "bare-made",
        "bare-made/x",
    ];
    for level in made {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = fs::symlink_metadata(world.sting(level)).unwrap();
        assert!(meta.is_dir(), "{level}");
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755, "{level}");
    }
    let records = parents_records(&run.applied);
    assert_eq!(records.len(), 3, "one record per item that made parents");
    for made in made {
        assert!(
            records
                .iter()
                .filter(|record| contains(record, &world.sting(made)))
                .count()
                >= 1,
            "{made} is recorded"
        );
    }
    let again = run.apply(&["--adopt"]);
    assert!(again.ok, "{}\n{}", again.stdout, again.stderr);
    assert!(!again.stdout.contains("parent-created"), "{}", again.stdout);
    assert_eq!(parents_records(&run.applied), records);
}

/// P106 PARENTS-POLICY (M2 finding 1): absent parents are never made
/// through a symlink. A workspace planned below a link to a directory
/// outside the plan, under a level that does not exist there, stays
/// `parent-missing` in `estate-diff` and `GIT_DESTINATION_PARENT_MISSING`
/// in the apply, and nothing is made where the link points.
#[test]
fn p106_absent_parents_are_never_made_through_a_link() {
    let world = Gftb::new();
    let elsewhere = world.path("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, world.sting("via-link")).unwrap();
    let dest = world.sting("via-link/new/lamesh");
    let run = named_run(
        &world,
        "policy",
        &[(&world.neo("lamesh"), &dest, Some(&dest))],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let line = run.diff(&[]).stdout;
    let line = diff_line(&line, &world.neo("lamesh"));
    assert!(line.contains("\"adopt\":\"parent-missing\""), "{line}");
    let ran = run.apply(&[]);
    assert!(!ran.ok, "{}", ran.stdout);
    let row = receipt(&receipts(&ran.stdout), &world.neo("lamesh")).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_PARENT_MISSING"), "{row:?}");
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    assert!(parents_records(&run.applied).is_empty());
}

/// P107 PARENTS-ORDER (M2 item 1 review): a bare repository planned below
/// a standalone workspace of the same plan lands after it, whatever the
/// plan's order: the bare landing never makes the workspace's own
/// directory as its parent (which would leave the workspace
/// `GIT_DESTINATION_OCCUPIED` for good).
#[test]
fn p107_a_bare_landing_below_a_planned_workspace_lands_after_it() {
    let world = Gftb::new();
    let outer = world.sting("outer");
    let bare = world.sting("outer/sub/preservation.git");
    let run = named_run(
        &world,
        "order",
        &[
            (&world.neo("preservation.git"), &bare, None),
            (&world.neo("lamesh"), &outer, Some(&outer)),
        ],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let ran = run.apply(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let rows = receipts(&ran.stdout);
    assert_eq!(
        receipt(&rows, &world.neo("lamesh")).1,
        "workspace-restored",
        "{rows:?}"
    );
    let row = receipt(&rows, &world.neo("preservation.git"));
    assert_eq!(row.1, "refs-imported", "{row:?}");
    assert!(row.2.contains("parent-created=1"), "{row:?}");
    assert_eq!(
        git(&bare, &["rev-parse", "--is-bare-repository"]).trim(),
        "true"
    );
}

const SHARED_OFF: (&str, &str) = ("BULKLOAD_DIFF_SHARED_IMPORT", "0");

fn counter(ran: &Ran, name: &str) -> u64 {
    *ran.counters
        .get(name)
        .unwrap_or_else(|| panic!("no {name} in {}", ran.stderr))
}

/// P108 DIFF-ONE-IMPORT-EQUAL (M2r2 item 3): over the GFTB shapes plus
/// four more source-only worktrees of `infra` (every row class the
/// fixture has: landed, diverged, first linked landings, dest-only),
/// `estate-diff` with the shared import prints exactly the bytes the
/// import-per-item arm prints, imports each plan base once, and lends it
/// to every scratch the other arm built its own import in.
#[test]
fn p108_estate_diff_shares_one_base_import_and_answers_the_same_bytes() {
    let world = Gftb::new();
    let infra = world.neo("infra");
    let mut rows = world.plan_rows();
    for n in 0..4 {
        let path = world.neo(&format!("infra.worktrees/extra-{n}"));
        git(
            &infra,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &format!("extra-{n}"),
                path.to_str().unwrap(),
                "v1",
            ],
        );
        fs::write(path.join("extra.txt"), format!("extra {n}\n")).unwrap();
        rows.push((
            path,
            world.sting("infra"),
            Some(world.sting(&format!("infra.worktrees/extra-{n}"))),
        ));
    }
    let run = Run::new(&world, &rows);
    settle();
    run.capture();
    private_dir(&run.applied);
    fs::write(
        world.sting("infra.worktrees/feat/feat.txt"),
        b"edited on sting\n",
    )
    .unwrap();
    let dest = freeze_all(&world.sting(""));
    let corpus = freeze_all(&run.corpus);
    let shared = run.diff(&["--since", "0"]);
    let off = run.diff_with(&["--since", "0"], &[(SHARED_OFF.0, SHARED_OFF.1.as_ref())]);
    assert_eq!(freeze_all(&world.sting("")), dest, "estate-diff wrote");
    assert_eq!(freeze_all(&run.corpus), corpus, "estate-diff left a stage");
    assert_eq!(shared.stdout, off.stdout, "the shared import changed a row");
    assert_eq!(
        shared.stdout.lines().count(),
        rows.len() + 2,
        "{}",
        shared.stdout
    );
    // One import per base (infra's), lent to every other scratch.
    assert_eq!(
        counter(&shared, "git_scratch_base_imports"),
        1,
        "{}",
        shared.stderr
    );
    let lent = counter(&shared, "git_scratch_base_borrowed");
    assert!(lent >= 7, "{lent}: {}", shared.stderr);
    assert_eq!(counter(&off, "git_scratch_base_borrowed"), 0);
    assert_eq!(
        counter(&off, "git_scratch_base_imports"),
        lent,
        "{}",
        off.stderr
    );
    for n in 0..4 {
        let line = diff_line(
            &shared.stdout,
            &world.neo(&format!("infra.worktrees/extra-{n}")),
        );
        assert!(line.contains("\"class\":\"source-only\""), "{line}");
    }
}

// Each `git index-pack` a GIT_TRACE2_EVENT file records, as the repository
// (worktree) it ran in.
fn index_packs(trace: &Path) -> Vec<String> {
    let text = fs::read_to_string(trace).unwrap();
    let field = |line: &str, key: &str| -> Option<String> {
        let start = line.find(&format!("\"{key}\":\""))? + key.len() + 4;
        let end = line[start..].find('"')? + start;
        Some(line[start..end].to_owned())
    };
    let mut worktrees = BTreeMap::new();
    let mut packs = Vec::new();
    for line in text.lines() {
        let Some(sid) = field(line, "sid") else {
            continue;
        };
        if line.contains("\"event\":\"def_repo\"") {
            worktrees.insert(sid.clone(), field(line, "worktree").unwrap_or_default());
        }
        if line.contains("\"event\":\"cmd_name\"") && line.contains("\"name\":\"index-pack\"") {
            packs.push(sid);
        }
    }
    packs
        .iter()
        .map(|sid| worktrees.get(sid).cloned().unwrap_or_default())
        .collect()
}

/// P109 DIFF-ONE-INDEX-PACK (M2r2 item 3, the 10-16 in-place finding:
/// estate-diff indexed a repository's whole bundle once per worktree
/// item): two repositories of a main and four linked worktrees each,
/// against destination repositories that hold none of their objects, so
/// every base import is a real `index-pack`. With the shared import Git
/// runs exactly one index-pack of each base (in its shared scratch), and
/// no item's scratch indexes more than its own capture; the
/// import-per-item arm indexes a base in every one of them.
#[test]
fn p109_estate_diff_runs_one_index_pack_per_repository() {
    let world = Gftb::new();
    let mut rows = Vec::new();
    for name in ["one", "two"] {
        let repo = world.neo(name);
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
        for n in 0..20 {
            fs::write(repo.join(format!("f{n}.txt")), format!("{name} {n}\n")).unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "--quiet", "-m", &format!("c{n}")]);
        }
        rows.push((repo.clone(), world.sting(name), Some(world.sting(name))));
        for n in 0..4 {
            let path = world.neo(&format!("{name}.worktrees/w{n}"));
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    &format!("w{n}"),
                    path.to_str().unwrap(),
                    "main",
                ],
            );
            rows.push((
                path,
                world.sting(name),
                Some(world.sting(&format!("{name}.worktrees/w{n}"))),
            ));
        }
        // The destination's own, unrelated repository at the same path.
        let dest = world.sting(name);
        fs::create_dir(&dest).unwrap();
        git(&dest, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(dest.join("own.txt"), b"sting's own\n").unwrap();
        git(&dest, &["add", "."]);
        git(&dest, &["commit", "--quiet", "-m", "own"]);
    }
    let run = Run::new(&world, &rows);
    settle();
    run.capture();
    private_dir(&run.applied);
    let traced = |name: &str, off: bool| {
        let trace = world.path(name);
        let mut envs: Vec<(&str, &std::ffi::OsStr)> = vec![("GIT_TRACE2_EVENT", trace.as_os_str())];
        if off {
            envs.push((SHARED_OFF.0, SHARED_OFF.1.as_ref()));
        }
        let ran = run.diff_with(&[], &envs);
        let packs = index_packs(&trace);
        (ran, packs)
    };
    let (shared, packs) = traced("shared.trace", false);
    let (off, off_packs) = traced("off.trace", true);
    assert_eq!(shared.stdout, off.stdout, "the shared import changed a row");
    assert_eq!(
        counter(&shared, "git_scratch_base_imports"),
        2,
        "{}",
        shared.stderr
    );
    let per_repo = |packs: &[String]| {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for worktree in packs {
            *counts.entry(worktree.clone()).or_default() += 1;
        }
        counts
    };
    let counts = per_repo(&packs);
    let bases: Vec<_> = counts
        .iter()
        .filter(|(worktree, _)| worktree.contains("/shared-base-"))
        .collect();
    assert_eq!(bases.len(), 2, "{counts:#?}");
    assert!(bases.iter().all(|(_, count)| **count == 1), "{counts:#?}");
    assert!(counts.values().all(|count| *count == 1), "{counts:#?}");
    // The other arm indexes a base in every scratch that needs one: two
    // index-packs in a scratch (the base, then the item's own capture).
    let off_counts = per_repo(&off_packs);
    assert!(
        off_counts
            .keys()
            .all(|worktree| !worktree.contains("/shared-base-")),
        "{off_counts:#?}"
    );
    let doubled = off_counts.values().filter(|count| **count == 2).count();
    assert!(doubled >= 8, "{off_counts:#?}");
    assert_eq!(
        u64::try_from(doubled).unwrap(),
        counter(&off, "git_scratch_base_imports"),
        "{off_counts:#?}"
    );
}
