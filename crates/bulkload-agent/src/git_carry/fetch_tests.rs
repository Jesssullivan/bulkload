//! gc-safe fetch by default (TIN-7492; operator 2026-10-27, Linear TIN-3692
//! `ae17f1bd`). Every Git fetch the agent runs is built by
//! [`super::git_fetch`]: `-c gc.auto=0 -c maintenance.auto=false` from
//! `git_env` before the subcommand, and `--no-prune` with the other
//! [`super::FETCH_FLAGS`] after it. The source command registry pins
//! `fetch` to that one builder. These tests pin its argv and show that a
//! fetch on a repository configured to prune, gc and run maintenance leaves
//! every ref and object in place.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::{git, git_fetch, FETCH_FLAGS};

fn fresh(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "bulkload-fetch-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir(&root).unwrap();
    fs::canonicalize(root).unwrap()
}

// A fixture Git child through the hardened builder, with a test identity.
fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
    let result = git(repo)
        .args(["-c", "user.name=Bulkload test"])
        .args(["-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn args_of(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// Every file under `dir`, relative to it.
fn files(dir: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                found.insert(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    found
}

#[test]
fn every_fetch_runs_with_gc_and_maintenance_off_and_never_prunes() {
    let mut command = git(Path::new("/nonexistent/repository"));
    git_fetch(&mut command);
    let args = args_of(&command);
    let fetch = args.iter().position(|arg| arg == "fetch").unwrap();
    let before = &args[..fetch];
    for value in ["gc.auto=0", "maintenance.auto=false"] {
        assert!(
            before
                .windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == value),
            "-c {value} before fetch in {args:?}"
        );
    }
    // Pinned by value: a change here is a change of the ruled contract.
    assert_eq!(
        &args[fetch + 1..],
        [
            "--no-prune",
            "--no-write-fetch-head",
            "--no-auto-gc",
            "--no-auto-maintenance",
            "--no-recurse-submodules",
        ]
    );
    assert_eq!(&args[fetch + 1..], FETCH_FLAGS);
    // No later `-c` turns gc, maintenance or pruning back on.
    for pair in args.windows(2).filter(|pair| pair[0] == "-c") {
        let key = pair[1].split('=').next().unwrap().to_ascii_lowercase();
        assert!(
            !matches!(
                key.as_str(),
                "fetch.prune" | "fetch.prunetags" | "gc.autodetach" | "remote.origin.prune"
            ),
            "{args:?}"
        );
    }
}

#[test]
fn a_fetch_into_a_repository_set_to_prune_and_gc_keeps_every_ref_and_object() {
    let source = fresh("source");
    g(&source, &["init", "-q", "-b", "main"]);
    fs::write(source.join("a.txt"), "one\n").unwrap();
    g(&source, &["add", "a.txt"]);
    g(&source, &["commit", "-q", "-m", "one"]);
    g(&source, &["branch", "gone"]);
    g(&source, &["branch", "keep"]);
    let spec = "+refs/heads/*:refs/remotes/origin/*";
    let url = source.to_str().unwrap();

    // The destination asks for everything this fetch must not do: prune on
    // every fetch, gc (with cruft packs) after every object, and maintenance
    // after every command.
    let destination = fresh("destination");
    g(&destination, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("fetch.prune", "true"),
        ("fetch.pruneTags", "true"),
        ("gc.auto", "1"),
        ("gc.autoPackLimit", "1"),
        ("gc.autoDetach", "false"),
        ("gc.cruftPacks", "true"),
        ("maintenance.auto", "true"),
    ] {
        g(&destination, &["config", key, value]);
    }
    let first = git_fetch(&mut git(&destination))
        .args(["--quiet", url, spec])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    // Loose objects nothing reaches, and loose objects only an index entry
    // reaches: what a parked admin dir or a reflog may be the last to hold.
    let mut unreachable = Vec::new();
    for index in 0..40 {
        let path = destination.join(format!("blob-{index}"));
        fs::write(&path, format!("unreachable {index}\n")).unwrap();
        let id = g(&destination, &["hash-object", "-w", path.to_str().unwrap()]);
        unreachable.push(String::from_utf8(id).unwrap().trim().to_owned());
        fs::remove_file(path).unwrap();
    }
    let git_dir = destination.join(".git");
    let objects_before = files(&git_dir.join("objects"));

    // The source loses a branch and gains a commit.
    g(&source, &["branch", "-D", "gone"]);
    fs::write(source.join("a.txt"), "two\n").unwrap();
    g(&source, &["commit", "-q", "-am", "two"]);

    let second = git_fetch(&mut git(&destination))
        .args(["--quiet", url, spec])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let refs = String::from_utf8(g(
        &destination,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/",
        ],
    ))
    .unwrap();
    assert!(
        refs.lines().any(|name| name == "refs/remotes/origin/gone"),
        "{refs}"
    );
    assert!(
        refs.lines().any(|name| name == "refs/remotes/origin/keep"),
        "{refs}"
    );
    let main =
        String::from_utf8(g(&destination, &["rev-parse", "refs/remotes/origin/main"])).unwrap();
    let source_main = String::from_utf8(g(&source, &["rev-parse", "main"])).unwrap();
    assert_eq!(main, source_main);
    for id in &unreachable {
        g(&destination, &["cat-file", "-e", id]);
    }
    let objects_after = files(&git_dir.join("objects"));
    let lost: Vec<_> = objects_before.difference(&objects_after).collect();
    assert!(lost.is_empty(), "objects removed by the fetch: {lost:?}");
    let added: Vec<_> = objects_after.difference(&objects_before).collect();
    for path in &added {
        let name = path.to_string_lossy();
        assert!(
            !name.ends_with(".mtimes") && !name.contains("multi-pack-index"),
            "the fetch wrote {name}"
        );
    }
    for name in ["gc.log", "gc.pid", "FETCH_HEAD"] {
        assert!(!git_dir.join(name).exists(), "the fetch left {name}");
    }

    // The fixture is real: the same fetch without the builder's flags
    // prunes the branch the source lost (repository-local fetch.prune).
    let control = fresh("control");
    g(&control, &["init", "-q", "-b", "main"]);
    g(&control, &["config", "fetch.prune", "true"]);
    g(&control, &["remote", "add", "origin", url]);
    g(&control, &["fetch", "--quiet", "origin"]);
    g(
        &control,
        &["update-ref", "refs/remotes/origin/gone", source_main.trim()],
    );
    g(&control, &["fetch", "--quiet", "origin"]);
    let control_refs = String::from_utf8(g(
        &control,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/",
        ],
    ))
    .unwrap();
    assert!(
        !control_refs
            .lines()
            .any(|name| name == "refs/remotes/origin/gone"),
        "{control_refs}"
    );

    for root in [source, destination, control] {
        fs::remove_dir_all(root).unwrap();
    }
}
