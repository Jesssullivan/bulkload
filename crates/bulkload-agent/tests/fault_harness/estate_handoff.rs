//! Estate hand-off crash rows (TIN-6569, OI-1003-Q183; design
//! `docs/agent-notes/2026-10-16-estate-handoff-design.md`, section 8): the
//! `estate.handoff.*` fault points.
//!
//! A source checkout with `AGENTS.md` and `CLAUDE.md` is planned, captured
//! and landed. A child (this test binary, re-run) runs one step with one
//! point armed and ends itself there with `_exit`; nothing here sends it a
//! signal (R-N11). The parent asserts:
//!
//! - **old or new**: every target holds its landed bytes, or a well-formed
//!   block followed by them, never anything else;
//! - **converge is not wedged**: an `estate-apply` of a source change right
//!   after the crash, before any rerun, converges (the overlay accepts the
//!   pending intent's blocks);
//! - **finishes**: the rerun finishes the step; no temporary and no intent
//!   is left;
//! - **idempotent**: a second rerun writes nothing.
//!
//! `Render`: a first render crashed. `Clear`: a clear of a rendered
//! workspace crashed. `Lift`: converge's lift of a created `CLAUDE.md`
//! before the source's add crashed. `Converge`: a converge that replaces a
//! handed-off `AGENTS.md` crashed at one of its own points, so its settle
//! classifies the block on the landed bytes as the landed seat (design 8,
//! the overlaid `estate.converge.*` row). `HStep`: `estate-apply`'s H step
//! (design 6.4) crashed after the converge's journal, so the rerun apply is
//! a journal no-op that must still settle the pending hand-off intent.
//! `RerenderOverEdit`: a re-render over a user's edit below the block
//! crashed, the next run settled it and refused (a target it cannot
//! write): the settled record never
//! vouches for the edit, and a converge that replaces the target refuses
//! MODIFIED with the edit kept (S5). `ClearNoOpApply`: a crashed clear is
//! finished, not reversed, by a no-op `estate-apply`'s H step.
//! `ClearCarried`: a `--clear --clear-carried` crashed; converge is not
//! wedged by the half-removed carried block, and the H step finishes it
//! (OI-1003-Q190). `ClearCarriedEditRevert`: the same crash, then a user
//! edit before the rerun that finishes it: the removal is still recorded.
//!
//! `_exit` models a process crash only (see the harness header).
//! Each legacy-target scenario runs in its own test subprocess with
//! `BULKLOAD_HANDOFF_TRACKED_TARGETS` set on that command. Other scenarios
//! retain the production default, `CLAUDE.local.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use bulkload_agent::estate::{self, HandoffMode, HandoffOptions, Item};
use bulkload_agent::fault::{Point, FAULT_ENV, FAULT_EXIT_CODE};
use bulkload_agent::git_carry::converge::handoff::TRACKED_TARGETS_ENV;

use super::{Scenario, CHILD_DEADLINE, CHILD_WATCHDOG, CHILD_WATCHDOG_EXIT_CODE, NEXT};

/// Set only in a child: the scratch base it works in.
const CHILD_ENV: &str = "BULKLOAD_W7_HANDOFF_CHILD_BASE";
/// Set only in a child: what it runs (`render`, `clear`, `apply`).
const CHILD_MODE_ENV: &str = "BULKLOAD_W7_HANDOFF_CHILD_MODE";

const AGENTS: &[u8] = b"# Agents\n\nRules.\n";
const CLAUDE: &[u8] = b"See AGENTS.md.\n";

/// What is crashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstateHandoff {
    /// A first render.
    Render,
    /// A clear of a rendered workspace.
    Clear,
    /// Converge's lift of a created file before the source's add.
    Lift,
    /// A converge replacing a handed-off target, crashed at its own point.
    Converge,
    /// `estate-apply`'s H step after a converge, crashed at a hand-off point.
    HStep,
    /// A re-render over a user's edit below the block, crashed; the next
    /// run settles it and then refuses (review of 2026-10-16, converge
    /// safety finding 2).
    RerenderOverEdit,
    /// A clear crashed, then a no-op `estate-apply` (review of 2026-10-16,
    /// converge safety finding 3).
    ClearNoOpApply,
    /// `--clear --clear-carried` over a carried block crashed
    /// (OI-1003-Q190): converge accepts what the crash left, and the next
    /// apply's H step finishes the removal and records it.
    ClearCarried,
    /// `--clear --clear-carried` crashed after its exchange; the user edits
    /// the stripped file before the rerun finishes the clear, then reverts
    /// the edit (re-review 3 of 2026-10-16, finding 3): the removal is
    /// recorded, so converge never blames the user for bulkload's edit.
    ClearCarriedEditRevert,
}

impl Scenario for EstateHandoff {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth, self);
    }

    fn run_named(self, point: Point, nth: u64, test: &str) {
        if std::env::var_os(TRACKED_TARGETS_ENV).is_some() {
            self.run(point, nth);
            return;
        }
        // The fixture's assertions cover the two legacy targets. Select
        // them for this scenario alone, including its crash children.
        // No environment variable in the parallel harness is changed.
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads", "1", "--nocapture"])
            .env(TRACKED_TARGETS_ENV, "1")
            .env_remove(FAULT_ENV)
            .env_remove(CHILD_ENV)
            .env_remove(super::CHILD_ENV)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{test}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(&format!("test {test} ... ok")),
            "{test}: the exact fixture did not run: {stdout}\n{stderr}"
        );
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(out.stdout).unwrap()
}

struct World {
    base: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(2_100));
}

impl World {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-w7-handoff-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let world = Self {
            base: fs::canonicalize(&base).unwrap(),
        };
        let src = world.src();
        fs::create_dir(&src).unwrap();
        git(&src, &["init", "--quiet", "--template=", "-b", "main"]);
        fs::write(src.join("AGENTS.md"), AGENTS).unwrap();
        fs::write(src.join("CLAUDE.md"), CLAUDE).unwrap();
        fs::write(src.join("a.txt"), b"alpha\n").unwrap();
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "--quiet", "-m", "c0"]);
        estate::add_batch(
            &world.base.join("plan"),
            &[Item {
                source: src,
                repository: world.dst(),
                workspace: Some(world.dst()),
            }],
        )
        .unwrap();
        tick();
        world.capture();
        assert_eq!(world.apply()[0].0, "workspace-restored");
        world
    }

    fn src(&self) -> PathBuf {
        self.base.join("src")
    }

    fn dst(&self) -> PathBuf {
        self.base.join("dst")
    }

    fn capture(&self) {
        estate::capture(
            &self.base.join("plan"),
            &self.base.join("corpus-state"),
            &self.base.join("corpus"),
            1,
            &|_| Ok(()),
        )
        .unwrap();
    }

    fn apply(&self) -> Vec<(&'static str, Option<String>)> {
        let rows = Mutex::new(Vec::new());
        let _ = estate::apply(
            &self.base.join("plan"),
            &self.base.join("corpus"),
            &self.base.join("applied"),
            "neo",
            1,
            &|row| {
                rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                Ok(())
            },
        );
        rows.into_inner().unwrap()
    }

    fn handoff(&self, mode: HandoffMode) -> Result<Vec<String>, String> {
        handoff_at(&self.base, mode)
    }

    /// A source change elsewhere, captured after a tick.
    fn source_change(&self, content: &[u8]) {
        fs::write(self.src().join("a.txt"), content).unwrap();
        git(&self.src(), &["commit", "--quiet", "-am", "a change"]);
        tick();
        self.capture();
    }

    fn temps(&self) -> Vec<String> {
        fs::read_dir(self.dst())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".bulkload-handoff-"))
            .collect()
    }

    fn intent(&self) -> bool {
        self.dst()
            .join(".git/bulkload/v1/neo/handoff.intent")
            .exists()
    }
}

fn handoff_at(base: &Path, mode: HandoffMode) -> Result<Vec<String>, String> {
    handoff_with(
        base,
        &HandoffOptions {
            mode,
            ..HandoffOptions::default()
        },
    )
}

fn handoff_with(base: &Path, options: &HandoffOptions) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    estate::handoff(
        &base.join("plan"),
        &base.join("corpus"),
        &base.join("applied"),
        "neo",
        options,
        &mut |line| {
            lines.push(line.to_owned());
            Ok(())
        },
    )
    .map(|_| lines)
    .map_err(|refusal| refusal.to_string())
}

/// The bytes after a well-formed block at byte 0, or `None` without one.
fn remainder(bytes: &[u8]) -> Option<Vec<u8>> {
    let begin = b"<!-- bulkload-handoff:v1 BEGIN id=";
    let end = b"<!-- bulkload-handoff:v1 END id=";
    if !bytes.starts_with(begin) {
        return None;
    }
    let at = bytes.windows(end.len()).position(|w| w == end)?;
    let line = at + bytes[at..].iter().position(|b| *b == b'\n')? + 1;
    (bytes.get(line) == Some(&b'\n')).then(|| bytes[line + 1..].to_vec())
}

/// Every target holds `body` (`None`: absent), or a block and `body`.
fn old_or_new(world: &World, label: &str, bodies: &[(&str, Option<&[u8]>)]) {
    for (name, body) in bodies {
        let path = world.dst().join(name);
        match (fs::read(&path).ok(), body) {
            (None, None) => {}
            (Some(bytes), Some(body)) if bytes == *body => {}
            (Some(bytes), body) => {
                let rest = remainder(&bytes)
                    .unwrap_or_else(|| panic!("{label}: {name} is neither old nor new"));
                assert_eq!(
                    Some(rest.as_slice()),
                    body.or(Some(&[][..])),
                    "{label}: {name}'s bytes under the block"
                );
            }
            (None, Some(_)) => panic!("{label}: {name} vanished"),
        }
    }
}

/// Run this test binary as a child that runs `mode` armed with `fault`: it
/// must end itself at that point.
fn crash(world: &World, mode: &str, fault: &str) {
    assert!(
        std::env::var_os(FAULT_ENV).is_none(),
        "the harness process itself must not be armed"
    );
    let running = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "estate_handoff::handoff_child_entry",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD_ENV, &world.base)
        .env(CHILD_MODE_ENV, mode)
        .env(TRACKED_TARGETS_ENV, "1")
        .env(FAULT_ENV, fault)
        .env_remove(super::CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A detached waiter reaps the child; on timeout the test fails and the
    // child is left alone, never signalled (R-N11).
    let (reaped, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = reaped.send(running.wait_with_output());
    });
    let child = outcome
        .recv_timeout(CHILD_DEADLINE)
        .unwrap_or_else(|timeout| {
            panic!("{fault}: hand-off child still running ({timeout}); left running, not signalled")
        })
        .unwrap();
    let stderr = String::from_utf8_lossy(&child.stderr).into_owned();
    assert_ne!(
        child.status.code(),
        Some(CHILD_WATCHDOG_EXIT_CODE),
        "{fault}: hung; the watchdog ended it after {CHILD_WATCHDOG:?} ({stderr})"
    );
    assert_eq!(
        child.status.code(),
        Some(FAULT_EXIT_CODE),
        "{fault}: the child must stop at its fault point ({:?}, {stderr})",
        child.status
    );
}

/// The child half of every hand-off scenario; a no-op in an ordinary run.
#[test]
fn handoff_child_entry() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_WATCHDOG);
        eprintln!("hand-off child watchdog: not done within {CHILD_WATCHDOG:?}");
        // SAFETY: `_exit` takes no pointers and never returns; this process
        // ends only itself.
        unsafe { libc::_exit(CHILD_WATCHDOG_EXIT_CODE) }
    });
    let mode = std::env::var(CHILD_MODE_ENV).unwrap();
    let outcome = match mode.as_str() {
        "render" => handoff_at(&base, HandoffMode::Render).map(drop),
        "clear" => handoff_at(&base, HandoffMode::Clear).map(drop),
        "clear-carried" => handoff_with(
            &base,
            &HandoffOptions {
                mode: HandoffMode::Clear,
                clear_carried: true,
                ..HandoffOptions::default()
            },
        )
        .map(drop),
        _ => estate::apply(
            &base.join("plan"),
            &base.join("corpus"),
            &base.join("applied"),
            "neo",
            1,
            &|_| Ok(()),
        )
        .map_err(|refusal| refusal.to_string()),
    };
    panic!("the armed point was not reached: {mode} returned {outcome:?}");
}

#[allow(clippy::too_many_lines)]
fn scenario(point: Point, nth: u64, kind: EstateHandoff) {
    let label = format!("{}:{nth}/{kind:?}", point.name());
    let world = World::new(&point.name().replace('.', "-"));
    let fault = format!("{}:{nth}", point.name());
    match kind {
        EstateHandoff::Render => {
            crash(&world, "render", &fault);
            let bodies: [(&str, Option<&[u8]>); 2] =
                [("AGENTS.md", Some(AGENTS)), ("CLAUDE.md", Some(CLAUDE))];
            old_or_new(&world, &label, &bodies);
            // Converge is not wedged by what the crash left.
            world.source_change(b"alpha 2\n");
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            old_or_new(&world, &label, &bodies);
            let ran = world.handoff(HandoffMode::Render);
            assert!(ran.is_ok(), "{label}: {ran:?}");
            for (name, body) in bodies {
                let bytes = fs::read(world.dst().join(name)).unwrap();
                assert_eq!(remainder(&bytes).as_deref(), body, "{label}: {name}");
            }
        }
        EstateHandoff::Clear => {
            world.handoff(HandoffMode::Render).unwrap();
            crash(&world, "clear", &fault);
            let bodies: [(&str, Option<&[u8]>); 2] =
                [("AGENTS.md", Some(AGENTS)), ("CLAUDE.md", Some(CLAUDE))];
            old_or_new(&world, &label, &bodies);
            world.source_change(b"alpha 2\n");
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            let ran = world.handoff(HandoffMode::Clear);
            assert!(ran.is_ok(), "{label}: {ran:?}");
            assert_eq!(fs::read(world.dst().join("AGENTS.md")).unwrap(), AGENTS);
            assert_eq!(fs::read(world.dst().join("CLAUDE.md")).unwrap(), CLAUDE);
            assert!(!world
                .dst()
                .join(".git/bulkload/v1/neo/handoff.landed")
                .exists());
        }
        EstateHandoff::Lift => {
            git(&world.src(), &["rm", "--quiet", "CLAUDE.md"]);
            git(&world.src(), &["commit", "--quiet", "-m", "no claude"]);
            tick();
            world.capture();
            assert_eq!(world.apply()[0].0, "workspace-restored");
            world.handoff(HandoffMode::Render).unwrap();
            fs::write(world.src().join("CLAUDE.md"), b"Back.\n").unwrap();
            git(&world.src(), &["add", "CLAUDE.md"]);
            git(&world.src(), &["commit", "--quiet", "-m", "claude back"]);
            tick();
            world.capture();
            crash(&world, "apply", &fault);
            // The created block is lifted, or still there; never the
            // source's bytes half-landed.
            old_or_new(&world, &label, &[("CLAUDE.md", None)]);
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            // The add landed, and the H step put the block on it.
            let bytes = fs::read(world.dst().join("CLAUDE.md")).unwrap();
            assert_eq!(
                remainder(&bytes).as_deref(),
                Some(&b"Back.\n"[..]),
                "{label}"
            );
            let ran = world.handoff(HandoffMode::Render);
            assert!(ran.is_ok(), "{label}: {ran:?}");
            let bytes = fs::read(world.dst().join("CLAUDE.md")).unwrap();
            assert_eq!(
                remainder(&bytes).as_deref(),
                Some(&b"Back.\n"[..]),
                "{label}"
            );
        }
        EstateHandoff::Converge => {
            world.handoff(HandoffMode::Render).unwrap();
            fs::write(world.src().join("AGENTS.md"), b"# Agents, v2\n").unwrap();
            git(&world.src(), &["commit", "--quiet", "-am", "agents v2"]);
            tick();
            world.capture();
            crash(&world, "apply", &fault);
            let bytes = fs::read(world.dst().join("AGENTS.md")).unwrap();
            assert!(
                bytes == b"# Agents, v2\n" || remainder(&bytes).as_deref() == Some(AGENTS),
                "{label}: AGENTS.md is neither the block on the landed bytes nor the new body"
            );
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            // The new body landed, and the H step put the block on it.
            let bytes = fs::read(world.dst().join("AGENTS.md")).unwrap();
            assert_eq!(
                remainder(&bytes).as_deref(),
                Some(&b"# Agents, v2\n"[..]),
                "{label}"
            );
            let converge_temps = fs::read_dir(world.dst())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".bulkload-converge-")
                })
                .count();
            assert_eq!(converge_temps, 0, "{label}: a converge temporary is left");
            let ran = world.handoff(HandoffMode::Render);
            assert!(ran.is_ok(), "{label}: {ran:?}");
            let bytes = fs::read(world.dst().join("AGENTS.md")).unwrap();
            assert_eq!(remainder(&bytes).as_deref(), Some(&b"# Agents, v2\n"[..]));
        }
        EstateHandoff::HStep => {
            world.handoff(HandoffMode::Render).unwrap();
            world.source_change(b"alpha 2\n");
            crash(&world, "apply", &fault);
            let bodies: [(&str, Option<&[u8]>); 2] =
                [("AGENTS.md", Some(AGENTS)), ("CLAUDE.md", Some(CLAUDE))];
            old_or_new(&world, &label, &bodies);
            // The rerun apply is a journal no-op for the item, and still
            // settles the H step the crash left.
            let rows = world.apply();
            assert!(rows[0].0.starts_with("previous-"), "{label}: {rows:?}");
            for (name, body) in bodies {
                let bytes = fs::read(world.dst().join(name)).unwrap();
                assert_eq!(remainder(&bytes).as_deref(), body, "{label}: {name}");
            }
        }
        EstateHandoff::RerenderOverEdit => {
            world.handoff(HandoffMode::Render).unwrap();
            tick();
            let agents = world.dst().join("AGENTS.md");
            let mut bytes = fs::read(&agents).unwrap();
            bytes.extend_from_slice(b"A local note.\n");
            fs::write(&agents, &bytes).unwrap();
            // A newer capture, not applied: the block's text moves, so the
            // rerun re-renders over the user's bytes.
            world.source_change(b"alpha 2\n");
            crash(&world, "render", &fault);
            // The next run settles the crash, then refuses on a target it
            // cannot write (a directory where `AGENTS.override.md` would
            // be), as the privacy guard did before OI-1003-Q188 removed it.
            let override_dir = world.dst().join("AGENTS.override.md");
            fs::create_dir(&override_dir).unwrap();
            let refused = world.handoff(HandoffMode::Render);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|refusal| refusal.contains("HANDOFF_TARGET_UNSUPPORTED")),
                "{label}: {refused:?}"
            );
            fs::remove_dir(&override_dir).unwrap();
            tick();
            let mut kept = AGENTS.to_vec();
            kept.extend_from_slice(b"A local note.\n");
            assert_eq!(remainder(&fs::read(&agents).unwrap()), Some(kept.clone()));
            fs::write(world.src().join("AGENTS.md"), b"# Agents, v2\n").unwrap();
            git(&world.src(), &["commit", "--quiet", "-am", "agents v2"]);
            tick();
            world.capture();
            let rows = world.apply();
            assert!(
                rows[0]
                    .1
                    .as_deref()
                    .is_some_and(|reason| reason.contains("GIT_DESTINATION_MODIFIED")),
                "{label}: {rows:?}"
            );
            assert_eq!(
                remainder(&fs::read(&agents).unwrap()),
                Some(kept),
                "{label}: the user's edit was landed over"
            );
        }
        EstateHandoff::ClearCarried => {
            let mut carried = b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef for=fedcba9876543210 BULKLOAD-HANDOFF-DO-NOT-COMMIT -->\n> another landing's block\n<!-- bulkload-handoff:v1 END id=0123456789abcdef -->\n\n".to_vec();
            carried.extend_from_slice(AGENTS);
            fs::write(world.src().join("AGENTS.md"), &carried).unwrap();
            git(
                &world.src(),
                &["commit", "--quiet", "-am", "a carried block"],
            );
            tick();
            world.capture();
            assert_eq!(world.apply()[0].0, "workspace-restored");
            world.handoff(HandoffMode::Render).unwrap();
            crash(&world, "clear-carried", &fault);
            let agents = world.dst().join("AGENTS.md");
            let bytes = fs::read(&agents).unwrap();
            assert!(
                bytes == AGENTS || remainder(&bytes).as_deref() == Some(carried.as_slice()),
                "{label}: AGENTS.md is neither before nor after the clear"
            );
            // Converge is not wedged by what the crash left, and the H step
            // finishes the clear with its carried blocks.
            world.source_change(b"alpha 2\n");
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            assert_eq!(fs::read(&agents).unwrap(), AGENTS, "{label}");
            assert!(world
                .dst()
                .join(".git/bulkload/v1/neo/handoff.stripped")
                .exists());
            world.source_change(b"alpha 3\n");
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            assert_eq!(fs::read(&agents).unwrap(), AGENTS, "{label}");
        }
        EstateHandoff::ClearCarriedEditRevert => {
            let mut carried = b"<!-- bulkload-handoff:v1 BEGIN id=0123456789abcdef for=fedcba9876543210 BULKLOAD-HANDOFF-DO-NOT-COMMIT -->\n> another landing's block\n<!-- bulkload-handoff:v1 END id=0123456789abcdef -->\n\n".to_vec();
            carried.extend_from_slice(AGENTS);
            fs::write(world.src().join("AGENTS.md"), &carried).unwrap();
            git(
                &world.src(),
                &["commit", "--quiet", "-am", "a carried block"],
            );
            tick();
            world.capture();
            assert_eq!(world.apply()[0].0, "workspace-restored");
            world.handoff(HandoffMode::Render).unwrap();
            crash(&world, "clear-carried", &fault);
            let agents = world.dst().join("AGENTS.md");
            assert_eq!(fs::read(&agents).unwrap(), AGENTS, "{label}: not stripped");
            let mut edited = AGENTS.to_vec();
            edited.extend_from_slice(b"A local note.\n");
            fs::write(&agents, &edited).unwrap();
            let ran = world.handoff(HandoffMode::Clear);
            assert!(ran.is_ok(), "{label}: {ran:?}");
            assert_eq!(fs::read(&agents).unwrap(), edited, "{label}");
            assert!(
                world
                    .dst()
                    .join(".git/bulkload/v1/neo/handoff.stripped")
                    .exists(),
                "{label}: the finished removal was not recorded"
            );
            // Converge blames the user for the user's edit only.
            world.source_change(b"alpha 2\n");
            let rows = world.apply();
            assert!(
                rows[0]
                    .1
                    .as_deref()
                    .is_some_and(|reason| reason.contains("GIT_DESTINATION_MODIFIED")),
                "{label}: {rows:?}"
            );
            assert_eq!(fs::read(&agents).unwrap(), edited, "{label}");
            fs::write(&agents, AGENTS).unwrap();
            let rows = world.apply();
            assert_eq!(rows[0].0, "workspace-restored", "{label}: {rows:?}");
            assert_eq!(fs::read(&agents).unwrap(), AGENTS, "{label}");
        }
        EstateHandoff::ClearNoOpApply => {
            world.handoff(HandoffMode::Render).unwrap();
            tick();
            crash(&world, "clear", &fault);
            assert!(world.intent(), "{label}: no intent after the crash");
            let rows = world.apply();
            assert!(rows[0].0.starts_with("previous-"), "{label}: {rows:?}");
            assert_eq!(fs::read(world.dst().join("AGENTS.md")).unwrap(), AGENTS);
            assert_eq!(fs::read(world.dst().join("CLAUDE.md")).unwrap(), CLAUDE);
            assert!(!world
                .dst()
                .join(".git/bulkload/v1/neo/handoff.landed")
                .exists());
        }
    }
    assert!(world.temps().is_empty(), "{label}: {:?}", world.temps());
    assert!(!world.intent(), "{label}: an intent is left");
    // A second rerun writes nothing (after one that may re-date a record
    // whose prints were racy).
    tick();
    let mode = if matches!(
        kind,
        EstateHandoff::Clear
            | EstateHandoff::ClearNoOpApply
            | EstateHandoff::ClearCarried
            | EstateHandoff::ClearCarriedEditRevert
    ) {
        HandoffMode::Clear
    } else {
        HandoffMode::Render
    };
    let ran = world.handoff(mode);
    assert!(ran.is_ok(), "{label}: {ran:?}");
    let frozen: Vec<(PathBuf, u64, i64)> = walk(&world.dst());
    let ran = world.handoff(mode);
    assert!(ran.is_ok(), "{label}: {ran:?}");
    assert_eq!(walk(&world.dst()), frozen, "{label}: a second rerun wrote");
}

/// Every entry under `root` with its inode and mtime.
fn walk(root: &Path) -> Vec<(PathBuf, u64, i64)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            out.push((path.clone(), meta.ino(), meta.mtime_nsec()));
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    out.sort();
    out
}
