//! `estate-carry-files` (M2r2 b) through the binary: a planned item whose
//! capture refused is carried byte-exact as files by a recorded transfer,
//! and `closure-report` counts it `carried`.
//!
//! The capture refusal is made the way one of the in-place run's items
//! refused: a checkout with no index (`GIT_INVENTORY_INDEX_ABSENT`), so the
//! apply records `CAPTURE_ABSENT`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const SOURCE: &str = "neo";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn agent(args: &[OsString]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("--min-free-percent=0")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    Ran {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn os(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|item| item.as_ref().to_owned()).collect()
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
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
}

struct World {
    base: PathBuf,
    source: PathBuf,
    workspace: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

impl World {
    /// A source checkout with no index, planned into `dst/repo`; captured
    /// (refused) and applied (`CAPTURE_ABSENT`).
    fn new(tag: &str, shape: impl FnOnce(&Path, &Path)) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-carried-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let base = fs::canonicalize(&base).unwrap();
        let source = base.join("src/repo");
        fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(source.join("a.txt"), b"alpha\n").unwrap();
        fs::create_dir(source.join("d")).unwrap();
        fs::write(source.join("d/b.txt"), b"beta\n").unwrap();
        fs::write(source.join("d/run.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(source.join("d/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", source.join("link")).unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "c0"]);
        fs::write(source.join("untracked.txt"), b"only here\n").unwrap();
        shape(&base, &source);
        // The refusal: a checkout with no index.
        fs::remove_file(source.join(".git/index")).unwrap();
        fs::create_dir(base.join("dst")).unwrap();
        let world = Self {
            workspace: base.join("dst/repo"),
            source,
            base,
        };
        let plan = world.path("plan");
        assert!(
            agent(&os(&[
                &"estate-add",
                &plan,
                &world.source,
                &world.workspace,
                &world.workspace
            ]))
            .ok
        );
        let captured = agent(&os(&[
            &"estate-capture",
            &plan,
            &world.path("capture-state"),
            &world.path("corpus"),
            &"1",
        ]));
        // Refused (the index, or a shape a test adds, decides which code).
        assert!(
            captured.stdout.contains("outcome=refused"),
            "{}{}",
            captured.stdout,
            captured.stderr
        );
        let applied = world.apply();
        assert!(
            applied.stdout.contains("CAPTURE_ABSENT"),
            "{}{}",
            applied.stdout,
            applied.stderr
        );
        world
    }

    fn path(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    fn apply(&self) -> Ran {
        agent(&os(&[
            &"estate-apply",
            &self.path("plan"),
            &self.path("corpus"),
            &self.path("state"),
            &SOURCE,
            &"1",
        ]))
    }

    fn carry(&self, extra: &[&str]) -> Ran {
        let mut args: Vec<OsString> = vec!["estate-carry-files".into()];
        args.extend(extra.iter().map(OsString::from));
        args.extend(os(&[
            &self.path("plan"),
            &self.path("corpus"),
            &self.path("state"),
            &SOURCE,
            &self.path("carry"),
        ]));
        agent(&args)
    }

    fn closure(&self) -> String {
        let ran = agent(&os(&[
            &"closure-report",
            &self.path("plan"),
            &self.path("corpus"),
            &SOURCE,
            &self.path("state"),
        ]));
        ran.stdout
    }
}

/// Every entry below `root` (`.git` included): kind, mode, bytes or link
/// target. What a byte-exact carry must reproduce.
fn tree(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let body = if meta.is_symlink() {
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_dir() {
                pending.push(path);
                Vec::new()
            } else {
                fs::read(&path).unwrap()
            };
            out.insert(rel, (meta.mode(), body));
        }
    }
    out
}

fn stamps(root: &Path) -> BTreeMap<PathBuf, (u64, i64)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                pending.push(entry.path());
            }
            out.insert(entry.path(), (meta.ino(), meta.mtime_nsec()));
        }
    }
    out
}

#[test]
fn a_capture_refused_item_is_carried_and_closure_counts_it() {
    let world = World::new("carry", |_, source| {
        // An inert pointer (a pruned worktree's copy) and one into the
        // carried repository's own administration.
        fs::create_dir_all(source.join("archive/old")).unwrap();
        fs::write(
            source.join("archive/old/.git"),
            b"gitdir: /nonexistent/repo/.git/worktrees/old\n",
        )
        .unwrap();
        fs::create_dir_all(source.join(".git/worktrees/inner")).unwrap();
        fs::create_dir_all(source.join("inner")).unwrap();
        fs::write(
            source.join("inner/.git"),
            b"gitdir: ../.git/worktrees/inner\n",
        )
        .unwrap();
    });
    let before = world.closure();
    assert!(
        before.contains("\"refusal\":\"CAPTURE_ABSENT\""),
        "{before}"
    );
    assert!(before.contains("\"gate\":\"fail\""), "{before}");

    // A dry run reads only.
    let state_before = stamps(&world.path("state"));
    let dry = world.carry(&["--dry-run"]);
    assert!(dry.ok, "{}{}", dry.stdout, dry.stderr);
    assert!(dry.stdout.contains(" now=absent "), "{}", dry.stdout);
    assert!(dry.stdout.contains(" would=copy "), "{}", dry.stdout);
    assert!(!world.workspace.exists());
    assert!(!world.path("carry").exists());
    assert_eq!(stamps(&world.path("state")), state_before);

    let ran = world.carry(&[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains(" outcome=carried-as-files "),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains(" gits=3"), "{}", ran.stdout);
    assert_eq!(tree(&world.workspace), tree(&world.source));

    let report = world.closure();
    assert!(
        report.contains("\"schema\":\"bulkload.closure.v5\""),
        "{report}"
    );
    assert!(
        report.contains("\"verdict\":\"pass\",\"gate\":\"pass\""),
        "{report}"
    );
    assert!(report.contains("\"carried\":1,"), "{report}");
    assert!(report.contains("\"disposition\":\"carried\""), "{report}");
    assert!(report.contains("\"beside\":false"), "{report}");

    // A re-apply keeps the carry; a rerun of the carry adopts everything.
    let reapplied = world.apply();
    assert!(
        reapplied.stdout.contains("outcome=carried-as-files"),
        "{}{}",
        reapplied.stdout,
        reapplied.stderr
    );
    let landed = stamps(&world.workspace);
    let again = world.carry(&[]);
    assert!(again.ok, "{}{}", again.stdout, again.stderr);
    assert!(
        again.stdout.contains(" outcome=carried-as-files "),
        "{}",
        again.stdout
    );
    assert_eq!(
        stamps(&world.workspace),
        landed,
        "a rerun rewrote landed bytes"
    );
    assert!(world.closure().contains("\"disposition\":\"carried\""));

    // The proof is the transfer record's bytes: a changed record unproves it.
    let records = world.path("carry").join(
        fs::read_dir(world.path("carry"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name(),
    );
    let record_dir = records.join("dest/transfer.closure.d");
    let record = fs::read_dir(&record_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "closure"))
        .unwrap();
    let mut bytes = fs::read(&record).unwrap();
    bytes.push(0);
    fs::write(&record, &bytes).unwrap();
    let tampered = world.closure();
    assert!(
        tampered.contains("\"unaccounted_reason\":\"carried-transfer-changed\""),
        "{tampered}"
    );
}

#[test]
fn beside_lands_next_to_an_occupied_workspace() {
    let world = World::new("beside", |_, _| {});
    fs::create_dir(&world.workspace).unwrap();
    fs::write(world.workspace.join("a.txt"), b"the live checkout's own\n").unwrap();
    let live = tree(&world.workspace);
    let ran = world.carry(&["--beside", ".neo-20261016"]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    let beside = world.path("dst/repo.neo-20261016");
    assert_eq!(tree(&beside), tree(&world.source));
    assert_eq!(tree(&world.workspace), live);
    let report = world.closure();
    assert!(report.contains("\"beside\":true"), "{report}");
    assert!(report.contains("\"disposition\":\"carried\""), "{report}");
}

#[test]
fn an_equal_destination_is_adopted_and_a_differing_file_is_never_overwritten() {
    // Equal: a copy the lane made by hand is adopted with nothing written.
    let world = World::new("equal", |_, _| {});
    let copied = Command::new("cp")
        .arg("-a")
        .arg(&world.source)
        .arg(&world.workspace)
        .status()
        .unwrap();
    assert!(copied.success());
    let ran = world.carry(&[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains(" created=false "), "{}", ran.stdout);
    assert_eq!(tree(&world.workspace), tree(&world.source));
    assert!(world.closure().contains("\"disposition\":\"carried\""));

    // Differing: refused, never overwritten, not carried.
    let world = World::new("differs", |_, _| {});
    fs::create_dir(&world.workspace).unwrap();
    fs::write(world.workspace.join("a.txt"), b"destination's own bytes\n").unwrap();
    let ran = world.carry(&[]);
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains(" carried=incomplete "),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("first_refused=\"a.txt\""),
        "{}",
        ran.stdout
    );
    assert_eq!(
        fs::read(world.workspace.join("a.txt")).unwrap(),
        b"destination's own bytes\n"
    );
    let report = world.closure();
    assert!(
        report.contains("\"refusal\":\"CAPTURE_ABSENT\""),
        "{report}"
    );
    assert!(!report.contains("\"disposition\":\"carried\""), "{report}");
}

#[test]
fn a_pointer_that_would_attach_elsewhere_refuses_typed() {
    let world = World::new("attached", |base, source| {
        // A pointer naming an administration that exists on this host,
        // outside the carried tree.
        let other = base.join("other");
        fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "--quiet", "--template="]);
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(
            source.join("nested/.git"),
            format!("gitdir: {}\n", other.join(".git").display()),
        )
        .unwrap();
    });
    let ran = world.carry(&[]);
    assert!(!ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout
            .contains("GIT_CARRY_POINTER_ATTACHED path=\"nested/.git\""),
        "{}",
        ran.stdout
    );
    let report = world.closure();
    assert!(
        report.contains("\"refusal\":\"GIT_CARRY_POINTER_ATTACHED\""),
        "{report}"
    );
    // The landed bytes are left for review, exactly as the source's.
    assert_eq!(tree(&world.workspace), tree(&world.source));
}

#[test]
fn only_capture_refused_workspace_items_are_eligible() {
    let world = World::new("eligible", |_, _| {});
    // A second item whose capture succeeds and applies.
    let good = world.path("src/good");
    fs::create_dir_all(&good).unwrap();
    git(&good, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(good.join("g.txt"), b"g\n").unwrap();
    git(&good, &["add", "."]);
    git(&good, &["commit", "--quiet", "-m", "g"]);
    let good_dst = world.path("dst/good");
    assert!(
        agent(&os(&[
            &"estate-add",
            &world.path("plan"),
            &good,
            &good_dst,
            &good_dst
        ]))
        .ok
    );
    assert!(agent(&os(&[
        &"estate-capture",
        &world.path("plan"),
        &world.path("capture-state"),
        &world.path("corpus"),
        &"1",
    ]))
    .stdout
    .contains("outcome=captured"));
    let _ = world.apply();
    let ran = world.carry(&["--dry-run"]);
    assert!(ran.stdout.contains(" skipped=captured"), "{}", ran.stdout);
    assert!(ran.stdout.contains(" would=copy "), "{}", ran.stdout);
}

#[test]
fn a_refs_only_item_rides_on_its_carried_sibling() {
    // The in-place plan's shape: one source planned twice, refs-only into
    // the live repository and as a beside workspace.
    let world = World::new("refs-only", |_, _| {});
    let live = world.path("dst/live");
    assert!(
        agent(&os(&[
            &"estate-add",
            &world.path("plan"),
            &world.source,
            &live
        ]))
        .ok
    );
    let _ = agent(&os(&[
        &"estate-capture",
        &world.path("plan"),
        &world.path("capture-state"),
        &world.path("corpus"),
        &"1",
    ]));
    let applied = world.apply();
    assert_eq!(
        applied.stdout.matches("CAPTURE_ABSENT").count(),
        2,
        "{}",
        applied.stdout
    );
    let dry = world.carry(&["--dry-run"]);
    assert!(
        dry.stdout.contains(" refs-only would=via "),
        "{}",
        dry.stdout
    );
    let ran = world.carry(&[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains(" refs-only destination="),
        "{}",
        ran.stdout
    );
    assert!(
        !live.exists(),
        "a refs-only carry writes nothing at its repository"
    );
    let report = world.closure();
    assert!(report.contains("\"carried\":2,"), "{report}");
    assert!(report.contains("\"gate\":\"pass\""), "{report}");
    assert!(report.contains("\"via\":\""), "{report}");
}

#[test]
fn bind_proves_an_earlier_recorded_session_without_writing() {
    let world = World::new("bind", |_, _| {});
    // The lane's way: a plain recorded `copy` into the destination.
    fs::create_dir(&world.workspace).unwrap();
    let lane_state = world.path("lane-state");
    let copied = agent(&os(&[
        &"copy",
        &world.source,
        &world.workspace,
        &world.path("lane-source-state"),
        &lane_state,
    ]));
    assert!(copied.ok, "{}{}", copied.stdout, copied.stderr);
    let item = fs::read_dir(world.path("state"))
        .unwrap()
        .find_map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .strip_suffix(".outcome")
                .map(str::to_owned)
        })
        .unwrap();
    // No record in an empty state: refused, nothing recorded.
    let empty = world.path("empty-state");
    fs::create_dir(&empty).unwrap();
    let missing = world.carry(&["--bind", &format!("{item}={}", empty.display())]);
    assert!(!missing.ok, "{}", missing.stdout);
    assert!(world.closure().contains("\"refusal\":\"CAPTURE_ABSENT\""));
    let bind = format!("{item}={}", lane_state.display());
    let dry = world.carry(&["--dry-run", "--bind", &bind]);
    assert!(
        dry.stdout.contains(" would=bind copy ")
            && dry.stdout.contains(" session=completed refused_paths=0"),
        "{}",
        dry.stdout
    );
    let landed = stamps(&world.workspace);
    let ran = world.carry(&["--bind", &bind]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains(" bound=true "), "{}", ran.stdout);
    assert_eq!(
        stamps(&world.workspace),
        landed,
        "a bind wrote under the destination"
    );
    assert!(!world.path("carry").join(&item).exists());
    let report = world.closure();
    assert!(report.contains("\"disposition\":\"carried\""), "{report}");
    assert!(report.contains("lane-state"), "{report}");
}

/// A second repository outside the carried tree, and its `.git`.
fn other_repository(base: &Path) -> PathBuf {
    let other = base.join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(other.join("o.txt"), b"other\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "--quiet", "-m", "o"]);
    other.join(".git")
}

#[test]
fn every_way_git_would_reach_outside_refuses_attached() {
    type Shape = fn(&Path, &Path, &Path);
    let shapes: [(&str, Shape, &str); 4] = [
        // A `.git` symlink to a gitfile that names an outside repository.
        ("symlink-gitfile", |_, source, other| {
            fs::create_dir_all(source.join("nested")).unwrap();
            fs::write(
                source.join("nested/gitlink"),
                format!("gitdir: {}\n", other.display()),
            )
            .unwrap();
            std::os::unix::fs::symlink("gitlink", source.join("nested/.git")).unwrap();
        }, "nested/.git"),
        // A nested real `.git` directory whose work tree is outside.
        ("core-worktree", |base, source, _| {
            let nested = source.join("vendor/lib");
            fs::create_dir_all(&nested).unwrap();
            git(&nested, &["init", "--quiet", "--template="]);
            let elsewhere = base.join("elsewhere");
            fs::create_dir_all(&elsewhere).unwrap();
            git(
                &nested,
                &["config", "core.worktree", elsewhere.to_str().unwrap()],
            );
        }, "vendor/lib/.git"),
        // An internal worktree administration whose common dir is outside.
        ("commondir", |_, source, other| {
            let admin = source.join(".git/worktrees/wt");
            fs::create_dir_all(&admin).unwrap();
            fs::write(admin.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
            fs::write(admin.join("commondir"), format!("{}\n", other.display())).unwrap();
            fs::create_dir_all(source.join("wt")).unwrap();
            fs::write(admin.join("gitdir"), format!("{}\n", source.join("wt/.git").display())).unwrap();
            fs::write(source.join("wt/.git"), b"gitdir: ../.git/worktrees/wt\n").unwrap();
        }, "wt/.git"),
        // The carried repository borrows objects from outside.
        ("alternates", |_, source, other| {
            fs::create_dir_all(source.join(".git/objects/info")).unwrap();
            fs::write(
                source.join(".git/objects/info/alternates"),
                format!("{}\n", other.join("objects").display()),
            )
            .unwrap();
        }, ".git"),
    ];
    for (tag, shape, entry) in shapes {
        let world = World::new(tag, |base, source| {
            let other = other_repository(base);
            shape(base, source, &other);
        });
        let ran = world.carry(&[]);
        assert!(!ran.ok, "{tag}: {}{}", ran.stdout, ran.stderr);
        assert!(
            ran.stdout
                .contains(&format!("GIT_CARRY_POINTER_ATTACHED path=\"{entry}\"")),
            "{tag}: {}",
            ran.stdout
        );
        assert!(
            world
                .closure()
                .contains("\"refusal\":\"GIT_CARRY_POINTER_ATTACHED\""),
            "{tag}"
        );
    }
}

#[test]
fn an_attachment_is_reported_when_the_session_is_incomplete() {
    let world = World::new("incomplete-attached", |base, source| {
        let other = other_repository(base);
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(
            source.join("nested/.git"),
            format!("gitdir: {}\n", other.display()),
        )
        .unwrap();
    });
    fs::create_dir(&world.workspace).unwrap();
    fs::write(world.workspace.join("a.txt"), b"destination's own\n").unwrap();
    let ran = world.carry(&[]);
    assert!(!ran.ok);
    assert!(
        ran.stdout
            .contains("GIT_CARRY_POINTER_ATTACHED path=\"nested/.git\""),
        "{}",
        ran.stdout
    );
}

#[test]
fn a_pointer_inert_at_carry_time_that_attaches_later_is_unaccounted() {
    let world = World::new("later", |base, source| {
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(
            source.join("nested/.git"),
            format!("gitdir: {}\n", base.join("later/.git").display()),
        )
        .unwrap();
    });
    let ran = world.carry(&[]);
    assert!(ran.ok, "{}{}", ran.stdout, ran.stderr);
    assert!(world.closure().contains("\"disposition\":\"carried\""));
    // The target appears afterwards (a sibling applied later, say).
    let later = world.path("later");
    fs::create_dir_all(&later).unwrap();
    git(&later, &["init", "--quiet", "--template="]);
    let report = world.closure();
    assert!(
        report.contains("\"unaccounted_reason\":\"carried-pointer-attached\""),
        "{report}"
    );
}

#[test]
fn selections_are_validated_before_any_write_and_carries_are_kept() {
    let world = World::new("select", |_, _| {});
    let typo = "0".repeat(64);
    // A typo'd --bind or --item refuses, with nothing written anywhere.
    let bad_bind = world.carry(&["--bind", &format!("{typo}={}", world.path("x").display())]);
    assert!(!bad_bind.ok);
    assert!(bad_bind.stdout.contains("refused=bind reason=unplanned"), "{}", bad_bind.stdout);
    let bad_item = world.carry(&["--item", &typo]);
    assert!(!bad_item.ok);
    assert!(bad_item.stdout.contains("refused=not-eligible reason=unplanned"), "{}", bad_item.stdout);
    // --bind-only with no bind for the eligible item refuses too.
    let bind_only = world.carry(&["--bind-only"]);
    assert!(!bind_only.ok);
    assert!(bind_only.stdout.contains("refused=bind-only reason=bind-missing"), "{}", bind_only.stdout);
    assert!(!world.workspace.exists());
    assert!(!world.path("carry").exists());
    // A carry, then a rerun without --recarry: kept, no session run.
    assert!(world.carry(&[]).ok);
    let state = stamps(&world.path("carry"));
    let landed = stamps(&world.workspace);
    let kept = world.carry(&[]);
    assert!(kept.ok, "{}", kept.stdout);
    assert!(kept.stdout.contains(" kept=proved"), "{}", kept.stdout);
    assert_eq!(stamps(&world.path("carry")), state, "a kept carry ran a session");
    assert_eq!(stamps(&world.workspace), landed);
}

#[test]
fn a_failed_recarry_writes_capture_absent_back_for_it_and_its_riders() {
    let world = World::new("revert", |_, _| {});
    let live = world.path("dst/live");
    assert!(agent(&os(&[&"estate-add", &world.path("plan"), &world.source, &live])).ok);
    let _ = agent(&os(&[
        &"estate-capture",
        &world.path("plan"),
        &world.path("capture-state"),
        &world.path("corpus"),
        &"1",
    ]));
    let _ = world.apply();
    assert!(world.carry(&[]).ok);
    assert!(world.closure().contains("\"carried\":2,"));
    // The destination now differs; a re-carry cannot carry it.
    fs::write(world.workspace.join("a.txt"), b"edited at the destination\n").unwrap();
    let ran = world.carry(&["--recarry"]);
    assert!(!ran.ok, "{}", ran.stdout);
    assert!(ran.stdout.contains(" carried=incomplete "), "{}", ran.stdout);
    let report = world.closure();
    assert!(report.contains("\"carried\":0,"), "{report}");
    assert_eq!(report.matches("\"refusal\":\"CAPTURE_ABSENT\"").count(), 2, "{report}");
}
