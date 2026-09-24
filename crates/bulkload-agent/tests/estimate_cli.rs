//! `git-carry-estimate` over several pairs (F13, PR #55 review).
//!
//! Every pair prints one whole block. A refused pair prints a `refused=` line
//! with its stable code instead of silently ending the output, the pairs after
//! it are still measured, and the verb exits nonzero.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

mod stderr_corpus;

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
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
        .arg(repo);
    command
}

fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?}");
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir(&repo).unwrap();
    run(git(&repo).args(["init", "--quiet", "--template=", "-b", "main"]));
    std::fs::write(repo.join("a.txt"), name).unwrap();
    run(git(&repo).args(["add", "a.txt"]));
    run(git(&repo).args(["commit", "--quiet", "-m", name]));
    repo
}

#[test]
fn a_refused_pair_prints_its_own_line_and_the_rest_are_still_measured() {
    let root = Root(std::env::temp_dir().join(format!(
        "bulkload-estimate-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    let source = repo(&root.0, "source");
    let destination = repo(&root.0, "destination");
    let plain = root.0.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("git-carry-estimate")
        .arg(&source)
        .arg(&destination)
        .arg(&source)
        .arg(&plain)
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    let blocks: Vec<&str> = stdout.trim_end().split("\n\n").collect();
    assert_eq!(blocks.len(), 3, "{stdout}");
    for index in [0, 2] {
        let block = blocks[index];
        assert!(block.contains("\nmissing_objects="), "{block}");
        assert!(block.contains("\nmissing_thin_pack_bytes="), "{block}");
        assert!(!block.contains("refused="), "{block}");
    }
    let refused = blocks[1];
    assert!(
        refused.contains(&format!("destination={}", plain.display())),
        "{refused}"
    );
    assert!(
        refused.contains("\nrefused=GIT_REPOSITORY_NOT_AT_PATH"),
        "{refused}"
    );
    assert!(!refused.contains("missing_objects="), "{refused}");
}

/// Keys a refused block may carry (R-N121): none holds stderr bytes.
const REFUSED_KEYS: [&str; 8] = [
    "source",
    "destination",
    "refused",
    "refused_reason",
    "stderr_class",
    "stderr_keyed_blake3",
    "stderr_file",
    "stderr_file_refused",
];

const CLASSES: [&str; 6] = [
    "not_a_repository",
    "auth_failed",
    "host_unreachable",
    "timeout",
    "bad_object",
    "other",
];

fn scratch(name: &str) -> Root {
    let root = Root(std::env::temp_dir().join(format!(
        "bulkload-estimate-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    root
}

/// A private (0700) state dir, as `--state-dir` requires.
fn state_dir(parent: &Path, name: &str) -> PathBuf {
    let state = parent.join(name);
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    state
}

/// The digest key the verb made in `state`.
fn key(state: &Path) -> [u8; 32] {
    std::fs::read(state.join("stderr/key"))
        .unwrap()
        .try_into()
        .unwrap()
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run the verb over `pairs`, in batches, with `bin` first on `PATH` and a
/// state dir; return stdout and stderr of every run, concatenated.
fn run_pairs(bin: &Path, state: &Path, pairs: &[(PathBuf, String)]) -> (String, String) {
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let (mut stdout, mut stderr) = (String::new(), String::new());
    for batch in pairs.chunks(400) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"));
        command
            .env("PATH", &path)
            .arg("git-carry-estimate")
            .arg("--state-dir")
            .arg(state);
        for (source, destination) in batch {
            command.arg(source).arg(destination);
        }
        let result = command.output().unwrap();
        assert!(!result.status.success());
        stdout.push_str(&String::from_utf8(result.stdout).unwrap());
        stdout.push_str("\n\n");
        stderr.push_str(&String::from_utf8_lossy(&result.stderr));
    }
    (stdout, stderr)
}

/// Check every refused block against its payload: only R-N121 keys, a class
/// from the closed set, the payload's digest, and a 0600 file holding
/// exactly the payload. No stdout or stderr byte holds a canary or secret.
fn assert_no_echo(state: &Path, payloads: &[(Vec<u8>, String)], stdout: &str, stderr: &str) {
    let blocks: Vec<&str> = stdout
        .split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .collect();
    assert_eq!(blocks.len(), payloads.len());
    let key = key(state);
    for (block, (payload, _)) in blocks.iter().zip(payloads) {
        let digest = blake3::keyed_hash(&key, payload).to_hex().to_string();
        for line in block.lines() {
            let (key, value) = line.split_once('=').unwrap();
            assert!(REFUSED_KEYS.contains(&key), "unexpected line {line:?}");
            match key {
                "refused" => assert_eq!(value, "GIT_UNAVAILABLE"),
                "stderr_class" => assert!(CLASSES.contains(&value), "{line}"),
                "stderr_keyed_blake3" => assert_eq!(value, digest),
                "stderr_file" => {
                    let file = std::fs::canonicalize(state)
                        .unwrap()
                        .join("stderr")
                        .join(format!("{digest}.log"));
                    assert_eq!(Path::new(value), file);
                    assert_eq!(std::fs::read(&file).unwrap(), *payload);
                    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
                    assert_eq!(mode, 0o600);
                }
                _ => {}
            }
        }
        assert!(block.contains("\nstderr_keyed_blake3="), "{block}");
        assert!(block.contains("\nstderr_file="), "{block}");
    }
    for text in [stdout, stderr] {
        assert!(!text.contains("CANARY"), "canary echoed");
        for (_, secret) in payloads {
            if secret.chars().count() >= 6 {
                assert!(!text.contains(secret.as_str()), "secret echoed: {secret:?}");
            }
        }
    }
    // The verb's stderr is its refusal line and the M2 W2 counters line
    // (`counters verb=... side=... scope=process <name>=<number> ...`), which
    // carries only the verb's own measurements.
    for line in stderr.lines() {
        if let Some(fields) =
            line.strip_prefix("counters verb=git-carry-estimate side=local scope=process ")
        {
            for field in fields.split(' ') {
                let (name, value) = field.split_once('=').unwrap();
                assert!(
                    name.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                    "{line}"
                );
                assert!(value.bytes().all(|b| b.is_ascii_digit()), "{line}");
            }
        } else {
            assert_eq!(line, "bulkload-agent: refused: GIT_UNAVAILABLE", "{stderr}");
        }
    }
}

/// Every r3–r6 probe, with a canary appended, as `(payload, secret)`.
fn payloads() -> Vec<(String, Vec<u8>, String)> {
    stderr_corpus::corpus()
        .into_iter()
        .enumerate()
        .map(|(index, probe)| {
            let mut input = probe.input;
            input.extend_from_slice(format!("\nCANARY-R-N121-{index}\n").as_bytes());
            (probe.group, input, probe.secret)
        })
        .collect()
}

/// R-N121, full corpus: every r3–r6 probe is kept through a real store and
/// classified; the refusal built from it prints only R-N121 lines, and
/// neither those lines, `Display` nor `Debug` hold its canary or secret.
#[test]
fn no_corpus_probe_reaches_a_refusal_line() {
    use bulkload_agent::git_carry::estimate::{
        Refused, StderrClass, StderrReceipt, StderrStore, CLASSIFY_LIMIT,
    };
    use bulkload_agent::BulkloadRefusal;
    let root = scratch("corpus");
    let state = state_dir(&root.0, "state");
    let store = StderrStore::open(&state).unwrap();
    let key = key(&state);
    let payloads = payloads();
    assert!(payloads.len() > 2000, "{}", payloads.len());
    for (group, input, secret) in &payloads {
        let mut capture = store.capture().unwrap();
        capture.write(input).unwrap();
        let (digest, file) = store.commit(capture).unwrap();
        assert_eq!(digest, blake3::keyed_hash(&key, input).to_hex().to_string());
        assert_eq!(std::fs::read(&file).unwrap(), *input);
        let head = &input[..input.len().min(CLASSIFY_LIMIT)];
        let refused = Refused {
            refusal: BulkloadRefusal::GitUnavailable,
            reason: None,
            stderr: Some(StderrReceipt {
                class: StderrClass::of(head),
                keyed_blake3: Some(digest.clone()),
                file: Some(file),
                file_refused: None,
            }),
        };
        let lines = refused.lines();
        for line in &lines {
            let (key, value) = line.split_once('=').unwrap();
            assert!(REFUSED_KEYS.contains(&key), "{group}: {line:?}");
            if key == "stderr_class" {
                assert!(CLASSES.contains(&value), "{group}: {line}");
            }
        }
        let shown = format!("{}\n{refused}\n{refused:?}", lines.join("\n"));
        assert!(!shown.contains("CANARY"), "{group}: canary echoed");
        if secret.chars().count() >= 6 {
            assert!(!shown.contains(secret.as_str()), "{group}: secret echoed");
        }
        assert!(shown.contains(&digest));
    }
}

/// B2: under an invalid locale the shell would warn through `setlocale`
/// ("... No such file or directory"). Every child runs with `LC_ALL=C`, so
/// a Git that is too old (no stderr of its own) is refused without any
/// stderr class, and never as `not_a_repository`.
#[test]
fn an_invalid_locale_never_classifies_a_refusal() {
    let root = scratch("locale");
    let source = repo(&root.0, "source");
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = root.0.join("old-git");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'git version 2.44.0'; exit 0; fi\nexec '{}' \"$@\"\n",
            real.trim()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env("PATH", path)
        .env("LC_ALL", "xx_XX.UTF-8")
        .env("LANG", "xx_XX.UTF-8")
        .env("LANGUAGE", "xx")
        .arg("git-carry-estimate")
        .arg(&source)
        .arg(&source)
        .output()
        .unwrap();
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("\nrefused=GIT_UNAVAILABLE"), "{stdout}");
    assert!(!stdout.contains("stderr_class="), "{stdout}");
}

/// R-N121, end to end: probes fed as a fake `git`'s stderr (local probe) and
/// as a fake `ssh`'s stderr (remote probe) never reach stdout, stderr or the
/// receipt; the raw bytes reach only the 0600 file under the state dir. It
/// takes every end-to-end and keep probe and a stride over the rest, because
/// each pair spawns the probe (and, remotely, a real source probe).
#[test]
fn no_stderr_byte_reaches_any_output_line() {
    let root = scratch("no-echo");
    let source = repo(&root.0, "source");
    let payload_dir = root.0.join("payloads");
    std::fs::create_dir(&payload_dir).unwrap();
    let all = payloads();
    let chosen: Vec<(Vec<u8>, String)> = all
        .iter()
        .enumerate()
        .filter(|(index, (group, _, _))| group.starts_with("e2e") || index % 80 == 0)
        .map(|(_, (_, input, secret))| (input.clone(), secret.clone()))
        .collect();
    for (index, (input, _)) in chosen.iter().enumerate() {
        std::fs::write(payload_dir.join(format!("payload-{index}")), input).unwrap();
    }
    let dir = payload_dir.display();
    // Local: the source probe's `git version` fails with payload N.
    let fake_git = root.0.join("fake-git");
    std::fs::create_dir(&fake_git).unwrap();
    std::fs::write(payload_dir.join("counter"), "0").unwrap();
    script(
        &fake_git.join("git"),
        &format!(
            "#!/bin/sh\nn=$(cat '{dir}/counter')\necho $((n + 1)) > '{dir}/counter'\ncat '{dir}/payload-'\"$n\" >&2\nexit 1\n"
        ),
    );
    let state = state_dir(&root.0, "state-local");
    let pairs: Vec<(PathBuf, String)> = (0..chosen.len())
        .map(|_| (source.clone(), source.display().to_string()))
        .collect();
    let (stdout, stderr) = run_pairs(&fake_git, &state, &pairs);
    assert_no_echo(&state, &chosen, &stdout, &stderr);
    // Remote: a real source probe, then `ssh` fails with payload N, where N
    // is the last path segment of the destination.
    let fake_ssh = root.0.join("fake-ssh");
    std::fs::create_dir(&fake_ssh).unwrap();
    script(
        &fake_ssh.join("ssh"),
        &format!(
            "#!/bin/sh\neval \"last=\\${{$#}}\"\nn=${{last##*/}}\nn=${{n%\\'}}\ncat '{dir}/payload-'\"$n\" >&2\nexit 255\n"
        ),
    );
    let state = state_dir(&root.0, "state-remote");
    let pairs: Vec<(PathBuf, String)> = (0..chosen.len())
        .map(|index| (source.clone(), format!("fakehost:/srv/r/{index}")))
        .collect();
    let (stdout, stderr) = run_pairs(&fake_ssh, &state, &pairs);
    assert_no_echo(&state, &chosen, &stdout, &stderr);
}
