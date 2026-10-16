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
        prefix: Vec::new(),
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

// ---- estate adopt (#226) -----------------------------------------------------

// A source with a commit, a staged edit, an intent-to-add entry, an
// executable, a symlink and an untracked file; its capture; and a fresh
// standalone landing of it at `dest`.
fn adopt_world() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    git_run(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("f"), b"one\n").unwrap();
    fs::write(source.join("run.sh"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("f", source.join("link")).unwrap();
    git_run(&source, &["add", "."]);
    git_run(&source, &["commit", "--quiet", "-m", "one"]);
    fs::write(source.join("f"), b"staged\n").unwrap();
    git_run(&source, &["add", "f"]);
    fs::create_dir(source.join("sub")).unwrap();
    fs::write(source.join("sub/new.sh"), b"intent\n").unwrap();
    fs::set_permissions(source.join("sub/new.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    git_run(&source, &["add", "--intent-to-add", "sub/new.sh"]);
    fs::write(source.join("notes"), b"untracked\n").unwrap();
    let bundle = super::super::export_repository(&source, &root.join("capture")).unwrap();
    let dest = root.join("dest");
    super::super::restore_bundle(&bundle, &dest, "neo").unwrap();
    (dir, source, bundle, dest)
}

fn adopt_landing(staged: &super::super::StagedBundle) -> Landing<'static> {
    Landing {
        item: "item",
        observed_ns: 1,
        capture: staged.digest(),
    }
}

/// The adopt scratch builds exactly the index entries digest a landing of
/// the capture installs, intent-to-add entries and modes included, so the
/// proof's index row compares like with like.
#[test]
fn the_adopt_scratch_builds_the_index_a_landing_installs() {
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    let scratch = adopt::Scratch::new(
        &staged,
        Some(&adopt::objects_of(&dest).unwrap()),
        None,
        "neo",
    )
    .unwrap();
    let meta = scratch.meta("neo").unwrap();
    let ledger: LandedWorkspace =
        landed::read(&landed::ledger_dir(&dest.join(".git"), "neo").join(landed::WORKSPACE_LANDED))
            .unwrap()
            .unwrap();
    assert_eq!(scratch.capture_index(&meta).unwrap(), ledger.index);
    assert_eq!(index_digest(&dest, None).unwrap(), ledger.index);
    match adopt::prove_adoptable(
        &dest,
        &dest.join(".git"),
        &scratch,
        &meta,
        true,
        &adopt_landing(&staged),
    )
    .unwrap()
    {
        adopt::Found::Equal(found) => {
            assert_eq!(found.head, ledger.head);
            assert_eq!(found.index, ledger.index);
            assert_eq!(found.exclude, ledger.exclude);
            assert_eq!(
                found
                    .seats
                    .iter()
                    .map(|seat| &seat.path)
                    .collect::<Vec<_>>(),
                ledger
                    .seats
                    .iter()
                    .map(|seat| &seat.path)
                    .collect::<Vec<_>>()
            );
        }
        adopt::Found::Differs(components) => panic!("{components:?}"),
    }
}

/// The adopt proof names every component that differs from the capture,
/// and an extra destination seat is no difference (finding 18).
#[test]
fn the_adopt_proof_names_each_differing_component() {
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    let scratch = adopt::Scratch::new(
        &staged,
        Some(&adopt::objects_of(&dest).unwrap()),
        None,
        "neo",
    )
    .unwrap();
    let meta = scratch.meta("neo").unwrap();
    let found = |dest: &Path| {
        adopt::prove_adoptable(
            dest,
            &dest.join(".git"),
            &scratch,
            &meta,
            true,
            &adopt_landing(&staged),
        )
        .unwrap()
    };
    fs::write(dest.join("extra"), b"the destination's own\n").unwrap();
    assert!(matches!(found(&dest), adopt::Found::Equal(_)));
    fs::write(dest.join("notes"), b"edited\n").unwrap();
    fs::write(dest.join(".git/info/exclude"), b"*.tmp\n").unwrap();
    git_run(&dest, &["switch", "--quiet", "-c", "elsewhere"]);
    git_run(&dest, &["add", "extra"]);
    let adopt::Found::Differs(components) = found(&dest) else {
        panic!("a changed destination proved equal");
    };
    assert_eq!(
        components,
        ["head", "index", "exclude", "seats=1", "seat=notes"]
    );
}

/// #226 item 6: only an scp-style or `ssh://` URL under a named prefix is
/// activated; a local path, HTTPS (its own rule) or a prefix match inside
/// a local path never.
#[test]
fn an_origin_prefix_activates_only_a_matching_remote_url() {
    use super::super::{activating_prefix, origin_prefix_allowed};
    for allowed in [
        "yoga:git/",
        "git@github.com:Jesssullivan/",
        "ssh://yoga/git/",
    ] {
        assert!(origin_prefix_allowed(allowed), "{allowed}");
    }
    for refused in [
        "",
        "/srv/git/",
        "./yoga:git/",
        "~/git:x",
        "https://github.com/",
        "file:///srv",
        "yoga git:",
        "yo/ga:git",
        // Review of 2026-10-16, finding 12: a prefix names its host whole.
        "ssh://yoga",
        "ssh:///git/",
    ] {
        assert!(!origin_prefix_allowed(refused), "{refused:?}");
    }
    let prefixes = [
        "yoga:git/".to_owned(),
        "git@github.com:Jesssullivan/".to_owned(),
    ];
    assert_eq!(
        activating_prefix("yoga:git/LA-Mesh", &prefixes),
        Some("yoga:git/")
    );
    assert_eq!(
        activating_prefix("git@github.com:Jesssullivan/bulkload.git", &prefixes),
        Some("git@github.com:Jesssullivan/")
    );
    for inert in [
        "yoga:other/LA-Mesh",
        "git@github.com:someone-else/x.git",
        "/srv/git/yoga:git/x",
        "https://yoga/git/x",
        "yoga:git/x\nmore",
    ] {
        assert_eq!(activating_prefix(inert, &prefixes), None, "{inert:?}");
    }
    // Review of 2026-10-16, finding 12: a prefix matches at a path
    // boundary only, never inside a host or an owner's name.
    let unterminated = [
        "git@github.com:Jesssullivan".to_owned(),
        "ssh://yoga/git".to_owned(),
    ];
    for inert in [
        "git@github.com:Jesssullivan-other/x.git",
        "ssh://yoga/gitlab/x",
        "ssh://yoga.example.net/git/x",
    ] {
        assert_eq!(activating_prefix(inert, &unterminated), None, "{inert:?}");
    }
    assert_eq!(
        activating_prefix("git@github.com:Jesssullivan/x.git", &unterminated),
        Some("git@github.com:Jesssullivan")
    );
    assert_eq!(
        activating_prefix("ssh://yoga/git/x", &unterminated),
        Some("ssh://yoga/git")
    );
}

/// Review of 2026-10-16, finding 12: a multi-valued origin activated by
/// two prefixes names both in the receipt file, not the last one only.
#[test]
fn every_matching_origin_prefix_is_named_in_the_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    git_run(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("f"), b"one\n").unwrap();
    git_run(&source, &["add", "f"]);
    git_run(&source, &["commit", "--quiet", "-m", "one"]);
    git_run(
        &source,
        &["config", "--add", "remote.origin.url", "yoga:git/a"],
    );
    git_run(
        &source,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "git@github.com:Jesssullivan/a",
        ],
    );
    let bundle = super::super::export_repository(&source, &root.join("capture")).unwrap();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    let dest = root.join("dest");
    let prefixes = [
        "yoga:git/".to_owned(),
        "git@github.com:Jesssullivan/".to_owned(),
    ];
    super::super::restore_staged_landed(
        &staged,
        &dest,
        "neo",
        (None, None),
        &adopt_landing(&staged),
        &prefixes,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(
            dest.join(".git/carry-config")
                .join(super::super::ORIGIN_PREFIX_RECEIPT)
        )
        .unwrap(),
        "yoga:git/\ngit@github.com:Jesssullivan/\n"
    );
}

/// T0 (#226): with no stash row, a destination reflog equal to the carried
/// order is adopted with no write; any other non-empty one is kept (T2).
#[test]
fn the_stash_table_adopts_an_equal_destination_reflog() {
    use super::super::landed::StashEntry;
    let carried = vec![
        StashEntry::new("a".repeat(40), "On main: one"),
        StashEntry::new("b".repeat(40), "On main: two"),
    ];
    assert_eq!(
        native_refs::stash_decision(&carried, None, &carried, 5),
        (None, Some(native_refs::STASH_ADOPTED))
    );
    // Equal commits under another message are not the carried stash
    // (OI-1003-Q169): kept, T2.
    let renamed = vec![
        StashEntry::new("a".repeat(40), "On main: renamed"),
        StashEntry::new("b".repeat(40), "On main: two"),
    ];
    assert_eq!(
        native_refs::stash_decision(&renamed, None, &carried, 5),
        (None, Some("stash-kept-destination"))
    );
    assert_eq!(
        native_refs::stash_decision(&[StashEntry::new("c".repeat(40), "x")], None, &carried, 5),
        (None, Some("stash-kept-destination"))
    );
}

// ---- review of 2026-10-16 (#226): adopt's guards, each made to bite --------

// adopt_world's landing made unlanded: its records removed, as a `cp -a`
// copy of a checkout bulkload never landed has none. Returns the ledger
// the landing had.
fn unlanded(dest: &Path) -> LandedWorkspace {
    let ledger: LandedWorkspace =
        landed::read(&landed::ledger_dir(&dest.join(".git"), "neo").join(landed::WORKSPACE_LANDED))
            .unwrap()
            .unwrap();
    fs::remove_dir_all(dest.join(".git/bulkload")).unwrap();
    ledger
}

// A11a at adopt_world's destination, as apply_workspace calls it.
fn adopt_at(staged: &super::super::StagedBundle, dest: &Path) -> Result<Applied> {
    let target = Target {
        repository: dest,
        workspace: Some(dest),
        source: "neo",
        base: None,
    };
    let place = Place {
        worktree: dest,
        base: None,
        git_dir: dest.join(".git"),
        common: dest.join(".git"),
        source: "neo",
        standalone: true,
    };
    adopt::workspace(staged, &target, &adopt_landing(staged), &place)
}

fn all_refs(repo: &Path) -> String {
    git_run(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// Finding 9 (mutation c1, `still` removed): a seat edit, a HEAD switch or
/// a staged hunk landing between the proof and the destination lock
/// refuses `GIT_DESTINATION_DIVERGED` with no ledger, marker or ref
/// written.
#[test]
fn adopt_rechecks_the_proof_under_the_lock() {
    for edit in ["seat", "head", "index"] {
        let (_dir, _source, bundle, dest) = adopt_world();
        let staged = super::super::stage_bundle(&bundle).unwrap();
        unlanded(&dest);
        let refs = all_refs(&dest);
        let at = dest.clone();
        super::super::mid_pass::arm_at(
            &dest,
            super::super::mid_pass::Stage::AdoptBeforeLock,
            move || match edit {
                "seat" => fs::write(at.join("notes"), b"edited!!!\n").unwrap(),
                "head" => {
                    git_run(&at, &["switch", "--quiet", "--detach"]);
                }
                _ => {
                    fs::write(at.join("extra"), b"staged\n").unwrap();
                    git_run(&at, &["add", "extra"]);
                }
            },
        );
        assert_eq!(
            adopt_at(&staged, &dest),
            Err(BulkloadRefusal::GitDestinationDiverged),
            "{edit}"
        );
        let records = landed::ledger_dir(&dest.join(".git"), "neo");
        for record in [
            landed::WORKSPACE_LANDED,
            landed::REFS_LANDED,
            landed::ADOPTED,
        ] {
            assert!(!records.join(record).exists(), "{edit}: {record} written");
        }
        assert_eq!(all_refs(&dest), refs, "{edit}: a ref was written");
    }
}

/// Finding 9 (mutation c4, the settled print not compared): a seat
/// rewritten after its bytes were read equal, before its print is taken
/// again, is no proof: the proof names it.
#[test]
fn the_adopt_proof_refuses_a_seat_that_moved_while_it_was_read() {
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    unlanded(&dest);
    let scratch = adopt::Scratch::new(
        &staged,
        Some(&adopt::objects_of(&dest).unwrap()),
        None,
        "neo",
    )
    .unwrap();
    let meta = scratch.meta("neo").unwrap();
    let at = dest.clone();
    // The first content seat read is `f` ("staged\n"): the same size,
    // other bytes.
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdoptSeatRead,
        move || {
            fs::write(at.join("f"), b"STAGED\n").unwrap();
        },
    );
    let found = adopt::prove_adoptable(
        &dest,
        &dest.join(".git"),
        &scratch,
        &meta,
        true,
        &adopt_landing(&staged),
    )
    .unwrap();
    assert_eq!(
        found,
        adopt::Found::Differs(vec!["seats=1".to_owned(), "seat=f".to_owned()])
    );
}

/// Finding 4: the adopted ledger's racy instant is taken before the
/// proof's first seat read (as Git's racy-index check takes it), so a seat
/// rewritten within its timestamp tick of being read stays racy for the
/// next converge, which then reads it by content.
#[test]
fn the_adopt_ledger_takes_its_racy_instant_before_the_first_read() {
    use std::sync::{Arc, Mutex};
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    unlanded(&dest);
    let scratch = adopt::Scratch::new(
        &staged,
        Some(&adopt::objects_of(&dest).unwrap()),
        None,
        "neo",
    )
    .unwrap();
    let meta = scratch.meta("neo").unwrap();
    let read_at = Arc::new(Mutex::new(0i128));
    let mark = Arc::clone(&read_at);
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdoptSeatRead,
        move || {
            *mark.lock().unwrap() = super::super::pass_start_ns();
        },
    );
    let adopt::Found::Equal(ledger) = adopt::prove_adoptable(
        &dest,
        &dest.join(".git"),
        &scratch,
        &meta,
        true,
        &adopt_landing(&staged),
    )
    .unwrap() else {
        panic!("the copy differs");
    };
    let first_read = *read_at.lock().unwrap();
    assert!(first_read > 0, "no seat was read");
    assert!(
        ledger.recorded_ns <= first_read,
        "recorded {} after the first read {first_read}",
        ledger.recorded_ns
    );
}

/// Finding 5: a workspace ledger another apply wrote while this one
/// waited for the destination lock is never overwritten: the adoption
/// refuses `GIT_DESTINATION_OCCUPIED`, the other ledger as it was.
#[test]
fn adopt_rechecks_under_the_lock_that_no_ledger_appeared() {
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    let mut other = unlanded(&dest);
    other.item = "another-plan's-item".to_owned();
    let path = landed::ledger_dir(&dest.join(".git"), "neo").join(landed::WORKSPACE_LANDED);
    let (at, theirs) = (dest.clone(), other.clone());
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdoptBeforeLock,
        move || {
            let directory = landed::ensure_dir(&at.join(".git"), "neo").unwrap();
            landed::write(&directory.join(landed::WORKSPACE_LANDED), &theirs).unwrap();
        },
    );
    assert_eq!(
        adopt_at(&staged, &dest),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    let now: LandedWorkspace = landed::read(&path).unwrap().unwrap();
    assert_eq!(now, other);
    assert!(!landed::ledger_dir(&dest.join(".git"), "neo")
        .join(landed::ADOPTED)
        .exists());
}

/// Finding 2(a): a ledger naming one of the run's own plan items is never
/// re-keyed, so two items of one plan never take a tree from each other;
/// a ledger of another plan, for the same capture, may be.
#[test]
fn a_rekey_never_takes_a_tree_from_an_item_of_the_same_plan() {
    let (_dir, _source, bundle, dest) = adopt_world();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    let ledger: LandedWorkspace =
        landed::read(&landed::ledger_dir(&dest.join(".git"), "neo").join(landed::WORKSPACE_LANDED))
            .unwrap()
            .unwrap();
    let landing = Landing {
        item: "this-item",
        observed_ns: 2,
        capture: ledger.capture,
    };
    let refusal = |items: &[String]| {
        adopt::rekey_refusal(
            &staged,
            &dest,
            &dest,
            (&dest.join(".git"), &dest.join(".git")),
            true,
            None,
            "neo",
            &ledger,
            &landing,
            items,
        )
        .unwrap()
    };
    assert_eq!(refusal(&[]), None);
    assert_eq!(
        refusal(&["this-item".to_owned(), ledger.item.clone()]),
        Some("item-in-plan")
    );
}
