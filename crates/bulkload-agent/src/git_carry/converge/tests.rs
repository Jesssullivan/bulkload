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
    let plan = head_plan(None, &h1, &native(None), None, &none, 5);
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.intended, None);
    // Checked out by another worktree (finding 1): detach and record.
    let others: BTreeSet<String> = [branch.to_owned()].into();
    let plan = head_plan(Some(branch), &h1, &native(Some(&h1)), None, &others, 5);
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
        &native(Some(&x)),
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
        &native(Some(&x)),
        Some(&ledger),
        &none,
        1,
    );
    assert_eq!(plan.head.symbolic, None);
    assert_eq!(plan.line, Some("head-detached-ref-newer"));
    // A foreign repository: an absent branch is created, a matching one
    // used, any other detaches (finding 11: never a refusal).
    let plan = head_plan(Some(branch), &h1, &native(None), None, &none, 5);
    assert_eq!(plan.branch, Some((branch.to_owned(), None, h1.clone())));
    let plan = head_plan(Some(branch), &h1, &native(Some(&h1)), None, &none, 5);
    assert_eq!(
        (plan.head.symbolic.as_deref(), plan.branch),
        (Some(branch), None)
    );
    let plan = head_plan(Some(branch), &h1, &native(Some(&x)), None, &none, 5);
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
