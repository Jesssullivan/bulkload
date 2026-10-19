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
//! - P130/P131 DIFF-ONE-IMPORT (M2r2 item 3): `estate-diff` imports a
//!   repository's plan base once, however many of its worktree items it
//!   answers, and its rows are byte-identical to an import per item.
//! - P120/P121 SIDE-ROOT-SEAT (M2r2 item e): live paths that are symlinks
//!   into a side root (the GFTB shape the 10-16 lane converted by hand
//!   with `cp -a`) are seated by `estate-apply --side-root` and re-keyed
//!   there; `estate-unseat` undoes it exactly.
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

/// P130 DIFF-ONE-IMPORT-EQUAL (M2r2 item 3): over the GFTB shapes plus
/// four more source-only worktrees of `infra` (every row class the
/// fixture has: landed, diverged, first linked landings, dest-only),
/// `estate-diff` with the shared import prints exactly the bytes the
/// import-per-item arm prints, imports each plan base once, and lends it
/// to every scratch the other arm built its own import in.
#[test]
fn p130_estate_diff_shares_one_base_import_and_answers_the_same_bytes() {
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

// One GIT_TRACE2_EVENT file's processes: each one's command name, the
// repository (worktree) it ran in, and its argv as traced.
struct Traced {
    name: String,
    worktree: String,
    argv: String,
}

fn traced(trace: &Path) -> Vec<Traced> {
    let text = fs::read_to_string(trace).unwrap();
    let field = |line: &str, key: &str| -> Option<String> {
        let start = line.find(&format!("\"{key}\":\""))? + key.len() + 4;
        let end = line[start..].find('"')? + start;
        Some(line[start..end].to_owned())
    };
    let mut processes: BTreeMap<String, Traced> = BTreeMap::new();
    for line in text.lines() {
        let Some(sid) = field(line, "sid") else {
            continue;
        };
        let process = processes.entry(sid).or_insert_with(|| Traced {
            name: String::new(),
            worktree: String::new(),
            argv: String::new(),
        });
        if line.contains("\"event\":\"start\"") {
            let start = line.find("\"argv\":[").unwrap();
            let end = line[start..].find(']').unwrap() + start;
            line[start..=end].clone_into(&mut process.argv);
        }
        if line.contains("\"event\":\"def_repo\"") {
            process.worktree = field(line, "worktree").unwrap_or_default();
        }
        if line.contains("\"event\":\"cmd_name\"") {
            process.name = field(line, "name").unwrap_or_default();
        }
    }
    processes.into_values().collect()
}

// The repository each `git index-pack` ran in.
fn index_packs(processes: &[Traced]) -> Vec<String> {
    processes
        .iter()
        .filter(|process| process.name == "index-pack")
        .map(|process| process.worktree.clone())
        .collect()
}

// Repositories `names` at neo, each a main checkout of 20 commits and
// `worktrees` linked worktrees, and at sting an unrelated repository of
// the same name (it holds none of their objects). The rows plan each main
// at `main(name)` and each worktree into the sting repository.
fn worktree_world(
    names: &[&str],
    worktrees: usize,
    main: &dyn Fn(&Gftb, &str) -> PathBuf,
) -> (Gftb, Vec<(PathBuf, PathBuf, Option<PathBuf>)>) {
    let world = Gftb::new();
    let mut rows = Vec::new();
    for name in names {
        let repo = world.neo(name);
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
        for n in 0..20 {
            fs::write(repo.join(format!("f{n}.txt")), format!("{name} {n}\n")).unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "--quiet", "-m", &format!("c{n}")]);
        }
        let landing = main(&world, name);
        rows.push((repo.clone(), landing.clone(), Some(landing)));
        for n in 0..worktrees {
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
        let dest = world.sting(name);
        fs::create_dir(&dest).unwrap();
        git(&dest, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(dest.join("own.txt"), b"sting's own\n").unwrap();
        git(&dest, &["add", "."]);
        git(&dest, &["commit", "--quiet", "-m", "own"]);
    }
    (world, rows)
}

/// P131 DIFF-ONE-INDEX-PACK (M2r2 item 3, the 10-16 in-place finding:
/// estate-diff indexed a repository's whole bundle once per worktree
/// item): two repositories of a main and four linked worktrees each,
/// against destination repositories that hold none of their objects, so
/// every base import is a real `index-pack`. With the shared import Git
/// runs exactly one index-pack of each base (in its shared scratch), no
/// item's scratch indexes more than its own capture, and no Git process in
/// an item's scratch names a base's bundle (an import of it there, even
/// one that `index-pack` would skip as connected, walks the whole base);
/// the import-per-item arm indexes a base in every one of them.
#[test]
fn p131_estate_diff_runs_one_index_pack_per_repository() {
    let (world, rows) = worktree_world(&["one", "two"], 4, &|world, name| world.sting(name));
    let run = Run::new(&world, &rows);
    settle();
    run.capture();
    private_dir(&run.applied);
    let diff = |name: &str, off: bool| {
        let trace = world.path(name);
        let mut envs: Vec<(&str, &std::ffi::OsStr)> = vec![("GIT_TRACE2_EVENT", trace.as_os_str())];
        if off {
            envs.push((SHARED_OFF.0, SHARED_OFF.1.as_ref()));
        }
        let ran = run.diff_with(&[], &envs);
        (ran, traced(&trace))
    };
    let (shared, processes) = diff("shared.trace", false);
    let (off, off_processes) = diff("off.trace", true);
    assert_eq!(shared.stdout, off.stdout, "the shared import changed a row");
    assert_eq!(
        counter(&shared, "git_scratch_base_imports"),
        2,
        "{}",
        shared.stderr
    );
    assert_eq!(
        counter(&shared, "git_scratch_base_borrowed"),
        10,
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
    let counts = per_repo(&index_packs(&processes));
    let bases: Vec<_> = counts
        .iter()
        .filter(|(worktree, _)| worktree.contains("/shared-base-"))
        .collect();
    assert_eq!(bases.len(), 2, "{counts:#?}");
    assert!(counts.values().all(|count| *count == 1), "{counts:#?}");
    // Each base's staged bundle (`<stage>/capture.bundle`, its shared
    // scratch at `<stage>/shared-base-<n>/repo`): read in its shared
    // scratch only.
    let bundles: Vec<String> = bases
        .iter()
        .map(|(worktree, _)| {
            let stage = &worktree[..worktree.find("/shared-base-").unwrap()];
            format!("{stage}/capture.bundle")
        })
        .collect();
    for bundle in &bundles {
        assert!(
            processes
                .iter()
                .any(|process| process.worktree.contains("/shared-base-")
                    && process.argv.contains(bundle.as_str())),
            "{bundle} never read"
        );
        let elsewhere: Vec<_> = processes
            .iter()
            .filter(|process| {
                process.worktree.contains("/adopt-") && process.argv.contains(bundle.as_str())
            })
            .map(|process| (&process.worktree, &process.argv))
            .collect();
        assert!(
            elsewhere.is_empty(),
            "{bundle} read by an item's scratch: {elsewhere:#?}"
        );
    }
    // The other arm indexes a base in every scratch that needs one: two
    // index-packs in a scratch (the base, then the item's own capture).
    let off_counts = per_repo(&index_packs(&off_processes));
    assert!(
        off_counts
            .keys()
            .all(|worktree| !worktree.contains("/shared-base-")),
        "{off_counts:#?}"
    );
    let doubled = off_counts.values().filter(|count| **count == 2).count();
    assert_eq!(doubled, 10, "{off_counts:#?}");
    assert_eq!(
        counter(&off, "git_scratch_base_imports"),
        10,
        "{}",
        off.stderr
    );
}

/// P132 DIFF-ONE-IMPORT-PER-STORE (M2r2 item 3 review): one base read
/// against two destination object stores. The main checkout is a first
/// landing at an absent path (its origin is read in a scratch that
/// borrows no store) and its worktrees land into an existing, unrelated
/// repository (their scratches borrow its store): one import per store,
/// never one store's import lent to the other's scratches, and rows
/// byte-identical to the import-per-item arm.
#[test]
fn p132_estate_diff_imports_a_base_once_per_destination_store() {
    let (world, rows) = worktree_world(&["one"], 3, &|world, _| world.sting("fresh"));
    let run = Run::new(&world, &rows);
    settle();
    run.capture();
    private_dir(&run.applied);
    let shared = run.diff(&[]);
    let off = run.diff_with(&[], &[(SHARED_OFF.0, SHARED_OFF.1.as_ref())]);
    assert_eq!(shared.stdout, off.stdout, "the shared import changed a row");
    let main = diff_line(&shared.stdout, &world.neo("one"));
    assert!(main.contains("\"adopt\":\"landing\""), "{main}");
    assert_eq!(
        counter(&shared, "git_scratch_base_imports"),
        2,
        "{}",
        shared.stderr
    );
    assert_eq!(
        counter(&shared, "git_scratch_base_borrowed"),
        4,
        "{}",
        shared.stderr
    );
    assert_eq!(
        counter(&off, "git_scratch_base_imports"),
        4,
        "{}",
        off.stderr
    );
}

/// P133 DIFF-SHARED-CORPUS-PATH (M2r2 item 3 review, finding 1): a corpus
/// whose path is not UTF-8 lends its shared import (the alternates line is
/// written as bytes), and one whose path holds a newline, which no
/// alternates line can name, imports the base per scratch as before; in
/// both, the rows are the import-per-item arm's.
#[test]
fn p133_estate_diff_shares_under_any_corpus_path_it_can_name() {
    use std::os::unix::ffi::OsStrExt as _;
    for (name, imports, borrowed) in [(&b"corpus-\xff"[..], 1, 4), (&b"corpus-\nline"[..], 4, 0)] {
        let (world, rows) = worktree_world(&["one"], 3, &|world, name| world.sting(name));
        let mut run = Run::new(&world, &rows);
        run.corpus = world.root.join(std::ffi::OsStr::from_bytes(name));
        settle();
        run.capture();
        private_dir(&run.applied);
        let shared = run.diff(&[]);
        let off = run.diff_with(&[], &[(SHARED_OFF.0, SHARED_OFF.1.as_ref())]);
        assert_eq!(
            shared.stdout, off.stdout,
            "{name:?}: the shared import changed a row"
        );
        assert!(!shared.stdout.contains("\"refused\""), "{}", shared.stdout);
        assert_eq!(
            counter(&shared, "git_scratch_base_imports"),
            imports,
            "{name:?}: {}",
            shared.stderr
        );
        assert_eq!(
            counter(&shared, "git_scratch_base_borrowed"),
            borrowed,
            "{name:?}"
        );
        assert_eq!(counter(&off, "git_scratch_base_imports"), 4, "{name:?}");
    }
}

fn seat_lines(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|line| line.starts_with("seat "))
        .collect()
}

fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    fs::symlink_metadata(path).unwrap().ino()
}

fn unseat(state: &Path, lives: &[&Path]) -> Ran {
    let mut list: Vec<OsString> = vec!["estate-unseat".into(), state.as_os_str().to_owned()];
    list.extend(lives.iter().map(|live| live.as_os_str().to_owned()));
    agent(&list)
}

/// P120 SIDE-ROOT-SEAT (M2r2 item e; the GFTB shape of 10-16): a family
/// (a main and a linked worktree) landed into a side root by an earlier
/// plan, the live main a symlink into it and the live worktrees directory
/// another. Under a live-path plan `estate-apply --adopt` refuses the
/// symlinked spelling; `estate-diff --side-root` says what a seat does and
/// answers each item at the tree it would seat, writing nothing;
/// `estate-apply --adopt --side-root` seats both links (the same inodes at
/// the live paths, a link back at each side-root path, no worktree byte
/// touched) and re-keys both ledgers; closure passes; a rerun seats and
/// writes nothing; `estate-unseat` puts both links and both trees back.
#[test]
fn p120_a_symlinked_live_path_into_a_side_root_is_seated_and_rekeyed() {
    let world = Gftb::new();
    let side = world.sting("estate");
    fs::create_dir(&side).unwrap();
    let (main_src, feat_src) = (world.neo("infra"), world.neo("infra.worktrees/feat"));
    let (main_side, feat_side) = (side.join("infra"), side.join("infra.worktrees/feat"));
    let first = named_run(
        &world,
        "side",
        &[
            (&main_src, &main_side, Some(&main_side)),
            (&feat_src, &main_side, Some(&feat_side)),
        ],
    );
    first.capture();
    private_dir(&first.applied);
    let landed = first.apply(&[]);
    assert!(landed.ok, "{}\n{}", landed.stdout, landed.stderr);
    // The live tree: links into the side root, as the GFTB carry left it.
    let live_root = world.sting("live");
    fs::create_dir(&live_root).unwrap();
    let (main_live, worktrees_live) = (live_root.join("infra"), live_root.join("infra.worktrees"));
    std::os::unix::fs::symlink(&main_side, &main_live).unwrap();
    std::os::unix::fs::symlink(side.join("infra.worktrees"), &worktrees_live).unwrap();
    let feat_live = worktrees_live.join("feat");
    let second = named_run(
        &world,
        "live",
        &[
            (&main_src, &main_live, Some(&main_live)),
            (&feat_src, &main_live, Some(&feat_live)),
        ],
    );
    settle();
    second.capture();
    private_dir(&second.applied);
    let (main_ino, worktrees_ino) = (inode(&main_side), inode(&side.join("infra.worktrees")));
    let trees = (freeze(&main_side), freeze(&side.join("infra.worktrees")));
    // Without --side-root: the symlinked spelling is refused, as before.
    let refused = second.apply(&["--adopt"]);
    assert!(!refused.ok);
    assert!(seat_lines(&refused.stdout).is_empty(), "{}", refused.stdout);
    for source in [&main_src, &feat_src] {
        let row = receipt(&receipts(&refused.stdout), source).clone();
        assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    }
    // The dry run: both links seatable, each item answered at its tree.
    let before = freeze_all(&world.sting(""));
    let diff = second.diff(&["--side-root", side.to_str().unwrap()]);
    assert_eq!(freeze_all(&world.sting("")), before, "estate-diff wrote");
    let seats: Vec<&str> = diff
        .stdout
        .lines()
        .filter(|line| line.contains("\"kind\":\"seat\""))
        .collect();
    assert_eq!(seats.len(), 2, "{}", diff.stdout);
    assert!(
        seats
            .iter()
            .all(|line| line.contains("\"seat\":\"seatable\"")),
        "{seats:?}"
    );
    for source in [&main_src, &feat_src] {
        let line = diff_line(&diff.stdout, source);
        assert!(line.contains("\"side-root-seat\""), "{line}");
        assert!(line.contains("\"adopt\":\"ledgered-other-item\""), "{line}");
    }
    // The apply: seated, then re-keyed at the live paths.
    let ran = second.apply(&["--adopt", "--side-root", side.to_str().unwrap()]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let lines = seat_lines(&ran.stdout);
    assert_eq!(lines.len(), 2, "{}", ran.stdout);
    assert!(
        lines
            .iter()
            .all(|line| line.contains("outcome=seated reason=none")),
        "{lines:?}"
    );
    for source in [&main_src, &feat_src] {
        let row = receipt(&receipts(&ran.stdout), source).clone();
        assert!(row.2.contains("rekeyed=1"), "{row:?}");
    }
    assert!(fs::symlink_metadata(&main_live).unwrap().is_dir());
    assert_eq!(inode(&main_live), main_ino);
    assert_eq!(inode(&worktrees_live), worktrees_ino);
    assert_eq!(fs::read_link(&main_side).unwrap(), main_live);
    assert_eq!(
        fs::read_link(side.join("infra.worktrees")).unwrap(),
        worktrees_live
    );
    assert_eq!(
        (freeze(&main_live), freeze(&worktrees_live)),
        trees,
        "a seat or the re-key touched a checkout byte or lstat field"
    );
    let closure = second.closure();
    assert!(
        closure
            .stdout
            .contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{}",
        closure.stdout
    );
    // A rerun: nothing to seat, the journal no-op.
    let rerun = second.apply(&["--adopt", "--side-root", side.to_str().unwrap()]);
    assert!(rerun.ok, "{}\n{}", rerun.stdout, rerun.stderr);
    assert!(seat_lines(&rerun.stdout).is_empty(), "{}", rerun.stdout);
    assert!(receipts(&rerun.stdout)
        .iter()
        .all(|row| row.1.starts_with("previous-")));
    // The undo: both trees back at their side-root paths, both links back.
    let undone = unseat(&second.applied, &[]);
    assert!(undone.ok, "{}\n{}", undone.stdout, undone.stderr);
    assert_eq!(undone.stdout.matches("outcome=unseated").count(), 2);
    assert_eq!(fs::read_link(&main_live).unwrap(), main_side);
    assert_eq!(
        fs::read_link(&worktrees_live).unwrap(),
        side.join("infra.worktrees")
    );
    assert_eq!(inode(&main_side), main_ino);
    assert_eq!(inode(&side.join("infra.worktrees")), worktrees_ino);
    assert!(
        !unseat(&second.applied, &[&main_live]).ok,
        "no record is left"
    );
}

/// P121 SIDE-ROOT-SEAT-REFUSED (M2r2 item e): only a single hop into a side
/// root is seated. A link that reaches the side root through another link
/// is refused `PATH_ESCAPES_ROOT` and left as it is (its item still refused
/// as a symlinked spelling); a link outside every side root is not the
/// verb's (no seat row).
#[test]
fn p121_only_a_single_hop_into_a_side_root_is_seated() {
    let world = Gftb::new();
    let side = world.sting("estate");
    fs::create_dir(&side).unwrap();
    let source = world.neo("lamesh");
    let tree = side.join("lamesh");
    fs::rename(world.sting("lamesh"), &tree).unwrap();
    let alias = world.sting("alias");
    std::os::unix::fs::symlink(&side, &alias).unwrap();
    let live = world.sting("live-lamesh");
    std::os::unix::fs::symlink(alias.join("lamesh"), &live).unwrap();
    let elsewhere = world.sting("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::rename(world.sting("sshrepo"), elsewhere.join("sshrepo")).ok();
    let outside = world.sting("live-ssh");
    std::os::unix::fs::symlink(elsewhere.join("sshrepo"), &outside).unwrap();
    let run = named_run(
        &world,
        "hops",
        &[
            (&source, &live, Some(&live)),
            (&world.neo("sshrepo"), &outside, Some(&outside)),
        ],
    );
    run.capture();
    private_dir(&run.applied);
    let before = freeze_all(&world.sting(""));
    // Without --adopt a seated tree could only refuse: refused up front.
    let unadopted = run.apply(&["--side-root", side.to_str().unwrap()]);
    assert!(!unadopted.ok);
    assert!(
        unadopted.stderr.contains("FIELD_DOMAIN_VIOLATION"),
        "{}",
        unadopted.stderr
    );
    assert!(seat_lines(&unadopted.stdout).is_empty());
    let ran = run.apply(&["--adopt", "--side-root", side.to_str().unwrap()]);
    let lines = seat_lines(&ran.stdout);
    assert_eq!(lines.len(), 1, "{}", ran.stdout);
    assert!(
        lines[0].contains("outcome=refused reason=PATH_ESCAPES_ROOT"),
        "{lines:?}"
    );
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(fs::read_link(&live).unwrap(), alias.join("lamesh"));
    assert!(fs::symlink_metadata(&tree).unwrap().is_dir());
    assert_eq!(freeze_all(&world.sting("")), before, "a refused seat wrote");
}

/// The `--exclude-union` shape of the 2026-10-16 in-place run (M2 round 2,
/// item 2: 250 new linked worktrees refused `GIT_IGNORE_POLICY_CONFLICT`
/// where the source's exclude was a superset of the destination's): the
/// source's common exclude is `captured`, the destination's `existing`,
/// and two source-only linked worktrees are planned into the destination
/// repository (no main item, which `--adopt` would refuse diverged).
fn exclude_union_world<'a>(
    world: &'a Gftb,
    captured: &[u8],
    existing: &[u8],
) -> (Run<'a>, [PathBuf; 2]) {
    let neo = world.neo("infra");
    let second = world.neo("infra.worktrees/only-src2");
    git(
        &neo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "src-only-2",
            second.to_str().unwrap(),
        ],
    );
    fs::write(second.join("second.txt"), b"second work\n").unwrap();
    fs::create_dir_all(neo.join(".git/info")).unwrap();
    fs::write(neo.join(".git/info/exclude"), captured).unwrap();
    let infra = world.sting("infra");
    fs::create_dir_all(infra.join(".git/info")).unwrap();
    fs::write(infra.join(".git/info/exclude"), existing).unwrap();
    let sources = [world.neo("infra.worktrees/only-src"), second];
    let run = named_run(
        world,
        "union",
        &[
            (
                &sources[0],
                &infra,
                Some(&world.sting("infra.worktrees/only-src")),
            ),
            (
                &sources[1],
                &infra,
                Some(&world.sting("infra.worktrees/only-src2")),
            ),
        ],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    (run, sources)
}

/// Every `*.exclude-union` record in `state`, decoded.
fn union_records(state: &Path) -> Vec<bulkload_agent::git_carry::exclude_union::ExcludeUnion> {
    fs::read_dir(state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "exclude-union"))
        .map(|path| {
            bulkload_agent::git_carry::exclude_union::read_record(&path)
                .unwrap()
                .unwrap()
        })
        .collect()
}

/// P108 EXCLUDE-UNION (M2 round 2, item 2): a destination exclude the
/// capture's is a superset of (plus one line of its own) refuses every new
/// linked worktree `GIT_IGNORE_POLICY_CONFLICT` without the flag, with
/// nothing written; `estate-diff` says so and names what the flag would
/// append (`exclude-union=3`), read-only. With `--exclude-union` both land:
/// the first appends the three missing captured lines after the
/// destination's own bytes (a byte prefix, kept exactly), counted
/// `exclude-appended=3` and recorded in STATE as appended; the second finds
/// nothing missing and writes nothing. A rerun is a journal no-op.
#[test]
fn p108_exclude_union_appends_the_missing_captured_lines_and_lands() {
    let world = Gftb::new();
    let captured = b"*.log\n# neo additions\nscratch/\n\n.envrc\n";
    let existing = b"*.log\nsting-local/\n";
    let (run, sources) = exclude_union_world(&world, captured, existing);
    let exclude = world.sting("infra/.git/info/exclude");

    let diff = run.diff(&[]);
    for source in &sources {
        let line = diff_line(&diff.stdout, source);
        assert!(
            line.contains("\"adopt\":\"ignore-policy-conflict\"")
                && line.contains("\"exclude-union=3\""),
            "{line}"
        );
    }
    assert_eq!(fs::read(&exclude).unwrap(), existing);

    let ran = run.apply(&[]);
    assert!(!ran.ok, "{}", ran.stdout);
    let rows = receipts(&ran.stdout);
    for source in &sources {
        let row = receipt(&rows, source);
        assert_eq!(row.1, "refused", "{row:?}");
        assert!(row.2.contains("GIT_IGNORE_POLICY_CONFLICT"), "{row:?}");
    }
    assert_eq!(fs::read(&exclude).unwrap(), existing);
    assert!(union_records(&run.applied).is_empty());

    let ran = run.apply(&["--exclude-union"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let rows = receipts(&ran.stdout);
    let mut appended = 0;
    for source in &sources {
        let row = receipt(&rows, source);
        assert_eq!(row.1, "workspace-restored", "{row:?}");
        if row.2.contains("exclude-appended=3") {
            appended += 1;
        }
        assert!(!row.2.contains("GIT_"), "{row:?}");
    }
    assert_eq!(appended, 1, "one append, then nothing missing: {rows:?}");
    let now = fs::read(&exclude).unwrap();
    assert!(now.starts_with(existing), "the destination's bytes moved");
    assert_eq!(
        now,
        b"*.log\nsting-local/\n# neo additions\nscratch/\n.envrc\n"
    );
    let records = union_records(&run.applied);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].appends.len(), 1);
    let append = &records[0].appends[0];
    assert_eq!(
        append.state,
        bulkload_agent::git_carry::exclude_union::AppendState::Appended
    );
    assert_eq!(
        append.lines,
        [&b"# neo additions"[..], b"scratch/", b".envrc"]
            .map(<[u8]>::to_vec)
            .to_vec()
    );
    assert_eq!(
        fs::read(world.sting("infra.worktrees/only-src2/second.txt")).unwrap(),
        b"second work\n"
    );
    assert_eq!(
        fs::read(world.sting("infra.worktrees/only-src/only-src.txt")).unwrap(),
        b"only-src work\n"
    );

    let ran = run.apply(&["--exclude-union"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    for source in &sources {
        let row = receipt(&receipts(&ran.stdout), source).clone();
        assert_eq!(
            row.1, "previous-workspace-restoration-not-revalidated",
            "{row:?}"
        );
    }
    assert_eq!(fs::read(&exclude).unwrap(), now);
    assert_eq!(union_records(&run.applied), records);
}

/// P109 EXCLUDE-UNION-NEGATION (M2 round 2, item 2): a negation makes a
/// union order-sensitive (the last matching pattern decides), so a
/// captured exclude holding `!pattern` keeps `GIT_IGNORE_POLICY_CONFLICT`
/// under `--exclude-union` too, with nothing written or recorded, and
/// `estate-diff` says the flag refuses too
/// (`exclude-union-refused=GIT_IGNORE_POLICY_CONFLICT`).
#[test]
fn p109_exclude_union_refuses_a_negation() {
    let world = Gftb::new();
    let captured = b"*.log\n!keep.log\nscratch/\n";
    let existing = b"*.log\n";
    let (run, sources) = exclude_union_world(&world, captured, existing);
    let exclude = world.sting("infra/.git/info/exclude");
    let infra = world.sting("infra");
    let refs_before = all_refs(&infra);
    let diff = run.diff(&[]);
    for source in &sources {
        let line = diff_line(&diff.stdout, source);
        assert!(
            line.contains("\"adopt\":\"ignore-policy-conflict\"")
                && line.contains("\"exclude-union-refused=GIT_IGNORE_POLICY_CONFLICT\""),
            "{line}"
        );
    }
    let ran = run.apply(&["--exclude-union"]);
    assert!(!ran.ok, "{}", ran.stdout);
    let rows = receipts(&ran.stdout);
    for source in &sources {
        let row = receipt(&rows, source);
        assert_eq!(row.1, "refused", "{row:?}");
        assert!(row.2.contains("GIT_IGNORE_POLICY_CONFLICT"), "{row:?}");
    }
    assert_eq!(fs::read(&exclude).unwrap(), existing);
    assert_eq!(all_refs(&infra), refs_before);
    assert!(union_records(&run.applied).is_empty());
    assert!(!world.sting("infra.worktrees/only-src").exists());
}

/// The suffix the beside tests land under.
const SUFFIX: &str = ".neo-20261016";

/// The beside checkout of `dest` under [`SUFFIX`].
fn beside_of(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(SUFFIX);
    PathBuf::from(name)
}

/// Every ref of `repo` by name, provenance (`refs/carry`) left out.
fn named_refs(repo: &Path) -> BTreeMap<String, String> {
    all_refs(repo)
        .lines()
        .filter(|line| !line.starts_with("refs/carry"))
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

/// A diverged `lamesh` (a seat edited at the destination) whose source is
/// on a branch the destination lacks, captured; its run.
fn diverged<'a>(world: &'a Gftb, source: &Path, dest: &Path, commit: bool) -> Run<'a> {
    fs::write(dest.join("README"), b"edited on sting\n").unwrap();
    if commit {
        git(dest, &["commit", "--quiet", "-am", "sting's own"]);
    }
    git(source, &["switch", "--quiet", "-c", "neo-feature"]);
    let run = Run::new(
        world,
        &[(
            source.to_path_buf(),
            dest.to_path_buf(),
            Some(dest.to_path_buf()),
        )],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    run
}

/// P110 BESIDE (M2 round 2 item c: 10 i02 items refused
/// `GIT_DESTINATION_DIVERGED`): a destination checkout that differs from
/// the capture refuses diverged under `--adopt` with nothing written; with
/// `--beside` alone (no `--adopt`) the verb refuses; `estate-diff --beside`
/// names the beside landing. With `--adopt --beside SUFFIX` the destination
/// checkout keeps every seat, HEAD, index and native ref (no branch is
/// created for the source's `neo-feature`, which it lacks); the capture
/// lands at `<dest>SUFFIX`, a linked worktree of the destination
/// repository detached at the captured head; every captured ref is
/// created as `refs/neo-20261016/<name>`; STATE records them as created;
/// the receipt counts them; `closure-report` says `applied-beside`; a
/// rerun is a journal no-op and the diff then says `beside=landed`.
#[test]
fn p110_a_diverged_checkout_lands_beside_and_the_live_one_stays() {
    let (world, source, dest) = small();
    let run = diverged(&world, &source, &dest, true);
    let frozen = freeze_all(&dest);
    let native = named_refs(&dest);
    let ran = run.apply(&["--adopt"]);
    assert!(
        receipt(&receipts(&ran.stdout), &source)
            .2
            .contains("GIT_DESTINATION_DIVERGED"),
        "{}",
        ran.stdout
    );
    let ran = run.apply(&["--beside", SUFFIX]);
    assert!(
        !ran.ok && ran.stderr.contains("REQUIRED_FIELD_MISSING"),
        "{}",
        ran.stderr
    );
    assert!(!beside_of(&dest).exists());
    let diff = run.diff(&["--beside", SUFFIX]);
    let line = diff_line(&diff.stdout, &source).to_owned();
    assert!(
        line.contains("\"adopt\":\"diverged\"")
            && line.contains(&format!(
                "\"beside=landing:{}\"",
                beside_of(&dest).display()
            )),
        "{line}"
    );
    assert!(!run.diff(&[]).stdout.contains("beside="));

    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    assert!(
        row.2.contains("beside-landed=1")
            && row.2.contains("beside-linked=1")
            && row.2.contains("beside-ref-created=5")
            && row.2.contains("beside-ref-kept=0"),
        "{row:?}"
    );
    // The destination checkout: every seat and its whole .git but the new
    // worktree administration, provenance and refs are as they were.
    let moved: Vec<_> = freeze_all(&dest)
        .into_iter()
        .filter(|row| !frozen.contains(row))
        .map(|row| row.0)
        .filter(|path| {
            let path = path.strip_prefix(&dest).unwrap();
            !(path.starts_with(".git/worktrees")
                || path.starts_with(".git/refs")
                || path.starts_with(".git/objects")
                || path.starts_with(".git/bulkload")
                || path.starts_with(".git/logs/refs")
                || path == Path::new(".git/packed-refs")
                || path == Path::new(".git")
                || path == Path::new(".git/logs")
                || path == Path::new(".git/worktrees"))
        })
        .collect();
    assert!(moved.is_empty(), "the beside landing moved {moved:?}");
    let now = named_refs(&dest);
    for (name, value) in &native {
        assert_eq!(now.get(name), Some(value), "{name} moved");
    }
    assert!(!now.contains_key("refs/heads/neo-feature"), "{now:?}");
    let carried = named_refs(&source);
    let mut wanted = BTreeMap::new();
    for (name, value) in &carried {
        let rest = name.strip_prefix("refs/").unwrap();
        wanted.insert(format!("refs/neo-20261016/{rest}"), value.clone());
    }
    for (name, value) in &wanted {
        assert_eq!(now.get(name), Some(value), "{name}");
    }
    assert_eq!(now.len(), native.len() + carried.len(), "{now:?}");
    let beside = beside_of(&dest);
    assert_eq!(fs::read(beside.join("README")).unwrap(), b"LA-Mesh\n");
    assert_eq!(fs::read(dest.join("README")).unwrap(), b"edited on sting\n");
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&source, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(
        fs::canonicalize(git(&beside, &["rev-parse", "--git-common-dir"]).trim()).unwrap(),
        fs::canonicalize(dest.join(".git")).unwrap()
    );
    let record = beside_record(&run.applied);
    assert!(record.pending.is_empty() && record.kept.is_empty());
    let created: BTreeMap<String, String> = record.created.into_iter().collect();
    assert_eq!(created, wanted);
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"applied_beside\":1")
            && closure
                .stdout
                .contains("\"disposition\":\"applied-beside\""),
        "{}",
        closure.stdout
    );
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    assert_eq!(
        receipt(&receipts(&ran.stdout), &source).1,
        "previous-workspace-beside-not-revalidated"
    );
    assert_eq!(named_refs(&dest), now);
    let diff = run.diff(&["--beside", SUFFIX]);
    assert!(
        diff_line(&diff.stdout, &source).contains("\"beside=landed:"),
        "{}",
        diff.stdout
    );
}

/// The `.beside` record in `state`.
fn beside_record(state: &Path) -> bulkload_agent::git_carry::beside::BesideRecord {
    let found: Vec<PathBuf> = fs::read_dir(state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "beside"))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    bulkload_agent::git_carry::beside::read_record(&found[0])
        .unwrap()
        .unwrap()
}

/// P111 BESIDE-OCCUPIED (M2 round 2 item c; gap C12): a beside path that
/// holds a directory that is no checkout refuses `GIT_BESIDE_FOREIGN`
/// naming the path (`estate-diff` says `beside=foreign`), and a ref named
/// `refs/<ns>` refuses `GIT_DESTINATION_OCCUPIED` (`beside=occupied`),
/// each with nothing written to the destination repository or the beside
/// path.
#[test]
fn p111_an_occupied_beside_path_refuses_and_nothing_is_written() {
    for occupy in ["path", "namespace"] {
        let (world, source, dest) = small();
        let run = diverged(&world, &source, &dest, false);
        let beside = beside_of(&dest);
        if occupy == "path" {
            fs::create_dir(&beside).unwrap();
            fs::write(beside.join("operator.txt"), b"mine\n").unwrap();
        } else {
            git(&dest, &["update-ref", "refs/neo-20261016", "HEAD"]);
        }
        let frozen = (
            freeze_all(&dest),
            beside.exists().then(|| freeze_all(&beside)),
            all_refs(&dest),
        );
        let (verdict, code) = if occupy == "path" {
            ("foreign", "GIT_BESIDE_FOREIGN")
        } else {
            ("occupied", "GIT_DESTINATION_OCCUPIED")
        };
        let diff = run.diff(&["--beside", SUFFIX]);
        assert!(
            diff_line(&diff.stdout, &source).contains(&format!("\"beside={verdict}:")),
            "{occupy}: {}",
            diff.stdout
        );
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refused", "{occupy}: {row:?}");
        assert!(row.2.contains(code), "{occupy}: {row:?}");
        assert_eq!(
            (
                freeze_all(&dest),
                beside.exists().then(|| freeze_all(&beside)),
                all_refs(&dest)
            ),
            frozen,
            "{occupy}"
        );
    }
}

/// P112 BESIDE-EXISTING (M2 round 2 item c: the in-place lane made
/// `<path>.neo-20261016` by hand for 9 of the 10 diverged i02 items): a
/// standalone checkout already at the beside path, equal to the capture,
/// is adopted in place under `--adopt --beside` (`estate-diff` says
/// `beside=existing`); the capture's refs go in under `refs/neo-*/` of the
/// destination repository; the rerun is a journal no-op. One that differs
/// refuses `GIT_BESIDE_FOREIGN` (gap C12: the beside path holds something
/// else, named) with nothing written to either.
#[test]
fn p112_an_existing_standalone_beside_is_adopted_when_equal() {
    for equal in [true, false] {
        let (world, source, dest) = small();
        let run = diverged(&world, &source, &dest, false);
        let beside = beside_of(&dest);
        copy_tree(&source, &beside);
        if !equal {
            fs::write(beside.join("README"), b"not the capture\n").unwrap();
        }
        let diff = run.diff(&["--beside", SUFFIX]);
        assert!(
            diff_line(&diff.stdout, &source).contains("\"beside=existing:"),
            "{}",
            diff.stdout
        );
        let frozen = (freeze_all(&dest), all_refs(&dest));
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        if !equal {
            assert_eq!(row.1, "refused", "{row:?}");
            assert!(row.2.contains("GIT_BESIDE_FOREIGN"), "{row:?}");
            assert_eq!((freeze_all(&dest), all_refs(&dest)), frozen);
            continue;
        }
        assert_eq!(row.1, "workspace-beside", "{row:?}");
        assert!(
            row.2.contains("beside-standalone=1") && row.2.contains("beside-ref-created=5"),
            "{row:?}"
        );
        assert!(beside
            .join(".git/bulkload/v1/neo/workspace.landed")
            .is_file());
        let now = named_refs(&dest);
        let created = now
            .keys()
            .filter(|name| name.starts_with("refs/neo-"))
            .count();
        assert_eq!(created, 5, "{now:?}");
        assert_eq!(fs::read(dest.join("README")).unwrap(), b"edited on sting\n");
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        assert_eq!(
            receipt(&receipts(&ran.stdout), &source).1,
            "previous-workspace-beside-not-revalidated"
        );
    }
}

/// P113 BESIDE-EXCLUDE (review of #266, A1): a beside landing never
/// appends to the live repository's shared `info/exclude` (it would change
/// what the live checkout ignores): with the captured exclude holding a
/// line the live one lacks, `--adopt --beside --exclude-union` refuses
/// `GIT_IGNORE_POLICY_CONFLICT` and the live exclude, checkout and refs
/// are unchanged; `estate-diff --beside` says `beside=ignore-policy-conflict`.
/// When the live exclude already holds every captured line (in another
/// order), the beside lands with nothing appended.
#[test]
fn p113_a_beside_landing_never_appends_to_the_live_exclude() {
    for held in [false, true] {
        let (world, source, dest) = small();
        fs::create_dir_all(source.join(".git/info")).unwrap();
        fs::write(source.join(".git/info/exclude"), b"a.log\nb.log\n").unwrap();
        fs::create_dir_all(dest.join(".git/info")).unwrap();
        let live_exclude: &[u8] = if held {
            b"b.log\na.log\nc.log\n"
        } else {
            b"a.log\n"
        };
        fs::write(dest.join(".git/info/exclude"), live_exclude).unwrap();
        fs::write(dest.join("operator.log"), b"untracked at the destination\n").unwrap();
        let run = diverged(&world, &source, &dest, false);
        let status = git(&dest, &["status", "--porcelain", "--untracked-files=all"]);
        let frozen = (freeze_all(&dest), all_refs(&dest));
        let diff = run.diff(&["--beside", SUFFIX]);
        let line = diff_line(&diff.stdout, &source).to_owned();
        let ran = run.apply(&["--adopt", "--exclude-union", "--beside", SUFFIX]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(
            fs::read(dest.join(".git/info/exclude")).unwrap(),
            live_exclude
        );
        if !held {
            // Before `git status`, which refreshes the index itself.
            assert_eq!((freeze_all(&dest), all_refs(&dest)), frozen);
        }
        assert_eq!(
            git(&dest, &["status", "--porcelain", "--untracked-files=all"]),
            status
        );
        if held {
            assert!(line.contains("\"beside=landing:"), "{line}");
            assert_eq!(row.1, "workspace-beside", "{row:?}");
            assert!(!row.2.contains("exclude-appended"), "{row:?}");
            continue;
        }
        assert!(line.contains("\"beside=ignore-policy-conflict:"), "{line}");
        assert_eq!(row.1, "refused", "{row:?}");
        assert!(row.2.contains("GIT_IGNORE_POLICY_CONFLICT"), "{row:?}");
        assert!(!beside_of(&dest).exists());
    }
}

/// P114 BESIDE-KEPT (review of #266, B4): a ref already at
/// `refs/neo-20261016/<name>` with another value is kept, never moved;
/// the beside checkout lands and the other refs go in, but the item is
/// refused `GIT_DESTINATION_OCCUPIED` (STATE records the kept ref) and
/// `closure-report` counts it pending review, never `applied-beside`.
#[test]
fn p114_a_kept_namespace_ref_keeps_the_item_refused() {
    let (world, source, dest) = small();
    let run = diverged(&world, &source, &dest, false);
    git(
        &dest,
        &[
            "update-ref",
            "refs/neo-20261016/heads/neo-feature",
            "HEAD~0",
        ],
    );
    let other = git(&dest, &["rev-parse", "HEAD"]).trim().to_owned();
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "neo moved"],
    );
    settle();
    run.capture();
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_OCCUPIED"), "{row:?}");
    assert_eq!(
        git(&dest, &["rev-parse", "refs/neo-20261016/heads/neo-feature"]).trim(),
        other
    );
    let record = beside_record(&run.applied);
    assert_eq!(record.kept.len(), 1, "{record:?}");
    assert_eq!(record.created.len(), 4, "{record:?}");
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"applied_beside\":0")
            && closure.stdout.contains("\"refused_pending_review\":1"),
        "{}",
        closure.stdout
    );
}

// ---- gap C12 (plan of record rev 2; operator ruling A-115, 2026-10-18:
// "Landings placed beside refuse every later pass once neo changes" →
// "Yes, converge them") -------------------------------------------------------

/// A hand-off block sealed as estate-handoff seals one (its `id=` the first
/// 16 hex digits of the blake3 of the block with the id blanked), with
/// `body` inside it.
fn sealed_block(body: &str) -> Vec<u8> {
    let blank = format!(
        "<!-- bulkload-handoff:v1 BEGIN id=0000000000000000 for=0123456789abcdef BULKLOAD-HANDOFF-DO-NOT-COMMIT -->\n{body}<!-- bulkload-handoff:v1 END id=0000000000000000 -->\n\n"
    );
    let id = blake3::hash(blank.as_bytes()).to_hex();
    blank
        .replace("id=0000000000000000", &format!("id={}", &id.as_str()[..16]))
        .into_bytes()
}

/// Whether the receipt reason `reason` (as [`receipts`] gives it, an
/// `Option`'s `Debug`: `Some("...")`) holds the word `word` exactly.
fn word(reason: &str, word: &str) -> bool {
    reason
        .strip_prefix("Some(")
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(reason)
        .trim_matches('"')
        .split_whitespace()
        .any(|held| held == word)
}

/// The live checkout's seats (its `.git` aside) that moved since `frozen`.
fn live_seats_moved(
    dest: &Path,
    frozen: &[(PathBuf, u64, i64, i64, i64, u64, u32)],
) -> Vec<PathBuf> {
    freeze_all(dest)
        .into_iter()
        .filter(|row| !frozen.contains(row))
        .map(|row| row.0)
        .filter(|path| !path.strip_prefix(dest).unwrap().starts_with(".git"))
        .collect()
}

/// `named_refs` without the beside namespaces.
fn native_refs(repo: &Path) -> BTreeMap<String, String> {
    named_refs(repo)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("refs/neo-"))
        .collect()
}

/// A new commit on the source's branch, settled and captured.
fn source_moves(run: &Run<'_>, source: &Path, file: &str) {
    fs::write(source.join(file), format!("{file}, from neo later\n")).unwrap();
    git(source, &["add", file]);
    git(source, &["commit", "--quiet", "-m", file]);
    settle();
    run.capture();
}

/// P115 BESIDE-CONVERGE (gap C12): a beside landing an earlier pass made
/// converges to the newer capture on the next pass, where it used to refuse
/// `GIT_DESTINATION_OCCUPIED` for good (`estate-diff` says
/// `beside=converge`): the beside checkout gains the source's new commit,
/// still detached at the captured head (`head-detached-beside`); the live
/// checkout's seats, HEAD and native refs are as they were; the item's own
/// namespace ref the source moved is moved after it (`beside-ref-moved=1`),
/// the rest stay present; closure says `applied-beside`. Then an edit in
/// the beside checkout since its landing refuses the next converge
/// `GIT_DESTINATION_MODIFIED`, and the edit stays.
#[test]
fn p115_a_beside_landing_converges_on_a_later_pass() {
    let (world, source, dest) = small();
    let run = diverged(&world, &source, &dest, true);
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        receipt(&receipts(&ran.stdout), &source).1,
        "workspace-beside"
    );
    let beside = beside_of(&dest);
    let frozen = freeze_all(&dest);
    let native = native_refs(&dest);
    let head = git(&dest, &["rev-parse", "HEAD"]);

    source_moves(&run, &source, "LATER");
    let diff = run.diff(&["--beside", SUFFIX]);
    assert!(
        diff_line(&diff.stdout, &source).contains("\"beside=converge:"),
        "{}",
        diff.stdout
    );
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    for held in [
        "converged",
        "beside-converged=1",
        "beside-linked=1",
        "beside-ref-moved=1",
        "beside-ref-retired=0",
        "beside-ref-kept=0",
        "head-detached-beside=1",
    ] {
        assert!(word(&row.2, held), "{held}: {row:?}");
    }
    assert_eq!(
        fs::read(beside.join("LATER")).unwrap(),
        b"LATER, from neo later\n"
    );
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&source, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert!(live_seats_moved(&dest, &frozen).is_empty());
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head);
    assert_eq!(native_refs(&dest), native);
    assert!(!dest.join("LATER").exists());
    let now = named_refs(&dest);
    assert_eq!(
        now.get("refs/neo-20261016/heads/neo-feature"),
        Some(&git(&source, &["rev-parse", "HEAD"]).trim().to_owned())
    );
    let record = beside_record(&run.applied);
    assert!(record.pending.is_empty() && record.kept.is_empty());
    let created: BTreeMap<String, String> = record.created.into_iter().collect();
    for (name, value) in &created {
        assert_eq!(now.get(name), Some(value), "{name}");
    }
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"applied_beside\":1"),
        "{}",
        closure.stdout
    );

    // An edit in the beside checkout since its landing: the next converge
    // refuses, and the edit stays.
    fs::write(beside.join("README"), b"edited in the beside checkout\n").unwrap();
    let beside_head = git(&beside, &["rev-parse", "HEAD"]);
    source_moves(&run, &source, "AGAIN");
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_MODIFIED"), "{row:?}");
    assert_eq!(
        fs::read(beside.join("README")).unwrap(),
        b"edited in the beside checkout\n"
    );
    assert_eq!(git(&beside, &["rev-parse", "HEAD"]), beside_head);
    assert!(!beside.join("AGAIN").exists());
}

/// P116 BESIDE-ADOPT-EQUAL (gap C12): a standalone checkout bulkload did
/// not make at the beside path (a hand copy, its own `.git`), equal to the
/// capture in HEAD, index, exclude, stash and every carried seat, is
/// adopted in its own repository (`beside-adopted=1`,
/// `beside-standalone=1`), never converted to a linked worktree; the
/// capture's refs, its stash entry included, go in under `refs/neo-*/`.
/// The next pass converges it like any standalone landing: its own branch
/// and HEAD move, the live checkout and its refs stay.
#[test]
fn p116_an_equal_foreign_beside_is_adopted_then_converges() {
    let (world, source, dest) = small();
    fs::write(source.join("README"), b"LA-Mesh, a stashed edit\n").unwrap();
    git(&source, &["stash", "push", "--quiet", "-m", "neo wip"]);
    let stash = git(&source, &["rev-parse", "refs/stash"]).trim().to_owned();
    let run = diverged(&world, &source, &dest, false);
    let beside = beside_of(&dest);
    copy_tree(&source, &beside);
    let native = native_refs(&dest);
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    assert!(
        word(&row.2, "beside-adopted=1") && word(&row.2, "beside-standalone=1"),
        "{row:?}"
    );
    assert!(fs::symlink_metadata(beside.join(".git")).unwrap().is_dir());
    assert!(beside
        .join(".git/bulkload/v1/neo/workspace.landed")
        .is_file());
    let now = named_refs(&dest);
    assert_eq!(now.get("refs/neo-20261016-stash/0"), Some(&stash));
    assert_eq!(native_refs(&dest), native);
    assert_eq!(fs::read(dest.join("README")).unwrap(), b"edited on sting\n");

    source_moves(&run, &source, "LATER");
    let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    assert!(
        word(&row.2, "beside-converged=1") && word(&row.2, "beside-standalone=1"),
        "{row:?}"
    );
    let head = git(&source, &["rev-parse", "HEAD"]);
    assert_eq!(git(&beside, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&beside, &["rev-parse", "refs/heads/neo-feature"]),
        head,
        "the beside repository's own branch moves with it"
    );
    assert_eq!(
        fs::read(beside.join("LATER")).unwrap(),
        b"LATER, from neo later\n"
    );
    assert_eq!(native_refs(&dest), native);
    assert!(!dest.join("LATER").exists());
    assert_eq!(
        named_refs(&dest).get("refs/neo-20261016/heads/neo-feature"),
        Some(&head.trim().to_owned())
    );
}

/// P117 BESIDE-ADOPT-HANDOFF (gap C12): a foreign standalone beside
/// checkout whose only difference from the capture is a well-formed
/// bulkload hand-off block at byte 0 of `AGENTS.md` (estate-handoff's own
/// marker) is adopted (`beside-handoff-block=1`) and the block is kept
/// byte for byte. A block above bytes that are not the captured ones, an
/// edit inside a sealed block, a well-formed block that estate-handoff did
/// not seal, or a BEGIN line with no END line, is a difference:
/// `GIT_BESIDE_FOREIGN`, and nothing at the beside path is written.
#[test]
fn p117_a_handoff_block_is_the_one_difference_an_adoption_allows() {
    for case in [
        "block",
        "edit-below",
        "edit-inside",
        "unsealed",
        "malformed",
    ] {
        let (world, source, dest) = small();
        fs::write(source.join("AGENTS.md"), b"# lamesh agents\n").unwrap();
        git(&source, &["add", "AGENTS.md"]);
        git(&source, &["commit", "--quiet", "-m", "agents"]);
        let run = diverged(&world, &source, &dest, false);
        let beside = beside_of(&dest);
        copy_tree(&source, &beside);
        let block = sealed_block("You are on sting; converge first.\n");
        let head: Vec<u8> = match case {
            "malformed" => {
                b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef -->\nno end line\n".to_vec()
            }
            // An edit inside a sealed block: still well formed, not sealed.
            "edit-inside" => String::from_utf8(block)
                .unwrap()
                .replacen("converge first", "converge later", 1)
                .into_bytes(),
            // A well-formed block a person wrote: no seal.
            "unsealed" => b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef -->\nmine\n<!-- bulkload-handoff:v1 END id=0123456789abcdef -->\n\n".to_vec(),
            _ => block,
        };
        let below: &[u8] = if case == "edit-below" {
            b"# lamesh agents, edited on sting\n"
        } else {
            b"# lamesh agents\n"
        };
        let agents = [head.as_slice(), below].concat();
        fs::write(beside.join("AGENTS.md"), &agents).unwrap();
        let frozen = freeze_all(&beside);
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(
            fs::read(beside.join("AGENTS.md")).unwrap(),
            agents,
            "{case}"
        );
        if case == "block" {
            assert_eq!(row.1, "workspace-beside", "{case}: {row:?}");
            assert!(
                word(&row.2, "beside-adopted=1") && word(&row.2, "beside-handoff-block=1"),
                "{case}: {row:?}"
            );
            continue;
        }
        assert_eq!(row.1, "refused", "{case}: {row:?}");
        assert!(row.2.contains("GIT_BESIDE_FOREIGN"), "{case}: {row:?}");
        assert_eq!(freeze_all(&beside), frozen, "{case}: the beside path moved");
    }
}

/// P118 BESIDE-FOREIGN (gap C12): a foreign standalone beside checkout
/// that differs from the capture (a seat, or a stash of its own) refuses
/// `GIT_BESIDE_FOREIGN` naming the beside path, never the generic
/// `GIT_DESTINATION_OCCUPIED`; nothing at the beside path or in the live
/// repository is written; closure holds the item pending review and names
/// the path (`refusal_detail`).
#[test]
fn p118_a_differing_foreign_beside_refuses_beside_foreign_named() {
    for case in ["seat", "stash"] {
        let (world, source, dest) = small();
        let run = diverged(&world, &source, &dest, false);
        let beside = beside_of(&dest);
        copy_tree(&source, &beside);
        if case == "seat" {
            fs::write(beside.join("README"), b"not the capture\n").unwrap();
        } else {
            fs::write(beside.join("README"), b"sting wip\n").unwrap();
            git(&beside, &["stash", "push", "--quiet", "-m", "sting wip"]);
        }
        let diff = run.diff(&["--beside", SUFFIX]);
        assert!(
            diff_line(&diff.stdout, &source).contains("\"beside=existing:"),
            "{case}: {}",
            diff.stdout
        );
        let frozen = (freeze_all(&beside), freeze_all(&dest), all_refs(&dest));
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refused", "{case}: {row:?}");
        let named = format!("GIT_BESIDE_FOREIGN path=\\\"{}\\\"", beside.display());
        assert!(row.2.contains(&named), "{case}: {named} in {row:?}");
        assert_eq!(
            (freeze_all(&beside), freeze_all(&dest), all_refs(&dest)),
            frozen,
            "{case}"
        );
        let closure = run.closure();
        assert!(
            closure.stdout.contains("\"GIT_BESIDE_FOREIGN\":1")
                && closure.stdout.contains("\"refused_pending_review\":1")
                && closure.stdout.contains("\"refusal_detail\":")
                && closure.stdout.contains(&beside.display().to_string()),
            "{case}: {}",
            closure.stdout
        );
    }
}

/// P119 BESIDE-REFS-IN (gap C12): when the beside landing refuses (here a
/// directory that is no checkout at the beside path, `GIT_BESIDE_FOREIGN`),
/// `--refs-in` still imports the item's branches, tags and stash entries
/// create-only under `refs/neo-*/` of the live repository, so the source's
/// work reaches it while the tree waits for review; the live checkout's
/// HEAD, seats and native refs and the beside path are untouched; closure
/// names the refs on the refused row (`refs_in`). A rerun creates nothing
/// again.
#[test]
fn p119_a_refused_beside_still_imports_its_refs() {
    let (world, source, dest) = small();
    fs::write(source.join("README"), b"LA-Mesh, a stashed edit\n").unwrap();
    git(&source, &["stash", "push", "--quiet", "-m", "neo wip"]);
    let stash = git(&source, &["rev-parse", "refs/stash"]).trim().to_owned();
    let run = diverged(&world, &source, &dest, true);
    let beside = beside_of(&dest);
    fs::create_dir(&beside).unwrap();
    fs::write(beside.join("operator.txt"), b"mine\n").unwrap();
    let frozen = (freeze_all(&beside), freeze_all(&dest));
    let native = native_refs(&dest);
    let head = git(&dest, &["rev-parse", "HEAD"]);
    let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_BESIDE_FOREIGN"), "{row:?}");
    assert_eq!(freeze_all(&beside), frozen.0);
    assert!(live_seats_moved(&dest, &frozen.1).is_empty());
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head);
    assert_eq!(native_refs(&dest), native);
    let now = named_refs(&dest);
    assert_eq!(
        now.get("refs/neo-20261016/heads/neo-feature"),
        Some(&git(&source, &["rev-parse", "HEAD"]).trim().to_owned())
    );
    assert_eq!(now.get("refs/neo-20261016-stash/0"), Some(&stash));
    let imported = now
        .keys()
        .filter(|name| name.starts_with("refs/neo-"))
        .count();
    assert!(imported >= 3, "{now:?}");
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"refused_pending_review\":1")
            && closure.stdout.contains(&format!(
                "\"refs_in\":{{\"ns\":\"neo-20261016\",\"created\":{imported},\"kept\":0}}"
            )),
        "{}",
        closure.stdout
    );
    let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
    assert_eq!(receipt(&receipts(&ran.stdout), &source).1, "refused");
    assert_eq!(named_refs(&dest), now);
}

/// P122 BESIDE-REKEY (gap C12, the `GloriousFlywheel` refusals of
/// 2026-10-18): the in-place lane's hand plan landed the source standalone
/// at `<path>.neo-20261016` as an item of its own, and estate-handoff
/// wrote its blocks there. The live plan's later pass, once the source has
/// moved, re-keys that landing to its own item (A11b, after its proof:
/// the hand-off blocks are bulkload's) and converges it
/// (`beside-rekeyed=1`), where it used to refuse `GIT_DESTINATION_OCCUPIED`
/// on every pass; the blocks stay, the live checkout and its native refs
/// are untouched, and the refs go in.
#[test]
fn p122_a_hand_planned_beside_landing_is_rekeyed_and_converged() {
    let (world, source, dest) = small();
    fs::write(source.join("AGENTS.md"), b"# lamesh agents\n").unwrap();
    git(&source, &["add", "AGENTS.md"]);
    git(&source, &["commit", "--quiet", "-m", "agents"]);
    fs::write(dest.join("README"), b"edited on sting\n").unwrap();
    git(&dest, &["commit", "--quiet", "-am", "sting's own"]);
    git(&source, &["switch", "--quiet", "-c", "neo-feature"]);
    let beside = beside_of(&dest);
    let hand = named_run(&world, "hand", &[(&source, &beside, Some(&beside))]);
    settle();
    hand.capture();
    private_dir(&hand.applied);
    let ran = hand.apply(&[]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let handed = agent(&args(&[
        &"estate-handoff",
        &hand.plan,
        &hand.corpus,
        &hand.applied,
        &SOURCE,
    ]));
    assert!(handed.ok, "{}\n{}", handed.stdout, handed.stderr);
    let agents = fs::read(beside.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with(b"<!-- bulkload-handoff:v1 BEGIN"));

    fs::write(source.join("LATER"), b"LATER, from neo later\n").unwrap();
    git(&source, &["add", "LATER"]);
    git(&source, &["commit", "--quiet", "-m", "later"]);
    let run = Run::new(
        &world,
        &[(source.clone(), dest.clone(), Some(dest.clone()))],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let diff = run.diff(&["--beside", SUFFIX]);
    assert!(
        diff_line(&diff.stdout, &source).contains("\"beside=rekey:"),
        "{}",
        diff.stdout
    );
    let native = native_refs(&dest);
    let frozen = freeze_all(&dest);
    let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    for held in [
        "beside-rekeyed=1",
        "beside-standalone=1",
        "rekeyed=1",
        "converged",
    ] {
        assert!(word(&row.2, held), "{held}: {row:?}");
    }
    let head = git(&source, &["rev-parse", "HEAD"]);
    assert_eq!(git(&beside, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read(beside.join("LATER")).unwrap(),
        b"LATER, from neo later\n"
    );
    assert_eq!(
        fs::read(beside.join("AGENTS.md")).unwrap(),
        agents,
        "the hand-off block and the landed bytes under it stay"
    );
    assert!(live_seats_moved(&dest, &frozen).is_empty());
    assert_eq!(native_refs(&dest), native);
    assert_eq!(
        named_refs(&dest).get("refs/neo-20261016/heads/neo-feature"),
        Some(&head.trim().to_owned())
    );
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"applied_beside\":1"),
        "{}",
        closure.stdout
    );
}

/// P123 BESIDE-RETIRED (gap C12): a beside checkout this item landed and
/// the operator's agent then folded and removed is never laid down again
/// on a later pass: `estate-diff` says `beside=retired`, the apply refuses
/// `GIT_DESTINATION_MODIFIED` (as a removed live checkout does), and the
/// beside path stays absent. With `--refs-in`, the namespace refs the
/// operator removed with it are not created again either
/// (`refs-in-retired`).
#[test]
fn p123_a_retired_beside_landing_is_not_laid_down_again() {
    for refs_in in [false, true] {
        let (world, source, dest) = small();
        let run = diverged(&world, &source, &dest, true);
        let ran = run.apply(&["--adopt", "--beside", SUFFIX]);
        assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
        let beside = beside_of(&dest);
        git(
            &dest,
            &["worktree", "remove", "--force", beside.to_str().unwrap()],
        );
        assert!(!beside.exists());
        if refs_in {
            for name in named_refs(&dest).into_keys() {
                if name.starts_with("refs/neo-") {
                    git(&dest, &["update-ref", "-d", &name]);
                }
            }
        }
        source_moves(&run, &source, "LATER");
        let diff = run.diff(&["--beside", SUFFIX]);
        assert!(
            diff_line(&diff.stdout, &source).contains("\"beside=retired:"),
            "{refs_in}: {}",
            diff.stdout
        );
        let mut flags = vec!["--adopt", "--beside", SUFFIX];
        if refs_in {
            flags.push("--refs-in");
        }
        let ran = run.apply(&flags);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refused", "{refs_in}: {row:?}");
        assert!(
            row.2.contains("GIT_DESTINATION_MODIFIED"),
            "{refs_in}: {row:?}"
        );
        assert!(!beside.exists());
        if refs_in {
            let left: Vec<String> = named_refs(&dest)
                .into_keys()
                .filter(|name| name.starts_with("refs/neo-"))
                .collect();
            assert!(left.is_empty(), "retired refs came back: {left:?}");
        }
    }
}

/// P124 BESIDE-GITFILE (review of #282, F1): a beside directory whose
/// `.git` gitfile names an administration directory that is gone (a copy
/// of a pruned or moved linked worktree, as the two sting `*.neo-20261016`
/// directories naming `/Users/jess/...` are) or another repository's
/// refuses `GIT_BESIDE_FOREIGN`, typed and pending review, never a bare
/// `IO`; nothing at the beside path is written; `estate-diff` says
/// `beside=foreign`; closure has nothing unaccounted.
#[test]
fn p124_a_beside_gitfile_naming_a_gone_or_foreign_gitdir_is_foreign() {
    for case in ["gone", "other"] {
        let (world, source, dest) = small();
        let run = diverged(&world, &source, &dest, false);
        let beside = beside_of(&dest);
        copy_tree(&source, &beside);
        fs::remove_dir_all(beside.join(".git")).unwrap();
        let gitdir = if case == "gone" {
            world.path("gone/.git/worktrees/lamesh")
        } else {
            let other = unrelated_source(&world, "other");
            let tree = world.path("other-wt");
            git(
                &other,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "w",
                    tree.to_str().unwrap(),
                ],
            );
            other.join(".git/worktrees/other-wt")
        };
        fs::write(
            beside.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let frozen = freeze_all(&beside);
        let diff = run.diff(&["--beside", SUFFIX]);
        assert!(
            diff_line(&diff.stdout, &source).contains("\"beside=foreign:"),
            "{case}: {}",
            diff.stdout
        );
        let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        assert_eq!(row.1, "refused", "{case}: {row:?}");
        assert!(row.2.contains("GIT_BESIDE_FOREIGN"), "{case}: {row:?}");
        assert_eq!(freeze_all(&beside), frozen, "{case}: the beside path moved");
        let closure = run.closure();
        assert!(
            closure.stdout.contains("\"unaccounted\":0")
                && closure.stdout.contains("\"refused_pending_review\":1")
                && closure.stdout.contains("\"GIT_BESIDE_FOREIGN\":1"),
            "{case}: {}",
            closure.stdout
        );
    }
}

/// P125 BESIDE-STASH-BY-COMMIT (review of #282, F2): stash entries are
/// decided by commit, never by index. After a pass put the source's stash
/// `A` at `refs/neo-20261016-stash/0` and the operator deleted it, the
/// source stashing `B` (its stash now `B`, `A`) makes the next pass put
/// `B` at index 0, and no ref holds `A` again under another index: through
/// a beside converge (a mirror, its own entries moved), and through the
/// create-only refs-in of a refused beside (a new commit at a free index).
#[test]
fn p125_stash_refs_are_decided_by_commit() {
    for refused in [false, true] {
        let (world, source, dest) = small();
        fs::write(source.join("README"), b"LA-Mesh, stash A\n").unwrap();
        git(&source, &["stash", "push", "--quiet", "-m", "A"]);
        let a = git(&source, &["rev-parse", "refs/stash"]).trim().to_owned();
        let run = diverged(&world, &source, &dest, true);
        let beside = beside_of(&dest);
        let mut flags = vec!["--adopt", "--beside", SUFFIX];
        if refused {
            fs::create_dir(&beside).unwrap();
            fs::write(beside.join("operator.txt"), b"mine\n").unwrap();
            flags.push("--refs-in");
        }
        let stash = |repo: &Path| -> BTreeMap<String, String> {
            named_refs(repo)
                .into_iter()
                .filter(|(name, _)| name.starts_with("refs/neo-20261016-stash/"))
                .collect()
        };
        run.apply(&flags);
        assert_eq!(
            stash(&dest),
            BTreeMap::from([("refs/neo-20261016-stash/0".to_owned(), a.clone())]),
            "{refused}"
        );
        git(&dest, &["update-ref", "-d", "refs/neo-20261016-stash/0"]);
        fs::write(source.join("README"), b"LA-Mesh, stash B\n").unwrap();
        git(&source, &["stash", "push", "--quiet", "-m", "B"]);
        let b = git(&source, &["rev-parse", "refs/stash"]).trim().to_owned();
        settle();
        run.capture();
        let ran = run.apply(&flags);
        let row = receipt(&receipts(&ran.stdout), &source).clone();
        let expected = if refused {
            "refused"
        } else {
            "workspace-beside"
        };
        assert_eq!(row.1, expected, "{refused}: {row:?}");
        assert_eq!(
            stash(&dest),
            BTreeMap::from([("refs/neo-20261016-stash/0".to_owned(), b)]),
            "{refused}: a removed stash commit came back"
        );
    }
}

// ---- an occupied non-repo directory (2026-10-19, the td01 batch: linked
// worktrees of tinyland.dev whose `.git` the 2026-08-29 ceremony stripped,
// their files left in place) ----------------------------------------------

/// The td01 shape: a source `app` with a linked worktree `app.worktrees/wt`
/// (a commit of its own, a tracked file edited, an untracked file); at the
/// destination `app` (a copy without its worktree administration) and the
/// worktree's files with no `.git` at all, plus `build.log`, a file only
/// the destination has. The run plans both and is captured.
fn nonrepo_world(world: &Gftb) -> (Run<'_>, PathBuf, PathBuf, PathBuf) {
    let main = world.neo("app");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(main.join("a.txt"), b"alpha\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "-m", "c1"]);
    fs::create_dir(world.neo("app.worktrees")).unwrap();
    let wt = world.neo("app.worktrees/wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            wt.to_str().unwrap(),
        ],
    );
    fs::create_dir(wt.join("d")).unwrap();
    fs::write(wt.join("d/b.txt"), b"beta\n").unwrap();
    git(&wt, &["add", "d/b.txt"]);
    git(&wt, &["commit", "--quiet", "-m", "wt c2"]);
    fs::write(wt.join("a.txt"), b"alpha, edited on neo\n").unwrap();
    fs::write(wt.join("notes.txt"), b"untracked on neo\n").unwrap();
    let dmain = world.sting("app");
    copy_tree(&main, &dmain);
    fs::remove_dir_all(dmain.join(".git/worktrees")).unwrap();
    fs::create_dir(world.sting("app.worktrees")).unwrap();
    let dwt = world.sting("app.worktrees/wt");
    copy_tree(&wt, &dwt);
    fs::remove_file(dwt.join(".git")).unwrap();
    fs::write(dwt.join("build.log"), b"only at the destination\n").unwrap();
    let run = Run::new(
        world,
        &[
            (main, dmain.clone(), Some(dmain.clone())),
            (wt.clone(), dmain.clone(), Some(dwt.clone())),
        ],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    (run, wt, dmain, dwt)
}

/// Every entry under `dir` but its top-level `.git`, with its `lstat`
/// identity: what an adoption of a non-repo directory must not change.
fn entries_of(dir: &Path) -> Vec<(PathBuf, u64, i64, i64, i64, u64, u32)> {
    freeze_all(dir)
        .into_iter()
        .filter(|row| row.0 != dir.join(".git"))
        .collect()
}

/// P126 NONREPO-ADOPT: a linked worktree's directory with no `.git` that
/// holds every captured path with its kind, mode and bytes (tracked and
/// untracked, the edit included) is adopted under `--adopt` by registering
/// it again: a `.git` gitfile and an administration directory of the
/// destination repository, the index from the capture. No file in it is
/// written (every entry keeps its inode, times, size and mode), the file
/// only the destination has stays, the worktree is on its branch with the
/// capture's status, `git worktree list` names it, `estate-diff` said
/// `nonrepo-adoptable` before, closure says `applied`, and the source's
/// next commit converges into it like any landing.
#[test]
fn p126_an_equal_nonrepo_worktree_is_adopted_by_registering_it() {
    let world = Gftb::new();
    let (run, wt, dmain, dwt) = nonrepo_world(&world);
    let before = entries_of(&dwt);
    let diff = run.diff(&[]);
    assert!(
        diff_line(&diff.stdout, &wt).contains("\"adopt\":\"nonrepo-adoptable\""),
        "{}",
        diff.stdout
    );
    let ran = run.apply(&["--adopt"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &wt).clone();
    assert_eq!(row.1, "workspace-restored", "{row:?}");
    assert!(row.2.contains("adopted-nonrepo=1"), "{row:?}");
    assert_eq!(
        entries_of(&dwt),
        before,
        "a file in the directory was written"
    );
    assert_eq!(
        fs::read(dwt.join("build.log")).unwrap(),
        b"only at the destination\n"
    );
    let gitfile = fs::read_to_string(dwt.join(".git")).unwrap();
    assert!(
        gitfile.starts_with(&format!(
            "gitdir: {}",
            dmain.join(".git/worktrees/").display()
        )),
        "{gitfile}"
    );
    assert_eq!(
        git(&dwt, &["rev-parse", "HEAD"]),
        git(&wt, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&dwt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "wt"
    );
    let status = git(&dwt, &["status", "--porcelain", "--untracked-files=all"]);
    let mut want: Vec<String> = git(&wt, &["status", "--porcelain", "--untracked-files=all"])
        .lines()
        .map(str::to_owned)
        .collect();
    want.push("?? build.log".to_owned());
    want.sort();
    let mut got: Vec<String> = status.lines().map(str::to_owned).collect();
    got.sort();
    assert_eq!(got, want);
    assert!(
        git(&dmain, &["worktree", "list", "--porcelain"])
            .contains(&format!("worktree {}", dwt.display())),
        "{}",
        git(&dmain, &["worktree", "list", "--porcelain"])
    );
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"unaccounted\":0") && closure.stdout.contains("\"applied\":2"),
        "{}",
        closure.stdout
    );
    let ran = run.apply(&["--adopt"]);
    assert_eq!(
        receipt(&receipts(&ran.stdout), &wt).1,
        "previous-workspace-restoration-not-revalidated"
    );
    // The adopted worktree converges like any landing.
    fs::write(wt.join("later.txt"), b"later, from neo\n").unwrap();
    git(&wt, &["add", "later.txt"]);
    git(&wt, &["commit", "--quiet", "-m", "later"]);
    settle();
    run.capture();
    let ran = run.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &wt).clone();
    assert_eq!(row.1, "workspace-restored", "{row:?}");
    assert!(row.2.contains("converged"), "{row:?}");
    assert_eq!(
        fs::read(dwt.join("later.txt")).unwrap(),
        b"later, from neo\n"
    );
    assert_eq!(
        fs::read(dwt.join("build.log")).unwrap(),
        b"only at the destination\n"
    );
}

/// P127 NONREPO-BESIDE: a linked worktree's directory with no `.git` that
/// differs from the capture (a captured file edited at the destination)
/// is never written: under `--adopt` alone it refuses
/// `GIT_DESTINATION_NONREPO_DIFFERS`, typed and pending review; under
/// `--adopt --beside --refs-in` the capture lands beside it at
/// `<path>.neo-20261016` as a linked worktree of the repository (detached,
/// its refs in under `refs/neo-20261016/`), the receipt and closure name
/// the reason (`beside-reason`, `beside_reason`), and the directory keeps
/// every entry as it was. `estate-diff` says `nonrepo-differs` with the
/// differing seat and `beside=landing`.
#[test]
fn p127_a_differing_nonrepo_worktree_stays_and_the_capture_lands_beside() {
    let world = Gftb::new();
    let (run, wt, dmain, dwt) = nonrepo_world(&world);
    fs::write(dwt.join("d/b.txt"), b"beta, edited on sting\n").unwrap();
    let before = entries_of(&dwt);
    let diff = run.diff(&["--beside", SUFFIX]);
    let line = diff_line(&diff.stdout, &wt).to_owned();
    assert!(
        line.contains("\"adopt\":\"nonrepo-differs\"")
            && line.contains("\"seat=d/b.txt\"")
            && line.contains("\"beside=landing:"),
        "{line}"
    );
    let ran = run.apply(&["--adopt"]);
    let row = receipt(&receipts(&ran.stdout), &wt).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_NONREPO_DIFFERS"), "{row:?}");
    assert_eq!(entries_of(&dwt), before);
    assert!(!dwt.join(".git").exists());
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"unaccounted\":0")
            && closure
                .stdout
                .contains("\"GIT_DESTINATION_NONREPO_DIFFERS\":1"),
        "{}",
        closure.stdout
    );
    let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
    assert!(ran.ok, "{}\n{}", ran.stdout, ran.stderr);
    let row = receipt(&receipts(&ran.stdout), &wt).clone();
    assert_eq!(row.1, "workspace-beside", "{row:?}");
    assert!(
        row.2
            .contains("beside-reason=GIT_DESTINATION_NONREPO_DIFFERS")
            && row.2.contains("beside-landed=1")
            && row.2.contains("beside-linked=1"),
        "{row:?}"
    );
    assert_eq!(
        entries_of(&dwt),
        before,
        "the non-repo directory was written"
    );
    assert!(!dwt.join(".git").exists());
    let beside = beside_of(&dwt);
    assert_eq!(
        git(&beside, &["rev-parse", "HEAD"]),
        git(&wt, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&beside, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(fs::read(beside.join("d/b.txt")).unwrap(), b"beta\n");
    assert_eq!(
        named_refs(&dmain).get("refs/neo-20261016/heads/wt"),
        Some(&git(&wt, &["rev-parse", "HEAD"]).trim().to_owned())
    );
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"applied_beside\":1")
            && closure
                .stdout
                .contains("\"beside_reason\":\"GIT_DESTINATION_NONREPO_DIFFERS\"")
            && closure.stdout.contains("\"unaccounted\":0"),
        "{}",
        closure.stdout
    );
}

/// P128 NONREPO-STANDALONE: a standalone item's planned path holding a
/// directory with no `.git` has no registration to re-create and no
/// repository to land beside into: under `--adopt --beside --refs-in` it
/// refuses `GIT_DESTINATION_NONREPO_DIFFERS`, typed and pending review,
/// and nothing there or beside it is written, even when its files are the
/// capture's.
#[test]
fn p128_a_standalone_nonrepo_directory_refuses_typed_and_stays() {
    let (world, source, dest) = small();
    fs::remove_dir_all(dest.join(".git")).unwrap();
    let run = Run::new(
        &world,
        &[(source.clone(), dest.clone(), Some(dest.clone()))],
    );
    settle();
    run.capture();
    private_dir(&run.applied);
    let before = entries_of(&dest);
    let ran = run.apply(&["--adopt", "--beside", SUFFIX, "--refs-in"]);
    let row = receipt(&receipts(&ran.stdout), &source).clone();
    assert_eq!(row.1, "refused", "{row:?}");
    assert!(row.2.contains("GIT_DESTINATION_NONREPO_DIFFERS"), "{row:?}");
    assert_eq!(entries_of(&dest), before);
    assert!(!dest.join(".git").exists());
    assert!(!beside_of(&dest).exists());
    let closure = run.closure();
    assert!(
        closure.stdout.contains("\"unaccounted\":0")
            && closure.stdout.contains("\"refused_pending_review\":1"),
        "{}",
        closure.stdout
    );
}
