//! P87 HEAD-INDEX-DRIFT-CUSTODY (#38, OI-1003-Q11; S5 under OI-1003-Q152:
//! no freeze window, the run closes by converging deltas while the source
//! stays live).
//!
//! An agent committing, switching and staging inside a captured checkout,
//! during a pass or between passes, never makes the item refuse. A pass it
//! races records the move as drift custody (`HeadMoved`, `IndexRewritten`)
//! and lays nothing down; the next quiet pass carries the new HEAD and index
//! (its key is never a reuse hit), and the apply converges the landing to
//! the source: seats, index entries, HEAD and its branch equal. Before #38
//! the raced pass refused `GIT_AUTHORITY_CHANGED`, a typed refusal that
//! needed an operator's review before the run could close.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use super::*;
use std::collections::BTreeMap;
use std::process::Command;

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("bulkload-p87-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A Git child with no global or system configuration and no hooks (R-N98:
// only the product's own tests disable hooks, in fixtures they own).
fn git_out(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "user.name=Bulkload test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

// Seats must be older than one timestamp tick to be reused (R-N76).
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

/// Every seat under `root` but `.git`, by path: its kind and bytes.
fn files(root: &Path) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (bool, Vec<u8>)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                out.insert(relative, (true, Vec::new()));
                walk(root, &path, out);
            } else {
                out.insert(relative, (false, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// What a checkout shows an agent: its seats, its index entries, the branch
/// HEAD names and the commit it resolves to, and the local branches.
type View = (
    BTreeMap<PathBuf, (bool, Vec<u8>)>,
    String,
    String,
    String,
    String,
);

fn view(worktree: &Path) -> View {
    (
        files(worktree),
        git_out(worktree, &["ls-files", "--stage"]),
        git_out(worktree, &["symbolic-ref", "-q", "HEAD"]),
        git_out(worktree, &["rev-parse", "HEAD"]),
        git_out(
            worktree,
            &[
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                "refs/heads",
            ],
        ),
    )
}

/// One capture's receipts: (outcome, reason, drift rows).
fn captured(
    plan: &Path,
    state: &Path,
    corpus: &Path,
) -> Vec<(&'static str, Option<String>, Vec<String>)> {
    let rows = Mutex::new(Vec::new());
    capture(plan, state, corpus, 1, &|row| {
        rows.lock()
            .unwrap()
            .push((row.outcome, row.reason.clone(), row.drift.clone()));
        Ok(())
    })
    .unwrap();
    rows.into_inner().unwrap()
}

/// One apply's receipts: (outcome, reason).
fn applied(plan: &Path, corpus: &Path, state: &Path) -> Vec<(&'static str, Option<String>)> {
    let rows = Mutex::new(Vec::new());
    let _ = apply(plan, corpus, state, "neo", 1, &|row| {
        rows.lock().unwrap().push((row.outcome, row.reason.clone()));
        Ok(())
    });
    rows.into_inner().unwrap()
}

#[test]
fn p87_an_agent_working_through_every_pass_is_carried_by_the_next_converging_delta() {
    let root = Root::new("agent");
    let (src, dst) = (root.join("src"), root.join("sting/src"));
    let (plan, cstate, corpus, astate) = (
        root.join("plan"),
        root.join("capture-state"),
        root.join("corpus"),
        root.join("apply-state"),
    );
    fs::create_dir(&src).unwrap();
    git_out(&src, &["init", "-q", "--template="]);
    fs::write(src.join("a.txt"), b"alpha\n").unwrap();
    fs::create_dir(src.join("dir")).unwrap();
    fs::write(src.join("dir/b.txt"), b"beta\n").unwrap();
    git_out(&src, &["add", "."]);
    git_out(&src, &["commit", "-q", "-m", "c1"]);
    git_out(&src, &["branch", "topic"]);
    fs::write(src.join("notes.txt"), b"untracked\n").unwrap();
    fs::write(src.join("staged.txt"), b"staged only\n").unwrap();
    git_out(&src, &["add", "staged.txt"]);
    fs::create_dir(root.join("sting")).unwrap();
    add(&plan, &src, &dst, Some(&dst)).unwrap();
    // A file the agent stages under pass 1: it exists before the pass, so
    // only the index (and HEAD, by the commit) moves under it.
    fs::write(src.join("later.txt"), b"staged under the pass\n").unwrap();
    settle();

    // Pass 1: the agent commits and stages while the capture runs.
    let agent = fs::canonicalize(&src).unwrap();
    git_carry::mid_pass::arm(&src, move || {
        git_out(
            &agent,
            &["commit", "-q", "--allow-empty", "-m", "agent, mid-pass"],
        );
        git_out(&agent, &["add", "later.txt"]);
    });
    let first = captured(&plan, &cstate, &corpus);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, "captured-with-drift", "{first:?}");
    assert!(
        first[0].2.contains(&"HeadMoved \"HEAD\"".to_owned()),
        "{first:?}"
    );
    assert!(
        first[0].2.contains(&"IndexRewritten \"index\"".to_owned()),
        "{first:?}"
    );
    // A drifted capture never lays a workspace down.
    let rows = applied(&plan, &corpus, &astate);
    assert_eq!(
        rows,
        vec![("refused", Some(BulkloadRefusal::CaptureDrifted.to_string()))]
    );
    assert!(!dst.exists());

    // Pass 2, quiet: the drifted record is no reuse hit; the new HEAD and
    // index are carried, and the apply lands exactly the source.
    settle();
    let second = captured(&plan, &cstate, &corpus);
    assert!(
        matches!(second[0].0, "captured" | "capture-extended-from-drift"),
        "{second:?}"
    );
    assert!(second[0].2.is_empty(), "{second:?}");
    let rows = applied(&plan, &corpus, &astate);
    assert_eq!(rows[0].0, "workspace-restored", "{rows:?}");
    assert_eq!(view(&dst), view(&src));

    // Between passes the agent commits, switches and stages.
    settle();
    fs::write(src.join("a.txt"), b"alpha, day two\n").unwrap();
    git_out(&src, &["commit", "-q", "-am", "c2"]);
    git_out(&src, &["switch", "-q", "topic"]);
    fs::write(src.join("c.txt"), b"staged on topic\n").unwrap();
    git_out(&src, &["add", "c.txt"]);
    settle();

    // Pass 3: the agent switches back under the pass (HEAD, the index and
    // the seats move together). Custody again; the landing is untouched.
    let landed = view(&dst);
    let agent = fs::canonicalize(&src).unwrap();
    git_carry::mid_pass::arm(&src, move || {
        git_out(&agent, &["switch", "-q", "main"]);
    });
    let third = captured(&plan, &cstate, &corpus);
    assert_eq!(third[0].0, "captured-with-drift", "{third:?}");
    assert!(
        third[0].2.contains(&"HeadMoved \"HEAD\"".to_owned()),
        "{third:?}"
    );
    let rows = applied(&plan, &corpus, &astate);
    assert_eq!(
        rows,
        vec![("refused", Some(BulkloadRefusal::CaptureDrifted.to_string()))]
    );
    assert_eq!(view(&dst), landed);

    // Pass 4, quiet: the converging delta. The landing equals the source.
    settle();
    let fourth = captured(&plan, &cstate, &corpus);
    assert!(fourth[0].2.is_empty(), "{fourth:?}");
    let rows = applied(&plan, &corpus, &astate);
    assert_eq!(rows[0].0, "workspace-restored", "{rows:?}");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_some_and(|reason| reason.starts_with("converged")),
        "{rows:?}"
    );
    assert_eq!(view(&dst), view(&src));
    assert_eq!(
        git_out(&dst, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );

    // S4: the run closes with nothing unaccounted and nothing to review.
    let ledger = ledger(&plan, &corpus, "neo", &[astate]).unwrap();
    let report = crate::closure::Report::from_ledger(&ledger);
    assert_eq!(report.unaccounted, 0);
    assert!(report.passes(), "{}", report.to_json());
}

/// The stash an agent sees, newest first: each entry's commit and subject.
fn stash_list(worktree: &Path) -> String {
    let listed = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(worktree)
        .args(["reflog", "show", "--format=%H %gs", "refs/stash"])
        .output()
        .expect("git reflog");
    // No stash at all is the empty list (refs/stash absent).
    if listed.status.success() {
        String::from_utf8(listed.stdout).unwrap()
    } else {
        String::new()
    }
}

/// One capture raced by `race` (an agent's stash op under the byte pass):
/// drift custody naming the stash, and nothing laid down.
fn raced_stash_pass(
    (src, dst): (&Path, &Path),
    (plan, cstate, corpus, astate): (&Path, &Path, &Path, &Path),
    race: &'static [&'static str],
) {
    // The agent also edits a note between passes, so the pass is no reuse
    // hit and its export runs (the race fires inside it).
    fs::write(src.join("notes.txt"), race.join(" ")).unwrap();
    settle();
    let landed = (view(dst), stash_list(dst));
    let agent = fs::canonicalize(src).unwrap();
    git_carry::mid_pass::arm(src, move || {
        git_out(&agent, race);
    });
    let raced = captured(plan, cstate, corpus);
    assert_eq!(raced.len(), 1);
    assert_eq!(raced[0].0, "captured-with-drift", "{race:?}: {raced:?}");
    assert!(
        raced[0].2.contains(&"StashMoved \"refs/stash\"".to_owned()),
        "{race:?}: {raced:?}"
    );
    let rows = applied(plan, corpus, astate);
    assert_eq!(
        rows,
        vec![("refused", Some(BulkloadRefusal::CaptureDrifted.to_string()))],
        "{race:?}"
    );
    assert_eq!((view(dst), stash_list(dst)), landed, "{race:?}");
}

/// The next quiet pass: no drift, and the apply converges the landing to
/// the source, its stash in the source's order included.
fn converging_pass(
    (src, dst): (&Path, &Path),
    (plan, cstate, corpus, astate): (&Path, &Path, &Path, &Path),
) {
    settle();
    let quiet = captured(plan, cstate, corpus);
    assert!(
        matches!(quiet[0].0, "captured" | "capture-extended-from-drift"),
        "{quiet:?}"
    );
    assert!(quiet[0].2.is_empty(), "{quiet:?}");
    let rows = applied(plan, corpus, astate);
    assert_eq!(rows[0].0, "workspace-restored", "{rows:?}");
    assert_eq!(view(dst), view(src));
    assert_eq!(stash_list(dst), stash_list(src));
}

// P87, the stash (OI-1003-Q161): an agent's `git stash push`, `pop` and
// `drop` during a pass are drift custody like its commits and switches.
// Each raced pass records `StashMoved` and lays nothing down; the next
// quiet pass carries the new stash, and the landing converges to the
// source with the stash present on the destination in the source's order.
// Before Q161 each raced pass refused GIT_AUTHORITY_CHANGED.
#[test]
fn p87_an_agent_stashing_through_every_pass_is_carried_by_the_next_converging_delta() {
    let root = Root::new("stash");
    let (src, dst) = (root.join("src"), root.join("sting/src"));
    let (plan, cstate, corpus, astate) = (
        root.join("plan"),
        root.join("capture-state"),
        root.join("corpus"),
        root.join("apply-state"),
    );
    fs::create_dir(&src).unwrap();
    git_out(&src, &["init", "-q", "--template="]);
    fs::write(src.join("a.txt"), b"alpha\n").unwrap();
    fs::write(src.join("b.txt"), b"beta\n").unwrap();
    git_out(&src, &["add", "."]);
    git_out(&src, &["commit", "-q", "-m", "c1"]);
    // A stash the agent made before the run.
    fs::write(src.join("b.txt"), b"beta, stashed before the run\n").unwrap();
    git_out(&src, &["stash", "push", "-q", "-m", "before the run"]);
    fs::create_dir(root.join("sting")).unwrap();
    add(&plan, &src, &dst, Some(&dst)).unwrap();
    let paths = (src.as_path(), dst.as_path());
    let state = (
        plan.as_path(),
        cstate.as_path(),
        corpus.as_path(),
        astate.as_path(),
    );

    // Pass 1, quiet: the first landing carries the stash.
    converging_pass(paths, state);
    assert_eq!(stash_list(&dst).lines().count(), 1);

    // Pass 2: the agent stashes an edit while the capture runs (refs/stash,
    // its reflog, the seat and the index move under the pass).
    settle();
    fs::write(src.join("a.txt"), b"alpha, an edit the agent stashes\n").unwrap();
    settle();
    raced_stash_pass(
        paths,
        state,
        &["stash", "push", "-q", "-m", "agent, mid-pass"],
    );
    // Pass 3, quiet: two stashes on sting, newest first, as on the source.
    converging_pass(paths, state);
    assert_eq!(stash_list(&dst).lines().count(), 2);
    assert!(stash_list(&dst)
        .lines()
        .next()
        .is_some_and(|line| line.ends_with("agent, mid-pass")));

    // Pass 4: the agent pops it back under the pass.
    settle();
    raced_stash_pass(paths, state, &["stash", "pop", "-q"]);
    converging_pass(paths, state);
    assert_eq!(stash_list(&dst).lines().count(), 1);
    assert_eq!(
        fs::read(dst.join("a.txt")).unwrap(),
        fs::read(src.join("a.txt")).unwrap()
    );

    // Pass 6: the agent drops the last stash under the pass; refs/stash
    // goes with it, and the converging delta leaves sting with no stash.
    settle();
    raced_stash_pass(paths, state, &["stash", "drop", "-q"]);
    converging_pass(paths, state);
    assert_eq!(stash_list(&dst), "");

    // S4: the run closes with nothing unaccounted and nothing to review.
    let ledger = ledger(&plan, &corpus, "neo", &[astate]).unwrap();
    let report = crate::closure::Report::from_ledger(&ledger);
    assert_eq!(report.unaccounted, 0);
    assert!(report.passes(), "{}", report.to_json());
}
