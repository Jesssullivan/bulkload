//! Unit rows of the converge: the seat delta's order, the attach rule
//! (findings 1, 2, 11 and 17), and the index digest's indifference to
//! `git status`.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use super::*;

fn side(kind: Kind, blob: char) -> Side {
    Side {
        kind,
        mode: if kind == Kind::Directory {
            0o755
        } else {
            0o644
        },
        blob: (kind != Kind::Directory).then(|| blob.to_string().repeat(40)),
        link: (kind == Kind::Symlink).then(|| vec![b'x']),
    }
}

fn map(rows: &[(&str, Side)]) -> BTreeMap<Vec<u8>, Side> {
    rows.iter()
        .map(|(path, side)| (path.as_bytes().to_vec(), side.clone()))
        .collect()
}

#[test]
fn the_delta_removes_deepest_first_and_creates_shallowest_first() {
    let old = map(&[
        ("a", side(Kind::Regular, 'a')),
        ("d", side(Kind::Directory, '-')),
        ("d/e", side(Kind::Directory, '-')),
        ("d/e/f", side(Kind::Regular, 'f')),
        ("k", side(Kind::Regular, 'k')),
        ("m", side(Kind::Regular, 'm')),
    ]);
    let mut moded = side(Kind::Regular, 'm');
    moded.mode = 0o755;
    let new = map(&[
        ("a", side(Kind::Regular, 'b')),
        ("d", side(Kind::Directory, '-')),
        ("d/e", side(Kind::Regular, 'e')),
        ("k", side(Kind::Symlink, 'k')),
        ("m", moded),
        ("n", side(Kind::Directory, '-')),
        ("n/o", side(Kind::Regular, 'o')),
    ]);
    let ops: Vec<(Action, String)> = delta(&old, &new)
        .into_iter()
        .map(|op| (op.action, String::from_utf8(op.path).unwrap()))
        .collect();
    assert_eq!(
        ops,
        vec![
            (Action::Remove, "d/e/f".into()),
            (Action::RmDir, "d/e".into()),
            (Action::MkDir, "n".into()),
            (Action::Replace, "a".into()),
            (Action::Add, "d/e".into()),
            (Action::Replace, "k".into()),
            (Action::Chmod, "m".into()),
            (Action::Add, "n/o".into()),
        ]
    );
    // Numbered in execution order: the temporaries' names.
    assert!(delta(&old, &new)
        .iter()
        .enumerate()
        .all(|(index, op)| op.n == index));
    assert!(delta(&old, &old).is_empty());
}

#[test]
fn ancestors_are_every_proper_prefix_directory() {
    let found: Vec<&[u8]> = ancestors(b"a/b/c").collect();
    assert_eq!(found, vec![b"a".as_slice(), b"a/b".as_slice()]);
    assert_eq!(ancestors(b"top").count(), 0);
}

fn oid(byte: char) -> String {
    byte.to_string().repeat(40)
}

#[test]
fn the_attach_rule() {
    let (h1, x) = (oid('1'), oid('x'));
    let branch = "refs/heads/feat";
    let created = |rows: &[(&str, LandedRef)]| RefLedger {
        created: true,
        refs: rows
            .iter()
            .map(|(name, row)| ((*name).to_owned(), row.clone()))
            .collect(),
        ..RefLedger::default()
    };
    let native = |value: Option<&String>| -> BTreeMap<String, String> {
        value
            .map(|value| (branch.to_owned(), value.clone()))
            .into_iter()
            .collect()
    };
    let none = BTreeSet::new();
    // Detached at the source: detached here.
    let plan = head_plan(None, &h1, (&native(None), &none), None, &none, 5);
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.intended, None);
    // Checked out by another worktree (finding 1): detach and record.
    let others: BTreeSet<String> = [branch.to_owned()].into();
    let plan = head_plan(
        Some(branch),
        &h1,
        (&native(Some(&h1)), &none),
        None,
        &others,
        5,
    );
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.intended.as_deref(), Some(branch));
    assert_eq!(plan.line, Some("head-detached-ref-conflict"));
    // A created repository: an owned branch moves with HEAD (W1, finding 2).
    let ledger = created(&[(
        branch,
        LandedRef {
            oid: Some(x.clone()),
            observed_ns: 1,
        },
    )]);
    let plan = head_plan(
        Some(branch),
        &h1,
        (&native(Some(&x)), &none),
        Some(&ledger),
        &none,
        5,
    );
    assert_eq!(plan.head.symbolic.as_deref(), Some(branch));
    assert_eq!(
        plan.branch,
        Some((branch.to_owned(), Some(x.clone()), h1.clone()))
    );
    // ... but not one a newer capture of another item set (R6): detach.
    let plan = head_plan(
        Some(branch),
        &h1,
        (&native(Some(&x)), &none),
        Some(&ledger),
        &none,
        1,
    );
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.line, Some("head-detached-ref-newer"));
    // A foreign repository: an absent branch is created, a matching one
    // used, any other detaches (finding 11: never a refusal).
    let plan = head_plan(Some(branch), &h1, (&native(None), &none), None, &none, 5);
    assert_eq!(plan.branch, Some((branch.to_owned(), None, h1.clone())));
    let plan = head_plan(
        Some(branch),
        &h1,
        (&native(Some(&h1)), &none),
        None,
        &none,
        5,
    );
    assert_eq!(
        (plan.head.symbolic.as_deref(), plan.branch),
        (Some(branch), None)
    );
    let plan = head_plan(
        Some(branch),
        &h1,
        (&native(Some(&x)), &none),
        None,
        &none,
        5,
    );
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.line, Some("head-detached-ref-conflict"));
}

#[test]
fn the_index_digest_ignores_a_status_refresh_and_sees_a_stage() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path();
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    run(&["init", "--quiet", "--template="]);
    fs::write(repo.join("f"), b"one\n").unwrap();
    run(&["add", "f"]);
    let before = index_digest(repo, None).unwrap();
    // A touch and a status refresh rewrite the index's stat cache only.
    let file = fs::OpenOptions::new()
        .write(true)
        .open(repo.join("f"))
        .unwrap();
    file.set_modified(std::time::SystemTime::now()).unwrap();
    drop(file);
    run(&["status", "--porcelain"]);
    assert_eq!(index_digest(repo, None).unwrap(), before);
    fs::write(repo.join("f"), b"two\n").unwrap();
    run(&["add", "f"]);
    assert_ne!(index_digest(repo, None).unwrap(), before);
}

// ---- the convergence lane's fixes (2026-10-09) ------------------------------

fn git_run(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

// A repository with two commits on `main` and a branch `side` at the first.
fn two_commits() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&repo).unwrap();
    git_run(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(repo.join("f"), b"one\n").unwrap();
    git_run(&repo, &["add", "f"]);
    git_run(&repo, &["commit", "--quiet", "-m", "one"]);
    git_run(&repo, &["branch", "side"]);
    fs::write(repo.join("f"), b"two\n").unwrap();
    git_run(&repo, &["commit", "--quiet", "-am", "two"]);
    (dir, repo)
}

/// Review 3 finding 2 (mutation `rebase_unguarded`): a worktree in the
/// middle of a rebase is detached, yet its branch is the rebase's; so is
/// the branch a bisect started from. Both count as checked out, as Git's
/// own `branch -f` counts them.
#[test]
fn a_branch_under_a_rebase_or_bisect_counts_as_checked_out() {
    let (_dir, repo) = two_commits();
    let branches = |repo: &Path| -> Vec<String> {
        checked_out_by(repo)
            .unwrap()
            .into_iter()
            .map(|(_, branch)| branch)
            .collect()
    };
    // A rebase of `side` onto `main` that stops on a conflict.
    git_run(&repo, &["switch", "--quiet", "side"]);
    fs::write(repo.join("f"), b"side\n").unwrap();
    git_run(&repo, &["commit", "--quiet", "-am", "side"]);
    let stopped = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=T", "-c", "user.email=t@l", "-C"])
        .arg(&repo)
        .args(["rebase", "--quiet", "main"])
        .output()
        .unwrap();
    assert!(
        !stopped.status.success(),
        "the rebase must stop on its conflict"
    );
    assert_eq!(branches(&repo), vec!["refs/heads/side".to_owned()]);
    git_run(&repo, &["rebase", "--abort"]);
    // A bisect started on `side`, then the worktree detached by hand.
    git_run(&repo, &["bisect", "start"]);
    git_run(&repo, &["switch", "--quiet", "--detach", "main"]);
    assert_eq!(branches(&repo), vec!["refs/heads/side".to_owned()]);
    git_run(&repo, &["bisect", "reset", "main"]);
    assert_eq!(branches(&repo), vec!["refs/heads/main".to_owned()]);
}

/// Review 3 finding 4 (mutation `unborn_head_untyped`): an unborn HEAD (an
/// orphan switch) reads as a HEAD with no commit, so the proof refuses
/// `GIT_DESTINATION_MODIFIED`; before the fix `rev-parse --verify HEAD`
/// failed and the item refused `GIT_CHILD_FAILED`.
#[test]
fn an_unborn_head_reads_as_no_commit() {
    let (_dir, repo) = two_commits();
    let born = head_of(&repo).unwrap();
    assert_eq!(born.symbolic.as_deref(), Some("refs/heads/main"));
    assert!(super::super::oid(&born.oid));
    git_run(&repo, &["switch", "--quiet", "--orphan", "fresh"]);
    assert_eq!(
        head_of(&repo).unwrap(),
        Head {
            symbolic: Some("refs/heads/fresh".to_owned()),
            oid: String::new(),
        }
    );
}

/// Review 3 finding 9 (mutation `probe_left`): the probe files a crashed
/// pass left in the worktree root are removed; nothing else is.
#[test]
fn the_probe_sweep_removes_only_probe_files() {
    let (_dir, repo) = two_commits();
    for name in [
        ".bulkload-converge-0123456789abcdef-probe-a",
        ".bulkload-converge-0123456789abcdef-probe-b",
        ".bulkload-converge-0123456789abcdef-3",
        "probe-a",
    ] {
        fs::write(repo.join(name), b"").unwrap();
    }
    assert_eq!(sweep_probes(&repo).unwrap(), 2);
    assert!(!repo
        .join(".bulkload-converge-0123456789abcdef-probe-a")
        .exists());
    assert!(repo.join(".bulkload-converge-0123456789abcdef-3").exists());
    assert!(repo.join("probe-a").exists());
    assert_eq!(sweep_probes(&repo).unwrap(), 0);
}

/// Review 3 finding 3 (mutation `ledger_reads_index`): the ledger records
/// the index digest `publish_index` installed, and the exclude the
/// converge wrote or proved, never what the worktree holds when the ledger
/// is written: an operator's `git add` in between stays an edit the next
/// proof sees.
#[test]
fn the_ledger_records_the_installed_index_not_a_re_read() {
    let (_dir, repo) = two_commits();
    let installed = index_digest(&repo, None).unwrap();
    // The operator stages a change after the publish.
    fs::write(repo.join("f"), b"operator\n").unwrap();
    git_run(&repo, &["add", "f"]);
    assert_ne!(index_digest(&repo, None).unwrap(), installed);
    let before = LandedWorkspace {
        item: "item".to_owned(),
        capture: [0; 32],
        observed_ns: 1,
        instance: "i".to_owned(),
        head: head_of(&repo).unwrap(),
        intended_branch: None,
        index: [1; 32],
        exclude: [2; 32],
        recorded_ns: 0,
        fsid: 0,
        seats: Vec::new(),
    };
    let next = next_ledger(
        &repo,
        &before,
        ([3; 32], 2, "j"),
        (before.head.clone(), None),
        (installed, [4; 32]),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(next.index, installed);
    assert_eq!(next.exclude, [4; 32]);
}

/// The earlier report's finding 1 (mutation `settle_remove_dir`): the delta
/// of a file the source turns into a directory marks its removal as one
/// whose path is a directory afterwards, which a settle finds done.
#[test]
fn a_file_turned_directory_marks_its_removal() {
    let old = map(&[("p", side(Kind::Regular, 'p'))]);
    let new = map(&[
        ("p", side(Kind::Directory, '-')),
        ("p/q", side(Kind::Regular, 'q')),
    ]);
    let ops = delta(&old, &new);
    let removal = ops.iter().find(|op| op.action == Action::Remove).unwrap();
    assert!(removal.to_dir);
    assert!(delta(&new, &old)
        .iter()
        .all(|op| !op.to_dir || op.action == Action::MkDir || op.action == Action::DirMode));
}

// ---- review 4 (2026-10-09, later) --------------------------------------------

fn head_at(repo: &Path, name: Option<&str>) -> Head {
    Head {
        symbolic: name.map(str::to_owned),
        oid: git_run(repo, &["rev-parse", "HEAD"]),
    }
}

/// Review 4 (mutation `set_head_one_txn`, MU15): a Git child killed inside
/// `set_head`'s transaction commits the lines it renamed first, and Git
/// renames HEAD (which sorts before `refs/`) first. The hook below is that
/// child: given HEAD's line and the branch's in one transaction it commits
/// HEAD's alone and fails. `set_head` must never leave HEAD on the branch
/// while the branch is at a value its line did not write. Before review 3's
/// fix both lines were one transaction; nothing but the model's row pinned
/// it.
#[test]
fn set_head_never_commits_head_without_its_branch() {
    let (_dir, repo) = two_commits();
    let one = git_run(&repo, &["rev-parse", "side"]);
    let two = git_run(&repo, &["rev-parse", "main"]);
    native_refs::UPDATE_REFS.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|repo: &Path, lines: &[u8], _names: &[&str]| {
            let text = String::from_utf8_lossy(lines).into_owned();
            let commands = text
                .split('\0')
                .filter(|field| {
                    ["create ", "update ", "delete ", "symref-update "]
                        .iter()
                        .any(|verb| field.starts_with(verb))
                })
                .count();
            if commands < 2 || !text.contains("symref-update HEAD") {
                return None;
            }
            // The killed child: HEAD's line renamed, the branch's not.
            let start = text.find("symref-update HEAD").unwrap();
            let head: Vec<&str> = text[start..].splitn(5, '\0').take(4).collect();
            let mut line = head.join("\0");
            line.push('\0');
            let mut child = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["update-ref", "--no-deref", "--stdin", "-z"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(child.stdin.as_mut().unwrap(), line.as_bytes()).unwrap();
            drop(child.stdin.take());
            assert!(child.wait().unwrap().success());
            Some(Err(BulkloadRefusal::GitChildFailed(
                bulkload_proto::StderrClass::Other,
            )))
        }));
    });
    let old = head_at(&repo, Some("refs/heads/main"));
    let new = Head {
        symbolic: Some("refs/heads/side".to_owned()),
        oid: two.clone(),
    };
    let branch = ("refs/heads/side".to_owned(), Some(one), two.clone());
    let result = set_head(&repo, &old, &new, Some(&branch));
    native_refs::UPDATE_REFS.with(|hook| *hook.borrow_mut() = None);
    let now = head_of(&repo).unwrap();
    assert!(
        now.symbolic.as_deref() != Some("refs/heads/side") || now.oid == two,
        "HEAD on side at {} ({result:?}): committed without its branch",
        now.oid
    );
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(now, new);
}

/// Review 4 (the blocker; mutation `head_attach_unchecked`, MU18): HEAD is
/// never attached to a branch another worktree has checked out (an
/// operator's `git worktree add` or `git switch` of it since the plan or a
/// crash), with or without a branch line: `set_head` refuses
/// `GIT_CONVERGE_INTERRUPTED` and HEAD stays. Before the fix its HEAD-only
/// path ran `symref-update HEAD` with nothing checked, and two worktrees
/// ended on one branch.
#[test]
fn set_head_never_puts_a_second_worktree_on_a_branch() {
    let (dir, repo) = two_commits();
    let side = git_run(&repo, &["rev-parse", "side"]);
    let operator = dir.path().canonicalize().unwrap().join("operator-wt");
    git_run(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            operator.to_str().unwrap(),
            "side",
        ],
    );
    let old = head_at(&repo, Some("refs/heads/main"));
    let new = Head {
        symbolic: Some("refs/heads/side".to_owned()),
        oid: side.clone(),
    };
    assert_eq!(
        set_head(&repo, &old, &new, None).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert_eq!(head_of(&repo).unwrap(), old);
    // With a branch line too: nothing moves.
    let main = old.oid.clone();
    let branch = (
        "refs/heads/side".to_owned(),
        Some(side.clone()),
        main.clone(),
    );
    assert_eq!(
        set_head(
            &repo,
            &old,
            &Head {
                symbolic: new.symbolic.clone(),
                oid: main
            },
            Some(&branch)
        )
        .unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert_eq!(git_run(&repo, &["rev-parse", "side"]), side);
    assert_eq!(head_of(&repo).unwrap(), old);
    // Every lock it held is gone; once the branch is free, it attaches.
    assert!(!repo.join(".git/HEAD.lock").exists());
    assert!(!repo.join(".git/worktrees/operator-wt/HEAD.lock").exists());
    git_run(&operator, &["switch", "--quiet", "--detach"]);
    set_head(&repo, &old, &new, None).unwrap();
    assert_eq!(head_of(&repo).unwrap(), new);
}
