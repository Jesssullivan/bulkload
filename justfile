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

# Resume power-loss proofs, lib tests that need `io-trace`: the directory
# resume paths (#74 review B1 and round 2 N1) and the adoption of a durable
# unrowed output from its capture record with 0 source bytes read (#169,
# R-N58). A name filter that matches nothing still reports `0 passed`, so this
# recipe fails unless all three proofs ran: on a cargo failure, on anything
# but one `3 passed; 0 failed` result, or when a proof is not among the
# passing tests (#74 round 2 N2, R-N122).
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
    passed=$(grep -c '^test result: ok\. 3 passed; 0 failed;' <<<"$output" || true)
    if [[ $results -ne 1 || $passed -ne 1 ]]; then
        echo "resume-power-loss: expected exactly one '3 passed; 0 failed' result" >&2
        exit 1
    fi
    for proof in an_adopted_fallback_directory_is_sealed_before_its_record_binds a_directory_adopted_by_its_bound_record_is_sealed_before_outputs_commit an_unrowed_output_is_adopted_without_source_reads; do
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
#                   its source and fault-harness gates (OI-1003-Q65).
#   check-optional  optional tier: spike evidence, bench-script stubs, the
#                   estate corpus self-test (OI-1003-Q19), the history secret
#                   scan and the Nix/Bazel graph. On demand.
#   check-full      both tiers (the lab `test-presubmit` / xoxd.ai `ci` shape).

# The repository and CI contract tests, run directly instead of through
# Bazel's //:tests (CI runs them through `ci-source` since OI-1003-Q65
# dropped the Bazel `test` gate).
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
# contract-test rides here since the Bazel `test` gate was dropped (OI-1003-Q65).
ci-source: check-source secrets-scan-history contract-test

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

# docs/formal's typed catalogue (OI-1003-Q32, OI-1003-Q43): render each
# module's run order (configs.tsv for BulkloadTransfer.tla, configs_gc.tsv
# for GitCarry.tla) and every MC_*.cfg from docs/formal/catalogue/
# Catalogue.dhall into OUT (default docs/formal). Every MC_*.cfg already in
# OUT is removed first, so a config dropped from the catalogue leaves no
# stale file. dhall-to-json evaluates the catalogue, which also checks its
# asserts (a mutation without a verdict or a primary row does not
# type-check), and jq writes one file per entry. Both come from the flake's
# pinned nixpkgs (no flake change). JSON, when given, keeps the evaluated
# catalogue for tla-check's grounding step. `just tla-render --check`
# renders into scratch instead and fails unless the committed files equal
# the rendering byte for byte, with no file missing or extra.
# Render docs/formal's TLC configs from the Dhall catalogue.
tla-render out="" json="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}/docs/formal
    out='{{ out }}'
    out=${out:-{{ root }}/docs/formal}
    json='{{ json }}'
    check=no
    scratch=
    if [[ $out == --check ]]; then
        check=yes
        scratch=$(mktemp -d "${TMPDIR:-/tmp}/tla-render.XXXXXX")
        out=$scratch/rendered
    fi
    if [[ -z $json ]]; then
        json=$(mktemp "${TMPDIR:-/tmp}/tla-render.XXXXXX")
        trap 'rm -f "$json"; if [[ -n $scratch ]]; then rm -rf "$scratch"; fi' EXIT
    fi
    read -r d2j jq < <(nix shell --inputs-from {{ root }} nixpkgs#dhall-json nixpkgs#jq --command sh -c 'printf "%s %s\n" "$(command -v dhall-to-json)" "$(command -v jq)"')
    "$d2j" --file catalogue/Catalogue.dhall --output "$json"
    if ! "$jq" -e '[.files[].name] | (length == (unique | length)) and all(test("^(MC_[A-Za-z0-9_]+[.]cfg|configs(_[a-z]+)?[.]tsv)$"))' "$json" >/dev/null; then
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
    if [[ $check == no ]]; then
        echo "tla-render: $count files into $out"
        exit 0
    fi
    committed=$(ls MC_*.cfg configs*.tsv | LC_ALL=C sort)
    rendered=$(cd "$out" && ls | LC_ALL=C sort)
    stale=0
    if [[ $committed != "$rendered" ]]; then
        echo "tla-render: the committed configs and the catalogue's differ in their file set:" >&2
        diff <(echo "$committed") <(echo "$rendered") >&2 || true
        stale=1
    fi
    for file in $rendered; do
        if [[ -f $file ]] && ! cmp -s "$file" "$out/$file"; then
            echo "tla-render: $file differs from the catalogue's rendering" >&2
            stale=1
        fi
    done
    if [[ $stale -ne 0 ]]; then
        echo "tla-render: stale configs; run just tla-render and commit the result" >&2
        exit 1
    fi
    echo "tla-render: all $count committed files equal the catalogue's rendering"

# TLA+ models of the proof package (OI-1003-Q7; docs/formal/README.md):
# BulkloadTransfer.tla (wire v5, Held, the group commits and resume) and
# GitCarry.tla (git carry's chain and base custody, OI-1003-Q43).
# Standalone and on demand: no tier depends on it, so check-fast,
# check-optional, check-full and CI never start TLC. TLC comes from the
# flake's pinned nixpkgs (no flake change). Before any TLC run, two
# catalogue checks (OI-1003-Q32) must pass. Staleness: the catalogue,
# rendered into scratch, equals the committed configs*.tsv and MC_*.cfg
# byte for byte, with no file missing or extra. Grounding, per module:
# every operator the catalogue names (properties, witnesses, actions, the
# specs) is defined in the module; the catalogue's constants are exactly
# the module's CONSTANTS, and its mutations exactly the module's Mutations
# set ("none" aside), and each closed union of the decision core exactly
# the module's set of the same name (GitCarry: Decisions, Bases, Rebases,
# Reuses, Refusals), each checked in both directions; and every code symbol
# is in the Rust sources, a crates/ .rs file outside tests/, as the
# module's symbolMatch says: `definition` (GitCarry) needs `fn`, `const`,
# `static`, `struct`, `enum`, `trait`, `type` or `mod` then the symbol as a
# whole word; `code` (BulkloadTransfer, whose symbols include variants,
# fields and parameters) needs the whole word on a line that is not a
# comment. A data file, a test or a comment that only names a symbol never
# grounds it. Symbols the code does not have yet are printed as pending,
# with the lane that lands them. A failed check removes the scratch and
# stops.
# Each module's rows run in its run order, one JVM at a time (-Xmx4g, 3
# workers, nice 10, coverage on), with TLC state and logs in a private
# mktemp directory under TMPDIR. A module's first row is its budget
# self-test: it must finish with WithinBudget, and nothing else, violated,
# or nothing else runs, because every other config relies on that budget.
# Outcomes: PASS (model checking finished, no error, and the never-enabled
# actions equal the row's never column), FAIL (exactly the row's named
# property violated, nothing else), REACHED (exactly the row's Witness_
# invariant violated: the scenario is reachable), SIMULATION (-simulate
# finished clean; never a model-checking result), INCONCLUSIVE (a
# WithinBudget trip; never a pass or a caught mutant), ABORTED (no Finished
# line: TLC did not end normally; matches no expectation), WRONG (anything
# else). Every outcome must equal the row's expect column. Optional
# arguments name configs to run after their module's self-test; a module
# none of them names is skipped. Scratch is removed when every row matches;
# otherwise the logs stay for review.
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
    committed=$(ls MC_*.cfg configs*.tsv | LC_ALL=C sort)
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
    # Grounding, per module: every catalogue name exists in its module,
    # every code symbol in crates/' Rust sources (outside tests/).
    rust=(-- 'crates/*.rs' ':!crates/*/tests/*')
    # The lines of non-test Rust code naming $1 as a whole word, comments
    # stripped (a // comment's text, and /* or * comment lines).
    code_lines() {
        { git -C {{ root }} grep -h -w -F -e "$1" "${rust[@]}" || true; } |
            sed -E -e 's|//.*$||' -e '/^[[:space:]]*(\/\*|\*)/d' | { grep -w -F -e "$1" || true; }
    }
    catalogue=$scratch/catalogue.json
    # The quoted strings of a module's set definition NAME == {...}.
    set_members() {
        awk -v name="$2" '$0 ~ "^" name " +==" { on = 1 } on { print } on && /}/ { exit }' "$1" |
            { grep -oE '"[A-Za-z0-9_]+"' || true; } | tr -d '"' | LC_ALL=C sort -u
    }
    # Both directions: names in the catalogue the module lacks, and back.
    agree() {
        local what=$1 where=$2 spec_side=$3 catalogue_side=$4 name
        while read -r name; do
            echo "tla-check: $what $name is not in $where" >&2
            ungrounded=$((ungrounded + 1))
        done < <(LC_ALL=C comm -13 <(echo "$spec_side") <(echo "$catalogue_side") | sed '/^$/d')
        while read -r name; do
            echo "tla-check: $where's $what $name has no catalogue entry" >&2
            ungrounded=$((ungrounded + 1))
        done < <(LC_ALL=C comm -23 <(echo "$spec_side") <(echo "$catalogue_side") | sed '/^$/d')
    }
    ungrounded=0
    modules=$("$jq" '.grounding | length' "$catalogue")
    summary=()
    for ((m = 0; m < modules; m++)); do
        g=".grounding[$m]"
        spec=$("$jq" -r "$g.spec" "$catalogue")
        operators=$("$jq" -r "$g.operators[]" "$catalogue")
        constants=$("$jq" -r "$g.constants[]" "$catalogue")
        mutants=$("$jq" -r "$g.mutations[]" "$catalogue")
        symbols=$("$jq" -r "$g.codeSymbols | unique | .[]" "$catalogue")
        symbol_match=$("$jq" -r "$g.symbolMatch" "$catalogue")
        for name in $operators; do
            if ! grep -qE "^${name}"'(\(.*\))? ==' "$spec"; then
                echo "tla-check: $name is not defined in $spec" >&2
                ungrounded=$((ungrounded + 1))
            fi
        done
        # Constants and mutations must agree in both directions: the
        # catalogue sets every declared constant and no other, and it has an
        # entry (a verdict and a primary MC_ neg row) for every rule break
        # in the module's Mutations set ("none" aside) and no other.
        declared=$(awk '/^CONSTANTS/ { on = 1; next } on && /^$/ { on = 0 } on' "$spec")
        declared_names=$({ grep -oE '^ +[A-Za-z][A-Za-z0-9_]*' <<<"$declared" || true; } | tr -d ' ' | LC_ALL=C sort -u)
        agree constant "$spec" "$declared_names" "$(LC_ALL=C sort -u <<<"$constants")"
        agree mutation "$spec" "$(set_members "$spec" Mutations | sed '/^none$/d')" "$(LC_ALL=C sort -u <<<"$mutants")"
        # The decision core's closed unions: the module's set holds exactly
        # the catalogue's labels.
        sets=$("$jq" -r "$g.labelSets | length" "$catalogue")
        for ((k = 0; k < sets; k++)); do
            set=$("$jq" -r "$g.labelSets[$k].name" "$catalogue")
            labels=$("$jq" -r "$g.labelSets[$k].labels[]" "$catalogue" | LC_ALL=C sort -u)
            agree "$set label" "$spec" "$(set_members "$spec" "$set")" "$labels"
        done
        for name in $symbols; do
            if [[ ! $name =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
                echo "tla-check: code symbol $name is not a Rust identifier" >&2
                ungrounded=$((ungrounded + 1))
            elif [[ $symbol_match == definition ]]; then
                if ! git -C {{ root }} grep -q -E \
                    -e "(^|[^A-Za-z0-9_])(fn|const|static|struct|enum|trait|type|mod)[[:space:]]+${name}([^A-Za-z0-9_]|\$)" \
                    "${rust[@]}"; then
                    echo "tla-check: code symbol $name has no definition in crates/' Rust sources" >&2
                    ungrounded=$((ungrounded + 1))
                fi
            elif [[ $symbol_match != code || -z $(code_lines "$name") ]]; then
                echo "tla-check: code symbol $name is not in crates/' Rust code ($symbol_match)" >&2
                ungrounded=$((ungrounded + 1))
            fi
        done
        "$jq" -r "$g.pendingSymbols | unique | .[]" "$catalogue" | sed "s|^|catalogue: $spec cites pending (no code yet): |"
        summary+=("$(printf '%s: %s operators, %s constants, %s mutations, %s label sets, %s code symbols' \
            "$spec" "$(wc -w <<<"$operators")" "$(wc -w <<<"$constants")" "$(wc -w <<<"$mutants")" \
            "$sets" "$(wc -w <<<"$symbols")")")
    done
    if [[ $ungrounded -ne 0 ]]; then
        echo "tla-check: $ungrounded catalogue name(s) are not grounded" >&2
        rm -rf "$scratch"
        exit 1
    fi
    printf 'catalogue: %s files current; grounded %s\n' "$(wc -w <<<"$rendered")" "$(IFS=';'; echo "${summary[*]}" | sed 's/;/; /g')"
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
    for ((m = 0; m < modules; m++)); do
        tsv=$("$jq" -r ".grounding[$m].tsv" "$catalogue")
        spec=$("$jq" -r ".grounding[$m].spec" "$catalogue")
        if [[ $wanted != "  " ]] && ! awk -F'\t' -v wanted="$wanted" '$1 !~ /^#/ && index(wanted, " " $1 " ") { found = 1 } END { exit !found }' "$tsv"; then
            continue
        fi
        run_module "$tsv" "$spec"
    done
    printf 'total wall %ss, peak rss %s MiB\n' "$((SECONDS - begun))" "$peak"
    if [[ $mismatches -ne 0 ]]; then
        echo "tla-check: $mismatches config(s) did not match their expectation" >&2
        exit 1
    fi
    rm -rf "$scratch"

# Haskell cross-checks (OI-1003-Q32, OI-1003-Q43; docs/formal/README.md,
# "Hybrid roles" and "GitCarry"). Standalone and on demand: no tier depends
# on it, so check-fast, check-optional, check-full and CI never build it.
# GHC, TLC, Dhall and jq come from the flake's pinned nixpkgs (no flake
# change). docs/formal/hs/Explorer.hs and hs/GitCarryCore.hs use base and
# containers only; each is built with -O1 -Wall -Werror into a private
# mktemp directory under TMPDIR. Its rows:
# - BulkloadTransfer: the presets nv_core and nv_ledger must reach TLC's
#   distinct-state counts of record, MC_nv_core's 15,834 and MC_nv_ledger's
#   142,450 (the transfer before #169), and nv_core_adopt and
#   nv_ledger_adopt must reach MC_nv_core_adopt's 17,027 and
#   MC_nv_ledger_adopt's 185,852 (the code since #169: the capture record
#   and its adoption), with no invariant violated and no deadlock;
# - every MC_neg_ row of the catalogue inside the explorer's domain (the
#   evaluated catalogue's nversion list) runs at its own bound, passed as
#   the explorer's bound flags and switches (--adopt, --strict-held),
#   checking TypeOK and the row's named property, as its TLC config does;
#   it must violate exactly that property;
# - each primary row among them runs again with every safety invariant
#   checked (and #169's, where the row's constants set them), on the
#   explorer and on TLC with one worker (the catalogue's scratch config for
#   it): both must stop at the same first violated invariant after the same
#   number of states;
# - GitCarry: crates/bulkload-agent/tests/data/decide_rows.tsv must be
#   GitCarryCore's rendering byte for byte (`rows --check`), and its
#   closed unions (`schema`) must equal the catalogue's Basis and Decision
#   labels;
# - GitCarry's explorer presets gc_core, gc_q46, gc_grouped, gc_fix2,
#   gc_reroot, gc_reroot_extended and gc_fix2_deep must reach TLC's
#   distinct-state counts of record for MC_gc_core (45,062), MC_gc_q46
#   (699,419), MC_gc_grouped (85,941), MC_gc_fix2 (392,515), MC_gc_reroot
#   (675,517), MC_gc_reroot_extended (706,430) and MC_gc_fix2_deep
#   (366,285), with no invariant violated, and each must take the capture
#   decisions it exists for (v1's re-base, Q46's re-root, the base refusal,
#   fix 2's chain under a base); every MC_gc_neg_ row (gitCarryNversion) is
#   checked as the MC_neg_ rows are, against GitCarry.tla.
# Each explorer counterexample is written as JSON; that directory is kept
# and printed, and so are the TLC logs.
# Cross-check the TLA+ models with the Haskell explorers and decision core.
formal-nv:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{ root }}/docs/formal
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/formal-nv.XXXXXX")
    mkdir -p "$scratch/build" "$scratch/build-gc" "$scratch/counterexamples" "$scratch/tlc/java"
    if ! (cd {{ root }} && {{ just_executable() }} tla-render "$scratch/rendered" "$scratch/catalogue.json") >"$scratch/render.log" 2>&1; then
        cat "$scratch/render.log" >&2
        echo "formal-nv: the catalogue did not evaluate; log kept at $scratch/render.log" >&2
        exit 1
    fi
    tlc=$(nix shell --inputs-from {{ root }} nixpkgs#tlaplus --command sh -c 'command -v tlc')
    jq=$(nix shell --inputs-from {{ root }} nixpkgs#jq --command sh -c 'command -v jq')
    for program in Explorer GitCarryCore; do
        out=$scratch/explorer
        dir=$scratch/build
        if [[ $program == GitCarryCore ]]; then
            out=$scratch/gitcarry
            dir=$scratch/build-gc
        fi
        if ! nix shell --inputs-from {{ root }} nixpkgs#ghc --command nice -n 10 ghc -O1 -Wall -Werror -outputdir "$dir" -o "$out" "hs/$program.hs" >"$scratch/ghc-$program.log" 2>&1; then
            cat "$scratch/ghc-$program.log" >&2
            echo "formal-nv: hs/$program.hs did not build; log kept at $scratch/ghc-$program.log" >&2
            exit 1
        fi
    done
    field() { sed -n "s/.* $1=\([^ ]*\).*/\1/p" <<<" $2"; }
    mismatches=0
    format='%-36s %-42s %-42s %9s %10s %6s %s\n'
    export JAVA_TOOL_OPTIONS="-Djava.io.tmpdir=$scratch/tlc/java -Xmx2g"
    # One module's presets and mutation rows: EXPLORER, the module's spec,
    # the catalogue's list of rows, then each preset as NAME=COUNT, TLC's
    # distinct-state count of record, or NAME=COUNT=DECISION,..., the
    # capture decisions its search must also take (GitCarryCore only).
    cross_check() {
        local spec=$2 rows_key=$3
        local preset want got line status i rows name mutation named primary flags every log tlc_first tlc_states first
        local -a explorer bound
        read -r -a explorer <<<"$1"
        shift 3
        printf "$format" row expect explorer distinct generated depth match
        local count required decision
        for preset in "$@"; do
            count=${preset#*=}
            required=
            if [[ $count == *=* ]]; then
                required=${count#*=}
                count=${count%%=*}
            fi
            status=0
            line=$(nice -n 10 "${explorer[@]}" --preset "${preset%%=*}") || status=$?
            want="pass ($count)"
            got="$(field outcome "$line") ($(field distinct "$line"))"
            match=yes
            if [[ $status -ne 0 || $got != "$want" ]]; then
                match=no
                mismatches=$((mismatches + 1))
            fi
            printf "$format" "$(field row "$line")" "$want" "$got" "$(field distinct "$line")" "$(field generated "$line")" "$(field depth "$line")" "$match"
            if [[ -n $required ]]; then
                printf '    decisions: %s\n' "$(field decisions "$line")"
                for decision in ${required//,/ }; do
                    if [[ ,$(field decisions "$line"), != *,"$decision",* ]]; then
                        echo "formal-nv: ${preset%%=*} never takes the decision $decision" >&2
                        mismatches=$((mismatches + 1))
                    fi
                done
            fi
        done
        rows=$("$jq" ".$rows_key | length" "$scratch/catalogue.json")
        if [[ $rows -eq 0 ]]; then
            echo "formal-nv: the catalogue has no $rows_key row in the explorer's domain" >&2
            exit 1
        fi
        for ((i = 0; i < rows; i++)); do
            if [[ $rows_key == nversion ]]; then
                IFS=$'\t' read -r name mutation named primary flags < <(
                    "$jq" -r ".nversion[$i] | [.name, .mutation, .property, .primary, \"--seats \" + .seats + \" --runs \" + (.runs | tostring) + \" --crashes \" + (.crashes | tostring) + \" --edits \" + (.edits | tostring) + \" --foreign \" + (.foreign | tostring) + .switches] | @tsv" "$scratch/catalogue.json")
            else
                IFS=$'\t' read -r name mutation named primary flags < <(
                    "$jq" -r ".$rows_key[$i] | [.name, .mutation, .property, .primary, .flags] | @tsv" "$scratch/catalogue.json")
            fi
            read -r -a bound <<<"$flags"
            bound+=(--mutation "$mutation")
            status=0
            line=$(nice -n 10 "${explorer[@]}" "${bound[@]}" --name "$name" --check "TypeOK,$named" --json "$scratch/counterexamples") || status=$?
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
            "$jq" -j ".$rows_key[$i].everyInvariant" "$scratch/catalogue.json" >"$scratch/tlc/$every.cfg"
            nice -n 10 "$tlc" -workers 1 -metadir "$scratch/tlc/$every.states" -config "$scratch/tlc/$every.cfg" "$spec" >"$log" 2>&1 || true
            rm -rf "$scratch/tlc/$every.states"
            tlc_first=$(grep -oE 'Invariant [A-Za-z0-9_]+ is violated' "$log" | head -n 1 | cut -d' ' -f2 || true)
            if grep -qF 'Deadlock reached' "$log"; then
                tlc_first=deadlock
            fi
            tlc_states=$(grep -cE '^State [0-9]+:' "$log" || true)
            status=0
            line=$(nice -n 10 "${explorer[@]}" "${bound[@]}" --name "$every" --check all --json "$scratch/counterexamples") || status=$?
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
    }
    # BulkloadTransfer.tla: TLC's distinct-state counts of record (README.md,
    # "N-version core").
    cross_check "$scratch/explorer" BulkloadTransfer.tla nversion nv_core=15834 nv_ledger=142450 nv_core_adopt=17027 nv_ledger_adopt=185852
    # GitCarry.tla (OI-1003-Q43): the pinned rows, the closed unions, then
    # the custody explorer at TLC's counts of record (README.md, "GitCarry").
    rows_file={{ root }}/crates/bulkload-agent/tests/data/decide_rows.tsv
    if ! "$scratch/gitcarry" rows --check "$rows_file"; then
        mismatches=$((mismatches + 1))
    fi
    schema_want=$("$jq" -r '.decide[] | "\(.name) \(.labels | sort | join(" "))"' "$scratch/catalogue.json" | LC_ALL=C sort)
    schema_got=$("$scratch/gitcarry" schema | while read -r name rest; do
        echo "$name $(tr ' ' '\n' <<<"$rest" | LC_ALL=C sort | paste -sd' ' -)"
    done | LC_ALL=C sort)
    if [[ $schema_got == "$schema_want" ]]; then
        echo "schema: GitCarryCore's closed unions equal the catalogue's ($(wc -l <<<"$schema_got") unions)"
    else
        echo "formal-nv: GitCarryCore's closed unions differ from the catalogue's:" >&2
        diff <(echo "$schema_want") <(echo "$schema_got") >&2 || true
        mismatches=$((mismatches + 1))
    fi
    cross_check "$scratch/gitcarry explore" GitCarry.tla gitCarryNversion \
        gc_core=45062=Hit,Export:Chain:NoRebase,Export:SelfContained:NewRoot \
        gc_q46=699419=Export:Chain:Reroot,Export:SelfContained:NewRoot \
        gc_grouped=85941=Export:Base:NoRebase,Refuse:ReceiptBindingInvalid \
        gc_fix2=392515=Export:BaseAndChain:NoRebase,Export:BaseAndChain:Reroot,Refuse:ReceiptBindingInvalid \
        gc_reroot=675517=Export:Chain:Reroot,Export:SelfContained:NewRoot \
        gc_reroot_extended=706430=Export:Chain:Reroot \
        gc_fix2_deep=366285=Export:BaseAndChain:NoRebase
    rm -rf "$scratch/build" "$scratch/build-gc" "$scratch/explorer" "$scratch/gitcarry" "$scratch/rendered" "$scratch/tlc/java"
    echo "counterexamples (JSON): $scratch/counterexamples; TLC logs: $scratch/tlc"
    if [[ $mismatches -ne 0 ]]; then
        echo "formal-nv: $mismatches row(s) disagree" >&2
        exit 1
    fi

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

# `--work-root NEW --agent BIN --source-corpus DEST --source-work NEW
# --source-repo CHECKOUT --remote-agent BIN [--ssh-config ABS]` pulls the sealed
# estate corpus (#159) from the source over ssh: 3 B reps of N/R/N/R/N for the
# initial copy and a 1 % delta, native `bulkload-agent pull` against `rclone
# copy` over sftp, R-N81 on both hosts, warm resume gated, RSS cap 2 GiB, JSON
# verdict and evidence draft; no wall-clock SLA. Refuses DEST_SPACE unless the
# work root keeps the agent's 25 % free floor after 5 destination copies. Refuses
# NATIVE_REMOTE_ARM_MISSING (#47) when the agent has no pull/serve pair or W5
# streams are asked for. `--dry-run` is a one-host loopback smoke (NOT a gate
# sample); `--under-load` is informational. No neo run until gate (a) passes.
# S1 gate (b) neo->sting pull harness (OI-1003-Q3, OI-1003-Q66)
bench-gate-b *args:
    cd {{ root }} && python3 crates/bulkload-bench/scripts/gate_b.py {{ args }}
