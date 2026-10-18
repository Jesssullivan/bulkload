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
    cd {{ root }} && shellcheck scripts/ci-public-read-guard.sh scripts/install-agent.sh scripts/local-gate-receipt.sh scripts/local-gate-verify.sh scripts/darwin-touched.sh

# Build a release bulkload-agent and install it as DEST_DIR/NAME with a GC
# root for every /nix/store path it names, so a Nix garbage collection cannot
# delete its interpreter or libraries (M2 item 4). The selftest it runs last
# reports whether its interpreter is present and rooted. Never replaces NAME.
install-agent DEST_DIR NAME:
    cd {{ root }} && cargo build --release --locked -p bulkload-agent
    cd {{ root }} && scripts/install-agent.sh "${CARGO_TARGET_DIR:-target}/release/bulkload-agent" "{{ DEST_DIR }}" "{{ NAME }}"

workflow-lint:
    cd {{ root }} && actionlint .github/workflows/*.yml

secrets-scan-dir:
    cd {{ root }} && if command -v gitleaks >/dev/null 2>&1; then gitleaks dir --config .gitleaks.toml --redact .; else echo "gitleaks unavailable; skipped local scan"; fi

secrets-scan-history:
    cd {{ root }} && command -v gitleaks >/dev/null 2>&1
    cd {{ root }} && gitleaks git --config .gitleaks.toml --redact .

# The merge gate's history scan (OI-1003-Q199): `secrets-scan-history`
# limited to HEAD's ancestry, the history the PR would merge. A local
# checkout also holds refs no PR contains (backup branches, other lanes'
# work), which CI's fresh clone never sees; scanning them failed #236's gate
# on a commit it does not contain.
secrets-scan-head:
    cd {{ root }} && command -v gitleaks >/dev/null 2>&1
    cd {{ root }} && gitleaks git --config .gitleaks.toml --redact --log-opts=HEAD .

# Rust gates for the workspace (fmt, clippy with warnings denied, tests
# including the agent's dependency-wall test). The io layer's syscall trace
# (R-N88) is linted here with `io-trace` alone, the one place PR CI compiles
# that configuration. Its tests are not here: the traced `io::` lib tests and
# the P5 partial-write proof run in the fault gate's feature-union build
# (`just fault-harness`, R-N122), so this gate builds the agent once
# (OI-1003-Q81).
rust-check:
    cd {{ root }} && cargo fmt --all -- --check
    cd {{ root }} && cargo clippy --workspace --all-targets --locked -- -D warnings
    cd {{ root }} && cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings
    cd {{ root }} && {{ just_executable() }} rust-test

# The workspace tests: what `cargo test --workspace --locked` runs, built
# once and then run as up to five concurrent cargo invocations (OI-1003-Q81;
# no new tool). The groups are the unit tests (`--lib --bins`), the doc
# tests (`--doc`), and the integration binaries the build made, read from
# its own `Executable tests/<name>.rs` lines and split by the first letter
# of the name: a to e, f to g, and every other name. So a new integration
# test joins a group without an edit here, and a target the build skipped
# for a missing feature is not asked for. Each group's output is printed
# whole, in order, once it ends. The recipe fails if any group fails, and
# unless the groups ran exactly as many test executables as the build made:
# a binary no group selects cannot pass silently (R-N122).
rust-test:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    status=0
    build=$(cargo test --workspace --locked --no-run 2>&1) || status=$?
    printf '%s\n' "$build"
    if [[ $status -ne 0 ]]; then
        echo "rust-test: the test build failed with status $status" >&2
        exit "$status"
    fi
    built=$(grep -c '^ *Executable ' <<<"$build" || true)
    early=()
    middle=()
    late=()
    while IFS= read -r name; do
        case $name in
            [a-e]*) early+=(--test "$name") ;;
            [f-g]*) middle+=(--test "$name") ;;
            *) late+=(--test "$name") ;;
        esac
    done < <(sed -n 's|^ *Executable tests/\(.*\)\.rs (.*)$|\1|p' <<<"$build")
    logs=$(mktemp -d "${TMPDIR:-/tmp}/rust-test.XXXXXX")
    trap 'rm -rf "$logs"' EXIT
    groups=()
    start() {
        local name=$1
        shift
        cargo test --workspace --locked "$@" >"$logs/$name" 2>&1 &
        groups+=("$!:$name")
    }
    start 1-unit --lib --bins
    if [[ ${#early[@]} -gt 0 ]]; then
        start 2-integration-a-e "${early[@]}"
    fi
    if [[ ${#middle[@]} -gt 0 ]]; then
        start 3-integration-f-g "${middle[@]}"
    fi
    if [[ ${#late[@]} -gt 0 ]]; then
        start 4-integration-rest "${late[@]}"
    fi
    start 5-doc --doc
    failed=0
    for group in "${groups[@]}"; do
        status=0
        wait "${group%%:*}" || status=$?
        echo "rust-test: group ${group#*:} ended with status $status"
        cat "$logs/${group#*:}"
        if [[ $status -ne 0 ]]; then
            failed=1
        fi
    done
    if [[ $failed -ne 0 ]]; then
        echo "rust-test: a test group failed" >&2
        exit 1
    fi
    ran=$(cat "$logs"/* | grep -c '^ *Running ' || true)
    if [[ $built -eq 0 || $ran -ne $built ]]; then
        echo "rust-test: the build made $built test executables but the groups ran $ran" >&2
        exit 1
    fi
    echo "rust-test: all $built test executables ran, in ${#groups[@]} groups"

# P5 partial-write proof (#69). It sets a process-wide RLIMIT_FSIZE, so it runs
# alone, and it acts only when BULKLOAD_IO_PARTIAL_WRITE_ALONE is set. A skipped
# proof still reports `1 passed`, so this recipe fails unless the proof really
# ran: on a cargo failure, on SKIPPED, or on anything but one `1 passed; 0
# failed` result (R-N122). It runs from `fault-harness`, in that gate's
# feature-union build (OI-1003-Q81).
io-partial-write-alone:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    status=0
    output=$(BULKLOAD_IO_PARTIAL_WRITE_ALONE=1 cargo test -p bulkload-agent --lib --locked --features fault-injection,io-trace --target-dir target/fault io::tests::traced::partial_write_prefix_is_traced -- --ignored --exact --test-threads=1 --nocapture 2>&1) || status=$?
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

# The fault gate. One feature-union build (`fault-injection` and `io-trace`,
# OI-1003-Q81) in its own target dir runs: the W7 crash-resume and
# live-writer harness, which needs `fault-injection`; the R-N88 power-loss
# harness, which runs the crash-state checker on the syscall trace of a real
# copy and needs `io-trace`; the `io::` lib tests with the trace compiled in
# (the traced tests and the crash checker's); the P5 partial-write proof,
# alone; and the in-crate directory resume proofs (#74 review B1 and round 2
# N1). Both features are inert until a test arms them: a fault point fires
# only under `BULKLOAD_FAULT`, and a traced call records only while a
# recorder is attached. Then the same suites in the hand-off's off arm
# (`handoff-off`) and in the delta lever's on arm (`delta-lever`). CI runs
# this as the separate `fault-harness`
# terminal gate, in parallel with the source gate (R-N122). The harness
# builds go to their own target dir, so `target/debug/bulkload-agent` is
# never replaced by a fault-enabled binary. The feature clippy pass stays in
# the shared dir: it only type-checks and writes no executables.
fault-harness:
    cd {{ root }} && cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings
    cd {{ root }} && cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test fault_harness
    cd {{ root }} && cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test power_loss
    cd {{ root }} && cargo test -p bulkload-agent --lib --locked --features fault-injection,io-trace --target-dir target/fault io::
    cd {{ root }} && {{ just_executable() }} io-partial-write-alone
    cd {{ root }} && {{ just_executable() }} resume-power-loss
    cd {{ root }} && {{ just_executable() }} handoff-off
    cd {{ root }} && {{ just_executable() }} delta-lever

# The loopback hand-off's off arm (OI-1003-Q154 review). A local `copy`
# takes the hand-off by default (levers 1 and 2: handed chunks, not
# hashed again on the destination, no per-frame flush), so every
# copy-based crash, power-loss and resume suite exercises that path only.
# This arm runs them again with `BULKLOAD_LOOPBACK_HANDOFF=0`: the plain
# protocol that `pull` runs across hosts, with the destination's
# worker-side verify, the inline verify before a stage, and every frame
# flushed as written. Crash children inherit the variable. `w3_engine`
# runs first in the same environment and proves the arm took: it asserts
# `blake3_dest_wire_verify_bytes == bytes_received` and
# `wire_handed_bytes == 0` when the variable is `0`.
handoff-off:
    cd {{ root }} && BULKLOAD_LOOPBACK_HANDOFF=0 cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test w3_engine
    cd {{ root }} && BULKLOAD_LOOPBACK_HANDOFF=0 cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test fault_harness --test power_loss --test s3_transfer_resume --test sqlite_carry
    cd {{ root }} && BULKLOAD_LOOPBACK_HANDOFF=0 {{ just_executable() }} resume-power-loss

# The delta lever's on arm (OI-1003-Q154 review). The lever is off by
# default, so every copy-based crash, power-loss and resume suite decides a
# new path in a populated destination manifest-first only. This arm runs
# them again with `BULKLOAD_LOCAL_NEW_PATH_STREAM=1`: such a seat streamed,
# unless a reuse row, a salvaged temporary or the source's ledger keeps it
# on its manifest. Crash children inherit the variable. `loopback_handoff`
# runs first in the same environment and proves the arm took: its default
# arm asserts the new path streamed when the variable is `1`.
# `s3_transfer_resume` and `w3_engine` are left out: with the lever on they
# fail only S3's inequality 2 as they prove it on `copy` (handed bytes
# counted as received), the operator's question in the 2026-10-16
# amendment of docs/slo.md.
delta-lever:
    cd {{ root }} && BULKLOAD_LOCAL_NEW_PATH_STREAM=1 cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test loopback_handoff
    cd {{ root }} && BULKLOAD_LOCAL_NEW_PATH_STREAM=1 cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test fault_harness --test power_loss --test estate_converge --test sqlite_carry
    cd {{ root }} && BULKLOAD_LOCAL_NEW_PATH_STREAM=1 {{ just_executable() }} resume-power-loss

# Resume power-loss proofs, lib tests that need `io-trace`, run in the fault
# gate's feature-union build (OI-1003-Q81): the directory
# resume paths (#74 review B1 and round 2 N1) and the adoption of a durable
# unrowed output from its capture record with 0 source bytes read (#169,
# R-N58), the batched directory levels (OI-1003-Q143 item 2: a level
# crashed after its commit, after some renames, in its finish, and a batch
# that must wait for a held adoptable parent, #217 review), and on Linux the
# batched group seal (S1, OI-1003-Q107; compiled for Linux only, where it
# fails rather than skips). A name filter that matches nothing still reports
# `0 passed`, so this recipe fails unless every proof for this platform ran:
# on a cargo failure, on anything but one `N passed; 0 failed` result (N = 8
# on Linux, 7 elsewhere), or when a proof
# is not among the passing tests (#74 round 2 N2, R-N122).
resume-power-loss:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    status=0
    output=$(cargo test -p bulkload-agent --lib --locked --features fault-injection,io-trace --target-dir target/fault materialize::adoption_power_loss:: 2>&1) || status=$?
    printf '%s\n' "$output"
    if [[ $status -ne 0 ]]; then
        echo "resume-power-loss: cargo test failed with status $status" >&2
        exit "$status"
    fi
    proofs=(an_adopted_fallback_directory_is_sealed_before_its_record_binds a_directory_adopted_by_its_bound_record_is_sealed_before_outputs_commit an_unrowed_output_is_adopted_without_source_reads a_level_crashed_after_its_commit_is_swept_and_its_records_cleared a_level_crashed_after_some_renames_adopts_the_renamed_and_makes_the_rest a_finish_crashed_after_some_seals_completes_every_directory_in_one_commit a_batch_waits_for_a_held_adoptable_directory_before_making_inside_it)
    if [[ $(uname -s) == Linux ]]; then
        proofs+=(a_batched_group_names_no_output_before_its_data_is_durable)
    fi
    results=$(grep -c '^test result: ' <<<"$output" || true)
    passed=$(grep -c "^test result: ok\. ${#proofs[@]} passed; 0 failed;" <<<"$output" || true)
    if [[ $results -ne 1 || $passed -ne 1 ]]; then
        echo "resume-power-loss: expected exactly one '${#proofs[@]} passed; 0 failed' result" >&2
        exit 1
    fi
    for proof in "${proofs[@]}"; do
        if [[ $(grep -c "^test materialize::adoption_power_loss::$proof \.\.\. ok$" <<<"$output" || true) -ne 1 ]]; then
            echo "resume-power-loss: $proof was not among the passing tests" >&2
            exit 1
        fi
    done

# Darwin check (S1 stack review, 2026-10-15, finding 3). The gates run on
# Linux only, so the Darwin code (`io/sys_darwin.rs`, the `apple` and
# `not(linux)` branches) is compiled and tested nowhere else. Run it on pzm,
# never on neo (OI-1003-Q196), before merging a change that touches that
# code (`just darwin-touched` says which do; OI-1003-Q199): the rust-check
# passes (fmt, both clippy passes, the workspace tests) and the whole fault
# harness, as on Linux. If pzm is on hold, the check is deferred and the
# deferral recorded with the PR's receipt.
darwin-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != Darwin ]]; then
        echo "darwin-check: run this on a Darwin host" >&2
        exit 2
    fi
    cd {{ root }}
    {{ just_executable() }} rust-check
    {{ just_executable() }} fault-harness

# Chunker micro-bench (M2 W4): fused slice-FastCDC + BLAKE3 against the
# current hash.rs path. Release build; size via BULKLOAD_CHUNKER_BENCH_MIB.
bench-io-chunker:
    cd {{ root }} && cargo test --release -p bulkload-agent --lib --locked io::chunker::tests::chunker_micro_bench -- --ignored --nocapture --test-threads=1

# bulkload-bench built at --rev-b (origin/main, the candidate) and --rev-a
# (informational: the rig's pinned A on a rig of record, OI-1003-Q103; else
# 7c3ecc7), each rep the full R23 bench with the flake-pinned rclone
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

# Git-heavy, many-small-file trees for S1 and S3, deterministic from (seed,
# scale) and sealed by the SHAKE-256 of their manifest. DEST must be new and
# absolute (for example under $TMPDIR). Check a copy with `estate_corpus.py
# verify DEST`; the S3 knob is `estate_corpus.py mutate DEST N`; `seal DEST`
# makes a kept copy read-only (0444/0555) and writes VERIFY-RECEIPT.json.
# Estate-shaped corpus generator, WP0(e) (OI-1003-Q19)
bench-estate-corpus dest seed="bulkload-estate-corpus-v1" scale="small":
    cd {{ root }} && python3 crates/bulkload-bench/scripts/estate_corpus.py generate "{{ dest }}" --seed "{{ seed }}" --scale "{{ scale }}"

# Generates scale=small twice under $TMPDIR, checks that both share one
# identity, then checks verify, the host-path scan, a one-of-each mutate,
# seal and tamper detection, and removes both copies. Optional tier only
# (OI-1003-Q7: CI stays slim).
# Estate corpus round-trip self-test (OI-1003-Q19)
bench-estate-corpus-selftest:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/estate_corpus.py selftest

# `run --agent BIN --work WORK [--scale small|estate]` generates the estate
# corpus in place under a new private WORK, runs a first pass, three unchanged
# reruns and reruns after `mutate 1` and `mutate 10` (copy, snapshot,
# estate-capture, and git-carry-estimate as the v2 projection), evaluates the
# OI-1003-Q18 inequalities and writes WORK/s3-estate.json. `build --out DIR`
# release-builds bulkload-agent at origin/main; `report JSON` renders tables;
# `evaluate JSON --out NEW` re-runs the evaluation of a recorded run into a
# new file. Informational and ungated; never deletes a target.
# S3 estate measurement harness, Sprint 2 lane A (OI-1003-Q35, OI-1003-Q18)
bench-s3-estate *args:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/s3_estate.py {{ args }}

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
#                   its source and fault-harness gates (OI-1003-Q65).
#   check-optional  optional tier: bench-script stubs, the estate corpus
#                   self-test (OI-1003-Q19), the history secret scan and the
#                   Nix/Bazel graph. On demand.
#   check-full      both tiers (the lab `test-presubmit` / xoxd.ai `ci` shape).

# The repository and CI contract tests, run directly instead of through
# Bazel's //:tests (CI runs them through `ci-source` since OI-1003-Q65
# dropped the Bazel `test` gate).
contract-test:
    cd {{ root }} && python3 scripts/validate_repo_manifest.py --self-test tinyland.repo.json
    cd {{ root }} && python3 tests/test_ci_contract.py

# Mandatory tier, local-first (OI-1001-Q2); the core of `local-gate`.
check-fast: check-source fault-harness contract-test

# Bazel graph load (OI-1003-Q199, closing the OI-1003-Q65 residual gap):
# resolve the bzlmod graph against MODULE.bazel.lock and load rules_python,
# the //:bulkload globs and the //:tests suite. A query, not a build: nothing
# is compiled, run or uploaded, and the Bazel
# server exits after 60 idle seconds. The Flywheel wrapper
# (`gloriousflywheel-bazel`) is not on the sting seat's PATH, so this runs
# the flake's bazelisk, as `test-local` does.
bazel-graph:
    cd {{ root }} && bazelisk --max_idle_secs=60 query --lockfile_mode=error 'deps(//:bulkload + //:tests)' --output=label >/dev/null
    @echo "bazel-graph: //:bulkload and //:tests loaded"

# The merge gate (OI-1003-Q199, 2026-10-16), on sting, on the exact PR head
# brought up to date with main. Run it through `just local-gate-receipt`
# (`scripts/local-gate-receipt.sh`), which runs CI's `nix flake check` and
# then this recipe in a clean `nix develop --ignore-environment .#default`,
# as the action does, and writes the receipt the merge needs. It runs
# everything CI's `ci-source` and `ci-fault-harness` run (the contract test
# holds the sets equal), with the history scan limited to HEAD's ancestry
# (`secrets-scan-head`), plus the Bazel graph load. CI runs only on `v*`
# tags and workflow_dispatch, as the backstop. It retires when
# GloriousFlywheel v4 fabric check runs land (TIN-5946).
local-gate: check-fast secrets-scan-head bazel-graph

# Run `local-gate` on HEAD and print its receipt line, led by PASS or FAIL;
# `--pr N --post` also comments it on PR N. See the script's header for the
# checks it makes.
local-gate-receipt *args:
    cd {{ root }} && scripts/local-gate-receipt.sh {{ args }}

# Is PR N's receipt still valid? A receipt holds only while its head is the
# PR head and its main is main's current tip. Run right before
# `gh pr merge --match-head-commit` (OI-1003-Q199).
local-gate-verify *args:
    cd {{ root }} && scripts/local-gate-verify.sh {{ args }}

# Does a diff touch Darwin code? Prints the reasons and a yes/no verdict. A
# yes means the PR needs `darwin-check` on pzm before it merges
# (OI-1003-Q199, OI-1003-Q196). The script exits 0 for yes, 1 for no and 2
# on error; this recipe fails only on error.
darwin-touched base="origin/main" head="HEAD":
    cd {{ root }} && { scripts/darwin-touched.sh "{{ base }}" "{{ head }}" || [[ $? -eq 1 ]]; }

# Optional tier: on demand, never a PR gate (OI-1001-Q2).
check-optional:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_r23_ab.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_r23_corpus.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_s3_estate.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_m3_link.py
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_link_common.py
    cd {{ root }} && {{ just_executable() }} bench-estate-corpus-selftest
    cd {{ root }} && {{ just_executable() }} secrets-scan-history
    cd {{ root }} && {{ just_executable() }} flake-check
    cd {{ root }} && {{ just_executable() }} test-local

# Both tiers.
check-full: check-fast check-optional

# Deep tier (OI-1003-Q7, OI-1003-Q78, OI-1003-Q81): on demand, never a PR
# gate, and in neither tier above, because it runs for many minutes.
# `props-deep` runs the workspace tests with BULKLOAD_PROPTEST_DEEP=1. Every
# property that runs through `test_support::prop_config` draws twenty times
# its PR-gate cases from the same fixed seed, so a deep failure of one
# reproduces with the same command; pin its shape as an explicit test row.
# One property does not run through the helper yet (the EXEMPT entry of
# `tests/prop_seed_guard.rs`): `refusal_taxonomy` keeps its own fixed seed
# and its PR-gate case count. No property draws a random seed. The heavy
# fixed rows that skip themselves in the PR gate run here: REFS-SCALE's
# 131,072 refs, and the distinct-heavy 32,768 and 110,000 commits. A skipped row still reports `ok`, and the workspace
# run captures its output, so each row is left out of that run and then run
# alone. The recipe fails unless each one really ran: on a cargo failure, on
# a `skipped: set` line, on anything but one `1 passed; 0 failed` result
# with the row's own `ok` line, or without the row's measurement line, which
# only its body prints (R-N122).
props-deep:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    export BULKLOAD_PROPTEST_DEEP=1
    cargo test --workspace --locked -- --skip refs_scale_131072_refs_carry_and_the_old_format_is_refused_typed --skip distinct_heavy_32768_refs_import_linearly_and_chain_thin --skip distinct_heavy_110000_refs_chain_falls_back_self_contained --skip an_old_format_capture_of_65536_refs_imports_like_the_new_format
    log=$(mktemp "${TMPDIR:-/tmp}/props-deep.XXXXXX")
    trap 'rm -f "$log"' EXIT
    row() {
        local name=$1 marker=$2 status=0
        shift 2
        cargo test -p bulkload-agent --locked "$@" "$name" -- --exact --nocapture 2>&1 | tee "$log" || status=$?
        if [[ $status -ne 0 ]]; then
            echo "props-deep: $name failed with status $status" >&2
            exit "$status"
        fi
        if grep -q 'skipped: set ' "$log"; then
            echo "props-deep: $name skipped itself" >&2
            exit 1
        fi
        results=$(grep -c '^test result: ' "$log" || true)
        passed=$(grep -c '^test result: ok\. 1 passed; 0 failed;' "$log" || true)
        proved=$(grep -c "^test $name \.\.\. ok$" "$log" || true)
        marked=$(grep -c "^$marker" "$log" || true)
        if [[ $results -ne 1 || $passed -ne 1 || $proved -ne 1 ]]; then
            echo "props-deep: $name: expected exactly one '1 passed; 0 failed' result, the row's own" >&2
            exit 1
        fi
        if [[ $marked -ne 1 ]]; then
            echo "props-deep: $name printed $marked '$marker' lines, not 1: the row did not run" >&2
            exit 1
        fi
    }
    row git_carry::refs_scale_tests::refs_scale_131072_refs_carry_and_the_old_format_is_refused_typed 'REFS-SCALE row=fixed refs=' --lib
    row git_carry::refs_scale_tests::an_old_format_capture_of_65536_refs_imports_like_the_new_format 'REFS-SCALE row=old-65536 refs=' --lib
    row distinct_heavy_32768_refs_import_linearly_and_chain_thin 'REFS-SCALE-DISTINCT row=deep-32768 pass=2 ' --test refs_scale_distinct
    row distinct_heavy_110000_refs_chain_falls_back_self_contained 'REFS-SCALE-DISTINCT row=deep pass=2 ' --test refs_scale_distinct
    echo "props-deep: the workspace and all 4 deep rows ran"

# Deep tier, on demand, never a PR gate: the W7 crash sweep. For every point
# of the fault harness's `scenarios!` table it crashes one copy at every hit
# (nth = 1, 2, ... until the copy runs out of hits), resumes, and holds each
# crash to the pinned scenarios' own checks (I1 to I4, convergence). It
# prints one `crash-sweep: <point> hits=<n>` line per point. The PR gate
# runs only the table's pinned (point, nth) rows. BULKLOAD_CRASH_SWEEP_JOBS
# sets how many points run at a time (default: a quarter of the cores).
# Without BULKLOAD_CRASH_SWEEP=1 the test passes after it prints `skipped`,
# and a name filter that matches nothing reports `0 passed`, so this recipe
# fails unless the sweep really ran: on a cargo failure, on a `skipped`
# line, on anything but one `1 passed; 0 failed` result with the sweep's own
# `ok` line, or without one `<n> points, <m> crashes, 0 failures` line and n
# `hits=` lines with n and every hit count above 0 (R-N122).
crash-sweep:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    log=$(mktemp "${TMPDIR:-/tmp}/crash-sweep.XXXXXX")
    trap 'rm -f "$log"' EXIT
    status=0
    BULKLOAD_CRASH_SWEEP=1 cargo test -p bulkload-agent --locked --features fault-injection,io-trace --target-dir target/fault --test fault_harness crash_sweep_every_point_and_nth -- --exact --nocapture 2>&1 | tee "$log" || status=$?
    if [[ $status -ne 0 ]]; then
        echo "crash-sweep: cargo test failed with status $status" >&2
        exit "$status"
    fi
    if grep -q 'crash sweep skipped' "$log"; then
        echo "crash-sweep: the sweep skipped itself" >&2
        exit 1
    fi
    results=$(grep -c '^test result: ' "$log" || true)
    passed=$(grep -c '^test result: ok\. 1 passed; 0 failed;' "$log" || true)
    proved=$(grep -c '^test crash_sweep_every_point_and_nth \.\.\. ok$' "$log" || true)
    if [[ $results -ne 1 || $passed -ne 1 || $proved -ne 1 ]]; then
        echo "crash-sweep: expected exactly one '1 passed; 0 failed' result, the sweep's own" >&2
        exit 1
    fi
    points=$(sed -n 's/^crash-sweep: \([1-9][0-9]*\) points, [1-9][0-9]* crashes, 0 failures, .*$/\1/p' "$log")
    swept=$(grep -c '^crash-sweep: [^ ]* hits=[1-9][0-9]* ' "$log" || true)
    if [[ ! $points =~ ^[1-9][0-9]*$ || $swept -ne $points ]]; then
        echo "crash-sweep: expected one '<n> points, <m> crashes, 0 failures' line and n 'hits=' lines; got '$points' and $swept" >&2
        exit 1
    fi
    echo "crash-sweep: all $points points were swept"

# On demand, never a PR gate (OI-1003-Q165 review, R-N122): P84
# SOURCE-NO-CLONE's kernel leg and the reflink halves of the bench and
# harness no-clone tests, required to run. Each returns early where its
# directory cannot clone, and cargo and unittest still report that as `ok`
# or a skip, so the PR gate's counts are no proof that they ran. Here
# BULKLOAD_P84_REQUIRE_REFLINK turns that early return into a failure,
# TMPDIR is P84_REFLINK_DIR (default `target/p84-reflink`, the build's own
# file system), and each leg must print its own line. It needs Linux with
# that directory on xfs `reflink=1`, btrfs or bcachefs; elsewhere it fails.
p84-reflink:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    dir=${P84_REFLINK_DIR:-${CARGO_TARGET_DIR:-target}/p84-reflink}
    mkdir -p "$dir"
    TMPDIR=$(cd "$dir" && pwd)
    export TMPDIR BULKLOAD_P84_REQUIRE_REFLINK=1
    log=$(mktemp "$TMPDIR/p84-reflink.XXXXXX")
    trap 'rm -f "$log"' EXIT
    cargo test -p bulkload-agent --locked --test source_lock_trace p84_ -- --nocapture 2>&1 | tee "$log"
    if [[ $(grep -c '^test result: ok\. 3 passed; 0 failed;' "$log" || true) -ne 1 ]]; then
        echo "p84-reflink: expected one '3 passed; 0 failed' result, the kernel leg's own" >&2
        exit 1
    fi
    for marker in 'p84 self-test: FICLONE shared ' 'p84 copy and serve: source ' 'p84 git and sqlite: repository '; do
        if [[ $(grep -c "^$marker" "$log" || true) -ne 1 ]]; then
            echo "p84-reflink: no '$marker' line: that leg did not run" >&2
            exit 1
        fi
    done
    cargo test -p bulkload-bench --locked tests::a_fixture_that_shares_an_extent_refuses_the_sample -- --exact --nocapture 2>&1 | tee "$log"
    if [[ $(grep -c '^bench no-clone: reflinked fixture and sealed corpus refused$' "$log" || true) -ne 1 ]]; then
        echo "p84-reflink: the bench's reflinked half did not run" >&2
        exit 1
    fi
    python3 crates/bulkload-bench/scripts/test_r23_corpus.py NoCloneTests
    python3 crates/bulkload-bench/scripts/test_r23_ab.py HarnessTests.test_a_cloning_copy_is_refused_by_fiemap_on_a_reflink_file_system
    python3 crates/bulkload-bench/scripts/test_link_common.py HelperTests.test_prepare_copies_by_read_and_write_and_refuses_a_shared_copy
    echo "p84-reflink: the kernel leg, the bench and the harness reflink halves ran under $TMPDIR"

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
# contract-test rides here since the Bazel `test` gate was dropped (OI-1003-Q65).
ci-source: check-source secrets-scan-history contract-test

# The CI `fault-harness` terminal gate (R-N122); the composite action execs it
# inside the repo flake, like `ci-source`. The tinyland-nix runner pod is
# capped at 2 GiB: the fault-feature build OOM-killed it on #230 and #233
# (2026-10-16, rustc at 1.5 GiB RSS beside a linker), so CI builds this
# gate two jobs wide with line-tables-only debug info. Local runs of
# `fault-harness` are unchanged.
ci-fault-harness:
    cd {{ root }} && CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only {{ just_executable() }} fault-harness

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

# docs/formal keeps EstateConverge.tla alone, until bulkload#237 is fixed
# (R-C843, Linear TIN-4543 ace8649c, 2026-10-17; docs/formal/README.md).
# The other modules, the Dhall catalogue that rendered the configs, its
# staleness and grounding checks and the Haskell cross-checks were deleted
# under R-C843, so configs_ec.tsv and MC_ec_*.cfg are now edited by hand.
# Standalone and on demand: no tier depends on it, so check-fast,
# check-optional, check-full and CI never start TLC. TLC comes from the
# flake's pinned nixpkgs (no flake change).
# The rows of configs_ec.tsv run in its order, one JVM at a time (-Xmx4g, 3
# workers, nice 10, coverage on), with TLC state and logs in a private
# mktemp directory under TMPDIR. The first row is the budget self-test: it
# must finish with WithinBudget, and nothing else, violated, or nothing
# else runs, because every other config relies on that budget.
# Outcomes: PASS (model checking finished, no error, and the never-enabled
# actions equal the row's never column), FAIL (exactly the row's named
# property violated, nothing else), REACHED (exactly the row's Witness_
# invariant violated: the scenario is reachable), SIMULATION (-simulate
# finished clean; never a model-checking result), INCONCLUSIVE (a
# WithinBudget trip; never a pass or a caught mutant), ABORTED (no Finished
# line: TLC did not end normally; matches no expectation), WRONG (anything
# else). Every outcome must equal the row's expect column. Optional
# arguments name configs to run after the self-test. Scratch is removed
# when every row matches; otherwise the logs stay for review.
# Model-check docs/formal/EstateConverge.tla with TLC.
tla-check *configs:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}/docs/formal
    tlc=$(nix shell --inputs-from {{ root }} nixpkgs#tlaplus --command sh -c 'command -v tlc')
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/tla-check.XXXXXX")
    mkdir -p "$scratch/java"
    export JAVA_TOOL_OPTIONS="-Djava.io.tmpdir=$scratch/java -Xmx4g"
    measure=()
    if /usr/bin/time -f %M -o /dev/null true 2>/dev/null; then
        measure=(/usr/bin/time -f %M -o)
    fi
    wanted=" {{ configs }} "
    mismatches=0
    peak=0
    begun=$SECONDS
    format='%-36s %-12s %-12s %-30s %11s %11s %5s %6s %7s\n'
    # One module's rows, in its run order, against its spec.
    run_module() {
        local tsv=$1 spec=$2 proven=no
        printf '%s (%s)\n' "$spec" "$tsv"
        printf "$format" config expect outcome violated distinct generated depth wall rss_mib
        while IFS=$'\t' read -r name expect prop never_want flags; do
            if [[ -z $name || $name == \#* || $name == name ]]; then
                continue
            fi
            if [[ $proven == no && $expect != inconclusive ]]; then
                echo "tla-check: the first row of $tsv must be the budget self-test" >&2
                exit 1
            fi
            if [[ $expect != inconclusive && $wanted != "  " && $wanted != *" $name "* ]]; then
                continue
            fi
            log="$scratch/$name.log"
            args=(-workers 3 -coverage 1 -metadir "$scratch/$name.states" -config "$name.cfg")
            if [[ $flags != - ]]; then
                read -r -a extra <<<"$flags"
                args+=("${extra[@]}")
            fi
            runner=()
            if [[ ${#measure[@]} -gt 0 ]]; then
                runner=("${measure[@]}" "$scratch/$name.rss")
            fi
            started=$SECONDS
            status=0
            nice -n 10 "${runner[@]}" "$tlc" "${args[@]}" "$spec" >"$log" 2>&1 || status=$?
            wall="$((SECONDS - started))s"
            distinct=$(grep -oE '[0-9,]+ distinct states found' "$log" | tail -n 1 | cut -d' ' -f1 || true)
            generated=$(grep -oE '[0-9,]+ states generated' "$log" | tail -n 1 | cut -d' ' -f1 || true)
            if [[ -z $distinct ]]; then
                # A simulation reports the states it checked, not distinct states.
                distinct=$(grep -oE '[0-9,]+ states checked' "$log" | tail -n 1 | cut -d' ' -f1 || true)
            fi
            depth=$(grep -oE 'depth of the complete state graph search is [0-9]+' "$log" | tail -n 1 | grep -oE '[0-9]+$' || true)
            rss=?
            if [[ -s $scratch/$name.rss ]]; then
                rss=$(($(tail -n 1 "$scratch/$name.rss") / 1024))
                if [[ $rss -gt $peak ]]; then
                    peak=$rss
                fi
            fi
            # Every property the log reports violated, comma-separated.
            violated=$(grep -oE 'Invariant [A-Za-z0-9_]+ is violated' "$log" | cut -d' ' -f2 | sort -u | paste -sd, - || true)
            if grep -qF 'Temporal properties were violated' "$log"; then
                # TLC does not name the temporal property; a fail row has one.
                if [[ $(grep -c '^PROPERTY ' "$name.cfg") -eq 1 ]]; then
                    temporal=$(grep '^PROPERTY ' "$name.cfg" | cut -d' ' -f2)
                else
                    temporal=temporal
                fi
                violated=${violated:+$violated,}$temporal
            fi
            if grep -qF 'Deadlock reached' "$log"; then
                violated=${violated:+$violated,}deadlock
            fi
            errors=$(grep -c '^Error:' "$log" || true)
            if ! grep -q '^Finished in' "$log"; then
                # The JVM ended early (out of memory, ended from outside, a
                # crash): nothing is known, and it never counts as a budget trip.
                outcome=ABORTED
            elif [[ $expect == inconclusive ]]; then
                # The budget self-test proves the budget only by tripping it alone.
                if [[ $violated == "$prop" ]]; then
                    outcome=INCONCLUSIVE
                else
                    outcome=WRONG
                fi
            elif [[ ,$violated, == *,WithinBudget,* ]]; then
                outcome=INCONCLUSIVE
            elif [[ $expect == simulate ]]; then
                if [[ $status -eq 0 && $errors -eq 0 ]]; then
                    outcome=SIMULATION
                else
                    outcome=WRONG
                fi
            elif [[ $status -eq 0 && $errors -eq 0 ]] && grep -qF 'No error has been found' "$log"; then
                outcome=PASS
            elif [[ $expect == fail && $violated == "$prop" ]]; then
                outcome=FAIL
            elif [[ $expect == reach && $violated == "$prop" ]]; then
                outcome=REACHED
            else
                outcome=WRONG
            fi
            # Coverage: the actions the last coverage report shows never enabled.
            never=$(awk '/^The coverage statistics/ { delete seen; delete order; n = 0 }
                /^<[A-Za-z_][A-Za-z0-9_]* line .*>: [0-9]+:[0-9]+$/ {
                    split($0, part, ">: "); split(part[2], count, ":")
                    action = substr($1, 2)
                    if (!(action in seen)) { seen[action] = 0; order[++n] = action }
                    seen[action] += count[1] + count[2]
                }
                END { for (i = 1; i <= n; i++) if (seen[order[i]] == 0) printf "%s ", order[i] }' "$log")
            never_got=$(tr ' ' '\n' <<<"$never" | sed '/^$/d' | sort | paste -sd, -)
            never_got=${never_got:--}
            coverage=ok
            if [[ $outcome == PASS && $never_want != '*' && $never_got != "$never_want" ]]; then
                # A pass row's search is complete, so its never-enabled set is
                # exact; any change means a spec edit enabled or lost an action.
                outcome=WRONG
                coverage=mismatch
            fi
            case $expect:$outcome in
                pass:PASS | fail:FAIL | reach:REACHED | simulate:SIMULATION | inconclusive:INCONCLUSIVE) matched=yes ;;
                *) matched=no ;;
            esac
            printf "$format" "$name" "$expect" "$outcome" "${violated:--}" "${distinct:-?}" "${generated:-?}" "${depth:--}" "$wall" "$rss"
            if [[ -n $never ]]; then
                printf '    never enabled: %s\n' "$never"
            fi
            if [[ $coverage == mismatch ]]; then
                printf '    coverage: expected never enabled %s, got %s\n' "$never_want" "$never_got"
            fi
            if [[ $matched == no ]]; then
                mismatches=$((mismatches + 1))
                echo "tla-check: $name expected $expect, got $outcome; log kept at $log" >&2
            fi
            if [[ $expect == inconclusive ]]; then
                if [[ $matched == no ]]; then
                    echo "tla-check: the budget self-test of $spec did not finish with WithinBudget alone violated; no other config runs" >&2
                    exit 1
                fi
                proven=yes
            fi
        done < "$tsv"
    }
    run_module configs_ec.tsv EstateConverge.tla
    printf 'total wall %ss, peak rss %s MiB\n' "$((SECONDS - begun))" "$peak"
    if [[ $mismatches -ne 0 ]]; then
        echo "tla-check: $mismatches config(s) did not match their expectation" >&2
        exit 1
    fi
    rm -rf "$scratch"

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

# `census ROOT --label L [--kind state|git] [--ssh-host H]` (read-only, lstat
# only), `generate DEST --profile CENSUS.json ... --bytes N`, `verify DEST`,
# `fixture DEST` and `run`: bulkload `pull` (B) against plain rclone over sftp
# (P, the gate comparator), tuned rclone (R), rclone + `sync -f` (S, equal
# durability) and `rsync -a` (Y, reference only), B/A/B/A/B per phase
# (initial, unchanged rerun, the C3 1 % in-place delta), plus with
# --git the estate arm (estate-capture, pull, estate-apply) against rclone of
# the same checkouts; iperf3 and ssh ceilings and both hosts' load per arm.
# rclone, iperf3 and rsync come from the flake-pinned nixpkgs, never a system
# package. A neo->sting run is informational until a ruling says which hosts
# carry gate (b)'s gated samples (SPEC R3); --dry-run is a one-host loopback,
# NOT A SAMPLE. Plan: docs/agent-notes/2026-10-16-m3-real-link-proof-plan.md.
# M3 real-link proof harness (axiom 7, OI-1003-Q194 C2)
[positional-arguments]
bench-m3-link *args:
    cd {{ root }} && nix shell --inputs-from . nixpkgs#rclone nixpkgs#iperf3 nixpkgs#rsync --command nice -n 10 python3 crates/bulkload-bench/scripts/m3_link.py "$@"

# The harness's loopback self-test on this host (NOT A SAMPLE): a 128 MiB
# corpus generated from the committed 2026-10-16 census profiles through every
# file arm, then the Git arm on a tiny fixture. WORK must be new; it is removed
# afterwards unless KEEP=1. AGENT is a local bulkload-agent build.
# M3 harness dry run (loopback, NOT A SAMPLE)
bench-m3-dry-run agent work="/dev/shm/m3-dry-run":
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}
    w={{ work }}
    if [[ -e $w ]]; then echo "bench-m3-dry-run: $w must be new" >&2; exit 2; fi
    mkdir -p "$w"
    p=docs/evidence/m3-census-2026-10-16
    m3() { nix shell --inputs-from . nixpkgs#rclone nixpkgs#iperf3 nixpkgs#rsync --command nice -n 10 python3 crates/bulkload-bench/scripts/m3_link.py "$@"; }
    m3 generate "$w/corpus" --profile "$p/claude.json" --profile "$p/codex.json" --profile "$p/git.json" --bytes 134217728
    m3 run --dry-run --work-root "$w/files" --agent "{{ agent }}" --source-corpus "$w/corpus" --reps 1 --rclone-tune 4,16
    m3 fixture "$w/fixture"
    m3 run --dry-run --work-root "$w/git" --agent "{{ agent }}" --source-subset "git=$w/fixture/git" --git --order BPBRBSY --reps 1 --iperf3 ''
    cat "$w/files/m3-link.md" "$w/git/m3-link.md"
    if [[ ${KEEP:-0} != 1 ]]; then chmod -R u+w "$w" && rm -rf "$w"; fi
