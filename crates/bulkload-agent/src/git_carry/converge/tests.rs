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
        parents: None,
        exclude_union: None,
        beside: false,
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
        beside: false,
        advanced: None,
    };
    adopt::workspace(staged, &target, &adopt_landing(staged), &place, None)
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
        parents: None,
        exclude_union: None,
        beside: false,
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

// ---- `--advance-ancestor` (operator ruling 2026-10-19) ---------------------

// Git in `repo`, its success and stderr (a command that may refuse).
fn git_try(repo: &Path, args: &[&str]) -> (bool, String) {
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
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// Move `branch` of `repo` to a new empty commit on top of it without
// touching the index (no lock an advance holds stops it).
fn commit_beside_the_index(repo: &Path, branch: &str, message: &str) -> String {
    let tree = git_run(repo, &["rev-parse", &format!("{branch}^{{tree}}")]);
    let parent = git_run(repo, &["rev-parse", branch]);
    let commit = git_run(repo, &["commit-tree", &tree, "-p", &parent, "-m", message]);
    git_run(
        repo,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            &commit,
            &parent,
        ],
    );
    commit
}

// A source on `main` two commits ahead of a `cp -a` copy of itself at the
// first (the live checkout, clean, an ancestor), with a dirty edit; its
// capture, staged.
fn advance_world() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    super::super::StagedBundle,
) {
    world_of(false)
}

// `advance_world`, the source's two commits on `feature` when `switch`
// (operator ruling 2026-10-19, "Switch when clean"): the live checkout
// stays on `main`.
fn world_of(
    switch: bool,
) -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    super::super::StagedBundle,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    git_run(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(source.join("f"), b"one\n").unwrap();
    git_run(&source, &["add", "f"]);
    git_run(&source, &["commit", "--quiet", "-m", "one"]);
    let dest = root.join("dest");
    let copied = std::process::Command::new("cp")
        .arg("-a")
        .arg(&source)
        .arg(&dest)
        .status()
        .unwrap();
    assert!(copied.success());
    if switch {
        git_run(&source, &["switch", "--quiet", "-c", "feature"]);
    }
    fs::write(source.join("f"), b"two\n").unwrap();
    git_run(&source, &["commit", "--quiet", "-am", "two"]);
    fs::write(source.join("g"), b"new\n").unwrap();
    git_run(&source, &["add", "g"]);
    git_run(&source, &["commit", "--quiet", "-m", "three"]);
    fs::write(source.join("f"), b"dirty\n").unwrap();
    let bundle = super::super::export_repository(&source, &root.join("capture")).unwrap();
    let staged = super::super::stage_bundle(&bundle).unwrap();
    (dir, source, dest, staged)
}

// `--advance-ancestor` at the standalone live checkout `dest`, as
// apply_workspace calls it.
fn advance_at(staged: &super::super::StagedBundle, dest: &Path) -> Result<Applied> {
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
        beside: false,
        advanced: None,
    };
    adopt::workspace(
        staged,
        &target,
        &adopt_landing(staged),
        &place,
        Some(".neo-20261016"),
    )
}

/// The live branch moved between the advance proof and its
/// compare-and-swap: before the lock (the proof re-checked under it), or
/// after the intent (the compare-and-swap itself loses; a `git commit`
/// there cannot run, bulkload holding the index lock, so the branch moves
/// by `update-ref`). Either refuses `GIT_ADVANCE_RACED` with nothing
/// written: the branch holds the commit the operator made, the worktree
/// and the index are as they were, no intent, record, ledger or lock is
/// left.
#[test]
fn an_advance_whose_branch_moved_refuses_raced_with_nothing_written() {
    for stage in [
        super::super::mid_pass::Stage::AdvanceBeforeLock,
        super::super::mid_pass::Stage::AdvanceBeforeBranch,
    ] {
        let (_dir, _source, dest, staged) = advance_world();
        let at = dest.clone();
        super::super::mid_pass::arm_at(&dest, stage, move || {
            if stage == super::super::mid_pass::Stage::AdvanceBeforeLock {
                git_run(
                    &at,
                    &["commit", "--quiet", "--allow-empty", "-m", "sting's own"],
                );
            } else {
                commit_beside_the_index(&at, "main", "sting's own");
            }
        });
        assert_eq!(
            advance_at(&staged, &dest),
            Err(BulkloadRefusal::GitAdvanceRaced),
            "{stage:?}"
        );
        assert_eq!(
            git_run(&dest, &["log", "-1", "--format=%s", "main"]),
            "sting's own",
            "{stage:?}"
        );
        assert_eq!(git_run(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n", "{stage:?}");
        assert!(!dest.join("g").exists(), "{stage:?}");
        assert_eq!(git_run(&dest, &["status", "--porcelain"]), "", "{stage:?}");
        assert!(!dest.join(".git/index.lock").exists(), "{stage:?}");
        let records = landed::ledger_dir(&dest.join(".git"), "neo");
        for record in [
            advance::INTENT,
            advance::ADVANCED,
            landed::WORKSPACE_LANDED,
            landed::REFS_LANDED,
        ] {
            assert!(
                !records.join(record).exists(),
                "{stage:?}: {record} written"
            );
        }
    }
}

/// The advance itself, at the library seam: the branch moved by
/// compare-and-swap to the captured HEAD, the captured tree, its dirty edit
/// and index laid down, the advance record and the workspace ledger
/// written, no repository marker set (no native ref step), no intent left.
#[test]
fn an_advance_lands_the_capture_on_the_live_branch() {
    let (_dir, source, dest, staged) = advance_world();
    let applied = advance_at(&staged, &dest).unwrap();
    assert!(applied.advanced);
    let words = applied.custody.unwrap_or_default();
    assert!(words.split(' ').any(|word| word == "advanced=1"), "{words}");
    assert_eq!(
        git_run(&dest, &["rev-parse", "HEAD"]),
        git_run(&source, &["rev-parse", "HEAD"])
    );
    assert_eq!(git_run(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"dirty\n");
    assert_eq!(fs::read(dest.join("g")).unwrap(), b"new\n");
    assert_eq!(
        git_run(&dest, &["status", "--porcelain"]),
        git_run(&source, &["status", "--porcelain"])
    );
    let records = landed::ledger_dir(&dest.join(".git"), "neo");
    assert!(!records.join(advance::INTENT).exists());
    assert!(!records.join(landed::REFS_LANDED).exists());
    let ledger: LandedWorkspace = landed::read(&records.join(landed::WORKSPACE_LANDED))
        .unwrap()
        .unwrap();
    assert_eq!(ledger.item, "item");
    let record = advance::read_advanced(&records).unwrap().unwrap();
    assert_eq!(record.to, ledger.head);
    assert_eq!(record.from.symbolic.as_deref(), Some("refs/heads/main"));
}

/// Review of this lane, finding 6: HEAD switched to another branch in the
/// instant before the branch's compare-and-swap. Git cannot verify HEAD's
/// target in the transaction that moves the branch, so the advance checks
/// HEAD after it: the branch is put back by compare-and-swap, and the
/// advance refuses `GIT_ADVANCE_RACED` with nothing else written.
#[test]
fn an_advance_whose_head_switched_puts_the_branch_back() {
    let (_dir, _source, dest, staged) = advance_world();
    let main = git_run(&dest, &["rev-parse", "main"]);
    let at = dest.clone();
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdvanceBeforeBranch,
        move || {
            // `git switch` cannot run (bulkload holds the index lock);
            // HEAD moves by `symbolic-ref`.
            git_run(&at, &["branch", "other"]);
            git_run(&at, &["symbolic-ref", "HEAD", "refs/heads/other"]);
        },
    );
    assert_eq!(
        advance_at(&staged, &dest),
        Err(BulkloadRefusal::GitAdvanceRaced)
    );
    assert_eq!(git_run(&dest, &["rev-parse", "main"]), main);
    assert_eq!(
        git_run(&dest, &["symbolic-ref", "HEAD"]),
        "refs/heads/other"
    );
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n");
    assert!(!dest.join("g").exists());
    let records = landed::ledger_dir(&dest.join(".git"), "neo");
    assert!(!records.join(advance::INTENT).exists());
    assert!(!records.join(landed::WORKSPACE_LANDED).exists());
}

// Nothing of an advance or a switch at `dest`: HEAD on `branch` at `head`,
// `f` and the index as they were, no `g`, no lock, intent or ledger.
fn untouched(dest: &Path, (branch, head): (&str, &str), what: &str) {
    assert_eq!(git_run(dest, &["rev-parse", "HEAD"]), head, "{what}");
    assert_eq!(git_run(dest, &["symbolic-ref", "HEAD"]), branch, "{what}");
    assert_eq!(
        git_run(dest, &["diff", "--cached", "--name-only"]),
        "",
        "{what}"
    );
    assert!(!dest.join("g").exists(), "{what}");
    assert!(
        !dest.join(".git/index.lock").exists() || what.contains("lock"),
        "{what}"
    );
    let records = landed::ledger_dir(&dest.join(".git"), "neo");
    for record in [advance::INTENT, advance::ADVANCED, landed::WORKSPACE_LANDED] {
        assert!(!records.join(record).exists(), "{what}: {record}");
    }
}

/// Review of #289, finding 1: a Git lock held at the live checkout (an
/// `index.lock` or `HEAD.lock` another command holds) is an operation in
/// progress: `GIT_ADVANCE_NOT_CLEAN`, nothing moved, the lock as it was.
/// One taken between the proof and the destination lock refuses
/// `GIT_ADVANCE_RACED`, nothing moved either.
#[test]
fn a_held_git_lock_refuses_the_advance_with_nothing_moved() {
    for lock in ["index.lock", "HEAD.lock"] {
        let (_dir, _source, dest, staged) = advance_world();
        let head = git_run(&dest, &["rev-parse", "HEAD"]);
        fs::write(dest.join(".git").join(lock), b"another command's lock").unwrap();
        assert_eq!(
            advance_at(&staged, &dest),
            Err(BulkloadRefusal::GitAdvanceNotClean),
            "{lock}"
        );
        assert_eq!(
            fs::read(dest.join(".git").join(lock)).unwrap(),
            b"another command's lock"
        );
        fs::remove_file(dest.join(".git").join(lock)).unwrap();
        untouched(&dest, ("refs/heads/main", &head), lock);
        assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n");
    }
    let (_dir, _source, dest, staged) = advance_world();
    let head = git_run(&dest, &["rev-parse", "HEAD"]);
    let at = dest.clone();
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdvanceBeforeLock,
        move || fs::write(at.join(".git/index.lock"), b"taken since the proof").unwrap(),
    );
    assert_eq!(
        advance_at(&staged, &dest),
        Err(BulkloadRefusal::GitAdvanceRaced)
    );
    assert_eq!(
        fs::read(dest.join(".git/index.lock")).unwrap(),
        b"taken since the proof"
    );
    fs::remove_file(dest.join(".git/index.lock")).unwrap();
    untouched(&dest, ("refs/heads/main", &head), "taken since the proof");
}

/// Review of #289, finding 1: bulkload holds the live checkout's own
/// `index.lock` from its proof under the destination lock to the index
/// publish, so a live `git commit -a` cannot interleave (it refuses), and
/// the advance lands whole.
#[test]
fn a_commit_cannot_interleave_with_an_advance() {
    let (_dir, source, dest, staged) = advance_world();
    let at = dest.clone();
    let refused = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&refused);
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdvanceAfterBranch,
        move || {
            *seen.lock().unwrap() =
                Some(git_try(&at, &["commit", "-a", "--allow-empty", "-m", "x"]));
        },
    );
    let applied = advance_at(&staged, &dest).unwrap();
    assert!(applied.advanced);
    let (ran, stderr) = refused.lock().unwrap().take().unwrap();
    assert!(!ran && stderr.contains("index.lock"), "{stderr}");
    assert_eq!(
        git_run(&dest, &["rev-parse", "HEAD"]),
        git_run(&source, &["rev-parse", "HEAD"])
    );
    assert_eq!(fs::read(dest.join("g")).unwrap(), b"new\n");
    assert!(!dest.join(".git/index.lock").exists());
}

/// Review of #289, finding 1: a third party's edit met after the branch
/// moved (here an untracked `g` where the advance adds one, after `f` was
/// replaced) rolls the advance back: `f` back to the live HEAD's bytes,
/// the branch back by compare-and-swap, the lock released, the edit left
/// as it is, `GIT_ADVANCE_ROLLED_BACK` naming it. Never HEAD advanced over
/// the old index.
#[test]
fn an_edit_met_after_the_branch_moved_rolls_the_advance_back() {
    let (_dir, _source, dest, staged) = advance_world();
    let head = git_run(&dest, &["rev-parse", "HEAD"]);
    let at = dest.clone();
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdvanceAfterBranch,
        move || fs::write(at.join("g"), b"the operator's g\n").unwrap(),
    );
    assert_eq!(
        advance_at(&staged, &dest),
        Err(BulkloadRefusal::GitAdvanceRolledBack(b"g".to_vec()))
    );
    assert_eq!(git_run(&dest, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_run(&dest, &["rev-parse", "main"]), head);
    assert_eq!(git_run(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n");
    assert_eq!(fs::read(dest.join("g")).unwrap(), b"the operator's g\n");
    assert_eq!(
        git_run(&dest, &["status", "--porcelain"]),
        "?? g",
        "the index is the live HEAD's"
    );
    assert!(!dest.join(".git/index.lock").exists());
    let records = landed::ledger_dir(&dest.join(".git"), "neo");
    for record in [advance::INTENT, advance::ADVANCED, landed::WORKSPACE_LANDED] {
        assert!(!records.join(record).exists(), "{record}");
    }
}

/// The coordinator's decision on #289 (C19): a process whose working
/// directory is inside the live checkout refuses the advance
/// `GIT_ADVANCE_IN_USE`, nothing written.
#[test]
fn an_advance_refuses_while_a_process_is_inside() {
    let (_dir, _source, dest, staged) = advance_world();
    let head = git_run(&dest, &["rev-parse", "HEAD"]);
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .current_dir(&dest)
        .spawn()
        .unwrap();
    let refused = advance_at(&staged, &dest);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(refused, Err(BulkloadRefusal::GitAdvanceInUse));
    untouched(&dest, ("refs/heads/main", &head), "in use");
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n");
}

/// Operator ruling 2026-10-19, "Switch when clean": a clean live checkout
/// on `main`, the capture on `feature` (absent there). HEAD goes to
/// `feature`, created create-only at the captured HEAD, with the captured
/// seats and index; `main` stays exactly where it was.
#[test]
fn a_switch_lands_the_capture_on_its_branch_and_leaves_the_live_one() {
    let (_dir, source, dest, staged) = world_of(true);
    let main = git_run(&dest, &["rev-parse", "main"]);
    let applied = advance_at(&staged, &dest).unwrap();
    assert!(applied.advanced);
    let words = applied.custody.unwrap_or_default();
    assert!(
        words.split(' ').any(|word| word == "advance-switched=1"),
        "{words}"
    );
    assert_eq!(
        git_run(&dest, &["symbolic-ref", "HEAD"]),
        "refs/heads/feature"
    );
    assert_eq!(
        git_run(&dest, &["rev-parse", "feature"]),
        git_run(&source, &["rev-parse", "HEAD"])
    );
    assert_eq!(git_run(&dest, &["rev-parse", "main"]), main);
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"dirty\n");
    assert_eq!(fs::read(dest.join("g")).unwrap(), b"new\n");
    assert_eq!(
        git_run(&dest, &["status", "--porcelain"]),
        git_run(&source, &["status", "--porcelain"])
    );
    let record = advance::read_advanced(&landed::ledger_dir(&dest.join(".git"), "neo"))
        .unwrap()
        .unwrap();
    assert!(record.switched());
    assert_eq!(record.from.symbolic.as_deref(), Some("refs/heads/main"));
    assert!(!dest.join(".git/index.lock").exists());
}

/// A switch onto a `feature` the live repository holds at an ancestor of
/// the captured HEAD moves it by compare-and-swap; one at a commit of its
/// own refuses `GIT_SWITCH_BRANCH_DIVERGED`, nothing written.
#[test]
fn a_switch_advances_an_ancestor_branch_and_refuses_a_diverged_one() {
    let (_dir, source, dest, staged) = world_of(true);
    git_run(&dest, &["branch", "feature"]);
    advance_at(&staged, &dest).unwrap();
    assert_eq!(
        git_run(&dest, &["rev-parse", "feature"]),
        git_run(&source, &["rev-parse", "HEAD"])
    );
    let (_dir, _source, dest, staged) = world_of(true);
    let main = git_run(&dest, &["rev-parse", "main"]);
    git_run(&dest, &["branch", "feature"]);
    let own = commit_beside_the_index(&dest, "feature", "sting's feature");
    assert_eq!(
        advance_at(&staged, &dest),
        Err(BulkloadRefusal::GitSwitchBranchDiverged)
    );
    assert_eq!(git_run(&dest, &["rev-parse", "feature"]), own);
    untouched(&dest, ("refs/heads/main", &main), "diverged");
}

/// Each condition of the ruling a switch needs, refused typed
/// (`GIT_SWITCH_*`) with nothing written: a dirty checkout, an operation in
/// progress or a lock, a process inside it, the capture's branch checked
/// out elsewhere, an untracked file where the capture lands one.
#[test]
fn a_switch_refuses_each_condition_typed_with_nothing_written() {
    for shape in ["dirty", "merge", "lock", "busy", "collides", "in-use"] {
        let (dir, _source, dest, staged) = world_of(true);
        let main = git_run(&dest, &["rev-parse", "main"]);
        let mut child = None;
        let expected = match shape {
            "dirty" => {
                fs::write(dest.join("f"), b"sting's edit\n").unwrap();
                BulkloadRefusal::GitSwitchNotClean
            }
            "merge" => {
                fs::write(dest.join(".git/MERGE_HEAD"), format!("{main}\n")).unwrap();
                BulkloadRefusal::GitSwitchInProgress
            }
            "lock" => {
                fs::write(dest.join(".git/index.lock"), b"another command's lock").unwrap();
                BulkloadRefusal::GitSwitchInProgress
            }
            "busy" => {
                git_run(&dest, &["branch", "feature"]);
                let other = dir.path().join("other");
                git_run(
                    &dest,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        other.to_str().unwrap(),
                        "feature",
                    ],
                );
                BulkloadRefusal::GitSwitchBranchBusy
            }
            "collides" => {
                fs::write(dest.join("g"), b"sting's untracked g\n").unwrap();
                BulkloadRefusal::GitSwitchUntrackedCollides(b"g".to_vec())
            }
            _ => {
                child = Some(
                    std::process::Command::new("sleep")
                        .arg("60")
                        .current_dir(&dest)
                        .spawn()
                        .unwrap(),
                );
                BulkloadRefusal::GitSwitchInUse
            }
        };
        let refused = advance_at(&staged, &dest);
        if let Some(mut child) = child {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert_eq!(refused, Err(expected), "{shape}");
        assert_eq!(git_run(&dest, &["rev-parse", "HEAD"]), main, "{shape}");
        assert_eq!(
            git_run(&dest, &["symbolic-ref", "HEAD"]),
            "refs/heads/main",
            "{shape}"
        );
        if shape != "busy" {
            assert!(
                git_try(
                    &dest,
                    &["rev-parse", "--verify", "--quiet", "refs/heads/feature"]
                )
                .0
                .eq(&false),
                "{shape}: no branch created"
            );
        }
        if shape != "collides" {
            assert!(!dest.join("g").exists(), "{shape}");
        }
        let records = landed::ledger_dir(&dest.join(".git"), "neo");
        assert!(!records.join(advance::INTENT).exists(), "{shape}");
        assert!(!records.join(landed::WORKSPACE_LANDED).exists(), "{shape}");
    }
}

/// A switch whose seats meet an edit is rolled back as an advance is: HEAD
/// back on `main`, the `feature` it created deleted by compare-and-swap,
/// `GIT_SWITCH_ROLLED_BACK`.
#[test]
fn a_switch_met_by_an_edit_is_rolled_back() {
    let (_dir, _source, dest, staged) = world_of(true);
    let main = git_run(&dest, &["rev-parse", "main"]);
    let at = dest.clone();
    super::super::mid_pass::arm_at(
        &dest,
        super::super::mid_pass::Stage::AdvanceAfterBranch,
        move || fs::write(at.join("g"), b"the operator's g\n").unwrap(),
    );
    assert_eq!(
        advance_at(&staged, &dest),
        Err(BulkloadRefusal::GitSwitchRolledBack(b"g".to_vec()))
    );
    assert_eq!(git_run(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(git_run(&dest, &["rev-parse", "HEAD"]), main);
    assert!(
        !git_try(
            &dest,
            &["rev-parse", "--verify", "--quiet", "refs/heads/feature"]
        )
        .0
    );
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"one\n");
    assert!(!dest.join(".git/index.lock").exists());
}
