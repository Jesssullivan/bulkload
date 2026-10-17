//! M2 item 6 (2026-10-16), the `GIT_INVENTORY_MALFORMED` half: on neo the
//! main checkout of a normal-looking repository with 28 linked worktrees
//! (telefreeze) refused `GIT_INVENTORY_MALFORMED` at capture, with the lane's
//! binary of that night (5beb0314900f). Its stderr and the repository were
//! not readable from the destination, so this pins the shapes it could have
//! held, as a regression row: a main with 28 linked worktrees, each on its
//! own branch with a commit of its own, plus a detached one, a locked one, a
//! prunable one whose directory is gone, per-worktree refs
//! (`refs/bisect/*`, `refs/worktree/*`), a stash and a staged file in a
//! worktree, `extensions.worktreeConfig` with a per-worktree key, worktrees
//! nested in the main checkout (`.claude/worktrees`, `.worktrees`), two
//! worktrees whose administration names collide, an orphan branch in a
//! worktree, and packed annotated tags. Planned as telefreeze was (the main
//! and every linked worktree as items), every item captures and lands. Run
//! against the night's binary it also captures, so this does not
//! reproduce telefreeze: it is a regression row for these shapes. The
//! evidence still needed is listed in
//! `docs/agent-notes/2026-10-16-partial-clone-carry-design.md`.
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
#[allow(dead_code)]
mod gftb;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gftb::{git, settle};

fn agent(args: &[OsString]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `(source, outcome)` of every receipt line.
fn outcomes(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter(|line| line.starts_with("item=") && line.contains(" outcome="))
        .map(|line| {
            let field = |key: &str| {
                let start = line.find(key).unwrap() + key.len();
                line[start..]
                    .split(" outcome=")
                    .next()
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned()
            };
            (field(" source="), field(" outcome="))
        })
        .collect()
}

/// Build the source repository at `root/src/tele` and return its linked
/// worktrees that still exist (the main is not in the list).
fn telefreeze_like(root: &Path) -> Vec<PathBuf> {
    let main = root.join("src/tele");
    fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "--quiet", "--template="]);
    fs::write(main.join("a"), b"a\n").unwrap();
    fs::write(main.join(".gitignore"), b".claude/\n.worktrees/\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "-m", "c0"]);
    git(&main, &["tag", "-a", "-m", "release", "v1"]);
    git(&main, &["tag", "light"]);
    let side = root.join("src/tele.worktrees");
    let mut linked = Vec::new();
    for index in 1..=28 {
        let path = side.join(format!("wt-{index}"));
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &format!("feat/wt-{index}"),
                path.to_str().unwrap(),
            ],
        );
        fs::write(path.join(format!("f{index}")), format!("{index}\n")).unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "--quiet", "-m", &format!("c{index}")]);
        linked.push(path);
    }
    let detached = side.join("detached");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            detached.to_str().unwrap(),
            "HEAD",
        ],
    );
    linked.push(detached);
    let locked = side.join("locked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "locked",
            locked.to_str().unwrap(),
        ],
    );
    git(
        &main,
        &[
            "worktree",
            "lock",
            "--reason",
            "usb disk",
            locked.to_str().unwrap(),
        ],
    );
    linked.push(locked);
    let gone = side.join("gone");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "gone",
            gone.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&gone).unwrap();
    let first = side.join("wt-1");
    git(&first, &["update-ref", "refs/bisect/bad", "HEAD"]);
    git(&first, &["update-ref", "refs/worktree/mine", "HEAD"]);
    let second = side.join("wt-2");
    fs::write(second.join("a"), b"dirty\n").unwrap();
    git(&second, &["stash", "--quiet"]);
    fs::write(second.join("staged"), b"staged\n").unwrap();
    git(&second, &["add", "staged"]);
    git(&main, &["config", "extensions.worktreeConfig", "true"]);
    git(
        &side.join("wt-3"),
        &["config", "--worktree", "core.sparseCheckout", "false"],
    );
    for (rel, branch) in [
        (".claude/worktrees/agent-x", "agent-x"),
        (".worktrees/inner-y", "inner-y"),
    ] {
        let path = main.join(rel);
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        linked.push(path);
    }
    for (parent, branch) in [("a", "collide-a"), ("b", "collide-b")] {
        let path = root.join("src").join(parent).join("same");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        linked.push(path);
    }
    let orphan = side.join("orphan");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "doomed",
            orphan.to_str().unwrap(),
        ],
    );
    git(&orphan, &["checkout", "--quiet", "--orphan", "newroot"]);
    git(&orphan, &["commit", "--quiet", "-m", "orphan root"]);
    linked.push(orphan);
    git(&main, &["pack-refs", "--all"]);
    linked
}

#[test]
fn a_main_with_many_linked_worktrees_captures_and_lands() {
    let dir = tempfile::Builder::new()
        .prefix("bulkload-many-worktrees-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let linked = telefreeze_like(&root);
    let main = root.join("src/tele");
    let dest = |source: &Path| {
        root.join("dst")
            .join(source.strip_prefix(root.join("src")).unwrap())
    };
    let plan = root.join("plan.postcard");
    let mut add: Vec<OsString> = vec!["estate-add-batch".into(), plan.clone().into()];
    for (source, workspace) in
        std::iter::once((&main, dest(&main))).chain(linked.iter().map(|path| (path, dest(path))))
    {
        add.push(source.clone().into());
        add.push(dest(&main).into());
        add.push(workspace.into());
    }
    let (ok, _, err) = agent(&add);
    assert!(ok, "{err}");
    settle();
    let (ok, stdout, stderr) = agent(&[
        "estate-capture".into(),
        plan.clone().into(),
        root.join("private").into(),
        root.join("corpus").into(),
        "2".into(),
    ]);
    let rows = outcomes(&stdout);
    assert_eq!(rows.len(), linked.len() + 1, "{stdout}");
    assert!(
        rows.iter().all(|(_, outcome)| outcome == "captured"),
        "{stdout}\n{stderr}"
    );
    assert!(ok, "{stderr}");
    let state = root.join("applied");
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    }
    let (ok, stdout, stderr) = agent(&[
        "estate-apply".into(),
        plan.into(),
        root.join("corpus").into(),
        state.into(),
        "neo".into(),
        "1".into(),
    ]);
    assert!(ok, "{stdout}\n{stderr}");
    let rows = outcomes(&stdout);
    assert!(
        rows.iter()
            .all(|(_, outcome)| outcome == "workspace-restored"),
        "{stdout}"
    );
    assert_eq!(
        git(&dest(&main), &["rev-parse", "HEAD"]),
        git(&main, &["rev-parse", "HEAD"])
    );
    for path in &linked {
        assert_eq!(
            git(&dest(path), &["rev-parse", "HEAD"]),
            git(path, &["rev-parse", "HEAD"]),
            "{}",
            path.display()
        );
    }
    // The stash and the staged entry of wt-2 land with it.
    let landed = dest(&root.join("src/tele.worktrees/wt-2"));
    assert_eq!(
        git(&landed, &["rev-parse", "refs/stash"]),
        git(
            &root.join("src/tele.worktrees/wt-2"),
            &["rev-parse", "refs/stash"]
        )
    );
    assert!(git(&landed, &["diff", "--cached", "--name-only"]).contains("staged"));
    // Every branch of the source is a native branch of the landing.
    assert_eq!(
        git(
            &dest(&main),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads"
            ]
        ),
        git(
            &main,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads"
            ]
        )
    );
}
