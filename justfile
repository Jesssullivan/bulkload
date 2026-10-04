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

# docs/formal's typed catalogue (OI-1003-Q32): render configs.tsv and every
# MC_*.cfg from docs/formal/catalogue/Catalogue.dhall into OUT (default
# docs/formal). Every MC_*.cfg already in OUT is removed first, so a config
# dropped from the catalogue leaves no stale file. dhall-to-json evaluates
# the catalogue, which also checks its asserts (a mutation without a verdict
# or a primary row does not type-check), and jq writes one file per entry.
# Both come from the flake's pinned nixpkgs (no flake change). JSON, when
# given, keeps the evaluated catalogue for tla-check's grounding step.
# Render docs/formal's TLC configs from the Dhall catalogue.
tla-render out="" json="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}/docs/formal
    out='{{ out }}'
    out=${out:-{{ root }}/docs/formal}
    json='{{ json }}'
    if [[ -z $json ]]; then
        json=$(mktemp "${TMPDIR:-/tmp}/tla-render.XXXXXX")
        trap 'rm -f "$json"' EXIT
    fi
    read -r d2j jq < <(nix shell --inputs-from {{ root }} nixpkgs#dhall-json nixpkgs#jq --command sh -c 'printf "%s %s\n" "$(command -v dhall-to-json)" "$(command -v jq)"')
    "$d2j" --file catalogue/Catalogue.dhall --output "$json"
    if ! "$jq" -e '[.files[].name] | (length == (unique | length)) and all(test("^(MC_[A-Za-z0-9_]+[.]cfg|configs[.]tsv)$"))' "$json" >/dev/null; then
        echo "tla-render: the catalogue's file names are duplicated or unsafe" >&2
        exit 1
    fi
    mkdir -p "$out"
    rm -f "$out"/MC_*.cfg
    count=$("$jq" '.files | length' "$json")
    for ((i = 0; i < count; i++)); do
        name=$("$jq" -r ".files[$i].name" "$json")
        "$jq" -j ".files[$i].text" "$json" >"$out/$name"
    done
    echo "tla-render: $count files into $out"

# TLA+ model of wire v5, Held, the group commits and resume (proof package,
# OI-1003-Q7; docs/formal/README.md). Standalone and on demand: no tier
# depends on it, so check-fast, check-optional, check-full and CI never start
# TLC. TLC comes from the flake's pinned nixpkgs (no flake change). Before
# any TLC run, two catalogue checks (OI-1003-Q32) must pass. Staleness: the
# catalogue, rendered into scratch, equals the committed configs.tsv and
# MC_*.cfg byte for byte, with no file missing or extra. Grounding: every
# operator the catalogue names (properties, witnesses, actions, the specs)
# is defined in BulkloadTransfer.tla; the catalogue's constants are exactly
# the spec's CONSTANTS, and its mutations exactly the spec's Mutations set
# ("none" aside), each checked in both directions; and every code symbol is
# found by `git grep -w` under crates/. A failed check removes the scratch
# and stops.
# The rows of docs/formal/configs.tsv run in order, one JVM at a time (-Xmx4g,
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
    jq=$(nix shell --inputs-from {{ root }} nixpkgs#jq --command sh -c 'command -v jq')
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/tla-check.XXXXXX")
    # Staleness: the committed configs are exactly the catalogue's rendering.
    (cd {{ root }} && {{ just_executable() }} tla-render "$scratch/rendered" "$scratch/catalogue.json") >/dev/null
    committed=$(ls MC_*.cfg configs.tsv | LC_ALL=C sort)
    rendered=$(cd "$scratch/rendered" && ls | LC_ALL=C sort)
    stale=0
    if [[ $committed != "$rendered" ]]; then
        echo "tla-check: the committed configs and the catalogue's differ in their file set:" >&2
        diff <(echo "$committed") <(echo "$rendered") >&2 || true
        stale=1
    fi
    for file in $rendered; do
        if [[ -f $file ]] && ! cmp -s "$file" "$scratch/rendered/$file"; then
            echo "tla-check: $file differs from the catalogue's rendering" >&2
            stale=1
        fi
    done
    if [[ $stale -ne 0 ]]; then
        echo "tla-check: stale configs; run just tla-render and commit the result" >&2
        rm -rf "$scratch"
        exit 1
    fi
    # Grounding: every catalogue name exists in the spec, every code symbol in crates/.
    ungrounded=0
    declared=$(awk '/^CONSTANTS/ { on = 1; next } on && /^$/ { on = 0 } on' BulkloadTransfer.tla)
    mutations=$(awk '/^Mutations ==/ { on = 1 } on { print } on && /}/ { exit }' BulkloadTransfer.tla)
    operators=$("$jq" -r '.grounding.operators[]' "$scratch/catalogue.json")
    constants=$("$jq" -r '.grounding.constants[]' "$scratch/catalogue.json")
    mutants=$("$jq" -r '.grounding.mutations[]' "$scratch/catalogue.json")
    symbols=$("$jq" -r '.grounding.codeSymbols | unique | .[]' "$scratch/catalogue.json")
    for name in $operators; do
        if ! grep -qE "^${name}"'(\(.*\))? ==' BulkloadTransfer.tla; then
            echo "tla-check: $name is not defined in BulkloadTransfer.tla" >&2
            ungrounded=$((ungrounded + 1))
        fi
    done
    # Constants and mutations must agree in both directions: the catalogue
    # sets every declared constant and no other, and it has an entry (a
    # verdict and a primary MC_neg_ row) for every rule break in the spec's
    # Mutations set ("none" aside) and no other.
    declared_names=$({ grep -oE '^ +[A-Za-z][A-Za-z0-9_]*' <<<"$declared" || true; } | tr -d ' ' | LC_ALL=C sort -u)
    catalogue_constants=$(LC_ALL=C sort -u <<<"$constants")
    while read -r name; do
        echo "tla-check: constant $name is not declared in BulkloadTransfer.tla" >&2
        ungrounded=$((ungrounded + 1))
    done < <(LC_ALL=C comm -13 <(echo "$declared_names") <(echo "$catalogue_constants") | sed '/^$/d')
    while read -r name; do
        echo "tla-check: constant $name is declared in BulkloadTransfer.tla but the catalogue does not set it" >&2
        ungrounded=$((ungrounded + 1))
    done < <(LC_ALL=C comm -23 <(echo "$declared_names") <(echo "$catalogue_constants") | sed '/^$/d')
    spec_mutations=$({ grep -oE '"[A-Za-z0-9_]+"' <<<"$mutations" || true; } | tr -d '"' | sed '/^none$/d' | LC_ALL=C sort -u)
    catalogue_mutations=$(LC_ALL=C sort -u <<<"$mutants")
    while read -r name; do
        echo "tla-check: mutation $name is not in the spec's Mutations set" >&2
        ungrounded=$((ungrounded + 1))
    done < <(LC_ALL=C comm -13 <(echo "$spec_mutations") <(echo "$catalogue_mutations") | sed '/^$/d')
    while read -r name; do
        echo "tla-check: the spec's mutation $name has no catalogue entry (no verdict, no MC_neg_ row)" >&2
        ungrounded=$((ungrounded + 1))
    done < <(LC_ALL=C comm -23 <(echo "$spec_mutations") <(echo "$catalogue_mutations") | sed '/^$/d')
    for name in $symbols; do
        if ! git -C {{ root }} grep -q -w -F -e "$name" -- crates/; then
            echo "tla-check: code symbol $name is not found under crates/" >&2
            ungrounded=$((ungrounded + 1))
        fi
    done
    if [[ $ungrounded -ne 0 ]]; then
        echo "tla-check: $ungrounded catalogue name(s) are not grounded" >&2
        rm -rf "$scratch"
        exit 1
    fi
    printf 'catalogue: %s files current; grounded %s operators, %s constants, %s mutations, %s code symbols\n' \
        "$(wc -w <<<"$rendered")" "$(wc -w <<<"$operators")" "$(wc -w <<<"$constants")" \
        "$(wc -w <<<"$mutants")" "$(wc -w <<<"$symbols")"
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

# Haskell N-version explorer (OI-1003-Q32; docs/formal/README.md, "Hybrid
# roles"). Standalone and on demand: no tier depends on it, so check-fast,
# check-optional, check-full and CI never build it. GHC, TLC, Dhall and jq
# come from the flake's pinned nixpkgs (no flake change).
# docs/formal/hs/Explorer.hs uses base and containers only; it is built with
# -O1 -Wall -Werror into a private mktemp directory under TMPDIR. Its rows:
# - the presets nv_core and nv_ledger must reach TLC's distinct-state counts
#   of record, MC_nv_core's 15,834 and MC_nv_ledger's 142,450, with no
#   invariant violated and no deadlock;
# - every MC_neg_ row of the catalogue inside the explorer's domain (the
#   evaluated catalogue's nversion list) runs at its own bound, passed as
#   the explorer's bound flags, checking TypeOK and the row's named
#   property, as its TLC config does; it must violate exactly that property;
# - each primary row among them runs again with every safety invariant
#   checked, on the explorer and on TLC with one worker (the catalogue's
#   scratch config for it): both must stop at the same first violated
#   invariant after the same number of states.
# Each explorer counterexample is written as JSON; that directory is kept
# and printed, and so are the TLC logs.
# Cross-check the TLA+ model with the Haskell N-version explorer.
formal-nv:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}/docs/formal
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/formal-nv.XXXXXX")
    mkdir -p "$scratch/build" "$scratch/counterexamples" "$scratch/tlc/java"
    if ! (cd {{ root }} && {{ just_executable() }} tla-render "$scratch/rendered" "$scratch/catalogue.json") >"$scratch/render.log" 2>&1; then
        cat "$scratch/render.log" >&2
        echo "formal-nv: the catalogue did not evaluate; log kept at $scratch/render.log" >&2
        exit 1
    fi
    tlc=$(nix shell --inputs-from {{ root }} nixpkgs#tlaplus --command sh -c 'command -v tlc')
    jq=$(nix shell --inputs-from {{ root }} nixpkgs#jq --command sh -c 'command -v jq')
    if ! nix shell --inputs-from {{ root }} nixpkgs#ghc --command nice -n 10 ghc -O1 -Wall -Werror -outputdir "$scratch/build" -o "$scratch/explorer" hs/Explorer.hs >"$scratch/ghc.log" 2>&1; then
        cat "$scratch/ghc.log" >&2
        echo "formal-nv: the explorer did not build; log kept at $scratch/ghc.log" >&2
        exit 1
    fi
    # TLC's distinct-state counts of record (README.md, "N-version core").
    declare -A tlc_distinct=([nv_core]=15834 [nv_ledger]=142450)
    field() { sed -n "s/.* $1=\([^ ]*\).*/\1/p" <<<" $2"; }
    mismatches=0
    format='%-34s %-42s %-42s %9s %10s %6s %s\n'
    printf "$format" row expect explorer distinct generated depth match
    for preset in nv_core nv_ledger; do
        status=0
        line=$(nice -n 10 "$scratch/explorer" --preset "$preset") || status=$?
        want="pass (${tlc_distinct[$preset]})"
        got="$(field outcome "$line") ($(field distinct "$line"))"
        match=yes
        if [[ $status -ne 0 || $got != "$want" ]]; then
            match=no
            mismatches=$((mismatches + 1))
        fi
        printf "$format" "$(field row "$line")" "$want" "$got" "$(field distinct "$line")" "$(field generated "$line")" "$(field depth "$line")" "$match"
    done
    rows=$("$jq" '.nversion | length' "$scratch/catalogue.json")
    if [[ $rows -eq 0 ]]; then
        echo "formal-nv: the catalogue has no mutation row in the explorer's domain" >&2
        exit 1
    fi
    export JAVA_TOOL_OPTIONS="-Djava.io.tmpdir=$scratch/tlc/java -Xmx2g"
    for ((i = 0; i < rows; i++)); do
        IFS=$'\t' read -r name mutation named primary seats runs crashes edits foreign < <(
            "$jq" -r ".nversion[$i] | [.name, .mutation, .property, .primary, .seats, .runs, .crashes, .edits, .foreign] | @tsv" "$scratch/catalogue.json")
        bound=(--seats "$seats" --runs "$runs" --crashes "$crashes" --edits "$edits" --foreign "$foreign" --mutation "$mutation")
        status=0
        line=$(nice -n 10 "$scratch/explorer" "${bound[@]}" --name "$name" --check "TypeOK,$named" --json "$scratch/counterexamples") || status=$?
        got="$(field outcome "$line") $(field violated "$line")"
        match=yes
        if [[ $status -ne 1 || $got != "violation $named" ]]; then
            match=no
            mismatches=$((mismatches + 1))
        fi
        printf "$format" "$name" "fail $named" "$got" "$(field distinct "$line")" "$(field generated "$line")" "$(field depth "$line")" "$match"
        if [[ $primary != true ]]; then
            continue
        fi
        # Every safety invariant, on TLC (one worker, so its breadth-first
        # order is fixed) and on the explorer: the first invariant violated,
        # in the configs' order, and the counterexample's length must agree.
        every="${name}_all"
        log="$scratch/tlc/$every.log"
        "$jq" -j ".nversion[$i].everyInvariant" "$scratch/catalogue.json" >"$scratch/tlc/$every.cfg"
        nice -n 10 "$tlc" -workers 1 -metadir "$scratch/tlc/$every.states" -config "$scratch/tlc/$every.cfg" BulkloadTransfer.tla >"$log" 2>&1 || true
        rm -rf "$scratch/tlc/$every.states"
        tlc_first=$(grep -oE 'Invariant [A-Za-z0-9_]+ is violated' "$log" | head -n 1 | cut -d' ' -f2 || true)
        if grep -qF 'Deadlock reached' "$log"; then
            tlc_first=deadlock
        fi
        tlc_states=$(grep -cE '^State [0-9]+:' "$log" || true)
        status=0
        line=$(nice -n 10 "$scratch/explorer" "${bound[@]}" --name "$every" --check all --json "$scratch/counterexamples") || status=$?
        first=$(field violated "$line")
        first=${first%%,*}
        want="TLC first ${tlc_first:-none}, $tlc_states states"
        got="explorer first $first, $(field states "$line") states"
        match=yes
        if [[ $status -ne 1 || -z $tlc_first || $got != "explorer first $tlc_first, $tlc_states states" ]]; then
            match=no
            mismatches=$((mismatches + 1))
            echo "formal-nv: $every disagrees with TLC; TLC log kept at $log" >&2
        fi
        printf "$format" "$every" "$want" "$got" "$(field distinct "$line")" "$(field generated "$line")" "$(field depth "$line")" "$match"
    done
    rm -rf "$scratch/build" "$scratch/explorer" "$scratch/rendered" "$scratch/tlc/java"
    echo "counterexamples (JSON): $scratch/counterexamples; TLC logs: $scratch/tlc"
    if [[ $mismatches -ne 0 ]]; then
        echo "formal-nv: $mismatches row(s) disagree with TLC" >&2
        exit 1
    fi
