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
#                   its source, fault-harness and test gates.
#   check-optional  optional tier: spike evidence, bench-script stubs, the
#                   estate corpus self-test (OI-1003-Q19), the history secret
#                   scan and the Nix/Bazel graph. On demand.
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
    cd {{ root }} && python3 crates/bulkload-bench/scripts/test_s3_estate.py
    cd {{ root }} && {{ just_executable() }} bench-estate-corpus-selftest
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

# TLA+ model of wire v5, Held, the group commits and resume (proof package,
# OI-1003-Q7; docs/formal/README.md). Standalone and on demand: no tier
# depends on it, so check-fast, check-optional, check-full and CI never start
# TLC. TLC comes from the flake's pinned nixpkgs (no flake change). The rows
# of docs/formal/configs.tsv run in order, one JVM at a time (-Xmx4g,
# 3 workers, nice 10, coverage on), with TLC state and logs in a private
# mktemp directory under TMPDIR. The first row is the budget self-test: it
# must finish with WithinBudget, and nothing else, violated, or nothing else
# runs, because every other config relies on that budget. Outcomes: PASS
# (model checking finished, no error, and the never-enabled actions equal
# the row's never column), FAIL (exactly the row's named property violated,
# nothing else), REACHED (exactly the row's Witness_ invariant violated: the
# scenario is reachable), SIMULATION (-simulate finished clean; never a
# model-checking result), INCONCLUSIVE (a WithinBudget trip; never a pass or
# a caught mutant), ABORTED (no Finished line: TLC did not end normally;
# matches no expectation), WRONG (anything else). Every outcome must equal
# the row's expect column. Optional arguments name configs to run after the
# self-test. Scratch is removed when every row matches; otherwise the logs
# stay for review.
# Model-check docs/formal with TLC: positives pass, mutations fail.
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
    proven=no
    peak=0
    begun=$SECONDS
    format='%-30s %-12s %-12s %-28s %11s %11s %5s %6s %7s\n'
    printf "$format" config expect outcome violated distinct generated depth wall rss_mib
    while IFS=$'\t' read -r name expect prop never_want flags; do
        if [[ -z $name || $name == \#* || $name == name ]]; then
            continue
        fi
        if [[ $proven == no && $expect != inconclusive ]]; then
            echo "tla-check: the first row must be the budget self-test" >&2
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
        nice -n 10 "${runner[@]}" "$tlc" "${args[@]}" BulkloadTransfer.tla >"$log" 2>&1 || status=$?
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
                echo "tla-check: the budget self-test did not finish with WithinBudget alone violated; no other config runs" >&2
                exit 1
            fi
            proven=yes
        fi
    done < configs.tsv
    printf 'total wall %ss, peak rss %s MiB\n' "$((SECONDS - begun))" "$peak"
    if [[ $mismatches -ne 0 ]]; then
        echo "tla-check: $mismatches config(s) did not match their expectation" >&2
        exit 1
    fi
    rm -rf "$scratch"
