//! `bulkload-handoff`: the credential-class handoff proof, as its own binary.
//!
//! It moved out of `bulkload-agent` (WP10, OI-1003-Q14) so the source-side
//! engine binary never spawns sops, kubectl, gpg, gh, claude or codex.
//! Argument parsing is hand-rolled, as in the agent.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bulkload_agent::refuse::RefuseAt as _;
use bulkload_handoff::{self as handoff, Outcome};
use bulkload_proto::{BulkloadRefusal, Result};

const USAGE: &str = "\
bulkload-handoff -- credential-class handoff proof (R-N3)

USAGE:
    bulkload-handoff verify [--json PATH] [--sops-fixture PATH] [--state-root PATH]
                            [--probe-timeout SECONDS] [--skip-ssh-host ALIAS]...
                Prove each credential class (sops, kubeconfig, ssh, gpg, gh,
                git, claude, codex) and emit a tcfs.bulkload.handoff.v1 receipt

NOTES:
    The probes never signal a child process (R-N11). Receipt evidence is exit
    statuses, counts and operator-known identifiers only.
";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let outcome = match args.next().as_ref().and_then(|value| value.to_str()) {
        Some("verify") => verify(&args.collect::<Vec<_>>()),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("bulkload-handoff: refused: {refusal}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `verify` flags, run the probe set, and emit the receipt.
///
/// The table always goes to stdout, pass or fail: an operator reading a failed
/// handoff needs the measurements more than a clean exit. `--json PATH` writes
/// the machine-readable receipt beside it.
fn verify(args: &[std::ffi::OsString]) -> Result<()> {
    let mut options = handoff::Options::from_environment();
    let mut state_root = std::env::temp_dir();
    let mut receipt_path: Option<PathBuf> = None;
    let mut index = 0_usize;
    while let Some(flag) = args.get(index) {
        let value = || {
            args.get(index + 1)
                .ok_or(BulkloadRefusal::RequiredFieldMissing)
        };
        match flag.to_str() {
            Some("--json") => receipt_path = Some(PathBuf::from(value()?)),
            Some("--sops-fixture") => options.sops_fixture = Some(PathBuf::from(value()?)),
            Some("--state-root") => state_root = PathBuf::from(value()?),
            Some("--skip-ssh-host") => {
                let alias = value()?
                    .to_str()
                    .ok_or(BulkloadRefusal::PathNotPortable)?
                    .to_owned();
                options.ssh_exclude.insert(alias);
            }
            Some("--probe-timeout") => options.probe_timeout = seconds(value()?)?,
            _ => return Err(BulkloadRefusal::FieldDomainViolation),
        }
        index += 2;
    }

    let probes = handoff::verify(&state_root, &options)?;
    print!("{}", handoff::render_table(&probes));
    let receipt = handoff::receipt(probes);
    if let Some(path) = receipt_path {
        write_receipt(&path, handoff::render_json(&receipt).as_bytes())?;
        println!("receipt  {}", path.display());
    }
    if handoff::summarize(&receipt.probes).verdict == Outcome::Pass {
        Ok(())
    } else {
        Err(BulkloadRefusal::ProbeFailed)
    }
}

/// A whole-second duration flag value.
fn seconds(value: &std::ffi::OsString) -> Result<std::time::Duration> {
    let parsed: u64 = value
        .to_str()
        .ok_or(BulkloadRefusal::PathNotPortable)?
        .parse()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    Ok(std::time::Duration::from_secs(parsed))
}

/// Write the receipt and make it durable before reporting its path.
fn write_receipt(path: &Path, payload: &[u8]) -> Result<()> {
    const SITE: &str = "handoff::main::write_receipt";
    let mut file = std::fs::File::create(path).refuse_at(SITE)?;
    file.write_all(payload).refuse_at(SITE)?;
    bulkload_agent::counters::sync_full(&file).refuse_at(SITE)?;
    Ok(())
}
