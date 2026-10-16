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
        let mut list: Vec<OsString> = vec!["estate-diff".into()];
        list.extend(extra.iter().map(OsString::from));
        list.extend(args(&[&self.plan, &self.corpus, &SOURCE, &self.applied]));
        let ran = agent(&list);
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
