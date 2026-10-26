//! SPEC rules 6 and 8, operator direction 2026-10-24: neo may reboot
//! repeatedly while an in-place bulkload transfer continues. The fake SSH
//! cuts a data frame after another file is durable, refuses the next dial
//! and returns. A separate case loses only the final SSH exit status.
//! No process is signalled and no source is frozen.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct World {
    root: tempfile::TempDir,
}

impl World {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "source",
            "dest",
            "bin",
            "state",
            "state/serve",
            "state/pull",
        ] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        fs::write(root.path().join("mode"), mode).unwrap();
        let ssh = root.path().join("bin/ssh");
        fs::write(
            &ssh,
            r"#!/usr/bin/env python3
import os, pathlib, sqlite3, subprocess, sys, threading, time
root = pathlib.Path(__file__).resolve().parent.parent
count = root / 'attempts'
n = int(count.read_text()) + 1 if count.exists() else 1
count.write_text(str(n))
(root / 'ssh-args').write_text('\n'.join(sys.argv[1:]))
mode = (root / 'mode').read_text()
if mode == 'offline' or (mode in ('reboot', 'partial') and n == 2):
    sys.exit(255)
remote = sys.argv[sys.argv.index('--') + 2:]
if mode == 'partial' and n == 1:
    child = subprocess.Popen(remote, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    def upstream():
        try:
            while True:
                data = os.read(0, 65536)
                if not data:
                    break
                child.stdin.write(data)
                child.stdin.flush()
        except (BrokenPipeError, OSError, ValueError):
            pass
    threading.Thread(target=upstream, daemon=True).start()
    def exact(size):
        data = bytearray()
        while len(data) < size:
            more = child.stdout.read(size - len(data))
            if not more:
                break
            data.extend(more)
        return bytes(data)
    def durable_a():
        db = root / 'state/pull/dest/transfer.sqlite'
        try:
            with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True, timeout=0.01) as conn:
                return conn.execute('SELECT EXISTS(SELECT 1 FROM output_hints WHERE path = ?)', (b'a',)).fetchone()[0] and conn.execute('SELECT EXISTS(SELECT 1 FROM outputs)').fetchone()[0]
        except sqlite3.DatabaseError:
            return False
    try:
        while True:
            header = exact(5)
            if len(header) != 5:
                raise RuntimeError('source ended before the data-frame cut')
            body = exact(int.from_bytes(header[:4], 'big') - 1)
            if header[4] == 2 and durable_a() and not (root / 'dest/b').exists():
                sys.stdout.buffer.write(header + body[:len(body) // 2])
                sys.stdout.buffer.flush()
                (root / 'cut-proof').write_text(str((root / 'dest/a').stat().st_size))
                break
            sys.stdout.buffer.write(header + body)
            sys.stdout.buffer.flush()
            time.sleep(0.005)
    finally:
        # EOF and EPIPE end this owned source naturally. Reap it before
        # the proxy exits. The upstream thread is daemonized because its
        # caller's stdin stays open until the proxy closes stdout.
        child.stdout.close()
        child.stdin.close()
        child.wait()
    sys.exit(255)
result = subprocess.run(remote)
sys.exit(255 if mode == 'statuslost' or (mode == 'reboot' and n in (1, 3)) else result.returncode)
",
        )
        .unwrap();
        fs::set_permissions(ssh, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn attempts(&self) -> u32 {
        fs::read_to_string(self.path("attempts"))
            .unwrap()
            .parse()
            .unwrap()
    }

    fn pull(&self, budget: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        let mut search = self.path("bin").into_os_string();
        search.push(":");
        search.push(std::env::var_os("PATH").unwrap_or_default());
        command
            .arg("--min-free-percent=0")
            .env("PATH", search)
            .env("BULKLOAD_DEST_GROUP_IDLE_MS", "1");
        if let Some(budget) = budget {
            command.arg(format!("--reconnect-seconds={budget}"));
        }
        command.arg("pull").arg("fakehost").args([
            self.path("source"),
            self.path("dest"),
            self.path("state/serve/source"),
            self.path("state/pull/dest"),
            PathBuf::from(env!("CARGO_BIN_EXE_bulkload-agent")),
        ]);
        command.output().unwrap()
    }
}

fn shown(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn source_reads(output: &Output) -> u64 {
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find_map(|word| word.strip_prefix("source_bytes_read="))
        .unwrap_or_else(|| panic!("no source read count: {}", shown(output)))
        .parse()
        .unwrap()
}

fn names(path: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_mid_frame_disconnect_preserves_committed_files_and_resumes_the_partial_file() {
    let world = World::new("partial");
    let first = vec![37_u8; 8192];
    let partial = vec![53_u8; 4 * 1024 * 1024];
    fs::write(world.path("source/a"), &first).unwrap();
    fs::write(world.path("source/b"), &partial).unwrap();
    bulkload_agent::transfer::settle_racy_window(&world.path("source")).unwrap();
    let output = world.pull(Some("10"));
    assert!(output.status.success(), "{}", shown(&output));
    assert_eq!(world.attempts(), 3, "{}", shown(&output));
    assert_eq!(fs::read_to_string(world.path("cut-proof")).unwrap(), "8192");
    // The completed file is reused. Only the file whose data frame was
    // cut is read on the successful resumed attempt.
    assert_eq!(
        source_reads(&output),
        partial.len() as u64,
        "{}",
        shown(&output)
    );
    assert_eq!(fs::read(world.path("dest/a")).unwrap(), first);
    assert_eq!(fs::read(world.path("dest/b")).unwrap(), partial);
    assert_eq!(names(&world.path("dest")), ["a", "b"]);
    let measured = bulkload_agent::state_payload::measure(&world.path("state")).unwrap();
    assert_eq!(measured.payload_bytes, 0, "{}", measured.words());
    let warm = world.pull(None);
    assert!(warm.status.success(), "{}", shown(&warm));
    assert_eq!(source_reads(&warm), 0);
}

#[test]
fn lost_ssh_exit_status_and_a_failed_dial_reuse_the_same_durable_state() {
    let world = World::new("reboot");
    fs::write(world.path("source/a"), vec![17; 256_000]).unwrap();
    fs::write(world.path("source/b"), vec![29; 320_000]).unwrap();
    // Past the engine's full two-second racy window, so published rows
    // are eligible for reuse. The fixture has no live writer.
    bulkload_agent::transfer::settle_racy_window(&world.path("source")).unwrap();
    let output = world.pull(Some("10"));
    assert!(output.status.success(), "{}", shown(&output));
    assert_eq!(world.attempts(), 4, "{}", shown(&output));
    assert_eq!(source_reads(&output), 0, "completed rows are reused");
    assert_eq!(names(&world.path("dest")), ["a", "b"]);
    assert_eq!(fs::read(world.path("dest/a")).unwrap(), vec![17; 256_000]);
    assert_eq!(fs::read(world.path("dest/b")).unwrap(), vec![29; 320_000]);
    let measured = bulkload_agent::state_payload::measure(&world.path("state")).unwrap();
    assert!(measured.payload.is_empty(), "{}", measured.words());
    let args = fs::read_to_string(world.path("ssh-args")).unwrap();
    assert!(args.contains("-oServerAliveInterval=15"), "{args}");
    assert!(args.contains("-oServerAliveCountMax=2"), "{args}");

    // A second reboot repeats the connection sequence against the same
    // persisted states. It neither duplicates outputs nor re-reads them.
    fs::write(world.path("attempts"), "0").unwrap();
    let again = world.pull(Some("10"));
    assert!(again.status.success(), "{}", shown(&again));
    assert_eq!(world.attempts(), 4);
    assert_eq!(source_reads(&again), 0);
    assert_eq!(names(&world.path("dest")), ["a", "b"]);
}

#[test]
fn a_permanently_lost_transport_exhausts_its_admission_window() {
    let world = World::new("offline");
    let output = world.pull(Some("3"));
    assert!(!output.status.success(), "{}", shown(&output));
    assert!(shown(&output).contains("TRANSPORT_LOST"));
    assert_eq!(world.attempts(), 2, "no dial begins after its budget");
    assert!(names(&world.path("dest")).is_empty());
}

#[test]
fn persistent_lost_final_status_is_bounded_for_empty_warm_adopted_and_cold_roots() {
    for kind in ["empty", "warm", "adopted", "cold"] {
        let world = World::new("online");
        if kind != "empty" {
            fs::write(world.path("source/a"), b"one retained file\n").unwrap();
        }
        if kind == "adopted" {
            fs::write(world.path("dest/a"), b"one retained file\n").unwrap();
        }
        if kind == "warm" {
            let initial = world.pull(None);
            assert!(initial.status.success(), "{}", shown(&initial));
            fs::write(world.path("attempts"), "0").unwrap();
        }
        fs::write(world.path("mode"), "statuslost").unwrap();
        let output = world.pull(Some("3"));
        assert!(!output.status.success(), "{kind}: {}", shown(&output));
        assert!(shown(&output).contains("TRANSPORT_LOST"));
        assert_eq!(
            world.attempts(),
            2,
            "{kind}: completion and bookkeeping never reset an outage: {}",
            shown(&output)
        );
        if kind == "empty" {
            assert!(names(&world.path("dest")).is_empty());
        } else {
            assert_eq!(names(&world.path("dest")), ["a"]);
            assert_eq!(
                fs::read(world.path("dest/a")).unwrap(),
                b"one retained file\n"
            );
        }
    }
}

#[test]
fn reconnect_is_opt_in_and_never_retries_a_typed_hold() {
    let offline = World::new("offline");
    assert!(!offline.pull(None).status.success());
    assert_eq!(offline.attempts(), 1);

    let held = World::new("online");
    fs::write(held.path("source/a"), b"neo's bytes\n").unwrap();
    fs::write(held.path("dest/a"), b"sting's own bytes\n").unwrap();
    let output = held.pull(Some("10"));
    assert!(!output.status.success(), "{}", shown(&output));
    assert!(shown(&output).contains("DESTINATION_OCCUPIED"));
    assert_eq!(held.attempts(), 1);
    assert_eq!(
        fs::read(held.path("dest/a")).unwrap(),
        b"sting's own bytes\n"
    );
}

#[test]
fn reconnect_owns_its_connection_and_default_pulls_keep_sharing_available() {
    for budget in [None, Some("0"), Some("1")] {
        let world = World::new("online");
        let output = world.pull(budget);
        assert!(output.status.success(), "{}", shown(&output));
        let args = fs::read_to_string(world.path("ssh-args")).unwrap();
        let enabled = budget == Some("1");
        assert_eq!(
            args.lines().any(|arg| arg == "-oControlMaster=no"),
            enabled,
            "{args}"
        );
        assert_eq!(
            args.lines().any(|arg| arg == "-oControlPath=none"),
            enabled,
            "{args}"
        );
        assert!(args.lines().any(|arg| arg == "-oServerAliveInterval=15"));
        assert!(args.lines().any(|arg| arg == "-oServerAliveCountMax=2"));
    }
}

#[test]
fn an_invalid_reconnect_budget_is_refused_before_a_transport_starts() {
    for budget in ["-1", "forever", "18446744073709551616"] {
        let world = World::new("offline");
        let output = world.pull(Some(budget));
        assert!(!output.status.success());
        assert!(shown(&output).contains("FIELD_DOMAIN_VIOLATION"));
        assert!(!world.path("attempts").exists());
    }
}

#[test]
fn a_remote_source_root_missing_is_refused_at_once_with_its_code() {
    // MG-1 (2026-10-26): neo's serve refused SOURCE_ROOT_MISSING before
    // any frame, and every such item spent the whole reconnect budget.
    let world = World::new("online");
    fs::remove_dir(world.path("source")).unwrap();
    let started = std::time::Instant::now();
    let output = world.pull(Some("600"));
    let text = shown(&output);
    assert!(!output.status.success(), "{text}");
    assert_eq!(
        world.attempts(),
        1,
        "an explicit refusal is never redialled: {text}"
    );
    assert!(!text.contains("reconnect transport=lost"), "{text}");
    assert!(!text.contains("TRANSPORT_LOST"), "{text}");
    // The remote's own line is relayed, and this side refuses with it.
    assert_eq!(
        text.matches("bulkload-agent: refused: SOURCE_ROOT_MISSING")
            .count(),
        2,
        "{text}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_mins(1),
        "{text}"
    );
    assert!(names(&world.path("dest")).is_empty());
}

#[test]
fn a_remote_that_ends_without_naming_a_refusal_still_reconnects() {
    // Exit 1 with no `refused:` line is not an agent refusal: retried.
    let world = World::new("online");
    let ssh = world.path("bin/ssh");
    let script = fs::read_to_string(&ssh).unwrap().replace(
        "result = subprocess.run(remote)",
        "result = subprocess.run(remote, stderr=subprocess.DEVNULL) if not (root / 'source').exists() else subprocess.run(remote)",
    );
    fs::write(&ssh, script).unwrap();
    fs::remove_dir(world.path("source")).unwrap();
    let output = world.pull(Some("3"));
    let text = shown(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("reconnect transport=lost"), "{text}");
    assert!(text.contains("TRANSPORT_LOST"), "{text}");
    assert!(world.attempts() >= 2, "{text}");
}
