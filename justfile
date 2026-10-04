set shell := ["bash", "-euo", "pipefail", "-c"]
export PYTHONDONTWRITEBYTECODE := "1"

root := justfile_directory()

_default:
    @just --list --unsorted

build:
    cd {{ root }} && just flywheel-build //:bulkload

test:
    cd {{ root }} && just flywheel-test //:tests

# Explicit source-only fallback for an unattached development shell. This is
# not GloriousFlywheel enrollment or CI evidence.
test-local:
    cd {{ root }} && bazelisk test //:tests

repo-manifest-validate:
    cd {{ root }} && python3 scripts/validate_repo_manifest.py tinyland.repo.json

python-lint:
    cd {{ root }} && ruff check scripts tests
    cd {{ root }} && ruff format --check scripts tests

shell-lint:
    cd {{ root }} && shellcheck scripts/ci-public-read-guard.sh

workflow-lint:
    cd {{ root }} && actionlint .github/workflows/*.yml

secrets-scan-dir:
    cd {{ root }} && if command -v gitleaks >/dev/null 2>&1; then gitleaks dir --config .gitleaks.toml --redact .; else echo "gitleaks unavailable; skipped local scan"; fi

secrets-scan-history:
    cd {{ root }} && command -v gitleaks >/dev/null 2>&1
    cd {{ root }} && gitleaks git --config .gitleaks.toml --redact .

# Rust gates for the workspace (fmt, clippy with warnings denied, tests
# including the agent's dependency-wall test). The io layer's syscall trace
# (R-N88) is linted, and its crash-state proofs run with `io-trace` on, ahead
# of the workspace tests so no unrelated failure can hide them; the partial-
# write proof sets a process-wide RLIMIT_FSIZE, so it runs alone. The W7 fault
# harness is not here: it runs in its own CI gate (`just fault-harness`,
# R-N122).
rust-check:
    cd {{ root }} && cargo fmt --all -- --check
    cd {{ root }} && cargo clippy --workspace --all-targets --locked -- -D warnings
    cd {{ root }} && cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings
    cd {{ root }} && cargo test -p bulkload-agent --lib --locked --features io-trace io::
    cd {{ root }} && {{ just_executable() }} io-partial-write-alone
    cd {{ root }} && cargo test --workspace --locked

# P5 partial-write proof (#69). It sets a process-wide RLIMIT_FSIZE, so it runs
# alone, and it acts only when BULKLOAD_IO_PARTIAL_WRITE_ALONE is set. A skipped
# proof still reports `1 passed`, so this recipe fails unless the proof really
# ran: on a cargo failure, on SKIPPED, or on anything but one `1 passed; 0
# failed` result (R-N122).
io-partial-write-alone:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    status=0
    output=$(BULKLOAD_IO_PARTIAL_WRITE_ALONE=1 cargo test -p bulkload-agent --lib --locked --features io-trace io::tests::traced::partial_write_prefix_is_traced -- --ignored --exact --test-threads=1 --nocapture 2>&1) || status=$?
    printf '%s\n' "$output"
    if [[ $status -ne 0 ]]; then
        echo "io-partial-write-alone: cargo test failed with status $status" >&2
        exit "$status"
    fi
    if [[ $output == *SKIPPED* ]]; then
        echo "io-partial-write-alone: the P5 proof skipped itself" >&2
        exit 1
    fi
    results=$(grep -c '^test result: ' <<<"$output" || true)
    passed=$(grep -c '^test result: ok\. 1 passed; 0 failed;' <<<"$output" || true)
    proved=$(grep -c '^test io::tests::traced::partial_write_prefix_is_traced \.\.\. ok$' <<<"$output" || true)
    if [[ $results -ne 1 || $passed -ne 1 ]]; then
        echo "io-partial-write-alone: expected exactly one '1 passed; 0 failed' result" >&2
        exit 1
    fi
    if [[ $proved -ne 1 ]]; then
        echo "io-partial-write-alone: the passing test was not the P5 proof" >&2
        exit 1
    fi

# W7 crash-resume and live-writer harness, which needs the agent's
# `fault-injection` feature; the R-N88 power-loss harness, which runs the
# crash-state checker on the syscall trace of a real copy and needs
# `io-trace`; and the in-crate directory resume proofs (#74 review B1 and
# round 2 N1). CI runs all three as the separate `fault-harness` terminal
# gate, in parallel with the source gate and under its own 15-minute cap
# (R-N122). The harness builds go to their own target dir, so
# `target/debug/bulkload-agent` is never replaced by a fault-enabled binary.
# The feature clippy pass stays in the shared dir: it only type-checks and
# writes no executables.
fault-harness:
    cd {{ root }} && cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings
    cd {{ root }} && cargo test -p bulkload-agent --locked --features fault-injection --target-dir target/fault --test fault_harness
    cd {{ root }} && cargo test -p bulkload-agent --locked --features io-trace --target-dir target/fault --test power_loss
    cd {{ root }} && {{ just_executable() }} resume-power-loss

# Directory resume power-loss proofs (#74 review B1 and round 2 N1), lib tests
# that need `io-trace`. A name filter that matches nothing still reports `0
# passed`, so this recipe fails unless both proofs ran: on a cargo failure, on
# anything but one `2 passed; 0 failed` result, or when either proof is not
# among the passing tests (#74 round 2 N2, R-N122).
resume-power-loss:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    status=0
    output=$(cargo test -p bulkload-agent --lib --locked --features io-trace --target-dir target/fault materialize::adoption_power_loss:: 2>&1) || status=$?
    printf '%s\n' "$output"
    if [[ $status -ne 0 ]]; then
        echo "resume-power-loss: cargo test failed with status $status" >&2
        exit "$status"
    fi
    results=$(grep -c '^test result: ' <<<"$output" || true)
    passed=$(grep -c '^test result: ok\. 2 passed; 0 failed;' <<<"$output" || true)
    if [[ $results -ne 1 || $passed -ne 1 ]]; then
        echo "resume-power-loss: expected exactly one '2 passed; 0 failed' result" >&2
        exit 1
    fi
    for proof in an_adopted_fallback_directory_is_sealed_before_its_record_binds a_directory_adopted_by_its_bound_record_is_sealed_before_outputs_commit; do
        if [[ $(grep -c "^test materialize::adoption_power_loss::$proof \.\.\. ok$" <<<"$output" || true) -ne 1 ]]; then
            echo "resume-power-loss: $proof was not among the passing tests" >&2
            exit 1
        fi
    done

# Chunker micro-bench (M2 W4): fused slice-FastCDC + BLAKE3 against the
# current hash.rs path. Release build; size via BULKLOAD_CHUNKER_BENCH_MIB.
bench-io-chunker:
    cd {{ root }} && cargo test --release -p bulkload-agent --lib --locked io::chunker::tests::chunker_micro_bench -- --ignored --nocapture --test-threads=1

# bulkload-bench built at --rev-b (origin/main, the candidate) and --rev-a
# (7c3ecc7, informational), each rep the full R23 bench with the rclone
# baseline, plus one v4 (41bf9a4) native rep for dedup loss. B passes R23
# iff every B rep's verdict passes. Gated runs only on neo, AC power, load1 < 2.5,
# lanes quiet (R-N81, R-N91); --dry-run makes a synthetic corpus and is NOT a
# gate sample.
# #88 gate (a) / R23 B/A/B/A/B harness (OI-1002-Q30, OI-1002-Q27)
bench-r23-ab *args:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/r23_ab.py {{ args }}

# `generate OUT` writes OUT/corpus plus a protected README; `verify CORPUS`
# checks the committed manifest and content identity f4a7619f...; `seal OUT`
# makes a generated copy read-only (0444/0555). Byte-identical on every host:
# SHAKE-256 counter mode from a fixed seed, no live files.
# R23 corpus v1 generator / verifier (OI-1002-Q28)
bench-r23-corpus *args:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/r23_corpus.py {{ args }}

flake-check:
    cd {{ root }} && nix flake check --no-build --no-write-lock-file

# Source gates owned by this repository's development shell.
check-source: repo-manifest-validate python-lint shell-lint workflow-lint secrets-scan-dir rust-check

# Local-first test tiers (OI-1001-Q2, 2026-10-01). GloriousFlywheel CI is under
# contention, so these run locally first and CI is the backstop. Run them
# inside `nix develop` so actionlint and gitleaks are on PATH.
#   check-fast      mandatory tier: every ratified-contract guard (R23/R25/
#                   R-N58 resume counters, durability and the R-N88/R-N119
#                   power-loss proofs, refusal taxonomy, R34 dependency wall,
#                   R33 lint wall, CI contract). PR CI runs this tier through
#                   its source, fault-harness and test gates.
#   check-optional  optional tier: spike evidence, bench-script stubs, the
#                   history secret scan and the Nix/Bazel graph. On demand.
#   check-full      both tiers (the lab `test-presubmit` / xoxd.ai `ci` shape).

# The repository and CI contract tests, run directly instead of through
# Bazel's //:tests (CI's `test` gate runs the same two files).
contract-test:
    cd {{ root }} && python3 scripts/validate_repo_manifest.py --self-test tinyland.repo.json
    cd {{ root }} && python3 tests/test_ci_contract.py

# Mandatory tier, local-first: what PR CI runs (OI-1001-Q2).
check-fast: check-source fault-harness contract-test

# Optional tier: on demand, never a PR gate (OI-1001-Q2).
check-optional:
    cd {{ root }} && cargo clippy -p bulkload-agent --all-targets --locked --features m1-spike -- -D warnings
    cd {{ root }} && cargo test -p bulkload-agent --locked --features m1-spike --test git_m1_spike
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_m0_gate_a.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_r23_ab.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_r23_corpus.py
    cd {{ root }} && {{ just_executable() }} secrets-scan-history
    cd {{ root }} && {{ just_executable() }} flake-check
    cd {{ root }} && {{ just_executable() }} test-local

# Both tiers.
check-full: check-fast check-optional

# Normal attached gate: materialize the repo tools, then use the Flywheel
# wrapper for the Bazel graph.
check:
    cd {{ root }} && nix develop .#default --command just check-source
    cd {{ root }} && nix develop .#default --command just fault-harness
    cd {{ root }} && just test

# Source-only local gate. It may not be cited as cache or runner proof.
check-local: check-source test-local

# Toolchain-complete source gates for CI. The repo flake owns these linters and
# scanners; the pinned GloriousFlywheel shell owns the front door and Bazel.
ci-source: check-source secrets-scan-history

# The CI `fault-harness` terminal gate (R-N122); the composite action execs it
# inside the repo flake, like `ci-source`.
ci-fault-harness: fault-harness

# CI is fail-closed on the fleet-managed GloriousFlywheel profile and drives
# every Bazel target through the canonical cache-backed wrapper.
ci:
    cd {{ root }} && just flywheel-verify
    cd {{ root }} && just flake-check
    cd {{ root }} && nix develop .#default --command just ci-source
    cd {{ root }} && nix develop .#default --command just ci-fault-harness
    cd {{ root }} && just flywheel-build //:bulkload
    cd {{ root }} && just flywheel-test //:tests

import? "justfile.flywheel"

# `run --source SRC --out OUT -- <bulkload command>` runs the v0 reference
# workload (JSONL+fsync, SQLite WAL, git status+diff, rg) at 1 Hz in a sibling
# directory on the source's device, samples load1 at 1 Hz, and alternates
# OFF/ON/OFF/ON/OFF windows of 300 s; the verdict is PASS (the bootstrap bounds
# of d_p95, on step and on each operation, and of the lag-corrected d_load1 are
# within +25 % and +2.0), FAIL (beyond them) or INCONCLUSIVE (bounds straddle,
# gates disagree, noise floor over half the budget, or too few samples). `--aa`
# is the A/A noise mode, `analyze TRACE` re-derives a verdict and `selftest`
# runs the synthetic-trace tests. NOT EVIDENCE unless --evidence with the whole
# SLO protocol; gated runs are separate from the S1 gate. Put an ON command that
# needs shell quoting in a script file: just re-joins arguments.
# S2 measured budget instrument (OI-1003-Q34)
bench-s2-budget *args:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/s2_budget.py {{ args }}
