from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

CI_TEMPLATES_REV = "139bd4c7deabbe07c918dc764a3b9f054066431d"
WORKFLOW_SHA256 = "6b3df5845e5f33c4d6acdabcd912898846d69695930373ff14bca7f8152f61f2"
LOCAL_ACTION = "./.github/actions/bulkload-public-read-ci"
LOCAL_ACTION_PATH = ".github/actions/bulkload-public-read-ci/action.yml"
GUARD_PATH = "scripts/ci-public-read-guard.sh"
PUBLIC_KEY = "main:eaUydxuDu7xBoy5cCo3MdknYAkVyTIASQ7DGuwxa+XA="
NIXOS_CACHE = "https://cache.nixos.org/"
NIXOS_PUBLIC_KEY = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
REVIEWED_PATH = "/nix/var/nix/profiles/default/bin:/usr/bin:/bin:/usr/sbin:/sbin"
REVIEWED_STEP_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
ACTION_SHA256 = "2cd1796c907f46d55af04b66d0f2f1c53211c78e80a092787a23edc06aa39ee1"
GUARD_SHA256 = "825d154fb96d8795185420e3b5049f1504bb5aaf29cd521a64f8fb5d500b8e6f"
FAULT_HARNESS_STEP_SHA256 = (
    "fc226b95e6553343298eb65a434df43ec6a07fdda7821b47a66e0ecff06a1bbe"
)
SOURCE_GATE_STEP_SHA256 = (
    "96893435532ebb5d5e4b53e813e2069a28c8c23303a70c1bb9b1a9fb4071bdf2"
)
EFFECTIVE_NIX_STEP_SHA256 = (
    "70c1bd9b875355d76e8649d2904f1aa82f4b6004041bb2c4300980ef79be4a4c"
)
BAZELRC_SHA256 = "f5a7f5116ce0a69471e71b44666fc868e361ed540a40c28a4ee8adc344c87592"
WORKSPACE_BAZELRC_SHA256 = (
    "15aa8306cc530bbc4d143dd7a6a2f0cfd3efbed19c01503d35bbec0c5e7cd357"
)
BOOTSTRAP_IMPL_LINE = (
    "common --@rules_python//python/config_settings:bootstrap_impl=script"
)
BAZEL_VERSION_SHA256 = (
    "4fa9948d0ae7007cbd1cc05768bc3e7cc6ec46ad0ea84c87df79e7a0c48d76b4"
)
FLAKE_SHA256 = "4c16e5b2f9f03342ba66592800f44ed2cfafd95c1ca0315789868495326438bf"
FLAKE_LOCK_SHA256 = "ccd790af791b173623983382a78bd9476760b9fa9e9e617108e2ae3d1040d19d"
EXPECTED_SHA_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.sha || "
    "github.event_name == 'merge_group' && "
    "github.event.merge_group.head_sha || github.sha }}"
)
# The audited trigger inventory (R-N124): main pushes and release tags, pull
# requests, and the main merge queue. Nothing else may start CI.
WORKFLOW_TRIGGERS = (
    "on:\n"
    "  push:\n"
    "    branches: [main]\n"
    '    tags: ["v*"]\n'
    "  pull_request:\n"
    "  merge_group:\n"
    "    types: [checks_requested]\n"
    "\n"
    "permissions:\n"
)
# Every guard invocation runs the digest-checked bytes as a `-c` argument. A
# stdin pipe (`printf | bash -s`) raced the guard's early `exit` and failed with
# a broken pipe when the reader closed before the writer finished.
GUARD_INVOCATION = (
    '/bin/bash --noprofile --norc -p -c "$guard_source" bulkload-public-read-guard'
)

HEAD_REPOSITORY_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.repo.full_name || github.repository }}"
)
SAME_REPOSITORY_GUARD = (
    "${{ github.event_name != 'pull_request' || "
    "github.event.pull_request.head.repo.full_name == github.repository }}"
)
UPLOAD_EXPRESSION = (
    "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' "
    "&& 'true' || 'false' }}"
)
MATRIX_GATE_EXPRESSION = "${{ matrix.gate }}"
TERMINAL_GATES = ("source", "build", "test", "fault-harness")
TERMINAL_CONSUMERS = {
    "source": "Run repository-owned source gates",
    "build": "Build the Bulkload documentation through the public Flywheel action",
    "test": "Test the complete Bulkload Bazel graph through the public Flywheel action",
    "fault-harness": "Run the repository-owned fault harness",
}
ACTION_STEP_GATES = {
    "Revalidate immutable Bazel build authority": "build",
    TERMINAL_CONSUMERS["build"]: "build",
    "Revalidate immutable Bazel test authority": "test",
    TERMINAL_CONSUMERS["test"]: "test",
    TERMINAL_CONSUMERS["fault-harness"]: "fault-harness",
    TERMINAL_CONSUMERS["source"]: "source",
}
JOB_FENCED_ACTION_ENV = {
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "BASH_XTRACEFD",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "FTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "CURL_CA_BUNDLE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "SSLKEYLOGFILE",
}
AUDITED_JOB_RUNNERS = {"test": "tinyland-nix"}
HOSTED_RUNNER_PATTERN = re.compile(
    r"(?:ubuntu|macos|windows)-(?:latest|[0-9][A-Za-z0-9.-]*)", re.IGNORECASE
)
PERMISSIONS_DECLARATION_PATTERN = re.compile(
    r"""^(?:    )?(?:permissions|["']permissions["']):(?:\s*.*)?$"""
)
USES_PATTERN = re.compile(r"(?m)^\s+uses:\s*([^\s#]+)")


class ContractError(ValueError):
    pass


# Exact recipe headers and bodies of the repository gates CI and `just check`
# run (R-N122). The justfile is not digest-pinned, so emptying or rewiring one
# of these recipes must fail here instead.
PINNED_JUST_RECIPES = {
    "rust-check": (
        "rust-check:",
        (
            "cd {{ root }} && cargo fmt --all -- --check",
            "cd {{ root }} && cargo clippy --workspace --all-targets --locked -- -D warnings",
            "cd {{ root }} && cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings",
            "cd {{ root }} && cargo test -p bulkload-agent --lib --locked --features io-trace io::",
            "cd {{ root }} && {{ just_executable() }} io-partial-write-alone",
            "cd {{ root }} && cargo test --workspace --locked",
        ),
    ),
    "io-partial-write-alone": (
        "io-partial-write-alone:",
        (
            "#!/usr/bin/env bash",
            "set -euo pipefail",
            "cd {{ root }}",
            "status=0",
            "output=$(BULKLOAD_IO_PARTIAL_WRITE_ALONE=1 cargo test -p bulkload-agent --lib --locked --features io-trace io::tests::traced::partial_write_prefix_is_traced -- --ignored --exact --test-threads=1 --nocapture 2>&1) || status=$?",
            "printf '%s\\n' \"$output\"",
            "if [[ $status -ne 0 ]]; then",
            '    echo "io-partial-write-alone: cargo test failed with status $status" >&2',
            '    exit "$status"',
            "fi",
            "if [[ $output == *SKIPPED* ]]; then",
            '    echo "io-partial-write-alone: the P5 proof skipped itself" >&2',
            "    exit 1",
            "fi",
            "results=$(grep -c '^test result: ' <<<\"$output\" || true)",
            "passed=$(grep -c '^test result: ok\\. 1 passed; 0 failed;' <<<\"$output\" || true)",
            "proved=$(grep -c '^test io::tests::traced::partial_write_prefix_is_traced \\.\\.\\. ok$' <<<\"$output\" || true)",
            "if [[ $results -ne 1 || $passed -ne 1 ]]; then",
            "    echo \"io-partial-write-alone: expected exactly one '1 passed; 0 failed' result\" >&2",
            "    exit 1",
            "fi",
            "if [[ $proved -ne 1 ]]; then",
            '    echo "io-partial-write-alone: the passing test was not the P5 proof" >&2',
            "    exit 1",
            "fi",
        ),
    ),
    "fault-harness": (
        "fault-harness:",
        (
            "cd {{ root }} && cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings",
            "cd {{ root }} && cargo test -p bulkload-agent --locked --features fault-injection --target-dir target/fault --test fault_harness",
            "cd {{ root }} && cargo test -p bulkload-agent --locked --features io-trace --target-dir target/fault --test power_loss",
            "cd {{ root }} && {{ just_executable() }} resume-power-loss",
        ),
    ),
    "resume-power-loss": (
        "resume-power-loss:",
        (
            "#!/usr/bin/env bash",
            "set -euo pipefail",
            "cd {{ root }}",
            "status=0",
            "output=$(cargo test -p bulkload-agent --lib --locked --features io-trace --target-dir target/fault materialize::adoption_power_loss:: 2>&1) || status=$?",
            "printf '%s\\n' \"$output\"",
            "if [[ $status -ne 0 ]]; then",
            '    echo "resume-power-loss: cargo test failed with status $status" >&2',
            '    exit "$status"',
            "fi",
            "results=$(grep -c '^test result: ' <<<\"$output\" || true)",
            "passed=$(grep -c '^test result: ok\\. 2 passed; 0 failed;' <<<\"$output\" || true)",
            "if [[ $results -ne 1 || $passed -ne 1 ]]; then",
            "    echo \"resume-power-loss: expected exactly one '2 passed; 0 failed' result\" >&2",
            "    exit 1",
            "fi",
            "for proof in an_adopted_fallback_directory_is_sealed_before_its_record_binds a_directory_adopted_by_its_bound_record_is_sealed_before_outputs_commit; do",
            '    if [[ $(grep -c "^test materialize::adoption_power_loss::$proof \\.\\.\\. ok$" <<<"$output" || true) -ne 1 ]]; then',
            '        echo "resume-power-loss: $proof was not among the passing tests" >&2',
            "        exit 1",
            "    fi",
            "done",
        ),
    ),
    "check-source": (
        "check-source: repo-manifest-validate python-lint shell-lint workflow-lint secrets-scan-dir rust-check",
        (),
    ),
    "ci-source": (
        "ci-source: check-source secrets-scan-history",
        (),
    ),
    "ci-fault-harness": (
        "ci-fault-harness: fault-harness",
        (),
    ),
    "check": (
        "check:",
        (
            "cd {{ root }} && nix develop .#default --command just check-source",
            "cd {{ root }} && nix develop .#default --command just fault-harness",
            "cd {{ root }} && just test",
        ),
    ),
}


def just_recipe(justfile: str, name: str) -> tuple[str, tuple[str, ...]]:
    lines = justfile.splitlines()
    headers = [
        index
        for index, line in enumerate(lines)
        if line == name + ":" or line.startswith(name + ": ")
    ]
    if len(headers) != 1:
        raise ContractError(f"just recipe {name} must be declared exactly once")
    body = []
    for line in lines[headers[0] + 1 :]:
        if not line.startswith("    "):
            break
        body.append(line.removeprefix("    "))
    return lines[headers[0]], tuple(body)


P5_COMMAND = (
    "output=$(BULKLOAD_IO_PARTIAL_WRITE_ALONE=1 cargo test -p bulkload-agent --lib "
    "--locked --features io-trace io::tests::traced::partial_write_prefix_is_traced "
    "-- --ignored --exact --test-threads=1 --nocapture 2>&1) || status=$?"
)


def validate_p5_alone(justfile: str) -> None:
    """The P5 proof (#69) must really run: it skips itself and still reports
    `1 passed` unless BULKLOAD_IO_PARTIAL_WRITE_ALONE is set, so its recipe
    must set the variable, select the one test exactly, and reject a skip or
    any result other than one `1 passed; 0 failed`."""
    _, rust_check = just_recipe(justfile, "rust-check")
    if rust_check.count(
        "cd {{ root }} && {{ just_executable() }} io-partial-write-alone"
    ) != 1 or any("partial_write_prefix_is_traced" in line for line in rust_check):
        raise ContractError(
            "rust-check must run P5 only through io-partial-write-alone"
        )
    _, body = just_recipe(justfile, "io-partial-write-alone")
    required = (
        P5_COMMAND,
        "if [[ $output == *SKIPPED* ]]; then",
        "results=$(grep -c '^test result: ' <<<\"$output\" || true)",
        "passed=$(grep -c '^test result: ok\\. 1 passed; 0 failed;' <<<\"$output\" || true)",
        "if [[ $results -ne 1 || $passed -ne 1 ]]; then",
        "proved=$(grep -c '^test io::tests::traced::partial_write_prefix_is_traced "
        '\\.\\.\\. ok$\' <<<"$output" || true)',
        "if [[ $proved -ne 1 ]]; then",
        "if [[ $status -ne 0 ]]; then",
    )
    for line in required:
        if body.count(line) != 1:
            raise ContractError(f"P5 must run alone and fail closed: {line}")


# Top-level justfile lines that change how every recipe runs: settings,
# exports, imports, modules, aliases and variables. A later `set
# allow-duplicate-recipes`, a redirected `root :=` or an `export PATH := stub`
# would rewire the pinned recipes without touching their bodies.
JUSTFILE_TOP_LEVEL = (
    'set shell := ["bash", "-euo", "pipefail", "-c"]',
    'export PYTHONDONTWRITEBYTECODE := "1"',
    "root := justfile_directory()",
    'import? "justfile.flywheel"',
)
JUST_TOP_LEVEL_PATTERN = re.compile(
    r"^(?:set|export|import|mod|alias)\b|^[A-Za-z_][A-Za-z0-9_-]*\s*:="
)
JUST_GLOBAL_DIRECTIVE_PATTERN = re.compile(r"^(?:set|export|import|mod|alias)\b")


def just_top_level(justfile: str) -> tuple[str, ...]:
    return tuple(
        line for line in justfile.splitlines() if JUST_TOP_LEVEL_PATTERN.match(line)
    )


def just_header_count(justfile: str, name: str) -> int:
    return len(re.findall(rf"(?m)^@?{re.escape(name)}(?=[\s:])", justfile))


def validate_just_recipes(justfile: str, imported: str | None = None) -> None:
    for name, expected in PINNED_JUST_RECIPES.items():
        if just_header_count(justfile, name) != 1:
            raise ContractError(f"just recipe {name} must be declared exactly once")
        if just_recipe(justfile, name) != expected:
            raise ContractError(f"just recipe {name} drifted from its pinned body")
    if just_top_level(justfile) != JUSTFILE_TOP_LEVEL:
        raise ContractError("justfile top-level settings or variables drifted")
    if imported is not None:
        # The fleet-managed import may change its own recipes, but it may not
        # set, export, import, alias or redefine anything the gates rely on.
        if any(
            JUST_GLOBAL_DIRECTIVE_PATTERN.match(line) for line in imported.splitlines()
        ):
            raise ContractError("imported justfile must not declare global directives")
        for name in PINNED_JUST_RECIPES:
            if just_header_count(imported, name):
                raise ContractError(f"imported justfile must not declare {name}")
    validate_p5_alone(justfile)


def sha256(source: str) -> str:
    return hashlib.sha256(source.encode()).hexdigest()


def find_workspace() -> Path:
    candidates = [Path.cwd(), Path(__file__).resolve()]
    runfiles = os.environ.get("RUNFILES_DIR")
    if runfiles:
        candidates.extend([Path(runfiles) / "_main", Path(runfiles) / "bulkload"])
    for candidate in candidates:
        for parent in [candidate, *candidate.parents]:
            if (parent / "tinyland.repo.json").is_file() and (
                parent / ".github/workflows/ci.yml"
            ).is_file():
                return parent
    raise AssertionError("cannot locate bulkload runfiles workspace")


def parse_job_contract(workflow: str) -> dict[str, dict[str, list[str]]]:
    lines = workflow.splitlines()
    jobs_markers = [index for index, line in enumerate(lines) if line == "jobs:"]
    if len(jobs_markers) != 1:
        raise ContractError("CI must declare exactly one block-style jobs mapping")

    jobs: dict[str, dict[str, list[str]]] = {}
    current_job: str | None = None
    for line in lines[jobs_markers[0] + 1 :]:
        if line and not line.startswith((" ", "\t", "#")):
            break
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        job_match = re.fullmatch(r"  ([A-Za-z_][A-Za-z0-9_-]*):", line)
        if job_match:
            current_job = job_match.group(1)
            if current_job in jobs:
                raise ContractError(f"duplicate workflow job: {current_job}")
            jobs[current_job] = {}
            continue
        if re.match(r"^  \S", line):
            raise ContractError("workflow jobs must use the audited block mapping form")
        if current_job is None:
            raise ContractError("workflow job property appears before a job identifier")
        property_match = re.fullmatch(
            r"    ([A-Za-z_][A-Za-z0-9_-]*):(?:\s*(.*))?", line
        )
        if property_match:
            key = property_match.group(1)
            value = property_match.group(2) or ""
            jobs[current_job].setdefault(key, []).append(value)
            continue
        if line.startswith("    ") and not line.startswith("      "):
            raise ContractError(
                "workflow job properties must use literal block mapping keys"
            )

    if not jobs:
        raise ContractError("CI must declare at least one audited job")
    return jobs


def validate_job_routing(workflow: str) -> None:
    jobs = parse_job_contract(workflow)
    if set(jobs) != set(AUDITED_JOB_RUNNERS):
        raise ContractError("workflow job inventory is not audited")
    for job, expected_runner in AUDITED_JOB_RUNNERS.items():
        properties = jobs[job]
        if properties.get("uses"):
            raise ContractError(f"job-level reusable workflow is forbidden: {job}")
        if properties.get("runs-on") != [expected_runner]:
            raise ContractError(f"job {job} must use literal {expected_runner}")
        if properties.get("if") != [SAME_REPOSITORY_GUARD]:
            raise ContractError(f"job {job} must reject fork pull requests")

    matrix = (
        "    strategy:\n"
        "      fail-fast: false\n"
        "      matrix:\n"
        "        gate: [source, build, test, fault-harness]\n"
    )
    if workflow.count(matrix) != 1:
        raise ContractError("terminal gate matrix must be one exact literal inventory")
    if workflow.count("matrix:") != 1 or workflow.count(MATRIX_GATE_EXPRESSION) != 2:
        raise ContractError("terminal gate matrix authority escaped its audited scope")

    if workflow.count(WORKFLOW_TRIGGERS) != 1 or not workflow.startswith(
        "name: CI\n\n" + WORKFLOW_TRIGGERS
    ):
        raise ContractError("workflow trigger inventory drifted")
    if re.search(
        r"(?m)^\s*(?:pull_request_target|workflow_run|workflow_dispatch)\s*:", workflow
    ):
        raise ContractError("workflow must not add an unaudited trigger")

    if re.search(
        r"(?mi)^\s*(?:continue-on-error|\"continue-on-error\"|'continue-on-error')\s*:",
        workflow,
    ):
        raise ContractError("CI workflow must not suppress a job or step failure")
    # The single job-level cap applies to every matrix gate (R-N122).
    timeouts = re.findall(
        r"(?mi)^\s*(?:timeout-minutes|\"timeout-minutes\"|'timeout-minutes')\s*:.*$",
        workflow,
    )
    if timeouts != ["    timeout-minutes: 15"]:
        raise ContractError("every terminal gate must keep the audited 15-minute cap")

    declarations = [
        line
        for line in workflow.splitlines()
        if "runs-on:" in line and not line.lstrip().startswith("#")
    ]
    if declarations != ["    runs-on: tinyland-nix"]:
        raise ContractError("every runs-on declaration must be an audited exact scalar")


def validate_permissions(workflow: str) -> None:
    lines = workflow.splitlines()
    declarations = [
        (index, line)
        for index, line in enumerate(lines)
        if PERMISSIONS_DECLARATION_PATTERN.fullmatch(line) is not None
    ]
    if len(declarations) != 1 or declarations[0][1] != "permissions:":
        raise ContractError("CI must declare one top-level permissions block only")
    start = declarations[0][0]
    block: list[str] = []
    for line in lines[start + 1 :]:
        if line and not line.startswith((" ", "\t", "#")):
            break
        if line.strip() and not line.lstrip().startswith("#"):
            block.append(line)
    if block != ["  contents: read"]:
        raise ContractError("CI permissions must be exactly contents: read")


def extract_uses(source: str) -> list[str]:
    canonical = USES_PATTERN.findall(source)
    non_comment_mentions = [
        line
        for line in source.splitlines()
        if "uses" in line and not line.lstrip().startswith("#")
    ]
    if len(canonical) != len(non_comment_mentions):
        raise ContractError("every uses declaration must use canonical block syntax")
    return canonical


def extract_action_step(action: str, name: str) -> str:
    marker = f"    - name: {name}\n"
    if action.count(marker) != 1:
        raise ContractError(f"composite step inventory drifted: {name}")
    start = action.index(marker)
    next_step = action.find("    - name: ", start + len(marker))
    end = len(action) if next_step == -1 else next_step
    return action[start:end]


def parse_canonical_env_entries(
    lines: list[str], *, start: int, entry_indent: int
) -> list[tuple[str, str]]:
    entry_prefix = " " * entry_indent
    literal_prefix = " " * (entry_indent + 2)
    parsed: list[tuple[str, str]] = []
    seen: dict[str, str] = {}
    index = start
    while index < len(lines):
        line = lines[index]
        if not line.startswith(entry_prefix) or line.startswith(entry_prefix + " "):
            break
        match = re.fullmatch(rf" {{{entry_indent}}}([^:#][^:]*): (.+)", line)
        if match is None:
            raise ContractError("environment must use canonical scalar entries")
        key = match.group(1).strip()
        if len(key) >= 2 and key[0] == key[-1] and key[0] in "\"'":
            key = key[1:-1]
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_%]*", key) is None:
            raise ContractError("environment key is not auditable")
        folded_key = key.casefold()
        if folded_key in seen:
            raise ContractError(
                "environment key is case-insensitively duplicated: "
                f"{seen[folded_key]} / {key}"
            )
        seen[folded_key] = key

        value = match.group(2)
        if value == "|-":
            literal: list[str] = []
            index += 1
            while index < len(lines) and lines[index].startswith(literal_prefix):
                literal_line = lines[index]
                if literal_line.startswith(literal_prefix + " "):
                    raise ContractError(
                        "environment literal must use canonical indentation"
                    )
                literal.append(literal_line[len(literal_prefix) :])
                index += 1
            if not literal:
                raise ContractError("environment literal must not be empty")
            value = "|-\n" + "\n".join(literal)
            parsed.append((key, value))
            continue
        if value.startswith(("|", ">", "&", "*", "{")):
            raise ContractError("environment value uses unaudited YAML syntax")
        parsed.append((key, value))
        index += 1
    return parsed


def parse_action_step_env(step: str) -> list[tuple[str, str]]:
    lines = step.splitlines()
    markers: list[int] = []
    for index, line in enumerate(lines):
        if not line.startswith("      ") or line.startswith("       "):
            continue
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        field = re.fullmatch(r"      ([a-z][a-z0-9-]*):(.*)", line)
        if field is None:
            raise ContractError(
                "composite step fields must use canonical block mapping syntax"
            )
        if field.group(1) == "env":
            if field.group(2):
                raise ContractError(
                    "composite step env must use canonical block mapping syntax"
                )
            markers.append(index)
    if len(markers) > 1:
        raise ContractError("composite step declares more than one env mapping")
    if not markers:
        return []
    return parse_canonical_env_entries(lines, start=markers[0] + 1, entry_indent=8)


def validate_action_step_environment_fences(action: str) -> None:
    for name in re.findall(r"(?m)^    - name: (.+)$", action):
        for key, value in parse_action_step_env(extract_action_step(action, name)):
            canonical = key.upper()
            if canonical in {"BASH_ENV", "ENV"}:
                if value != "/dev/null":
                    raise ContractError(f"step overrides the {canonical} shell fence")
                continue
            if canonical in JOB_FENCED_ACTION_ENV or canonical.startswith("BASH_FUNC_"):
                raise ContractError(f"step overrides job-fenced environment: {key}")


def action_step_condition(step: str) -> str | None:
    conditions = re.findall(r"(?m)^      if: (.+)$", step)
    if len(conditions) > 1:
        raise ContractError("composite step declares more than one condition")
    return conditions[0] if conditions else None


def selected_action_path(action: str, gate: str) -> list[str]:
    if gate not in TERMINAL_GATES:
        raise ContractError(f"unknown terminal gate: {gate}")
    names = re.findall(r"(?m)^    - name: (.+)$", action)
    selected: list[str] = []
    for name in names:
        condition = action_step_condition(extract_action_step(action, name))
        expected_gate = ACTION_STEP_GATES.get(name)
        if expected_gate is None:
            if condition is not None:
                raise ContractError(f"common step is conditional: {name}")
            selected.append(name)
            continue
        expected_condition = f"${{{{ inputs.gate == '{expected_gate}' }}}}"
        if condition != expected_condition:
            raise ContractError(f"terminal gate condition drifted: {name}")
        if gate == expected_gate:
            selected.append(name)
    return selected


def validate_terminal_consumer_paths(action: str) -> None:
    common = [
        "Validate the terminal gate selection",
        "Snapshot the exact public-read guard",
        "Preflight raw runner endpoint authority",
        "Discover sanctioned runner endpoint authority",
        "Enforce the discovered public-read boundary",
        "Verify effective Nix client authority",
    ]
    expected_paths = {
        "source": [*common, TERMINAL_CONSUMERS["source"]],
        "fault-harness": [*common, TERMINAL_CONSUMERS["fault-harness"]],
        "build": [
            *common,
            "Revalidate immutable Bazel build authority",
            TERMINAL_CONSUMERS["build"],
        ],
        "test": [
            *common,
            "Revalidate immutable Bazel test authority",
            TERMINAL_CONSUMERS["test"],
        ],
    }
    consumer_names = set(TERMINAL_CONSUMERS.values())
    for gate in TERMINAL_GATES:
        selected = selected_action_path(action, gate)
        if selected != expected_paths[gate]:
            raise ContractError(f"selected {gate} action path drifted")
        consumers = [name for name in selected if name in consumer_names]
        if consumers != [TERMINAL_CONSUMERS[gate]]:
            raise ContractError(f"{gate} must select exactly one repository consumer")
        if selected[-1] != TERMINAL_CONSUMERS[gate]:
            raise ContractError(f"{gate} repository consumer is not terminal")


def extract_workflow_step(workflow: str, name: str) -> str:
    marker = f"      - name: {name}\n"
    if workflow.count(marker) != 1:
        raise ContractError(f"workflow step inventory drifted: {name}")
    start = workflow.index(marker)
    next_step = workflow.find("      - name: ", start + len(marker))
    end = len(workflow) if next_step == -1 else next_step
    return workflow[start:end]


def parse_workflow_step_env(step: str) -> list[tuple[str, str]]:
    lines = step.splitlines()
    try:
        start = lines.index("        env:") + 1
        end = lines.index("        run: |")
    except ValueError as exc:
        raise ContractError("workflow step must use an explicit env mapping") from exc
    parsed = parse_canonical_env_entries(lines, start=start, entry_indent=10)
    if start + len(parsed) != end:
        raise ContractError("materialization env must use canonical scalar entries")
    return parsed


def parse_job_env(workflow: str) -> list[tuple[str, str]]:
    lines = workflow.splitlines()
    markers = [index for index, line in enumerate(lines) if line == "    env:"]
    if len(markers) != 1:
        raise ContractError("workflow must declare one audited job environment")
    start = markers[0] + 1
    ends = [
        index for index, line in enumerate(lines[start:], start) if line == "    steps:"
    ]
    if len(ends) != 1:
        raise ContractError("workflow job environment must precede steps")
    parsed = parse_canonical_env_entries(lines, start=start, entry_indent=6)
    if start + len(parsed) != ends[0]:
        raise ContractError("job environment must use canonical scalar entries")
    return parsed


def validate_workflow(workflow: str, *, exact_digest: bool = True) -> None:
    if exact_digest and sha256(workflow) != WORKFLOW_SHA256:
        raise ContractError("CI workflow digest drifted")
    if re.search(r"(?i)(?<![A-Za-z0-9_])GRPC_PROXY_EXP(?![A-Za-z0-9_])", workflow):
        raise ContractError("CI workflow must not declare the gRPC proxy override")
    validate_job_routing(workflow)
    validate_permissions(workflow)
    if HOSTED_RUNNER_PATTERN.search(workflow):
        raise ContractError("GitHub-hosted runner label is forbidden")
    if "self-hosted" in workflow or re.search(r"(?m)^\s*runs-on:\s*[\[$]", workflow):
        raise ContractError("generic, array, or dynamic runner routing is forbidden")
    for forbidden in (
        "id-token:",
        "permissions: write-all",
        "contents: write",
        "secrets.",
        "tinyland-inc/GloriousFlywheel",
        "github:tinyland-inc/GloriousFlywheel",
        "bazel-contrib/setup-bazel",
        "cachix/install-nix-action",
        "BAZEL_CREDENTIAL_HELPER",
        "BAZEL_REMOTE_HEADER",
        "BAZEL_REMOTE_CACHE_HEADER",
        "BAZEL_REMOTE_EXEC_HEADER",
    ):
        if forbidden in workflow:
            raise ContractError(f"workflow contains forbidden authority: {forbidden}")
    if (
        workflow.count('"https://github.com"') != 1
        or workflow.count("http.https://github.com/.extraheader") != 1
    ):
        raise ContractError("checkout origin authority must be exactly GitHub")
    endpoint_scan = workflow.replace('"https://github.com"', "").replace(
        "http.https://github.com/.extraheader", ""
    )
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", endpoint_scan):
        raise ContractError("workflow must not bake a deployment endpoint")
    if re.search(r"type\s*=\s*gha", workflow, re.IGNORECASE):
        raise ContractError("GitHub Actions cache authority is forbidden")

    expected_job_env = [
        ("BASH_ENV", "/dev/null"),
        ("ENV", "/dev/null"),
        ("SHELLOPTS", '""'),
        ("BASHOPTS", '""'),
        ("PS4", '""'),
        ("BASH_XTRACEFD", '""'),
        ("LD_PRELOAD", '""'),
        ("LD_LIBRARY_PATH", '""'),
        ("LD_AUDIT", '""'),
        ("http_proxy", '""'),
        ("https_proxy", '""'),
        ("ftp_proxy", '""'),
        ("all_proxy", '""'),
        ("no_proxy", '""'),
        ("CURL_CA_BUNDLE", '""'),
        ("SSL_CERT_FILE", '""'),
        ("SSL_CERT_DIR", '""'),
        ("SSLKEYLOGFILE", '""'),
        ("ATTIC_TOKEN", '""'),
        ("NIX_ACCESS_TOKENS", '""'),
    ]
    if parse_job_env(workflow) != expected_job_env:
        raise ContractError(
            "job-wide loader, function, proxy, CA, or token fence drifted"
        )

    if extract_uses(workflow) != [LOCAL_ACTION]:
        raise ContractError("workflow action inventory drifted")
    if "actions/checkout" in workflow or re.search(
        r"(?mi)^\s+(?:post|post-if)\s*:", workflow
    ):
        raise ContractError("workflow must not register an action post")

    step_names = re.findall(r"(?m)^      - name: (.+)$", workflow)
    if step_names != [
        "Materialize exact revision with fixed Git",
        "Bulkload public-read cache-first validation",
    ]:
        raise ContractError("workflow step order or inventory drifted")
    if len(re.findall(r"(?m)^      - \S", workflow)) != 2:
        raise ContractError("workflow steps must use the audited named inventory")
    materialize = extract_workflow_step(
        workflow, "Materialize exact revision with fixed Git"
    )
    local_action = extract_workflow_step(
        workflow, "Bulkload public-read cache-first validation"
    )
    if f"        uses: {LOCAL_ACTION}\n" not in local_action:
        raise ContractError("local public-read action must remain the final step")
    if not workflow.endswith(local_action):
        raise ContractError("no workflow step may follow the local action")

    token_declaration = "          BULKLOAD_CHECKOUT_TOKEN: ${{ github.token }}"
    expected_declaration = f"          BULKLOAD_EXPECTED_SHA: {EXPECTED_SHA_EXPRESSION}"
    if workflow.count(token_declaration) != 1 or workflow.count("github.token") != 1:
        raise ContractError(
            "checkout token must exist only in the materialization step"
        )
    if token_declaration not in materialize or "github.token" in local_action:
        raise ContractError("checkout token escaped its materialization scope")
    if workflow.count(expected_declaration) != 1:
        raise ContractError("materialization must bind the exact event SHA once")
    if workflow.count("GITHUB_ENV") != 1:
        raise ContractError("runner environment command file escaped materialization")

    expected_materializer_env = [
        ("BASH_ENV", "/dev/null"),
        ("ENV", "/dev/null"),
        ("SHELLOPTS", '""'),
        ("BASHOPTS", '""'),
        ("PS4", '""'),
        ("BASH_XTRACEFD", '""'),
        ("LD_PRELOAD", '""'),
        ("LD_LIBRARY_PATH", '""'),
        ("LD_AUDIT", '""'),
        ("HTTP_PROXY", '""'),
        ("HTTPS_PROXY", '""'),
        ("FTP_PROXY", '""'),
        ("ALL_PROXY", '""'),
        ("NO_PROXY", '""'),
        ("CURL_CA_BUNDLE", '""'),
        ("SSL_CERT_FILE", '""'),
        ("SSL_CERT_DIR", '""'),
        ("SSLKEYLOGFILE", '""'),
        ("BULKLOAD_CHECKOUT_TOKEN", "${{ github.token }}"),
        ("BULKLOAD_EXPECTED_SHA", EXPECTED_SHA_EXPRESSION),
        ("BULKLOAD_REPOSITORY", "${{ github.repository }}"),
        ("BULKLOAD_SERVER_URL", "${{ github.server_url }}"),
    ]
    if parse_workflow_step_env(materialize) != expected_materializer_env:
        raise ContractError("materialization bootstrap environment drifted")

    required_materialization = (
        "        shell: /bin/bash --noprofile --norc -p {0}",
        "          set +x",
        "          set +v",
        "          set -euo pipefail",
        "          umask 077",
        "          [[ $- == *p* ]]",
        "          [[ $- != *a* ]]",
        '          [[ -n "${BULKLOAD_CHECKOUT_TOKEN:-}" ]]',
        "          builtin printf '::add-mask::%s\\n' \"$BULKLOAD_CHECKOUT_TOKEN\"",
        "          checkout_token=$BULKLOAD_CHECKOUT_TOKEN",
        "          export -n checkout_token SHELLOPTS BASHOPTS",
        "          unset BULKLOAD_CHECKOUT_TOKEN",
        '          [[ -z "${BULKLOAD_CHECKOUT_TOKEN:-}" ]]',
        "          trap 'unset BULKLOAD_CHECKOUT_TOKEN checkout_token encoded_token git_http_header BULKLOAD_GIT_HTTP_HEADER' EXIT",
        "          unset BASH_ENV ENV CDPATH PS4 BASH_XTRACEFD",
        "          unset LD_PRELOAD LD_LIBRARY_PATH LD_AUDIT",
        "          unset HTTP_PROXY HTTPS_PROXY FTP_PROXY ALL_PROXY NO_PROXY",
        "          unset http_proxy https_proxy ftp_proxy all_proxy no_proxy",
        "          unset CURL_CA_BUNDLE SSL_CERT_FILE SSL_CERT_DIR SSLKEYLOGFILE",
        "          unset GH_TOKEN GITHUB_TOKEN GITLAB_TOKEN SSH_AUTH_SOCK SSH_ASKPASS",
        '          for variable in "${!GIT_@}"; do',
        "          encoded_token=$(",
        "              /usr/bin/base64 -w0",
        "          export -n encoded_token",
        "          builtin printf '::add-mask::%s\\n' \"$encoded_token\"",
        '          git_http_header="AUTHORIZATION: basic $encoded_token"',
        "          export -n git_http_header",
        "          builtin printf '::add-mask::%s\\n' \"$git_http_header\"",
        "          unset checkout_token encoded_token",
        '          [[ -z "${checkout_token:-}" ]]',
        '          [[ -z "${encoded_token:-}" ]]',
        '          [[ "$expected_sha" =~ ^[0-9a-f]{40}$ ]]',
        '          [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]',
        '          test "$server_url" = "https://github.com"',
        "          runner_commands=$runner_temp/_runner_file_commands",
        "          github_env=$GITHUB_ENV",
        '          test ! -L "$workspace"',
        '          test -O "$workspace"',
        '          test "$(/usr/bin/readlink -f -- "$workspace")" = "$workspace"',
        '          test ! -L "$runner_temp"',
        '          test -O "$runner_temp"',
        '          test "$(/usr/bin/readlink -f -- "$runner_temp")" = "$runner_temp"',
        '          test -d "$runner_commands"',
        '          test ! -L "$runner_commands"',
        '          test -O "$runner_commands"',
        '          test "$(/usr/bin/readlink -f -- "$runner_commands")" = "$runner_commands"',
        '          test -f "$github_env"',
        '          test ! -L "$github_env"',
        '          test -O "$github_env"',
        '          test "$(/usr/bin/readlink -f -- "$github_env")" = "$github_env"',
        '          test "$(/usr/bin/dirname -- "$github_env")" = "$runner_commands"',
        '          test -z "$(/usr/bin/find "$workspace" -xdev -mindepth 1 -maxdepth 1 -print -quit)"',
        '          checkout_state=$(/usr/bin/mktemp -d "$runner_temp/bulkload-checkout.XXXXXXXX")',
        '            "$checkout_state/home" \\',
        '            "$checkout_state/xdg" \\',
        '            "$checkout_state/template" \\',
        '            "$checkout_state/tmp"',
        '          : >"$checkout_state/home/.gitconfig"',
        '          /bin/chmod 600 "$checkout_state/home/.gitconfig"',
        "          export HOME=$checkout_state/home",
        "          export XDG_CONFIG_HOME=$checkout_state/xdg",
        "          export GIT_CONFIG_GLOBAL=$checkout_state/home/.gitconfig",
        "          export GIT_CONFIG_NOSYSTEM=1",
        "          export GIT_TERMINAL_PROMPT=0",
        "          export GCM_INTERACTIVE=never",
        '          /usr/bin/git init --template="$checkout_state/template" "$workspace"',
        '          /usr/bin/git -C "$workspace" remote add origin "$origin_url"',
        "            fetch_env_unsets=()",
        "            done < <(compgen -e)",
        "            export PATH=/usr/bin:/bin",
        "            export HOME XDG_CONFIG_HOME GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM",
        "            export GIT_TERMINAL_PROMPT GCM_INTERACTIVE",
        '            export TMPDIR="$checkout_state/tmp"',
        "            export LANG=C LC_ALL=C",
        '            export BULKLOAD_GIT_HTTP_HEADER="$git_http_header"',
        '            exec /usr/bin/env "${fetch_env_unsets[@]}" \\',
        "                --config-env=http.https://github.com/.extraheader=BULKLOAD_GIT_HTTP_HEADER \\",
        "                fetch --force --prune --no-recurse-submodules --no-tags \\",
        "                --no-auto-maintenance --no-write-commit-graph origin \\",
        '                "$expected_sha" \\',
        "                '+refs/heads/*:refs/remotes/origin/*' \\",
        "                '+refs/tags/*:refs/tags/*'",
        "          unset BULKLOAD_GIT_HTTP_HEADER git_http_header",
        '          [[ -z "${BULKLOAD_GIT_HTTP_HEADER:-}" ]]',
        '          [[ -z "${git_http_header:-}" ]]',
        "          trap - EXIT",
        '            checkout --detach --force "$expected_sha"',
        '          test "$(/usr/bin/git -C "$workspace" rev-parse --verify HEAD)" = "$expected_sha"',
        '          test "$(/usr/bin/git -C "$workspace" rev-parse --is-shallow-repository)" = false',
        '          test "$(/usr/bin/git -C "$workspace" rev-parse --show-toplevel)" = "$workspace"',
        '          test -z "$(/usr/bin/git -C "$workspace" status --porcelain=v1 --untracked-files=all)"',
        '          test ! -e "$workspace/.git/objects/info/alternates"',
        '          test -z "${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"',
        '          test "$(/usr/bin/git -C "$workspace" remote get-url --all origin)" = "$origin_url"',
        '          test "$(/usr/bin/git -C "$workspace" remote get-url --push --all origin)" = "$origin_url"',
        "            '^(credential\\.|http\\.|include\\.|includeif\\.|core\\.askpass$|core\\.sshcommand$|ssh\\.|remote\\..*\\.uploadpack$|url\\..*\\.insteadof$)' \\",
        "          for variable in GIT_SSH GIT_SSH_COMMAND GIT_ASKPASS SSH_ASKPASS SSH_AUTH_SOCK; do",
        '            test -z "${!variable:-}"',
        '          /bin/rm -rf --one-file-system -- "$checkout_state"',
        "          builtin printf '%s\\n' \\",
        "            'HTTP_PROXY=' \\",
        "            'HTTPS_PROXY=' \\",
        "            'FTP_PROXY=' \\",
        "            'ALL_PROXY=' \\",
        "            'NO_PROXY=' \\",
        '            >> "$github_env"',
    )
    for declaration in required_materialization:
        if materialize.count(declaration) != 1:
            raise ContractError(
                f"Git materialization contract drifted: {declaration.strip()}"
            )

    if materialize.count("/usr/bin/git") != 11:
        raise ContractError("materialization must use the audited fixed Git binary")
    if materialize.count("/usr/bin/base64") != 1:
        raise ContractError("raw checkout token must have one fixed encoder")
    if materialize.count("/usr/bin/env") != 1:
        raise ContractError("fetch must have one fixed minimal-environment launcher")
    if materialize.count("/usr/bin/dirname") != 1:
        raise ContractError("runner command-file containment must use fixed dirname")
    if materialize.count("/bin/rm") != 1:
        raise ContractError("materialization must never delete workspace contents")
    if re.search(r"(?<![/A-Za-z0-9_.-])git(?:\s|$)", materialize):
        raise ContractError("materialization must not resolve Git through PATH")
    if materialize.count("--config-env=") != 1:
        raise ContractError("HTTP authorization must use one transient config-env")
    for variable in (
        "BASH_ENV",
        "ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "PS4",
        "BASH_XTRACEFD",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "FTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "CURL_CA_BUNDLE",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "SSLKEYLOGFILE",
    ):
        expected_mentions = (
            3
            if variable
            in {"HTTP_PROXY", "HTTPS_PROXY", "FTP_PROXY", "ALL_PROXY", "NO_PROXY"}
            else 2
        )
        if (
            len(re.findall(rf"\b{re.escape(variable)}\b", materialize))
            != expected_mentions
        ):
            raise ContractError(
                f"bootstrap or transport environment reintroduced: {variable}"
            )
    for variable in (
        "http_proxy",
        "https_proxy",
        "ftp_proxy",
        "all_proxy",
        "no_proxy",
    ):
        if len(re.findall(rf"\b{variable}\b", materialize)) != 1:
            raise ContractError(f"lower-case proxy reintroduced: {variable}")

    raw_mask = materialize.index(
        "builtin printf '::add-mask::%s\\n' \"$BULKLOAD_CHECKOUT_TOKEN\""
    )
    raw_copy = materialize.index("checkout_token=$BULKLOAD_CHECKOUT_TOKEN")
    raw_deexport = materialize.index("export -n checkout_token SHELLOPTS BASHOPTS")
    raw_unset = materialize.index("\n          unset BULKLOAD_CHECKOUT_TOKEN\n")
    encode = materialize.index("encoded_token=$(")
    encoder = materialize.index("/usr/bin/base64 -w0")
    encoded_mask = materialize.index(
        "builtin printf '::add-mask::%s\\n' \"$encoded_token\""
    )
    header = materialize.index('git_http_header="AUTHORIZATION: basic $encoded_token"')
    header_mask = materialize.index(
        "builtin printf '::add-mask::%s\\n' \"$git_http_header\""
    )
    raw_encoded_clear = materialize.index("unset checkout_token encoded_token")
    first_workspace_child = materialize.index("/usr/bin/readlink")
    fetch_boundary = materialize.index("fetch_env_unsets=()")
    header_export = materialize.index(
        'export BULKLOAD_GIT_HTTP_HEADER="$git_http_header"'
    )
    env_exec = materialize.index('exec /usr/bin/env "${fetch_env_unsets[@]}"')
    fetch = materialize.index("fetch --force --prune --no-recurse-submodules --no-tags")
    clear = materialize.index("unset BULKLOAD_GIT_HTTP_HEADER git_http_header")
    trap_clear = materialize.index("trap - EXIT")
    checkout = materialize.index('checkout --detach --force "$expected_sha"')
    verify = materialize.index("rev-parse --verify HEAD")
    cleanup = materialize.index('/bin/rm -rf --one-file-system -- "$checkout_state"')
    persist_proxies = materialize.index("builtin printf '%s\\n'")
    if not (
        raw_mask
        < raw_copy
        < raw_deexport
        < raw_unset
        < encode
        < encoder
        < encoded_mask
        < header
        < header_mask
        < raw_encoded_clear
        < first_workspace_child
        < fetch_boundary
        < header_export
        < env_exec
        < fetch
        < clear
        < trap_clear
        < checkout
        < verify
        < cleanup
        < persist_proxies
    ):
        raise ContractError(
            "bootstrap, credential, fetch, checkout, or verification order drifted"
        )
    if not materialize.rstrip().endswith('>> "$github_env"'):
        raise ContractError(
            "upper-case proxy persistence must terminate materialization"
        )
    if materialize.count("export BULKLOAD_GIT_HTTP_HEADER=") != 1:
        raise ContractError("fetch header must be exported only inside its closure")
    if materialize.count("--no-auto-maintenance") != 1:
        raise ContractError("fetch must suppress automatic maintenance")
    if materialize.count("--no-write-commit-graph") != 1:
        raise ContractError("fetch must suppress commit-graph writers")
    if any(
        credential in materialize[checkout:]
        for credential in (
            "BULKLOAD_CHECKOUT_TOKEN",
            "BULKLOAD_GIT_HTTP_HEADER",
            "checkout_token",
            "encoded_token",
            "git_http_header",
        )
    ):
        raise ContractError("checkout or verification retained credential material")

    if workflow.count('      ATTIC_TOKEN: ""') != 1:
        raise ContractError("workflow must empty ATTIC_TOKEN exactly once")
    if workflow.count('      NIX_ACCESS_TOKENS: ""') != 1:
        raise ContractError("workflow must empty NIX_ACCESS_TOKENS exactly once")

    required_inputs = (
        f"          gate: {MATRIX_GATE_EXPRESSION}",
        "          event-name: ${{ github.event_name }}",
        f"          expected-sha: {EXPECTED_SHA_EXPRESSION}",
        "          ref: ${{ github.ref }}",
        "          repository: ${{ github.repository }}",
        f"          head-repository: {HEAD_REPOSITORY_EXPRESSION}",
        f"          upload-bazel-results: {UPLOAD_EXPRESSION}",
    )
    for declaration in required_inputs:
        if workflow.count(declaration) != 1:
            raise ContractError(
                f"local front-door input drifted: {declaration.strip()}"
            )


def parse_action_inputs(action: str) -> set[str]:
    lines = action.splitlines()
    try:
        start = lines.index("inputs:") + 1
        end = lines.index("runs:")
    except ValueError as exc:
        raise ContractError("local action must declare inputs before runs") from exc
    return {
        match.group(1)
        for line in lines[start:end]
        if (match := re.fullmatch(r"  ([a-z][a-z0-9-]*):", line)) is not None
    }


def validate_local_action(action: str, *, exact_digest: bool = True) -> None:
    if exact_digest and sha256(action) != ACTION_SHA256:
        raise ContractError("local public-read action digest drifted")
    if re.search(r"(?i)(?<![A-Za-z0-9_])GRPC_PROXY_EXP(?![A-Za-z0-9_])", action):
        raise ContractError("local public-read action must keep GRPC_PROXY_EXP absent")
    if parse_action_inputs(action) != {
        "gate",
        "event-name",
        "expected-sha",
        "head-repository",
        "ref",
        "repository",
        "upload-bazel-results",
    }:
        raise ContractError("local public-read action input inventory drifted")

    step_names = re.findall(r"(?m)^    - name: (.+)$", action)
    expected_step_names = [
        "Validate the terminal gate selection",
        "Snapshot the exact public-read guard",
        "Preflight raw runner endpoint authority",
        "Discover sanctioned runner endpoint authority",
        "Enforce the discovered public-read boundary",
        "Verify effective Nix client authority",
        "Revalidate immutable Bazel build authority",
        "Build the Bulkload documentation through the public Flywheel action",
        "Revalidate immutable Bazel test authority",
        "Test the complete Bulkload Bazel graph through the public Flywheel action",
        "Run the repository-owned fault harness",
        "Run repository-owned source gates",
    ]
    if step_names != expected_step_names:
        raise ContractError("local action step order or inventory drifted")
    if len(re.findall(r"(?m)^    - \S", action)) != len(expected_step_names):
        raise ContractError("local action steps must use the audited named inventory")
    expected_step_fields = {
        "Validate the terminal gate selection": ("shell", "env", "run"),
        "Snapshot the exact public-read guard": ("id", "shell", "env", "run"),
        "Preflight raw runner endpoint authority": ("shell", "env", "run"),
        "Discover sanctioned runner endpoint authority": (
            "id",
            "uses",
            "env",
            "with",
        ),
        "Enforce the discovered public-read boundary": (
            "id",
            "shell",
            "env",
            "run",
        ),
        "Verify effective Nix client authority": ("shell", "env", "run"),
        "Revalidate immutable Bazel build authority": (
            "if",
            "id",
            "shell",
            "env",
            "run",
        ),
        "Build the Bulkload documentation through the public Flywheel action": (
            "if",
            "uses",
            "env",
            "with",
        ),
        "Revalidate immutable Bazel test authority": (
            "if",
            "id",
            "shell",
            "env",
            "run",
        ),
        "Test the complete Bulkload Bazel graph through the public Flywheel action": (
            "if",
            "uses",
            "env",
            "with",
        ),
        "Run the repository-owned fault harness": ("if", "shell", "env", "run"),
        "Run repository-owned source gates": ("if", "shell", "env", "run"),
    }
    for name in expected_step_names:
        step = extract_action_step(action, name)
        fields = tuple(
            match.group(1)
            for line in step.splitlines()
            if (match := re.fullmatch(r"      ([a-z][a-z0-9-]*):(.*)", line))
            is not None
        )
        if fields != expected_step_fields[name]:
            raise ContractError(f"composite step field inventory drifted: {name}")
        shells = re.findall(r"(?m)^      shell: ", step)
        uses = re.findall(r"(?m)^      uses: ", step)
        runs = re.findall(r"(?m)^      run: \|$", step)
        if len(shells) + len(uses) != 1:
            raise ContractError(f"step must select one execution mechanism: {name}")
        if (shells and len(runs) != 1) or (uses and runs):
            raise ContractError(f"step run mapping drifted: {name}")
    if re.search(r"(?mi)^ {4,}(?:post|post-if)\s*:", action):
        raise ContractError("local action closure must not register a post")
    if re.search(
        r"(?mi)^ {6}(?:continue-on-error|\"continue-on-error\"|'continue-on-error')\s*:",
        action,
    ):
        raise ContractError("local action closure must not suppress an error")
    validate_action_step_environment_fences(action)
    validate_terminal_consumer_paths(action)

    effective_nix_step = extract_action_step(
        action, "Verify effective Nix client authority"
    )
    if exact_digest and sha256(effective_nix_step) != EFFECTIVE_NIX_STEP_SHA256:
        raise ContractError("effective Nix authority step mapping drifted")
    if "sort -u" in effective_nix_step:
        raise ContractError(
            "effective Nix inventories must preserve duplicate evidence"
        )
    if (
        f"      run: |\n        set -euo pipefail\n        nixos_cache={NIXOS_CACHE}\n"
    ) not in effective_nix_step:
        raise ContractError("effective Nix authority must fail fast")

    source_gate_step = extract_action_step(action, "Run repository-owned source gates")
    if exact_digest and sha256(source_gate_step) != SOURCE_GATE_STEP_SHA256:
        raise ContractError("repository source-gate step mapping drifted")
    if not action.endswith(source_gate_step):
        raise ContractError("repository source gates must be the recursive action tail")
    source_exec = "          .#default --command just ci-source"
    source_flake = "        nix flake check --no-build --no-write-lock-file"
    source_keep_names = tuple(
        line.removeprefix("          --keep ").removesuffix(" \\")
        for line in source_gate_step.splitlines()
        if line.startswith("          --keep ") and line.endswith(" \\")
    )
    if (
        "      shell: /bin/bash --noprofile --norc -p {0}\n" not in source_gate_step
        or "      run: |\n        set -euo pipefail\n" not in source_gate_step
        or source_gate_step.count(source_flake) != 1
        or source_gate_step.count(source_exec) != 1
        or source_gate_step.count(
            "        exec nix develop --no-write-lock-file --ignore-environment \\\n"
        )
        != 1
        or source_keep_names
        != (
            "HOME",
            "NIX_CACHE_HOME",
            "NIX_CONFIG_HOME",
            "NIX_DATA_HOME",
            "NIX_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        )
        or source_gate_step.index(source_flake) >= source_gate_step.index(source_exec)
        or not source_gate_step.rstrip().endswith(source_exec)
    ):
        raise ContractError("source gate must end by execing the audited source suite")
    if re.search(
        r"(?m)^\s*[A-Za-z_][A-Za-z0-9_]*\s*\(\)\s*\{|\|\|\s*true\s*$|&\s*$|^\s*exit\s+0\s*$",
        source_gate_step,
    ):
        raise ContractError("source gate must not define wrappers or detach failures")

    # The fault-harness gate (R-N122) is the audited source step with exactly
    # three substitutions: its name, its gate condition and its just recipe.
    harness_step = extract_action_step(action, TERMINAL_CONSUMERS["fault-harness"])
    if exact_digest and sha256(harness_step) != FAULT_HARNESS_STEP_SHA256:
        raise ContractError("repository fault-harness step mapping drifted")
    derived_harness_step = (
        source_gate_step.replace(
            f"    - name: {TERMINAL_CONSUMERS['source']}\n",
            f"    - name: {TERMINAL_CONSUMERS['fault-harness']}\n",
            1,
        )
        .replace(
            "      if: ${{ inputs.gate == 'source' }}\n",
            "      if: ${{ inputs.gate == 'fault-harness' }}\n",
            1,
        )
        .replace(source_exec, "          .#default --command just ci-fault-harness", 1)
    )
    if harness_step != derived_harness_step + "\n":
        raise ContractError(
            "fault-harness gate must be the audited source step with its own recipe"
        )

    nix_setup = (
        "tinyland-inc/ci-templates/.github/actions/nix-setup@" + CI_TEMPLATES_REV
    )
    flywheel_bazel = (
        "tinyland-inc/ci-templates/.github/actions/flywheel-bazel@" + CI_TEMPLATES_REV
    )
    if extract_uses(action) != [nix_setup, flywheel_bazel, flywheel_bazel]:
        raise ContractError("public ci-templates action closure drifted")
    for action_ref in extract_uses(action):
        if re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", action_ref) is None:
            raise ContractError("every external action must use an immutable SHA")

    for forbidden in (
        "tinyland-inc/GloriousFlywheel",
        "github:tinyland-inc/GloriousFlywheel",
        "secrets.",
        "id-token:",
        "flywheel-executor",
        "executor-backed",
        "runner.labels",
    ):
        if forbidden in action:
            raise ContractError(
                f"local action contains forbidden authority: {forbidden}"
            )
    if re.search(r"(?m)^ {6}(?:\"if\"|'if')\s*:", action):
        raise ContractError("terminal conditions must use canonical unquoted keys")
    action_without_reviewed_cache = action.replace(NIXOS_CACHE, "")
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", action_without_reviewed_cache):
        raise ContractError("local action must not bake a deployment endpoint")
    if re.search(r"type\s*=\s*gha", action, re.IGNORECASE):
        raise ContractError("GitHub Actions cache authority is forbidden")

    required = (
        "        BULKLOAD_GATE: ${{ inputs.gate }}",
        '        case "$BULKLOAD_GATE" in',
        "          source | build | test | fault-harness) ;;",
        "        attic-cache: main",
        "      id: guard-snapshot",
        "      id: authority",
        "      id: bazel-build-authority",
        "      id: bazel-test-authority",
        f"        {GUARD_INVOCATION} preflight",
        f"        {GUARD_INVOCATION} enforce",
        '        [[ "$(nix config show store)" == local ]]',
        '        [[ "$(nix config show allow-symlinked-store)" == false ]]',
        '        [[ "$(nix eval --raw --expr builtins.storeDir)" == /nix/store ]]',
        '        [[ "$(nix config show accept-flake-config)" == false ]]',
        '        [[ "$(nix config show netrc-file)" == /dev/null ]]',
        '        [[ -z "$(nix config show access-tokens)" ]]',
        '        [[ -z "$(nix config show trusted-substituters)" ]]',
        '        [[ -z "$(nix config show builders)" ]]',
        '        [[ "$(nix config show builders-use-substitutes)" == false ]]',
        '        [[ -z "$(nix config show build-hook)" ]]',
        '        [[ -z "$(nix config show pre-build-hook)" ]]',
        '        [[ -z "$(nix config show post-build-hook)" ]]',
        '        [[ -z "$(nix config show diff-hook)" ]]',
        '        [[ "$(nix config show run-diff-hook)" == false ]]',
        '        [[ "$(nix config show require-sigs)" == true ]]',
        '        [[ -z "$(nix config show secret-key-files)" ]]',
        '        [[ -z "$(nix config show plugin-files)" ]]',
        f"        nixos_cache={NIXOS_CACHE}",
        f"        nixos_public_key={NIXOS_PUBLIC_KEY}",
        "        actual_substituters=$(nix config show substituters | tr ' ' '\\n' | sed '/^$/d' | LC_ALL=C sort)",
        '        expected_substituters=$(printf \'%s\\n\' "${ATTIC_SERVER%/}/${ATTIC_CACHE}" "$nixos_cache" | LC_ALL=C sort)',
        '        [[ "$actual_substituters" == "$expected_substituters" ]]',
        "        actual_public_keys=$(nix config show trusted-public-keys | tr ' ' '\\n' | sed '/^$/d' | LC_ALL=C sort)",
        '        expected_public_keys=$(printf \'%s\\n\' "$ATTIC_PUBLIC_KEY" "$nixos_public_key" | LC_ALL=C sort)',
        '        [[ "$actual_public_keys" == "$expected_public_keys" ]]',
        "        command: build",
        "        targets: //:bulkload",
        "        command: test",
        "        targets: //:tests",
    )
    for declaration in required:
        if action.count(declaration) != 1:
            raise ContractError(f"local action contract drifted: {declaration.strip()}")
    # Once in each repository consumer: the fault-harness and source gates.
    if (
        action.count(
            "        exec nix develop --no-write-lock-file --ignore-environment \\"
        )
        != 2
    ):
        raise ContractError("repository consumer exec inventory drifted")

    reviewed_env_steps = expected_step_names[1:]
    environment = {
        name: dict(parse_action_step_env(extract_action_step(action, name)))
        for name in reviewed_env_steps
    }
    action_owned_empty = (
        "USER",
        "USERNAME",
        "LOGNAME",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "NIX_STORE_DIR",
        "NIX_STORE",
        "NIX_STATE_DIR",
        "NIX_DATA_DIR",
        "NIX_LOG_DIR",
        "NIX_CONF_DIR",
        "NIX_DAEMON_SOCKET_PATH",
        "NIX_IGNORE_SYMLINK_STORE",
        "NIX_LIBEXEC_DIR",
        "NIX_BIN_DIR",
        "NIX_REMOTE_SYSTEMS",
        "NIX_SSL_CERT_FILE",
        "NIX_CURL_FLAGS",
        "NIX_HASHED_MIRRORS",
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "JAVA_HOME",
        "JAVACMD",
        "BAZEL_SH",
        "BAZELISK_NOJDK",
        "BAZELISK_CLEAN",
        "BAZELISK_SHUTDOWN",
        "USE_BAZEL_FALLBACK_VERSION",
    )
    common_keys = set(action_owned_empty) | {
        "PATH",
        "HOME",
        "BASH_ENV",
        "ENV",
        "TEST_TMPDIR",
        "NIX_CACHE_HOME",
        "NIX_CONFIG_HOME",
        "NIX_DATA_HOME",
        "NIX_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "ATTIC_TOKEN",
        "NIX_ACCESS_TOKENS",
        "NIX_USER_CONF_FILES",
        "NETRC",
        "NIX_CONFIG",
        "NIX_REMOTE",
        "BAZELISK_BASE_URL",
        "BAZELISK_FORMAT_URL",
        "BAZELISK_GITHUB_TOKEN",
        "BAZELISK_HOME",
        "BAZELISK_HOME_DARWIN",
        "BAZELISK_HOME_LINUX",
        "BAZELISK_INCOMPATIBLE_FLAGS",
        "BAZELISK_VERIFY_SHA256",
        "BAZELISK_SKIP_WRAPPER",
        "BAZELISK_WRAPPER_DIRECTORY",
        "USE_BAZEL_VERSION",
    }
    guard_context = {
        "BULKLOAD_GUARD_PATH": "${{ steps.guard-snapshot.outputs.guard_path }}",
        "BULKLOAD_RUNTIME_HOME": "${{ steps.guard-snapshot.outputs.runtime_home }}",
        "BULKLOAD_EVENT_NAME": "${{ inputs.event-name }}",
        "BULKLOAD_EXPECTED_SHA": "${{ inputs.expected-sha }}",
        "BULKLOAD_REF": "${{ inputs.ref }}",
        "BULKLOAD_REPOSITORY": "${{ inputs.repository }}",
        "BULKLOAD_HEAD_REPOSITORY": "${{ inputs.head-repository }}",
        "BULKLOAD_UPLOAD_BAZEL_RESULTS": "${{ inputs.upload-bazel-results }}",
        "BULKLOAD_RUNNER_ENVIRONMENT": "${{ runner.environment }}",
        "BULKLOAD_RUNNER_NAME": "${{ runner.name }}",
    }
    discovered_context = {
        "BULKLOAD_ATTIC_REACHABLE": "${{ steps.endpoints.outputs.attic_reachable }}",
        "BULKLOAD_BAZEL_CACHE_REACHABLE": "${{ steps.endpoints.outputs.bazel_cache_reachable }}",
    }
    captured_bazel_context = {
        "BULKLOAD_CAPTURED_BAZEL_REMOTE_CACHE": "${{ steps.authority.outputs.bazel_remote_cache }}",
        "BULKLOAD_CAPTURED_BAZEL_UPLOAD": "${{ steps.authority.outputs.bazel_remote_upload }}",
        "BULKLOAD_CAPTURED_NIX_CONFIG": "${{ steps.authority.outputs.nix_config }}",
        "ATTIC_SERVER": "${{ steps.authority.outputs.attic_server }}",
        "BAZEL_REMOTE_CACHE": "${{ steps.authority.outputs.bazel_remote_cache }}",
        "BAZEL_REMOTE_EXECUTOR": '""',
        "BAZEL_REMOTE_EXEC_HEADER": '""',
        "BAZEL_CREDENTIAL_HELPER": '""',
        "BAZEL_REMOTE_HEADER": '""',
        "BAZEL_REMOTE_CACHE_HEADER": '""',
        "GF_BAZEL_REMOTE_UPLOAD": "${{ steps.authority.outputs.bazel_remote_upload }}",
    }
    bazel_consumer_context = {
        key: value
        for key, value in captured_bazel_context.items()
        if not key.startswith("BULKLOAD_CAPTURED_") and key != "ATTIC_SERVER"
    }
    # The fault-harness and source gates are the two repository consumers; they
    # share one exact environment.
    repository_consumer_context = {
        "ATTIC_SERVER": "${{ steps.authority.outputs.attic_server }}",
        "ATTIC_CACHE": "main",
        "ATTIC_PUBLIC_KEY": PUBLIC_KEY,
        "ATTIC_PUBLIC_READ_SITE": "bulkload-ci",
    }
    exact_extras = {
        reviewed_env_steps[0]: {},
        reviewed_env_steps[1]: guard_context,
        reviewed_env_steps[2]: {},
        reviewed_env_steps[3]: {**guard_context, **discovered_context},
        reviewed_env_steps[4]: {
            "ATTIC_SERVER": "${{ steps.authority.outputs.attic_server }}",
            "ATTIC_CACHE": "main",
            "ATTIC_PUBLIC_KEY": PUBLIC_KEY,
        },
        reviewed_env_steps[5]: {
            **guard_context,
            **captured_bazel_context,
            "BULKLOAD_BAZEL_PHASE": "build",
        },
        reviewed_env_steps[6]: bazel_consumer_context,
        reviewed_env_steps[7]: {
            **guard_context,
            **captured_bazel_context,
            "BULKLOAD_BAZEL_PHASE": "test",
        },
        reviewed_env_steps[8]: bazel_consumer_context,
        reviewed_env_steps[9]: repository_consumer_context,
        reviewed_env_steps[10]: repository_consumer_context,
    }
    for name, env in environment.items():
        extras = exact_extras[name]
        if set(env) != common_keys | set(extras):
            raise ContractError(f"local action {name} environment key set drifted")
        if any(env[key] != value for key, value in extras.items()):
            raise ContractError(f"local action {name} authority inputs drifted")

    for name, env in environment.items():
        for key in action_owned_empty:
            if env.get(key) != '""':
                raise ContractError(f"local action {name} must empty {key}")
        for key in ("BASH_ENV", "ENV"):
            if env.get(key) != "/dev/null":
                raise ContractError(f"local action {name} lost its {key} fence")
        for key in ("ATTIC_TOKEN", "NIX_ACCESS_TOKENS"):
            if env.get(key) != '""':
                raise ContractError(f"local action {name} must empty {key}")
        if env.get("NIX_USER_CONF_FILES") != "/dev/null":
            raise ContractError(f"local action {name} lost its Nix config fence")
        if env.get("NETRC") != "/dev/null":
            raise ContractError(f"local action {name} lost its netrc fence")
        if env.get("NIX_REMOTE") != "local":
            raise ContractError(f"local action {name} escaped the local Nix store")
        for key in (
            "BAZELISK_BASE_URL",
            "BAZELISK_FORMAT_URL",
            "BAZELISK_GITHUB_TOKEN",
            "BAZELISK_HOME_DARWIN",
            "BAZELISK_HOME_LINUX",
            "BAZELISK_INCOMPATIBLE_FLAGS",
            "BAZELISK_VERIFY_SHA256",
            "BAZELISK_WRAPPER_DIRECTORY",
            "USE_BAZEL_VERSION",
        ):
            if env.get(key) != '""':
                raise ContractError(f"local action {name} must empty {key}")
        if env.get("BAZELISK_SKIP_WRAPPER") != '"true"':
            raise ContractError(f"local action {name} lost its Bazelisk fence")

    gate_env = dict(
        parse_action_step_env(
            extract_action_step(action, "Validate the terminal gate selection")
        )
    )
    if gate_env != {
        "BASH_ENV": "/dev/null",
        "ENV": "/dev/null",
        "BULKLOAD_GATE": "${{ inputs.gate }}",
    }:
        raise ContractError("terminal gate validation environment drifted")

    preflight_nix_config = "|-\n" + "\n".join(
        (
            f"substituters = {NIXOS_CACHE}",
            "store = local",
            "allow-symlinked-store = false",
            f"trusted-public-keys = {NIXOS_PUBLIC_KEY}",
            "trusted-substituters =",
            "builders =",
            "builders-use-substitutes = false",
            "build-hook =",
            "pre-build-hook =",
            "post-build-hook =",
            "diff-hook =",
            "run-diff-hook = false",
            "require-sigs = true",
            "access-tokens =",
            "netrc-file = /dev/null",
            "accept-flake-config = false",
            "secret-key-files =",
            "plugin-files =",
        )
    )
    for name in reviewed_env_steps[:4]:
        if environment[name].get("NIX_CONFIG") != preflight_nix_config:
            raise ContractError(f"local action {name} preflight Nix config drifted")
    for name in reviewed_env_steps[4:]:
        if environment[name].get("NIX_CONFIG") != (
            "${{ steps.authority.outputs.nix_config }}"
        ):
            raise ContractError(f"local action {name} captured Nix config drifted")

    expected_paths = {
        reviewed_env_steps[0]: REVIEWED_PATH,
        reviewed_env_steps[1]: REVIEWED_PATH,
        reviewed_env_steps[2]: REVIEWED_PATH,
        reviewed_env_steps[3]: REVIEWED_STEP_PATH,
        **{
            name: "${{ steps.authority.outputs.trusted_path }}"
            for name in reviewed_env_steps[4:]
        },
    }
    common_home = "${{ steps.guard-snapshot.outputs.runtime_home }}"
    source_home = "${{ steps.guard-snapshot.outputs.source_runtime_home }}"
    expected_homes = {
        reviewed_env_steps[0]: "/var/empty",
        reviewed_env_steps[1]: common_home,
        reviewed_env_steps[2]: common_home,
        reviewed_env_steps[3]: common_home,
        reviewed_env_steps[4]: common_home,
        reviewed_env_steps[5]: common_home,
        reviewed_env_steps[6]: "${{ steps.bazel-build-authority.outputs.bazel_home }}",
        reviewed_env_steps[7]: common_home,
        reviewed_env_steps[8]: "${{ steps.bazel-test-authority.outputs.bazel_home }}",
        reviewed_env_steps[9]: source_home,
        reviewed_env_steps[10]: source_home,
    }
    for name in reviewed_env_steps:
        env = environment[name]
        if (
            env.get("PATH") != expected_paths[name]
            or env.get("HOME") != expected_homes[name]
        ):
            raise ContractError(f"local action {name} path or home authority drifted")

    private_home_keys = (
        "NIX_CACHE_HOME",
        "NIX_CONFIG_HOME",
        "NIX_DATA_HOME",
        "NIX_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    )
    expected_runtime_homes = {
        reviewed_env_steps[0]: '""',
        reviewed_env_steps[1]: common_home,
        reviewed_env_steps[2]: common_home,
        reviewed_env_steps[3]: common_home,
        reviewed_env_steps[4]: common_home,
        reviewed_env_steps[5]: common_home,
        reviewed_env_steps[
            6
        ]: "${{ steps.bazel-build-authority.outputs.bazel_runtime_home }}",
        reviewed_env_steps[7]: common_home,
        reviewed_env_steps[
            8
        ]: "${{ steps.bazel-test-authority.outputs.bazel_runtime_home }}",
        reviewed_env_steps[9]: source_home,
        reviewed_env_steps[10]: source_home,
    }
    expected_test_tmpdirs = {name: '""' for name in reviewed_env_steps}
    expected_test_tmpdirs[reviewed_env_steps[6]] = (
        "${{ steps.bazel-build-authority.outputs.bazel_test_tmpdir }}"
    )
    expected_test_tmpdirs[reviewed_env_steps[8]] = (
        "${{ steps.bazel-test-authority.outputs.bazel_test_tmpdir }}"
    )
    expected_bazelisk_homes = {name: '""' for name in reviewed_env_steps}
    expected_bazelisk_homes[reviewed_env_steps[6]] = (
        "${{ steps.bazel-build-authority.outputs.bazelisk_home }}"
    )
    expected_bazelisk_homes[reviewed_env_steps[8]] = (
        "${{ steps.bazel-test-authority.outputs.bazelisk_home }}"
    )
    for name, env in environment.items():
        if any(
            env.get(key) != expected_runtime_homes[name] for key in private_home_keys
        ):
            raise ContractError(f"local action {name} private home inventory drifted")
        if env.get("TEST_TMPDIR") != expected_test_tmpdirs[name]:
            raise ContractError(f"local action {name} test tmpdir drifted")
        if env.get("BAZELISK_HOME") != expected_bazelisk_homes[name]:
            raise ContractError(f"local action {name} Bazelisk home drifted")

    bazel_steps = reviewed_env_steps[5:9]
    for name in bazel_steps:
        env = environment[name]
        if env.get("BAZEL_REMOTE_CACHE") != (
            "${{ steps.authority.outputs.bazel_remote_cache }}"
        ):
            raise ContractError(f"local action {name} Bazel cache authority drifted")
        if env.get("GF_BAZEL_REMOTE_UPLOAD") != (
            "${{ steps.authority.outputs.bazel_remote_upload }}"
        ):
            raise ContractError(f"local action {name} Bazel upload authority drifted")
        for key in (
            "BAZEL_REMOTE_EXECUTOR",
            "BAZEL_REMOTE_EXEC_HEADER",
            "BAZEL_CREDENTIAL_HELPER",
            "BAZEL_REMOTE_HEADER",
            "BAZEL_REMOTE_CACHE_HEADER",
        ):
            if env.get(key) != '""':
                raise ContractError(f"local action {name} must empty {key}")
    for name, phase in (
        (reviewed_env_steps[5], "build"),
        (reviewed_env_steps[7], "test"),
    ):
        env = environment[name]
        if env.get("BULKLOAD_BAZEL_PHASE") != phase:
            raise ContractError(f"local action {name} Bazel phase drifted")
        if env.get("BULKLOAD_CAPTURED_BAZEL_REMOTE_CACHE") != (
            "${{ steps.authority.outputs.bazel_remote_cache }}"
        ):
            raise ContractError(f"local action {name} captured cache drifted")
        if env.get("BULKLOAD_CAPTURED_BAZEL_UPLOAD") != (
            "${{ steps.authority.outputs.bazel_remote_upload }}"
        ):
            raise ContractError(f"local action {name} captured upload drifted")
        if env.get("BULKLOAD_CAPTURED_NIX_CONFIG") != (
            "${{ steps.authority.outputs.nix_config }}"
        ):
            raise ContractError(f"local action {name} captured Nix config drifted")

    if action.count("      shell: /bin/bash --noprofile --norc -p {0}") != 9:
        raise ContractError(
            "every direct shell boundary must use absolute privileged non-profile Bash"
        )

    if action.count(GUARD_SHA256) != 5:
        raise ContractError("snapshot guard digest inventory drifted")
    if (
        action.count('guard_source=$(/bin/cat -- "$BULKLOAD_GUARD_PATH"; printf x)')
        != 4
    ):
        raise ContractError("every guard wrapper must capture immutable bytes")
    if action.count("        guard_source=${guard_source%x}") != 4:
        raise ContractError("every guard wrapper must preserve trailing newlines")
    digest_check = (
        '[[ "$(printf \'%s\' "$guard_source" | /usr/bin/sha256sum | '
        "/usr/bin/awk '{print $1}')\" == " + GUARD_SHA256 + " ]]"
    )
    if action.count(digest_check) != 4:
        raise ContractError("every captured guard must have an external digest check")
    if action.count(f"        {GUARD_INVOCATION} bazel") != 2:
        raise ContractError("each Bazel invocation needs an immediate authority guard")
    if (
        action.count(
            "        bazel_remote_cache: ${{ steps.authority.outputs.bazel_remote_cache }}"
        )
        != 2
    ):
        raise ContractError("Bazel endpoint must come from captured authority")
    if action.count('        bazel_remote_executor: ""') != 2:
        raise ContractError("Bazel executor input must remain empty")

    snapshot = action.index("- name: Snapshot the exact public-read guard")
    preflight = action.index(f"{GUARD_INVOCATION} preflight")
    setup = action.index(f"uses: {nix_setup}")
    enforce = action.index(f"{GUARD_INVOCATION} enforce")
    effective_nix = action.index(
        "\n    - name: Verify effective Nix client authority\n"
    )
    source = action.index("\n    - name: Run repository-owned source gates\n")
    bazel_guards = [
        match.start()
        for match in re.finditer(re.escape(f"{GUARD_INVOCATION} bazel"), action)
    ]
    bazel_actions = [
        match.start()
        for match in re.finditer(re.escape(f"uses: {flywheel_bazel}"), action)
    ]
    if not (
        snapshot
        < preflight
        < setup
        < enforce
        < effective_nix
        < bazel_guards[0]
        < bazel_actions[0]
        < bazel_guards[1]
        < bazel_actions[1]
        < source
    ):
        raise ContractError(
            "guard snapshot, discovery, per-Bazel boundaries, or source tail are misordered"
        )
    if action.count("        config: flywheel") != 2:
        raise ContractError("both Bazel invocations must be cache-only Flywheel calls")


def validate_guard(guard: str, *, exact_digest: bool = True) -> None:
    if exact_digest and sha256(guard) != GUARD_SHA256:
        raise ContractError("public-read guard digest drifted")
    guard_without_reviewed_cache = guard.replace(NIXOS_CACHE, "")
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", guard_without_reviewed_cache):
        raise ContractError("guard must not bake a deployment endpoint")
    for forbidden in (
        "accept-flake-config = true",
        "secrets.",
        "ATTIC_TOKEN=${",
        "GF_BAZEL_REMOTE_UPLOAD=true",
    ):
        if forbidden in guard:
            raise ContractError(f"guard contains forbidden authority: {forbidden}")
    required = (
        f"readonly ci_templates_rev={CI_TEMPLATES_REV}",
        f"readonly public_key='{PUBLIC_KEY}'",
        f"readonly nixos_cache='{NIXOS_CACHE}'",
        f"readonly nixos_public_key='{NIXOS_PUBLIC_KEY}'",
        "readonly public_site=bulkload-ci",
        "readonly cache_name=main",
        f"readonly reviewed_path={REVIEWED_PATH}",
        f"readonly reviewed_step_path={REVIEWED_STEP_PATH}",
        "require_forbidden_environment_names_absent",
        'GIT_*) die "Git environment overrides are forbidden" ;;',
        'JUST_*) die "Just environment overrides are forbidden" ;;',
        'NIX_MIRRORS_*) die "dynamic Nix mirror overrides are forbidden" ;;',
        'SHELLCHECK_OPTS) die "ShellCheck environment overrides are forbidden" ;;',
        'require_equal "command search path" "${PATH:-}" "$reviewed_path"',
        'require_equal "private runtime home" "${HOME:-}" "${BULKLOAD_RUNTIME_HOME:-}"',
        'require_private_directory "Nix/XDG runtime home" "${BULKLOAD_RUNTIME_HOME:-}"',
        'require_equal "runner environment" "${BULKLOAD_RUNNER_ENVIRONMENT:-}" self-hosted',
        "^bulkload-nix-[a-z0-9]+-runner-[a-z0-9]+$",
        'require_equal "event" "${BULKLOAD_EVENT_NAME:-}" "${GITHUB_EVENT_NAME:-}"',
        'require_equal "ref" "${BULKLOAD_REF:-}" "${GITHUB_REF:-}"',
        '"pull-request head repository"',
        "  merge_group)\n",
        '"merge-group repository"',
        '[[ "${BULKLOAD_REF:-}" == refs/heads/gh-readonly-queue/main/* ]] ||',
        'die "merge-group ref is outside the main merge queue"',
        '*) die "event is outside the reviewed push/pull_request/merge_group inventory" ;;',
        '"$(git -C "${GITHUB_WORKSPACE:?}" rev-parse HEAD)"',
        'if [[ "$BULKLOAD_EVENT_NAME" == push && "$BULKLOAD_REF" == refs/heads/main ]]; then',
        'case "$mode" in',
        "preflight | enforce | bazel) ;;",
        'require_empty "user-name override" "${USER:-}"',
        'require_empty "Nix store directory override" "${NIX_STORE_DIR:-}"',
        'require_empty "Nix daemon socket override" "${NIX_DAEMON_SOCKET_PATH:-}"',
        'require_empty "Nix remote systems override" "${NIX_REMOTE_SYSTEMS:-}"',
        'require_empty "Nix TLS certificate override" "${NIX_SSL_CERT_FILE:-}"',
        'require_empty "Nix curl flags" "${NIX_CURL_FLAGS:-}"',
        'require_empty "Nix hashed mirrors" "${NIX_HASHED_MIRRORS:-}"',
        'GRPC_PROXY_EXP) die "gRPC proxy override must be absent" ;;',
        'require_empty "Java tool options" "${JAVA_TOOL_OPTIONS:-}"',
        'require_empty "Bazel shell override" "${BAZEL_SH:-}"',
        'require_empty "Bazelisk no-JDK selector" "${BAZELISK_NOJDK:-}"',
        'require_empty "Bazelisk clean command" "${BAZELISK_CLEAN:-}"',
        'require_empty "Bazelisk shutdown command" "${BAZELISK_SHUTDOWN:-}"',
        'require_empty "Bazelisk fallback version" "${USE_BAZEL_FALLBACK_VERSION:-}"',
        'require_empty "Bazel test temporary root" "${TEST_TMPDIR:-}"',
        "  NIX_CACHE_HOME \\",
        "  NIX_CONFIG_HOME \\",
        "  NIX_DATA_HOME \\",
        "  NIX_STATE_HOME \\",
        "  XDG_CACHE_HOME \\",
        "  XDG_CONFIG_HOME \\",
        "  XDG_DATA_HOME \\",
        "  XDG_STATE_HOME; do",
        'require_empty "Attic token" "${ATTIC_TOKEN:-}"',
        'require_empty "Nix access tokens" "${NIX_ACCESS_TOKENS:-}"',
        'require_equal "Nix user configuration" "${NIX_USER_CONF_FILES:-}" /dev/null',
        'require_equal "netrc environment" "${NETRC:-}" /dev/null',
        'require_equal "Nix remote store" "${NIX_REMOTE:-}" local',
        'require_endpoint ATTIC_SERVER "${ATTIC_SERVER:-}"',
        'require_endpoint BAZEL_REMOTE_CACHE "${BAZEL_REMOTE_CACHE:-}"',
        'require_empty "remote executor" "${BAZEL_REMOTE_EXECUTOR:-}"',
        'require_empty "remote execution header" "${BAZEL_REMOTE_EXEC_HEADER:-}"',
        'require_empty "Bazel credential helper" "${BAZEL_CREDENTIAL_HELPER:-}"',
        'require_empty "Bazel remote header" "${BAZEL_REMOTE_HEADER:-}"',
        'require_empty "Bazel cache header" "${BAZEL_REMOTE_CACHE_HEADER:-}"',
        'require_empty "Bazelisk GitHub token" "${BAZELISK_GITHUB_TOKEN:-}"',
        'require_empty "Bazelisk base URL override" "${BAZELISK_BASE_URL:-}"',
        'require_empty "Bazelisk format URL override" "${BAZELISK_FORMAT_URL:-}"',
        'require_empty "Bazelisk wrapper directory" "${BAZELISK_WRAPPER_DIRECTORY:-}"',
        'require_empty "Bazelisk incompatible flags" "${BAZELISK_INCOMPATIBLE_FLAGS:-}"',
        'require_empty "Bazelisk verification override" "${BAZELISK_VERIFY_SHA256:-}"',
        'require_empty "Bazelisk version override" "${USE_BAZEL_VERSION:-}"',
        'require_empty "Bazelisk Linux home override" "${BAZELISK_HOME_LINUX:-}"',
        'require_empty "Bazelisk Darwin home override" "${BAZELISK_HOME_DARWIN:-}"',
        'require_equal "Bazelisk wrapper skip" "${BAZELISK_SKIP_WRAPPER:-}" true',
        'require_empty "Bazelisk home" "${BAZELISK_HOME:-}"',
        'if [[ "$mode" == preflight ]]; then',
        "printf 'BASH_ENV=/dev/null\\n'",
        "printf 'ENV=/dev/null\\n'",
        'require_equal "GitHub environment directory" "$env_dir" "$runner_temp/_runner_file_commands"',
        "set_env_*) ;;",
        "set_output_*) ;;",
        'if [[ "$mode" == bazel ]]; then',
        "build | test) ;;",
        '"captured Bazel endpoint"',
        '"active Bazel upload gate"',
        '"captured Nix client configuration"',
        '"reviewed Nix client configuration"',
        'require_absent_or_empty_file "system Bazel rc" /etc/bazel.bazelrc',
        'die "workspace Bazelisk rc is forbidden"',
        'die "workspace Bazelisk wrapper is forbidden"',
        f"readonly workspace_bazelrc_sha256={WORKSPACE_BAZELRC_SHA256}",
        f"readonly flywheel_bazelrc_sha256={BAZELRC_SHA256}",
        f"readonly bazel_version_sha256={BAZEL_VERSION_SHA256}",
        f"readonly flake_sha256={FLAKE_SHA256}",
        f"readonly flake_lock_sha256={FLAKE_LOCK_SHA256}",
        'mktemp -d "$RUNNER_TEMP/bulkload-bazelisk-${BULKLOAD_BAZEL_PHASE}.XXXXXXXX"',
        'require_absent_or_empty_file "isolated home netrc" "$bazel_home/.netrc"',
        "printf 'bazelisk_home=%s\\n' \"$bazelisk_home\"",
        "printf 'bazel_home=%s\\n' \"$bazel_home\"",
        "printf 'bazel_test_tmpdir=%s\\n' \"$bazel_test_tmpdir\"",
        "printf 'bazel_runtime_home=%s\\n' \"$bazel_runtime_home\"",
        "substituters = ${ATTIC_SERVER%/}/${cache_name} ${nixos_cache}",
        "store = local",
        "allow-symlinked-store = false",
        "trusted-public-keys = ${public_key} ${nixos_public_key}",
        "trusted-substituters =",
        "builders =",
        "builders-use-substitutes = false",
        "build-hook =",
        "pre-build-hook =",
        "access-tokens =",
        "netrc-file = /dev/null",
        "accept-flake-config = false",
        "post-build-hook =",
        "diff-hook =",
        "run-diff-hook = false",
        "require-sigs = true",
        "secret-key-files =",
        "plugin-files =",
        "printf 'NIX_ACCESS_TOKENS=\\n'",
        "printf 'NIX_USER_CONF_FILES=/dev/null\\n'",
        "printf 'NETRC=/dev/null\\n'",
        "printf 'NIX_REMOTE=local\\n'",
        "printf 'BAZEL_CREDENTIAL_HELPER=\\n'",
        "printf 'BAZEL_REMOTE_CACHE_HEADER=\\n'",
        "printf 'BAZEL_REMOTE_EXECUTOR=\\n'",
        "printf 'BAZEL_REMOTE_EXEC_HEADER=\\n'",
        "printf 'BAZEL_REMOTE_HEADER=\\n'",
        "printf 'BAZELISK_GITHUB_TOKEN=\\n'",
        "printf 'BAZELISK_HOME=\\n'",
        "printf 'BAZELISK_SKIP_WRAPPER=true\\n'",
        "printf 'BAZELISK_VERIFY_SHA256=\\n'",
        "printf 'trusted_path=%s\\n' \"$reviewed_step_path\"",
        "printf 'nix_config<<BULKLOAD_NIX_OUTPUT_%s\\n' \"$ci_templates_rev\"",
    )
    for declaration in required:
        if declaration not in guard:
            raise ContractError(f"guard contract drifted: {declaration}")
    if guard.count("GRPC_PROXY_EXP") != 1:
        raise ContractError("gRPC proxy exact-absence fence drifted")

    preflight_nix_config_match = re.search(
        r'readonly preflight_nix_config="([^\"]*)"', guard
    )
    expected_preflight_nix_config = "\n".join(
        (
            "substituters = ${nixos_cache}",
            "store = local",
            "allow-symlinked-store = false",
            "trusted-public-keys = ${nixos_public_key}",
            "trusted-substituters =",
            "builders =",
            "builders-use-substitutes = false",
            "build-hook =",
            "pre-build-hook =",
            "post-build-hook =",
            "diff-hook =",
            "run-diff-hook = false",
            "require-sigs = true",
            "access-tokens =",
            "netrc-file = /dev/null",
            "accept-flake-config = false",
            "secret-key-files =",
            "plugin-files =",
        )
    )
    if (
        preflight_nix_config_match is None
        or preflight_nix_config_match.group(1) != expected_preflight_nix_config
    ):
        raise ContractError("pre-discovery Nix client configuration drifted")

    nix_config_match = re.search(r'readonly nix_config="([^\"]*)"', guard)
    expected_nix_config = "\n".join(
        (
            "substituters = ${ATTIC_SERVER%/}/${cache_name} ${nixos_cache}",
            "store = local",
            "allow-symlinked-store = false",
            "trusted-public-keys = ${public_key} ${nixos_public_key}",
            "trusted-substituters =",
            "builders =",
            "builders-use-substitutes = false",
            "build-hook =",
            "pre-build-hook =",
            "post-build-hook =",
            "diff-hook =",
            "run-diff-hook = false",
            "require-sigs = true",
            "access-tokens =",
            "netrc-file = /dev/null",
            "accept-flake-config = false",
            "secret-key-files =",
            "plugin-files =",
        )
    )
    if nix_config_match is None or nix_config_match.group(1) != expected_nix_config:
        raise ContractError("isolated Nix client configuration drifted")

    endpoint_check = guard.index('require_endpoint ATTIC_SERVER "${ATTIC_SERVER:-}"')
    preflight_exit = guard.index('if [[ "$mode" == preflight ]]; then')
    reachability = guard.index('require_equal "Attic reachability"')
    if not endpoint_check < preflight_exit < reachability:
        raise ContractError(
            "raw endpoints must be checked before preflight exits and discovery is trusted"
        )


def validate_bazelrc(
    workspace_bazelrc: str, flywheel_bazelrc: str, *, exact_digest: bool = True
) -> None:
    if exact_digest and sha256(workspace_bazelrc) != WORKSPACE_BAZELRC_SHA256:
        raise ContractError("workspace bazelrc bytes drifted")
    if exact_digest and sha256(flywheel_bazelrc) != BAZELRC_SHA256:
        raise ContractError("vendored ci-templates bazelrc bytes drifted")
    workspace_bootstrap = [
        line
        for line in workspace_bazelrc.splitlines()
        if "bootstrap_impl" in line.casefold()
    ]
    if workspace_bootstrap != [BOOTSTRAP_IMPL_LINE]:
        raise ContractError(
            "workspace bazelrc must select the exact rules_python script bootstrap"
        )
    if any(
        "bootstrap_impl" in line.casefold() for line in flywheel_bazelrc.splitlines()
    ):
        raise ContractError(
            "vendored bazelrc must not override the workspace Python bootstrap"
        )
    if workspace_bazelrc.count("try-import %workspace%/.bazelrc.flywheel") != 1:
        raise ContractError(
            "workspace bazelrc must import one reviewed Flywheel profile"
        )

    workspace_options = "\n".join(
        line.strip()
        for line in workspace_bazelrc.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    )
    if re.search(
        r"(?m)^(?:try-)?import\s+(?!%workspace%/\.bazelrc\.flywheel$)",
        workspace_options,
    ):
        raise ContractError("workspace bazelrc imports an unaudited rc file")
    forbidden_workspace = (
        r"--remote_cache",
        r"--remote_executor",
        r"--remote_(?:exec|cache)?_?header",
        r"--credential_helper",
        r"--(?:google_credentials|tls_client_(?:certificate|key))",
        r"--remote_upload_local_results=true",
        r"--config=flywheel-executor",
        r"type\s*=\s*gha",
        r"(?:grpc|grpcs|http|https)://",
    )
    for pattern in forbidden_workspace:
        if re.search(pattern, workspace_options, re.IGNORECASE):
            raise ContractError("workspace bazelrc contains forbidden authority")

    flywheel_options = [
        line.strip()
        for line in flywheel_bazelrc.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    for line in flywheel_options:
        if re.search(
            r"(?:grpc|grpcs|http|https)://|type\s*=\s*gha", line, re.IGNORECASE
        ):
            raise ContractError("vendored bazelrc contains embedded cache authority")
        if not line.startswith("common:flywheel-executor ") and re.search(
            r"--remote_cache|--remote_executor|--remote_(?:exec|cache)?_?header|--credential_helper|--(?:google_credentials|tls_client_(?:certificate|key))",
            line,
            re.IGNORECASE,
        ):
            raise ContractError(
                "selected Flywheel profile contains credential authority"
            )
    if "common:flywheel --remote_upload_local_results=false" not in flywheel_bazelrc:
        raise ContractError("vendored bazelrc lost the read-only default")


def validate_flake_topology(
    flake: str, lock_source: str, *, exact_digest: bool = True
) -> None:
    if exact_digest and sha256(flake) != FLAKE_SHA256:
        raise ContractError("flake source bytes drifted")
    if exact_digest and sha256(lock_source) != FLAKE_LOCK_SHA256:
        raise ContractError("flake lock bytes drifted")
    forbidden = (
        "tinyland-inc/GloriousFlywheel",
        "github:tinyland-inc/GloriousFlywheel",
        "git+ssh://",
        "ssh://",
        "path:",
    )
    for source in (flake, lock_source):
        for marker in forbidden:
            if marker in source:
                raise ContractError(f"flake contains private or local source: {marker}")
    if flake.count('inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";') != 1:
        raise ContractError("flake input inventory drifted")
    source_literals = re.findall(
        r'"((?:github|https?|git\+ssh|ssh|path):[^"\n]+)"', flake
    )
    if source_literals != ["github:NixOS/nixpkgs/nixos-unstable"]:
        raise ContractError("flake contains an unaudited source literal")
    if re.search(r"\b(?:fetchGit|fetchTree|fetchTarball|getFlake)\b", flake):
        raise ContractError("flake contains an unaudited source fetcher")
    declared_inputs = set(
        re.findall(r"(?m)^\s*inputs\.([A-Za-z0-9_-]+)\.url\s*=", flake)
    )
    if declared_inputs != {"nixpkgs"}:
        raise ContractError("flake declares an unaudited input")

    try:
        lock = json.loads(lock_source)
    except json.JSONDecodeError as exc:
        raise ContractError("flake lock is not canonical JSON") from exc
    if lock.get("root") != "root" or set(lock.get("nodes", {})) != {
        "nixpkgs",
        "root",
    }:
        raise ContractError("flake lock node inventory drifted")
    if lock["nodes"]["root"].get("inputs") != {"nixpkgs": "nixpkgs"}:
        raise ContractError("flake root input wiring drifted")
    nixpkgs = lock["nodes"]["nixpkgs"]
    locked = nixpkgs.get("locked", {})
    original = nixpkgs.get("original", {})
    if set(locked) != {
        "lastModified",
        "narHash",
        "owner",
        "repo",
        "rev",
        "type",
    }:
        raise ContractError("locked nixpkgs field inventory drifted")
    if set(original) != {"owner", "ref", "repo", "type"}:
        raise ContractError("original nixpkgs field inventory drifted")
    if {key: locked.get(key) for key in ("type", "owner", "repo")} != {
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs",
    }:
        raise ContractError("locked nixpkgs source authority drifted")
    if re.fullmatch(r"[0-9a-f]{40}", str(locked.get("rev", ""))) is None:
        raise ContractError("locked nixpkgs revision is not immutable")
    if (
        re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", str(locked.get("narHash", "")))
        is None
    ):
        raise ContractError("locked nixpkgs content hash is not canonical")
    if {key: original.get(key) for key in ("type", "owner", "repo", "ref")} != {
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "ref": "nixos-unstable",
    }:
        raise ContractError("original nixpkgs source authority drifted")


class CiContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = find_workspace()
        cls.workflow = (cls.root / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )
        cls.action = (cls.root / LOCAL_ACTION_PATH).read_text(encoding="utf-8")
        cls.guard = (cls.root / GUARD_PATH).read_text(encoding="utf-8")
        cls.workspace_bazelrc = (cls.root / ".bazelrc").read_text(encoding="utf-8")
        cls.bazelrc = (cls.root / ".bazelrc.flywheel").read_text(encoding="utf-8")
        cls.build = (cls.root / "BUILD.bazel").read_text(encoding="utf-8")
        cls.flake = (cls.root / "flake.nix").read_text(encoding="utf-8")
        cls.flake_lock = (cls.root / "flake.lock").read_text(encoding="utf-8")
        cls.bazel_version = (cls.root / ".bazelversion").read_text(encoding="utf-8")

    def test_complete_public_front_door_contract(self) -> None:
        validate_workflow(self.workflow)
        validate_local_action(self.action)
        validate_guard(self.guard)
        validate_bazelrc(self.workspace_bazelrc, self.bazelrc)
        validate_flake_topology(self.flake, self.flake_lock)
        self.assertEqual(sha256(self.bazel_version), BAZEL_VERSION_SHA256)
        mode = (self.root / GUARD_PATH).stat().st_mode
        self.assertNotEqual(mode & stat.S_IXUSR, 0)

    def test_yaml_environment_keys_are_casefold_unique(self) -> None:
        workflow_collision = self.workflow.replace(
            '      ftp_proxy: ""\n',
            '      ftp_proxy: ""\n      FTP_PROXY: ""\n',
            1,
        )
        action_collision = self.action.replace(
            '        JAVA_TOOL_OPTIONS: ""\n',
            '        ftp_proxy: ""\n'
            '        FTP_PROXY: ""\n'
            '        JAVA_TOOL_OPTIONS: ""\n',
            1,
        )
        for source, validator in (
            (workflow_collision, validate_workflow),
            (action_collision, validate_local_action),
        ):
            with self.subTest(validator=validator.__name__):
                with self.assertRaisesRegex(
                    ContractError, "environment key is case-insensitively duplicated"
                ):
                    validator(source, exact_digest=False)

    def test_grpc_proxy_requires_exact_absence(self) -> None:
        grpc_name = "GRPC_PROXY_EXP"
        grpc_pattern = re.compile(
            r"(?i)(?<![A-Za-z0-9_])GRPC_PROXY_EXP(?![A-Za-z0-9_])"
        )
        self.assertIsNone(grpc_pattern.search(self.workflow))
        self.assertIsNone(grpc_pattern.search(self.action))

        def add_consumer_environment(
            action: str, step_name: str, key: str, value: str
        ) -> str:
            step = extract_action_step(action, step_name)
            mutated_step = step.replace(
                "      env:\n", f"      env:\n        {key}: {value}\n", 1
            )
            self.assertNotEqual(mutated_step, step)
            return action.replace(step, mutated_step, 1)

        action_variants = [
            add_consumer_environment(self.action, step_name, grpc_name, value)
            for step_name in (
                TERMINAL_CONSUMERS["build"],
                TERMINAL_CONSUMERS["test"],
            )
            for value in ('""', "dns:///proxy.invalid")
        ]
        action_variants.extend(
            (
                add_consumer_environment(
                    self.action,
                    "Preflight raw runner endpoint authority",
                    grpc_name.lower(),
                    '""',
                ),
                self.action.replace(
                    "        set -euo pipefail\n",
                    "        set -euo pipefail\n        export GRPC_PROXY_EXP=\n",
                    1,
                ),
            )
        )
        for index, unsafe in enumerate(action_variants):
            with self.subTest(action_variant=index):
                with self.assertRaisesRegex(ContractError, "GRPC_PROXY_EXP absent"):
                    validate_local_action(unsafe, exact_digest=False)

        workflow_variant = self.workflow.replace(
            '      ATTIC_TOKEN: ""\n',
            '      GRPC_PROXY_EXP: ""\n      ATTIC_TOKEN: ""\n',
            1,
        )
        with self.assertRaisesRegex(ContractError, "gRPC proxy override"):
            validate_workflow(workflow_variant, exact_digest=False)

    def test_each_gate_has_one_semantically_terminal_repository_consumer(self) -> None:
        expected = {
            "source": [
                "Validate the terminal gate selection",
                "Snapshot the exact public-read guard",
                "Preflight raw runner endpoint authority",
                "Discover sanctioned runner endpoint authority",
                "Enforce the discovered public-read boundary",
                "Verify effective Nix client authority",
                TERMINAL_CONSUMERS["source"],
            ],
            "build": [
                "Validate the terminal gate selection",
                "Snapshot the exact public-read guard",
                "Preflight raw runner endpoint authority",
                "Discover sanctioned runner endpoint authority",
                "Enforce the discovered public-read boundary",
                "Verify effective Nix client authority",
                "Revalidate immutable Bazel build authority",
                TERMINAL_CONSUMERS["build"],
            ],
            "test": [
                "Validate the terminal gate selection",
                "Snapshot the exact public-read guard",
                "Preflight raw runner endpoint authority",
                "Discover sanctioned runner endpoint authority",
                "Enforce the discovered public-read boundary",
                "Verify effective Nix client authority",
                "Revalidate immutable Bazel test authority",
                TERMINAL_CONSUMERS["test"],
            ],
            "fault-harness": [
                "Validate the terminal gate selection",
                "Snapshot the exact public-read guard",
                "Preflight raw runner endpoint authority",
                "Discover sanctioned runner endpoint authority",
                "Enforce the discovered public-read boundary",
                "Verify effective Nix client authority",
                TERMINAL_CONSUMERS["fault-harness"],
            ],
        }
        all_consumers = set(TERMINAL_CONSUMERS.values())
        for gate in TERMINAL_GATES:
            with self.subTest(gate=gate):
                selected = selected_action_path(self.action, gate)
                self.assertEqual(selected, expected[gate])
                self.assertEqual(
                    [name for name in selected if name in all_consumers],
                    [TERMINAL_CONSUMERS[gate]],
                )
                self.assertEqual(selected[-1], TERMINAL_CONSUMERS[gate])

    def test_workflow_triggers_cap_and_failure_suppression_mutations_fail_closed(
        self,
    ) -> None:
        cap = "    timeout-minutes: 15\n"
        queue = "  merge_group:\n    types: [checks_requested]\n"
        variants = [
            self.workflow.replace(queue, "", 1),
            self.workflow.replace(
                queue, "  merge_group:\n    types: [checks_requested, destroyed]\n", 1
            ),
            self.workflow.replace(queue, queue + "  workflow_dispatch:\n", 1),
            self.workflow.replace(queue, queue + "  pull_request_target:\n", 1),
            self.workflow.replace(
                "    branches: [main]\n", "    branches: ['**']\n", 1
            ),
            self.workflow.replace(
                "github.event_name == 'merge_group' && "
                "github.event.merge_group.head_sha || ",
                "",
                1,
            ),
            self.workflow.replace(cap, "    timeout-minutes: 30\n", 1),
            self.workflow.replace(cap, "", 1),
            self.workflow.replace(cap, cap + "    continue-on-error: true\n", 1),
            self.workflow.replace(cap, cap + '    "continue-on-error": true\n', 1),
            self.workflow.replace(
                "      - name: Bulkload public-read cache-first validation\n",
                "      - name: Bulkload public-read cache-first validation\n"
                "        timeout-minutes: 60\n",
                1,
            ),
            self.workflow.replace(
                "      - name: Bulkload public-read cache-first validation\n",
                "      - name: Bulkload public-read cache-first validation\n"
                "        continue-on-error: true\n",
                1,
            ),
        ]
        for index, unsafe in enumerate(variants):
            with self.subTest(index=index):
                self.assertNotEqual(unsafe, self.workflow)
                with self.assertRaises(ContractError):
                    validate_workflow(unsafe, exact_digest=False)

    def test_fault_harness_gate_mutations_fail_closed(self) -> None:
        harness_exec = "          .#default --command just ci-fault-harness"
        harness_condition = "      if: ${{ inputs.gate == 'fault-harness' }}\n"
        variants = [
            self.action.replace(
                harness_exec, "          .#default --command just rust-check", 1
            ),
            self.action.replace(harness_exec, harness_exec + "\n        /bin/true", 1),
            self.action.replace(harness_condition, "", 1),
            self.action.replace(
                harness_condition, "      if: ${{ inputs.gate == 'source' }}\n", 1
            ),
            self.action.replace(
                "          source | build | test | fault-harness) ;;",
                "          source | build | test) ;;",
                1,
            ),
            self.action.replace(
                "        --keep XDG_STATE_HOME \\\n" + harness_exec,
                harness_exec,
                1,
            ),
        ]
        for index, unsafe in enumerate(variants):
            with self.subTest(index=index):
                self.assertNotEqual(unsafe, self.action)
                with self.assertRaises(ContractError):
                    validate_local_action(unsafe, exact_digest=False)

    def test_matrix_terminal_and_injection_mutations_fail_closed(self) -> None:
        workflow_variants = [
            self.workflow.replace(
                "        gate: [source, build, test, fault-harness]",
                "        gate: [source, build, test]",
                1,
            ),
            self.workflow.replace(
                "        gate: [source, build, test, fault-harness]",
                "        gate: [build, source, test, fault-harness]",
                1,
            ),
            self.workflow.replace("      fail-fast: false", "      fail-fast: true", 1),
            self.workflow.replace(
                f"          gate: {MATRIX_GATE_EXPRESSION}",
                "          gate: source",
                1,
            ),
            self.workflow.replace(
                "      BASH_ENV: /dev/null", "      BASH_ENV: /tmp/evil", 1
            ),
            self.workflow.replace(
                '      LD_PRELOAD: ""', "      LD_PRELOAD: /tmp/evil.so", 1
            ),
            self.workflow.replace(
                '      http_proxy: ""',
                "      http_proxy: http://proxy.invalid",
                1,
            ),
            self.workflow.replace(
                '      https_proxy: ""',
                "      https_proxy: http://proxy.invalid",
                1,
            ),
            self.workflow.replace(
                '      ftp_proxy: ""',
                "      ftp_proxy: http://proxy.invalid",
                1,
            ),
            self.workflow.replace(
                '      all_proxy: ""',
                "      all_proxy: socks5://proxy.invalid",
                1,
            ),
            self.workflow.replace(
                '      no_proxy: ""', "      no_proxy: metadata.internal", 1
            ),
            self.workflow.replace(
                '      CURL_CA_BUNDLE: ""',
                "      CURL_CA_BUNDLE: /tmp/ca.pem",
                1,
            ),
            self.workflow.replace(
                '      SSLKEYLOGFILE: ""',
                "      SSLKEYLOGFILE: /tmp/keylog",
                1,
            ),
            self.workflow.replace(
                "      BASH_ENV: /dev/null\n",
                "      BASH_ENV: /dev/null\n"
                "      BASH_FUNC_nix%%: '() { /bin/true; }'\n",
                1,
            ),
        ]
        self.assert_workflow_mutations_fail_closed(workflow_variants)

        build_condition = "      if: ${{ inputs.gate == 'build' }}\n"
        test_condition = "      if: ${{ inputs.gate == 'test' }}\n"
        source_condition = "      if: ${{ inputs.gate == 'source' }}\n"

        def action_env_override(key: str, value: str) -> str:
            marker = "        BASH_ENV: /dev/null\n"
            return self.action.replace(marker, marker + f"        {key}: {value}\n", 1)

        def action_step_env_override(declaration: str) -> str:
            marker = "        BULKLOAD_GATE: ${{ inputs.gate }}\n      run: |\n"
            return self.action.replace(
                marker,
                "        BULKLOAD_GATE: ${{ inputs.gate }}\n"
                f"{declaration}"
                "      run: |\n",
                1,
            )

        alias_prelude = (
            "x-poison-env: &poison-env\n  LD_PRELOAD: /tmp/poison.so\nruns:\n"
        )
        action_with_alias = self.action.replace("runs:\n", alias_prelude, 1)
        alias_marker = "        BULKLOAD_GATE: ${{ inputs.gate }}\n      run: |\n"
        action_with_env_alias = action_with_alias.replace(
            alias_marker,
            "        BULKLOAD_GATE: ${{ inputs.gate }}\n"
            "      env: *poison-env\n"
            "      run: |\n",
            1,
        )
        action_with_env_merge = action_with_alias.replace(
            "      env:\n        BASH_ENV: /dev/null\n",
            "      env:\n        <<: *poison-env\n        BASH_ENV: /dev/null\n",
            1,
        )
        alternate_env_syntax_variants = [
            action_step_env_override("      env: {LD_PRELOAD: /tmp/flow-poison.so}\n"),
            action_step_env_override(
                '      "env":\n        http_proxy: http://proxy.invalid\n'
            ),
            action_step_env_override(
                "      'env':\n        SSL_CERT_FILE: /tmp/poison-ca.pem\n"
            ),
            action_step_env_override(
                '      "e\\u006ev":\n        SSLKEYLOGFILE: /tmp/keylog\n'
            ),
            action_with_env_alias,
            action_with_env_merge,
        ]
        for index, unsafe in enumerate(alternate_env_syntax_variants):
            with self.subTest(alternate_env_syntax_variant=index):
                self.assertNotEqual(unsafe, self.action)
                with self.assertRaisesRegex(
                    ContractError,
                    r"(?:canonical block mapping syntax|environment key is not auditable|composite step field inventory drifted)",
                ):
                    validate_local_action(unsafe, exact_digest=False)

        action_variants = [
            self.action.replace(build_condition, "", 1),
            self.action.replace(test_condition, build_condition, 1),
            self.action.replace(source_condition, "      if: ${{ always() }}\n", 1),
            self.action.replace(
                "        BASH_ENV: /dev/null", "        BASH_ENV: /tmp/evil", 1
            ),
            action_env_override("LD_PRELOAD", "/tmp/evil.so"),
            action_env_override('"LD_PRELOAD"', "/tmp/quoted-evil.so"),
            action_env_override('"LD_AUDIT"', "/tmp/audit.so"),
            action_env_override("BASH_FUNC_nix%%", "'() { /bin/true; }'"),
            action_env_override('"BASH_FUNC_nix%%"', "'() { /bin/true; }'"),
            action_env_override("SHELLOPTS", "xtrace"),
            action_env_override("PS4", "secret"),
            action_env_override("http_proxy", "http://proxy.invalid"),
            action_env_override("HTTPS_PROXY", '""'),
            action_env_override("ftp_proxy", '""'),
            action_env_override("FTP_PROXY", '""'),
            action_env_override("CURL_CA_BUNDLE", "/tmp/ca.pem"),
            action_env_override("SSL_CERT_DIR", "/tmp/certs"),
            action_env_override("SSLKEYLOGFILE", "/tmp/keylog"),
            self.action.replace(
                "        exec nix develop --no-write-lock-file --ignore-environment \\",
                "        nix develop --no-write-lock-file --ignore-environment \\",
                1,
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n"
                "      if: ${{ inputs.gate == 'source' }}\n",
                "    - name: Run repository-owned source gates\n"
                "      if: ${{ inputs.gate == 'source' }}\n"
                "      working-directory: ${{ github.workspace }}/attacker\n",
                1,
            ),
            self.action.replace(
                "          .#default --command just ci-source",
                "          .#default --command just ci-source\n        /bin/true",
                1,
            ),
            self.action + "\n    - name: Consume poisoned environment\n"
            "      shell: /bin/bash --noprofile --norc -p {0}\n"
            "      run: /bin/true\n",
            self.action.replace(
                "    - name: Revalidate immutable Bazel test authority\n",
                "    - name: Consume build output after its consumer\n"
                "      if: ${{ inputs.gate == 'build' }}\n"
                "      shell: /bin/bash --noprofile --norc -p {0}\n"
                "      run: /bin/true\n\n"
                "    - name: Revalidate immutable Bazel test authority\n",
                1,
            ),
        ]
        for index, unsafe in enumerate(action_variants):
            with self.subTest(action_variant=index):
                self.assertNotEqual(unsafe, self.action)
                with self.assertRaises(ContractError):
                    validate_local_action(unsafe, exact_digest=False)

    def assert_workflow_mutations_fail_closed(self, unsafe_variants: list[str]) -> None:
        for index, unsafe in enumerate(unsafe_variants):
            with self.subTest(index=index, unsafe=unsafe[-160:]):
                self.assertNotEqual(unsafe, self.workflow)
                with self.assertRaises(ContractError):
                    validate_workflow(unsafe, exact_digest=False)

    def test_hosted_dynamic_and_fork_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace("runs-on: tinyland-nix", "runs-on: ubuntu-latest"),
            self.workflow.replace(
                "runs-on: tinyland-nix", "runs-on: ${{ vars.RUNNER }}"
            ),
            self.workflow.replace(
                "runs-on: tinyland-nix", "runs-on: [self-hosted, tinyland-nix]"
            ),
            self.workflow.replace(f"    if: {SAME_REPOSITORY_GUARD}\n", ""),
            self.workflow
            + "\n  unaudited:\n    runs-on: tinyland-nix\n    steps: []\n",
        ]
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_materialization_token_scope_and_no_post_fail_closed(self) -> None:
        clear = "          unset BULKLOAD_GIT_HTTP_HEADER git_http_header\n"
        checkout = '            checkout --detach --force "$expected_sha"\n'
        clear_after_checkout = self.workflow.replace(clear, "", 1).replace(
            checkout, checkout + clear, 1
        )
        unsafe_variants = [
            self.workflow.replace(
                "${{ github.token }}", "${{ secrets.GITHUB_TOKEN }}", 1
            ),
            self.workflow.replace(
                f"        uses: {LOCAL_ACTION}\n",
                f"        uses: {LOCAL_ACTION}\n"
                "        env:\n"
                "          BULKLOAD_CHECKOUT_TOKEN: ${{ github.token }}\n",
                1,
            ),
            self.workflow.replace(clear, "", 1),
            clear_after_checkout,
            self.workflow.replace(
                "                --config-env=http.https://github.com/.extraheader=BULKLOAD_GIT_HTTP_HEADER \\\n",
                "                -c http.https://github.com/.extraheader=$BULKLOAD_GIT_HTTP_HEADER \\\n",
                1,
            ),
            self.workflow.replace(
                "        run: |\n", "        post: /tmp/post.sh\n        run: |\n", 1
            ),
            self.workflow.replace(
                f"        uses: {LOCAL_ACTION}", "        uses: actions/checkout@main"
            ),
            self.workflow.replace(
                "          builtin printf '::add-mask::%s\\n' \"$BULKLOAD_CHECKOUT_TOKEN\"\n",
                "",
                1,
            ),
            self.workflow.replace(
                "          builtin printf '::add-mask::%s\\n' \"$encoded_token\"\n",
                "",
                1,
            ),
            self.workflow.replace(
                "          builtin printf '::add-mask::%s\\n' \"$git_http_header\"\n",
                "",
                1,
            ),
            self.workflow.replace(
                "          export -n checkout_token SHELLOPTS BASHOPTS\n",
                "          export checkout_token\n",
                1,
            ),
            self.workflow.replace(
                '            export BULKLOAD_GIT_HTTP_HEADER="$git_http_header"\n',
                '          export BULKLOAD_GIT_HTTP_HEADER="$git_http_header"\n',
                1,
            ),
            self.workflow.replace("            fetch_env_unsets=()\n", "", 1),
        ]
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_materialization_bootstrap_and_transport_fences_fail_closed(self) -> None:
        proxy_persistence = (
            "          builtin printf '%s\\n' \\\n"
            "            'HTTP_PROXY=' \\\n"
            "            'HTTPS_PROXY=' \\\n"
            "            'FTP_PROXY=' \\\n"
            "            'ALL_PROXY=' \\\n"
            "            'NO_PROXY=' \\\n"
            '            >> "$github_env"\n'
        )
        checkout = '            checkout --detach --force "$expected_sha"\n'
        persistence_before_checkout = self.workflow.replace(
            proxy_persistence, "", 1
        ).replace(checkout, proxy_persistence + checkout, 1)
        unsafe_variants = [
            self.workflow.replace(
                "        shell: /bin/bash --noprofile --norc -p {0}",
                "        shell: /bin/bash --noprofile --norc {0}",
                1,
            ),
            self.workflow.replace(
                "          BASH_ENV: /dev/null", "          BASH_ENV: /tmp/evil", 1
            ),
            self.workflow.replace(
                '          SHELLOPTS: ""', "          SHELLOPTS: xtrace", 1
            ),
            self.workflow.replace('          PS4: ""', "          PS4: leaked", 1),
            self.workflow.replace(
                '          LD_AUDIT: ""', "          LD_AUDIT: /tmp/audit.so", 1
            ),
            self.workflow.replace(
                '          HTTPS_PROXY: ""', "          HTTPS_PROXY: inherited", 1
            ),
            self.workflow.replace(
                '          FTP_PROXY: ""', "          FTP_PROXY: inherited", 1
            ),
            self.workflow.replace(
                '          HTTP_PROXY: ""\n',
                '          HTTP_PROXY: ""\n          http_proxy: inherited\n',
                1,
            ),
            self.workflow.replace(
                '          CURL_CA_BUNDLE: ""',
                "          CURL_CA_BUNDLE: /tmp/ca.pem",
                1,
            ),
            self.workflow.replace(
                '          SSLKEYLOGFILE: ""', "          SSLKEYLOGFILE: /tmp/keys", 1
            ),
            self.workflow.replace(
                "          unset HTTP_PROXY HTTPS_PROXY FTP_PROXY ALL_PROXY NO_PROXY\n",
                "          unset HTTP_PROXY HTTPS_PROXY FTP_PROXY ALL_PROXY NO_PROXY\n"
                "          export HTTPS_PROXY=inherited\n",
                1,
            ),
            self.workflow.replace(
                "github_env=$GITHUB_ENV", "github_env=$GITHUB_PATH", 1
            ),
            self.workflow.replace(
                '          test "$(/usr/bin/dirname -- "$github_env")" = "$runner_commands"\n',
                "",
                1,
            ),
            self.workflow.replace("            'HTTP_PROXY=' \\\n", "", 1),
            self.workflow.replace(
                "            'HTTPS_PROXY=' \\\n",
                "            'HTTPS_PROXY=http://proxy.invalid' \\\n",
                1,
            ),
            self.workflow.replace("            'FTP_PROXY=' \\\n", "", 1),
            self.workflow.replace(
                "            'ALL_PROXY=' \\\n",
                "            'ALL_PROXY=$BULKLOAD_CHECKOUT_TOKEN' \\\n",
                1,
            ),
            persistence_before_checkout,
        ]
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_materialization_history_sha_and_order_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace(
                "                fetch --force --prune --no-recurse-submodules --no-tags \\\n",
                "                fetch --depth=1 \\\n",
                1,
            ),
            self.workflow.replace(
                "                --no-auto-maintenance ", "                ", 1
            ),
            self.workflow.replace("--no-write-commit-graph ", "", 1),
            self.workflow.replace(
                "                '+refs/heads/*:refs/remotes/origin/*' \\\n", "", 1
            ),
            self.workflow.replace(
                "                '+refs/tags/*:refs/tags/*'\n", "", 1
            ),
            self.workflow.replace(
                '            checkout --detach --force "$expected_sha"',
                "            checkout --detach --force origin/main",
                1,
            ),
            self.workflow.replace(
                '          test "$(/usr/bin/git -C "$workspace" rev-parse --verify HEAD)" = "$expected_sha"\n',
                "",
                1,
            ),
            self.workflow.replace(
                '          test "$(/usr/bin/git -C "$workspace" rev-parse --is-shallow-repository)" = false\n',
                "",
                1,
            ),
            self.workflow.replace(EXPECTED_SHA_EXPRESSION, "${{ github.sha }}", 1),
            self.workflow
            + "\n      - run: /bin/true\n        shell: /bin/bash --noprofile --norc -p {0}\n",
        ]
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_materialization_workspace_config_and_auth_guards_fail_closed(self) -> None:
        required_lines = (
            '          test ! -L "$workspace"\n',
            '          test -O "$workspace"\n',
            '          test "$(/usr/bin/readlink -f -- "$workspace")" = "$workspace"\n',
            '          test -z "$(/usr/bin/find "$workspace" -xdev -mindepth 1 -maxdepth 1 -print -quit)"\n',
            "          export GIT_CONFIG_GLOBAL=$checkout_state/home/.gitconfig\n",
            "          export GIT_CONFIG_NOSYSTEM=1\n",
            '          /usr/bin/git init --template="$checkout_state/template" "$workspace"\n',
            '          test ! -e "$workspace/.git/objects/info/alternates"\n',
            '          test -z "${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"\n',
            '          test "$(/usr/bin/git -C "$workspace" remote get-url --all origin)" = "$origin_url"\n',
            "          for variable in GIT_SSH GIT_SSH_COMMAND GIT_ASKPASS SSH_ASKPASS SSH_AUTH_SOCK; do\n",
        )
        unsafe_variants = [
            self.workflow.replace(line, "", 1) for line in required_lines
        ]
        unsafe_variants.extend(
            [
                self.workflow.replace(
                    '          test -z "$(/usr/bin/find "$workspace" -xdev -mindepth 1 -maxdepth 1 -print -quit)"\n',
                    '          /bin/rm -rf --one-file-system -- "$workspace"/*\n',
                    1,
                ),
                self.workflow.replace(
                    "credential\\.|http\\.|include\\.|includeif\\.|core\\.askpass$|core\\.sshcommand$|ssh\\.|remote\\..*\\.uploadpack$|url\\..*\\.insteadof$",
                    "credential\\.|http\\.",
                    1,
                ),
                self.workflow.replace(
                    "          origin_url=$server_url/$repository.git",
                    "          origin_url=https://x-access-token:$BULKLOAD_CHECKOUT_TOKEN@github.com/$repository.git",
                    1,
                ),
            ]
        )
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_permission_upload_and_endpoint_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace("contents: read", "contents: write"),
            self.workflow.replace(EXPECTED_SHA_EXPRESSION, "${{ github.sha }}"),
            self.workflow.replace(UPLOAD_EXPRESSION, "true"),
            self.workflow.replace('ATTIC_TOKEN: ""', "ATTIC_TOKEN: inherited"),
            self.workflow.replace(
                LOCAL_ACTION, "tinyland-inc/GloriousFlywheel/action@main"
            ),
            self.workflow.replace(
                '      ATTIC_TOKEN: ""',
                '      ATTIC_TOKEN: ""\n      BAZEL_CREDENTIAL_HELPER: /tmp/helper',
            ),
            self.workflow + "\n# type=gha\n",
            self.workflow + "\n# https://cache.invalid\n",
        ]
        self.assert_workflow_mutations_fail_closed(unsafe_variants)

    def test_recursive_action_pin_and_capability_regressions_fail_closed(self) -> None:
        preflight_start = self.action.index("    - name: Preflight raw runner")
        setup_start = self.action.index("    - name: Discover sanctioned runner")
        enforce_start = self.action.index("    - name: Enforce the discovered")
        setup_before_preflight = (
            self.action[:preflight_start]
            + self.action[setup_start:enforce_start]
            + self.action[preflight_start:setup_start]
            + self.action[enforce_start:]
        )
        source_start = self.action.index(
            "    - name: Run repository-owned source gates\n"
        )
        source_block = self.action[source_start:]
        without_source = self.action[:source_start]
        test_start = without_source.index(
            "    - name: Test the complete Bulkload Bazel graph"
        )
        source_before_test = (
            without_source[:test_start] + source_block + without_source[test_start:]
        )
        unsafe_variants = [
            self.action.replace("@" + CI_TEMPLATES_REV, "@v2.13.0", 1),
            self.action.replace(
                "tinyland-inc/ci-templates", "tinyland-inc/GloriousFlywheel", 1
            ),
            self.action.replace("config: flywheel", "config: flywheel-executor", 1),
            self.action.replace("targets: //:tests", "targets: //..."),
            self.action.replace(
                "        nix flake check --no-build --no-write-lock-file\n", ""
            ),
            self.action.replace(
                "        nix flake check --no-build --no-write-lock-file",
                "        nix flake check --no-build --no-write-lock-file || true",
            ),
            self.action.replace(
                "        nix flake check --no-build --no-write-lock-file",
                "        nix flake check --no-build --no-write-lock-file &",
            ),
            self.action.replace(
                "        nix flake check --no-build --no-write-lock-file",
                "        exit 0\n"
                "        nix flake check --no-build --no-write-lock-file",
            ),
            self.action.replace(
                "        set -euo pipefail\n        nixos_cache=https://cache.nixos.org/\n",
                "        set +e\n        nixos_cache=https://cache.nixos.org/\n",
            ),
            self.action.replace(
                "        nix flake check --no-build --no-write-lock-file",
                "        nix() { return 0; }\n"
                "        nix flake check --no-build --no-write-lock-file",
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n",
                "    - name: Run repository-owned source gates\n"
                "      continue-on-error: true\n",
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n",
                "    - name: Run repository-owned source gates\n"
                '      "continue-on-error": true\n',
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n",
                "    - name: Run repository-owned source gates\n"
                "      if: ${{ false }}\n",
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n"
                "      if: ${{ inputs.gate == 'source' }}\n"
                "      shell: /bin/bash --noprofile --norc -p {0}\n",
                "    - name: Run repository-owned source gates\n"
                "      if: ${{ inputs.gate == 'source' }}\n"
                "      shell: /bin/true {0}\n",
                1,
            ),
            self.action.replace(
                "    - name: Revalidate immutable Bazel build authority\n",
                "    - name: Revalidate immutable Bazel build authority\n"
                "      shell: bash\n",
                1,
            ),
            self.action.replace(
                f"{GUARD_INVOCATION} preflight",
                '/bin/bash --noprofile --norc -p "$GITHUB_WORKSPACE/scripts/ci-public-read-guard.sh" preflight',
                1,
            ),
            self.action.replace(
                f"{GUARD_INVOCATION} preflight",
                "printf '%s' \"$guard_source\" | /bin/bash --noprofile --norc -p -s -- preflight",
                1,
            ),
            self.action.replace(f"        {GUARD_INVOCATION} bazel\n", "", 1),
            self.action.replace(GUARD_SHA256, "0" * 64, 1),
            self.action.replace(
                "        BAZEL_REMOTE_CACHE: ${{ steps.authority.outputs.bazel_remote_cache }}",
                "        BAZEL_REMOTE_CACHE: ${{ env.BAZEL_REMOTE_CACHE }}",
                1,
            ),
            self.action.replace(
                'BAZELISK_SKIP_WRAPPER: "true"', 'BAZELISK_SKIP_WRAPPER: "false"', 1
            ),
            self.action.replace(
                'BAZELISK_HOME: ""',
                "BAZELISK_HOME: ${{ runner.temp }}/predictable",
                1,
            ),
            self.action.replace(
                "        NIX_REMOTE: local", "        NIX_REMOTE: daemon", 1
            ),
            self.action.replace(
                '        BAZEL_SH: ""', "        BAZEL_SH: /tmp/unaudited-sh", 1
            ),
            self.action.replace(
                '        BAZELISK_CLEAN: ""', "        BAZELISK_CLEAN: expunge", 1
            ),
            self.action.replace(" --ignore-environment \\", " \\", 1),
            self.action.replace(
                "          --keep HOME \\",
                "          --keep HOME \\\n          --keep ATTIC_TOKEN \\",
                1,
            ),
            self.action.replace("LC_ALL=C sort)", "LC_ALL=C sort -u)", 1),
            self.action.replace(
                "        NIX_REMOTE: local\n",
                "        NIX_REMOTE: local\n        NIX_REMOTE: local\n",
                1,
            ),
            self.action.replace(
                "        NIX_REMOTE: local\n",
                "        NIX_REMOTE: local\n        UNREVIEWED_SELECTOR: value\n",
                1,
            ),
            self.action.replace("        NIX_CONFIG: |-", "        NIX_CONFIG: >-", 1),
            setup_before_preflight,
            self.action.replace(
                'accept-flake-config)" == false', 'accept-flake-config)" == true'
            ),
            self.action.replace(
                '        [[ -z "$(nix config show post-build-hook)" ]]\n', ""
            ),
            self.action.replace(
                '        ATTIC_TOKEN: ""',
                '        ATTIC_TOKEN: ""\n        BAZEL_REMOTE_HEADER: secret',
            ),
            source_before_test,
            self.action + "\n    - name: Consume poisoned environment\n"
            "      shell: bash\n"
            "      run: /bin/true\n",
            self.action.replace(
                "    - name: Run repository-owned source gates\n",
                "    - name: Run repository-owned source gates\n"
                "      post: /tmp/post.sh\n",
                1,
            ),
            self.action.replace(
                "    - name: Run repository-owned source gates\n",
                "    - name: Run repository-owned source gates\n"
                "      post-if: always()\n",
                1,
            ),
            self.action + "\n# ${{ join(runner.labels, ',') }}\n",
            self.action + "\n# type=gha\n",
            self.action + "\n# https://cache.invalid\n",
        ]
        for index, unsafe in enumerate(unsafe_variants):
            with self.subTest(index=index, unsafe=unsafe[-160:]):
                self.assertNotEqual(unsafe, self.action)
                with self.assertRaises(ContractError):
                    validate_local_action(unsafe, exact_digest=False)

    def test_guard_authority_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.guard.replace("public_site=bulkload-ci", "public_site=other"),
            self.guard.replace("cache_name=main", "cache_name=write"),
            self.guard.replace(PUBLIC_KEY, "main:wrong"),
            self.guard.replace(
                "accept-flake-config = false", "accept-flake-config = true"
            ),
            self.guard.replace("access-tokens =", "access-tokens = inherited"),
            self.guard.replace("post-build-hook =", "post-build-hook = /tmp/hook"),
            self.guard.replace("secret-key-files =", "secret-key-files = /tmp/key"),
            self.guard.replace("plugin-files =", "plugin-files = /tmp/plugin"),
            self.guard.replace(
                "NIX_USER_CONF_FILES=/dev/null", "NIX_USER_CONF_FILES=/tmp/nix.conf"
            ),
            self.guard.replace("NETRC=/dev/null", "NETRC=$HOME/.netrc"),
            self.guard.replace("BAZEL_REMOTE_EXECUTOR:-", "IGNORED_EXECUTOR:-"),
            self.guard.replace("preflight | enforce", "skip | enforce"),
            self.guard.replace(
                'require_endpoint ATTIC_SERVER "${ATTIC_SERVER:-}"',
                'printf "%s\\n" "${ATTIC_SERVER:-}"',
            ),
            self.guard.replace("refs/heads/main", "refs/heads/*"),
            self.guard.replace(
                "refs/heads/gh-readonly-queue/main/*", "refs/heads/gh-readonly-queue/*"
            ),
            self.guard.replace('"merge-group repository"', '"merge-group source"'),
            self.guard.replace(
                'die "merge-group ref is outside the main merge queue"', "true"
            ),
            self.guard.replace(
                "  merge_group)\n", "  merge_group | workflow_dispatch)\n"
            ),
            self.guard.replace('NIX_REMOTE:-}" local', 'NIX_REMOTE:-}" daemon'),
            self.guard.replace(REVIEWED_PATH, "/tmp/unaudited:/usr/bin:/bin"),
            self.guard.replace(
                '      GRPC_PROXY_EXP) die "gRPC proxy override must be absent" ;;\n',
                "",
            ),
            self.guard.replace(
                'require_empty "Bazel shell override" "${BAZEL_SH:-}"',
                'printf "%s\\n" "${BAZEL_SH:-}"',
            ),
            self.guard.replace(
                'require_empty "Bazelisk clean command" "${BAZELISK_CLEAN:-}"',
                'printf "%s\\n" "${BAZELISK_CLEAN:-}"',
            ),
            self.guard.replace("store = local", "store = daemon"),
            self.guard.replace(
                "allow-symlinked-store = false", "allow-symlinked-store = true"
            ),
            self.guard.replace("require-sigs = true", "require-sigs = false"),
            self.guard.replace(
                'GIT_*) die "Git environment overrides are forbidden" ;;', ""
            ),
        ]
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_guard(unsafe, exact_digest=False)

    def test_guard_executes_pr_and_main_upload_policy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            workspace = temporary_path / "workspace"
            workspace.mkdir()
            for path in (
                ".bazelrc",
                ".bazelrc.flywheel",
                ".bazelversion",
                "flake.nix",
                "flake.lock",
            ):
                shutil.copy2(self.root / path, workspace / path)
            subprocess.run(["/usr/bin/git", "init", "-q"], cwd=workspace, check=True)
            subprocess.run(["/usr/bin/git", "add", "."], cwd=workspace, check=True)
            subprocess.run(
                [
                    "/usr/bin/git",
                    "-c",
                    "user.name=Bulkload CI contract",
                    "-c",
                    "user.email=bulkload-ci@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "-qm",
                    "fixture",
                ],
                cwd=workspace,
                check=True,
            )
            command_files = temporary_path / "_runner_file_commands"
            command_files.mkdir()
            github_env = command_files / "set_env_bulkload"
            github_env.touch()
            github_output = command_files / "set_output_bulkload"
            github_output.touch()
            head = subprocess.check_output(
                ["/usr/bin/git", "rev-parse", "HEAD"],
                cwd=workspace,
                text=True,
            ).strip()
            runtime_home = temporary_path / "runtime-home"
            runtime_home.mkdir(mode=0o700)
            nix_config = "\n".join(
                (
                    f"substituters = https://cache.example.invalid/main {NIXOS_CACHE}",
                    "store = local",
                    "allow-symlinked-store = false",
                    f"trusted-public-keys = {PUBLIC_KEY} {NIXOS_PUBLIC_KEY}",
                    "trusted-substituters =",
                    "builders =",
                    "builders-use-substitutes = false",
                    "build-hook =",
                    "pre-build-hook =",
                    "post-build-hook =",
                    "diff-hook =",
                    "run-diff-hook = false",
                    "require-sigs = true",
                    "access-tokens =",
                    "netrc-file = /dev/null",
                    "accept-flake-config = false",
                    "secret-key-files =",
                    "plugin-files =",
                )
            )
            preflight_nix_config = "\n".join(
                (
                    f"substituters = {NIXOS_CACHE}",
                    "store = local",
                    "allow-symlinked-store = false",
                    f"trusted-public-keys = {NIXOS_PUBLIC_KEY}",
                    "trusted-substituters =",
                    "builders =",
                    "builders-use-substitutes = false",
                    "build-hook =",
                    "pre-build-hook =",
                    "post-build-hook =",
                    "diff-hook =",
                    "run-diff-hook = false",
                    "require-sigs = true",
                    "access-tokens =",
                    "netrc-file = /dev/null",
                    "accept-flake-config = false",
                    "secret-key-files =",
                    "plugin-files =",
                )
            )
            base_env = {
                "PATH": REVIEWED_PATH,
                "HOME": str(runtime_home),
                "USER": "",
                "USERNAME": "",
                "LOGNAME": "",
                "GITHUB_ENV": str(github_env),
                "GITHUB_OUTPUT": str(github_output),
                "RUNNER_TEMP": temporary,
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_REF": "refs/pull/9/merge",
                "GITHUB_WORKSPACE": str(workspace),
                "GITHUB_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_RUNTIME_HOME": str(runtime_home),
                "BULKLOAD_EVENT_NAME": "pull_request",
                "BULKLOAD_EXPECTED_SHA": head,
                "BULKLOAD_REF": "refs/pull/9/merge",
                "BULKLOAD_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_HEAD_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_UPLOAD_BAZEL_RESULTS": "false",
                "BULKLOAD_RUNNER_ENVIRONMENT": "self-hosted",
                "BULKLOAD_RUNNER_NAME": "bulkload-nix-g5qc5-runner-n4jx8",
                "BULKLOAD_ATTIC_REACHABLE": "true",
                "BULKLOAD_BAZEL_CACHE_REACHABLE": "true",
                "ATTIC_TOKEN": "",
                "NIX_ACCESS_TOKENS": "",
                "NIX_USER_CONF_FILES": "/dev/null",
                "NETRC": "/dev/null",
                "NIX_CONFIG": preflight_nix_config,
                "NIX_REMOTE": "local",
                "NIX_CACHE_HOME": str(runtime_home),
                "NIX_CONFIG_HOME": str(runtime_home),
                "NIX_DATA_HOME": str(runtime_home),
                "NIX_STATE_HOME": str(runtime_home),
                "XDG_CACHE_HOME": str(runtime_home),
                "XDG_CONFIG_HOME": str(runtime_home),
                "XDG_DATA_HOME": str(runtime_home),
                "XDG_STATE_HOME": str(runtime_home),
                "ATTIC_SERVER": "https://cache.example.invalid",
                "ATTIC_CACHE": "main",
                "BAZEL_REMOTE_CACHE": "https://bazel.example.invalid",
                "BAZEL_REMOTE_EXECUTOR": "",
                "BAZEL_REMOTE_EXEC_HEADER": "",
                "BAZEL_CREDENTIAL_HELPER": "",
                "BAZEL_REMOTE_HEADER": "",
                "BAZEL_REMOTE_CACHE_HEADER": "",
                "BAZELISK_BASE_URL": "",
                "BAZELISK_FORMAT_URL": "",
                "BAZELISK_GITHUB_TOKEN": "",
                "BAZELISK_HOME": "",
                "BAZELISK_HOME_DARWIN": "",
                "BAZELISK_HOME_LINUX": "",
                "BAZELISK_INCOMPATIBLE_FLAGS": "",
                "BAZELISK_VERIFY_SHA256": "",
                "BAZELISK_SKIP_WRAPPER": "true",
                "BAZELISK_WRAPPER_DIRECTORY": "",
                "USE_BAZEL_VERSION": "",
                "BAZELISK_NOJDK": "",
                "BAZELISK_CLEAN": "",
                "BAZELISK_SHUTDOWN": "",
                "USE_BAZEL_FALLBACK_VERSION": "",
                "BAZEL_SH": "",
                "TEST_TMPDIR": "",
            }

            def run_guard(
                mode: str, env: dict[str, str], *, check: bool = True
            ) -> subprocess.CompletedProcess[str]:
                # Exactly the action's invocation: the guard bytes as `-c`.
                result = subprocess.run(
                    [
                        "/bin/bash",
                        "--noprofile",
                        "--norc",
                        "-p",
                        "-c",
                        (self.root / GUARD_PATH).read_text(encoding="utf-8"),
                        "bulkload-public-read-guard",
                        mode,
                    ],
                    check=False,
                    env=env,
                    capture_output=True,
                    text=True,
                )
                if check:
                    self.assertEqual(result.returncode, 0, result.stderr)
                return result

            run_guard("preflight", base_env)
            preflight_emitted = github_env.read_text()
            self.assertIn("BASH_ENV=/dev/null\n", preflight_emitted)
            self.assertIn("ENV=/dev/null\n", preflight_emitted)
            self.assertIn("ATTIC_TOKEN=\n", preflight_emitted)
            self.assertIn("NIX_ACCESS_TOKENS=\n", preflight_emitted)
            self.assertIn("NIX_REMOTE=local\n", preflight_emitted)
            self.assertIn(f"HOME={runtime_home}\n", preflight_emitted)
            self.assertIn(f"substituters = {NIXOS_CACHE}\n", preflight_emitted)
            self.assertIn("store = local\n", preflight_emitted)
            self.assertIn("allow-symlinked-store = false\n", preflight_emitted)
            self.assertIn("builders =\n", preflight_emitted)
            self.assertIn("require-sigs = true\n", preflight_emitted)
            self.assertIn("BAZEL_CREDENTIAL_HELPER=\n", preflight_emitted)
            for key in (
                "JAVA_TOOL_OPTIONS",
                "BAZEL_SH",
                "BAZELISK_NOJDK",
                "BAZELISK_CLEAN",
                "BAZELISK_SHUTDOWN",
                "USE_BAZEL_FALLBACK_VERSION",
                "TEST_TMPDIR",
            ):
                self.assertIn(f"{key}=\n", preflight_emitted)
            self.assertNotIn("GRPC_PROXY_EXP", preflight_emitted)
            for key in (
                "NIX_CACHE_HOME",
                "NIX_CONFIG_HOME",
                "NIX_DATA_HOME",
                "NIX_STATE_HOME",
                "XDG_CACHE_HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
            ):
                self.assertIn(f"{key}={runtime_home}\n", preflight_emitted)

            run_guard("enforce", base_env)
            emitted = github_env.read_text()
            self.assertIn("ATTIC_TOKEN=\n", emitted)
            self.assertIn("ATTIC_PUBLIC_READ_SITE=bulkload-ci\n", emitted)
            self.assertIn("BAZEL_CREDENTIAL_HELPER=\n", emitted)
            self.assertIn("BAZEL_REMOTE_CACHE_HEADER=\n", emitted)
            self.assertIn("BAZEL_REMOTE_EXECUTOR=\n", emitted)
            self.assertIn("BAZEL_REMOTE_EXEC_HEADER=\n", emitted)
            self.assertIn("BAZEL_REMOTE_HEADER=\n", emitted)
            self.assertIn("GF_BAZEL_REMOTE_UPLOAD=false\n", emitted)
            self.assertIn("NIX_ACCESS_TOKENS=\n", emitted)
            self.assertIn("NIX_USER_CONF_FILES=/dev/null\n", emitted)
            self.assertIn("NETRC=/dev/null\n", emitted)
            self.assertIn("access-tokens =\n", emitted)
            self.assertIn("accept-flake-config = false\n", emitted)
            self.assertIn("post-build-hook =\n", emitted)
            self.assertIn("secret-key-files =\n", emitted)
            self.assertIn("plugin-files =\n", emitted)
            self.assertNotIn("GRPC_PROXY_EXP", emitted)
            authority_outputs = github_output.read_text()
            self.assertIn(
                "attic_server=https://cache.example.invalid\n", authority_outputs
            )
            self.assertIn(
                "bazel_remote_cache=https://bazel.example.invalid\n",
                authority_outputs,
            )
            self.assertIn("bazel_remote_upload=false\n", authority_outputs)
            self.assertIn("trusted_path=", authority_outputs)
            self.assertIn("nix_config<<BULKLOAD_NIX_OUTPUT_", authority_outputs)
            self.assertNotIn("GRPC_PROXY_EXP", authority_outputs)

            bazel_env = dict(base_env)
            bazel_env.update(
                {
                    "BULKLOAD_BAZEL_PHASE": "build",
                    "BULKLOAD_CAPTURED_BAZEL_REMOTE_CACHE": "https://bazel.example.invalid",
                    "BULKLOAD_CAPTURED_BAZEL_UPLOAD": "false",
                    "BULKLOAD_CAPTURED_NIX_CONFIG": nix_config,
                    "GF_BAZEL_REMOTE_UPLOAD": "false",
                    "NIX_CONFIG": nix_config,
                }
            )
            before_bazel_output = github_output.read_text()
            run_guard("bazel", bazel_env)
            build_output = github_output.read_text()[len(before_bazel_output) :]
            build_home = Path(
                next(
                    line.removeprefix("bazelisk_home=")
                    for line in build_output.splitlines()
                    if line.startswith("bazelisk_home=")
                )
            )
            build_user_home = Path(
                next(
                    line.removeprefix("bazel_home=")
                    for line in build_output.splitlines()
                    if line.startswith("bazel_home=")
                )
            )
            self.assertTrue(build_home.is_dir())
            self.assertEqual(build_home.stat().st_mode & 0o777, 0o700)
            self.assertEqual(build_user_home.parent, build_home)
            self.assertEqual(list(build_user_home.iterdir()), [])

            test_bazel_env = dict(bazel_env)
            test_bazel_env["BULKLOAD_BAZEL_PHASE"] = "test"
            before_test_output = github_output.read_text()
            run_guard("bazel", test_bazel_env)
            test_output = github_output.read_text()[len(before_test_output) :]
            test_home = Path(
                next(
                    line.removeprefix("bazelisk_home=")
                    for line in test_output.splitlines()
                    if line.startswith("bazelisk_home=")
                )
            )
            self.assertNotEqual(build_home, test_home)
            self.assertNotIn("GRPC_PROXY_EXP", github_env.read_text())
            self.assertNotIn("GRPC_PROXY_EXP", github_output.read_text())

            absence_cases = (
                ("preflight", "preflight", base_env),
                ("enforce", "enforce", base_env),
                ("bazel-build", "bazel", bazel_env),
                ("bazel-test", "bazel", test_bazel_env),
            )
            for label, mode, clean_env in absence_cases:
                for value in ("", "dns:///private-proxy.invalid"):
                    unsafe_env = dict(clean_env)
                    unsafe_env["GRPC_PROXY_EXP"] = value
                    before_env = github_env.read_text()
                    before_output = github_output.read_text()
                    with self.subTest(mode=label, grpc_proxy=value or "present-empty"):
                        result = run_guard(mode, unsafe_env, check=False)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(github_env.read_text(), before_env)
                        self.assertEqual(github_output.read_text(), before_output)
                        if value:
                            self.assertNotIn(value, result.stdout)
                            self.assertNotIn(value, result.stderr)

            main_env = dict(base_env)
            main_env.update(
                {
                    "BULKLOAD_EVENT_NAME": "push",
                    "BULKLOAD_REF": "refs/heads/main",
                    "BULKLOAD_UPLOAD_BAZEL_RESULTS": "true",
                    "GITHUB_EVENT_NAME": "push",
                    "GITHUB_REF": "refs/heads/main",
                }
            )
            run_guard("preflight", main_env)
            run_guard("enforce", main_env)

            queue_ref = "refs/heads/gh-readonly-queue/main/pr-67-" + "a" * 40
            queue_env = dict(base_env)
            queue_env.update(
                {
                    "BULKLOAD_EVENT_NAME": "merge_group",
                    "BULKLOAD_REF": queue_ref,
                    "GITHUB_EVENT_NAME": "merge_group",
                    "GITHUB_REF": queue_ref,
                }
            )
            run_guard("preflight", queue_env)
            run_guard("enforce", queue_env)
            for key, value in (
                ("BULKLOAD_UPLOAD_BAZEL_RESULTS", "true"),
                ("BULKLOAD_HEAD_REPOSITORY", "fork/bulkload"),
                ("BULKLOAD_REF", "refs/heads/gh-readonly-queue/other/pr-1-x"),
                ("BULKLOAD_REF", "refs/heads/main"),
            ):
                unsafe_env = dict(queue_env)
                unsafe_env[key] = value
                if key == "BULKLOAD_REF":
                    unsafe_env["GITHUB_REF"] = value
                with self.subTest(merge_group=key, value=value):
                    self.assertNotEqual(
                        run_guard("enforce", unsafe_env, check=False).returncode, 0
                    )

            for key, value in (
                ("BULKLOAD_HEAD_REPOSITORY", "fork/bulkload"),
                ("BULKLOAD_UPLOAD_BAZEL_RESULTS", "true"),
                ("BULKLOAD_RUNNER_NAME", "other-pool-runner-12345"),
                ("BAZEL_REMOTE_EXECUTOR", "grpc://executor.invalid"),
                ("BAZEL_REMOTE_EXEC_HEADER", "x-auth:secret"),
                ("BAZEL_CREDENTIAL_HELPER", "/tmp/helper"),
                ("BAZEL_REMOTE_HEADER", "x-auth:secret"),
                ("BAZEL_REMOTE_CACHE_HEADER", "x-auth:secret"),
                ("NIX_ACCESS_TOKENS", "github.com=secret"),
                ("NIX_USER_CONF_FILES", "/tmp/nix.conf"),
                ("NETRC", "/tmp/netrc"),
                ("BAZELISK_GITHUB_TOKEN", "secret"),
                ("BAZELISK_BASE_URL", "https://binary.invalid"),
                ("BAZELISK_SKIP_WRAPPER", "false"),
                ("BAZELISK_HOME", "/tmp/predictable"),
                ("NIX_REMOTE", "daemon"),
                ("PATH", "/usr/bin:/bin"),
                ("HOME", "/tmp"),
                ("NIX_CACHE_HOME", "/tmp/cache"),
                ("BAZEL_SH", "/tmp/unaudited-sh"),
                ("BAZELISK_NOJDK", "true"),
                ("BAZELISK_CLEAN", "expunge"),
                ("BAZELISK_SHUTDOWN", "true"),
                ("USE_BAZEL_FALLBACK_VERSION", "7.0.0"),
                ("TEST_TMPDIR", "/tmp/reused"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("JUST_UNSTABLE", "1"),
                ("NIX_MIRRORS_TEST", "https://mirror.invalid"),
                ("SHELLCHECK_OPTS", "--exclude=all"),
            ):
                unsafe_env = dict(base_env)
                unsafe_env[key] = value
                before = github_env.read_text()
                with self.subTest(key=key):
                    result = run_guard("preflight", unsafe_env, check=False)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(github_env.read_text(), before)

            endpoint_variants = (
                ("ATTIC_SERVER", "https://token@cache.example.invalid"),
                ("ATTIC_SERVER", "https://cache.example.invalid/secret"),
                ("ATTIC_SERVER", "https://cache.example.invalid?token=secret"),
                (
                    "ATTIC_SERVER",
                    "https://cache.example.invalid\nBASH_ENV=/tmp/evil",
                ),
                ("BAZEL_REMOTE_CACHE", "grpc://cache.example.invalid/secret"),
                (
                    "BAZEL_REMOTE_CACHE",
                    "grpc://cache.example.invalid\nBAZEL_REMOTE_HEADER=secret",
                ),
            )
            for key, value in endpoint_variants:
                unsafe_env = dict(base_env)
                unsafe_env[key] = value
                before = github_env.read_text()
                with self.subTest(key=key, endpoint_case=value.splitlines()[0]):
                    result = run_guard("preflight", unsafe_env, check=False)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn(value, result.stderr)
                    self.assertEqual(github_env.read_text(), before)

            stale_bazel_variants = (
                ("BAZEL_REMOTE_CACHE", "https://other.invalid"),
                ("GF_BAZEL_REMOTE_UPLOAD", "true"),
                ("NIX_CONFIG", nix_config + "\npost-build-hook = /tmp/hook"),
                ("BULKLOAD_BAZEL_PHASE", "publish"),
                ("BAZELISK_GITHUB_TOKEN", "secret"),
            )
            for key, value in stale_bazel_variants:
                unsafe_env = dict(bazel_env)
                unsafe_env[key] = value
                with self.subTest(stale_bazel_key=key):
                    result = run_guard("bazel", unsafe_env, check=False)
                    self.assertNotEqual(result.returncode, 0)

            (workspace / ".bazeliskrc").write_text("BAZELISK_GITHUB_TOKEN=secret\n")
            result = run_guard("bazel", bazel_env, check=False)
            self.assertNotEqual(result.returncode, 0)
            (workspace / ".bazeliskrc").unlink()

            tools = workspace / "tools"
            tools.mkdir()
            wrapper = tools / "bazel"
            wrapper.write_text("#!/bin/sh\nexit 0\n")
            wrapper.chmod(0o755)
            result = run_guard("bazel", bazel_env, check=False)
            self.assertNotEqual(result.returncode, 0)

    def test_frontdoor_files_are_bazel_data_and_profile_is_endpoint_free(self) -> None:
        build = (self.root / "BUILD.bazel").read_text(encoding="utf-8")
        for path in (
            "BUILD.bazel",
            LOCAL_ACTION_PATH,
            GUARD_PATH,
            ".bazelrc",
            ".bazelrc.flywheel",
            ".bazelversion",
            "flake.nix",
            "flake.lock",
        ):
            self.assertIn(f'        "{path}",', build)
        self.assertNotRegex(self.bazelrc, r"(?:grpc|grpcs|http|https)://")
        self.assertIn(
            "try-import %workspace%/.bazelrc.flywheel", self.workspace_bazelrc
        )

    def test_bazelrc_authority_mutations_fail_closed(self) -> None:
        workspace_variants = (
            self.workspace_bazelrc.replace(f"{BOOTSTRAP_IMPL_LINE}\n", "", 1),
            self.workspace_bazelrc.replace(
                BOOTSTRAP_IMPL_LINE,
                BOOTSTRAP_IMPL_LINE.replace("script", "system_python"),
                1,
            ),
            self.workspace_bazelrc.replace(
                BOOTSTRAP_IMPL_LINE,
                BOOTSTRAP_IMPL_LINE.replace("common ", "build ", 1),
                1,
            ),
            self.workspace_bazelrc + f"\n{BOOTSTRAP_IMPL_LINE}\n",
            self.workspace_bazelrc + "\ncommon --@rules_python//python/config_settings:"
            "bootstrap_impl=system_python\n",
            self.workspace_bazelrc + "\ncommon --remote_executor=grpc://exec.invalid\n",
            self.workspace_bazelrc + "\ncommon --remote_header=x-auth=secret\n",
            self.workspace_bazelrc + "\ncommon --credential_helper=/tmp/helper\n",
            self.workspace_bazelrc + "\ncommon --config=flywheel-executor\n",
            self.workspace_bazelrc + "\ntry-import %workspace%/.bazelrc.private\n",
            self.workspace_bazelrc + "\ncommon --remote_cache=type=gha\n",
        )
        for workspace in workspace_variants:
            with self.assertRaises(ContractError):
                validate_bazelrc(workspace, self.bazelrc, exact_digest=False)

        flywheel_variants = (
            self.bazelrc + "\ncommon:flywheel --@rules_python//python/config_settings:"
            "bootstrap_impl=system_python\n",
            self.bazelrc + "\ncommon:flywheel --remote_cache=https://cache.invalid\n",
            self.bazelrc + "\ncommon:flywheel --remote_cache_header=x-auth=secret\n",
            self.bazelrc + "\ncommon:flywheel --credential_helper=/tmp/helper\n",
            self.bazelrc + "\ncommon:flywheel --remote_cache=type=gha\n",
        )
        for flywheel in flywheel_variants:
            with self.assertRaises(ContractError):
                validate_bazelrc(self.workspace_bazelrc, flywheel, exact_digest=False)

    def test_flake_private_source_mutations_fail_closed(self) -> None:
        flake_variants = (
            self.flake.replace(
                'inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";',
                'inputs.gf.url = "github:tinyland-inc/GloriousFlywheel/main";',
            ),
            self.flake.replace(
                'inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";',
                'inputs.nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs";',
            ),
        )
        for flake in flake_variants:
            with self.assertRaises(ContractError):
                validate_flake_topology(flake, self.flake_lock, exact_digest=False)

        lock = json.loads(self.flake_lock)
        lock["nodes"]["private"] = {
            "locked": {
                "type": "github",
                "owner": "tinyland-inc",
                "repo": "GloriousFlywheel",
                "rev": "a" * 40,
                "narHash": "sha256-" + "A" * 43 + "=",
            }
        }
        with self.assertRaises(ContractError):
            validate_flake_topology(self.flake, json.dumps(lock), exact_digest=False)

    def test_manifest_and_repo_owned_frontdoor_truth(self) -> None:
        manifest = json.loads((self.root / "tinyland.repo.json").read_text())
        self.assertEqual(
            manifest["enrollment"],
            {
                "forgeScope": "Jesssullivan",
                "operatorOverlay": "jesssullivan-infra",
                "executionPool": "tinyland-nix",
                "substrateMode": "shared-cache-backed",
            },
        )
        justfile = (self.root / "justfile").read_text(encoding="utf-8")
        self.assertIn('import? "justfile.flywheel"', justfile)
        self.assertIn("just flywheel-build //:bulkload", justfile)
        self.assertIn("just flywheel-test //:tests", justfile)
        self.assertIn("scripts/ci-public-read-guard.sh", justfile)
        validate_just_recipes(
            justfile, (self.root / "justfile.flywheel").read_text(encoding="utf-8")
        )

    def test_pinned_just_recipe_mutations_fail_closed(self) -> None:
        justfile = (self.root / "justfile").read_text(encoding="utf-8")
        harness_test = PINNED_JUST_RECIPES["fault-harness"][1][1]
        variants = [
            justfile.replace(
                "    cd {{ root }} && cargo test --workspace --locked\n", "", 1
            ),
            justfile.replace("    " + harness_test + "\n", "", 1),
            justfile.replace("ci-fault-harness: fault-harness", "ci-fault-harness:", 1),
            justfile.replace(
                "ci-source: check-source secrets-scan-history",
                "ci-source: secrets-scan-history",
                1,
            ),
            justfile.replace(
                "    cd {{ root }} && nix develop .#default --command just fault-harness\n",
                "",
                1,
            ),
            justfile.replace("secrets-scan-dir rust-check", "secrets-scan-dir", 1),
            justfile.replace(harness_test, harness_test + " || true", 1),
            justfile.replace(
                "BULKLOAD_IO_PARTIAL_WRITE_ALONE=1 cargo test", "cargo test", 1
            ),
            justfile.replace(
                "-- --ignored --exact --test-threads=1 --nocapture",
                "-- --ignored --test-threads=1 --nocapture",
                1,
            ),
            justfile.replace(
                "-- --ignored --exact --test-threads=1 --nocapture",
                "-- --ignored --exact --nocapture",
                1,
            ),
            justfile.replace(
                "    if [[ $output == *SKIPPED* ]]; then\n", "    if false; then\n", 1
            ),
            justfile.replace(
                "if [[ $results -ne 1 || $passed -ne 1 ]]; then",
                "if [[ $passed -lt 1 ]]; then",
                1,
            ),
            justfile.replace(
                "    cd {{ root }} && {{ just_executable() }} io-partial-write-alone\n",
                "",
                1,
            ),
            justfile.replace(
                "    if [[ $proved -ne 1 ]]; then\n", "    if false; then\n", 1
            ),
            justfile.replace(
                "partial_write_prefix_is_traced \\.\\.\\. ok$'",
                "[a-z_:]* \\.\\.\\. ok$'",
                1,
            ),
            # The review's seven escapes that rewire pinned recipes without
            # touching their bodies (R-N122, #71 review A1).
            "set allow-duplicate-recipes := true\n"
            + justfile
            + "\nio-partial-write-alone *args:\n    @echo 'test result: ok. 1 passed; 0 failed;'\n",
            "set allow-duplicate-recipes := true\n"
            + justfile
            + "\nrust-check *args:\n    true\n",
            "set allow-duplicate-recipes := true\n"
            + justfile
            + "\nfault-harness *args:\n    true\n",
            "set allow-duplicate-recipes := true\n"
            + justfile
            + "\n[private]\nio-partial-write-alone *args:\n    true\n",
            justfile.replace(
                "root := justfile_directory()\n",
                "root := justfile_directory() / 'decoy'\n",
                1,
            ),
            justfile.replace(
                'export PYTHONDONTWRITEBYTECODE := "1"\n',
                'export PYTHONDONTWRITEBYTECODE := "1"\n'
                'export PATH := justfile_directory() / "stub" + ":" + env("PATH")\n',
                1,
            ),
            justfile.replace(
                'export PYTHONDONTWRITEBYTECODE := "1"\n',
                'export PYTHONDONTWRITEBYTECODE := "1"\n'
                'export RUSTFLAGS := "--cfg skip_p5"\n',
                1,
            ),
        ]
        for index, unsafe in enumerate(variants):
            with self.subTest(index=index):
                self.assertNotEqual(unsafe, justfile)
                with self.assertRaises(ContractError):
                    validate_just_recipes(unsafe)
        imported = (self.root / "justfile.flywheel").read_text(encoding="utf-8")
        validate_just_recipes(justfile, imported)
        for index, unsafe_import in enumerate(
            [
                'export PATH := "/tmp/stub:" + env("PATH")\n' + imported,
                "set allow-duplicate-recipes := true\n" + imported,
                imported + "\nrust-check:\n    true\n",
                imported + "\n@fault-harness:\n    true\n",
                'import? "elsewhere.just"\n' + imported,
            ]
        ):
            with self.subTest(imported=index):
                with self.assertRaises(ContractError):
                    validate_just_recipes(justfile, unsafe_import)

    def test_actionlint_knows_only_the_sanctioned_custom_label(self) -> None:
        config = (self.root / ".github/actionlint.yaml").read_text(encoding="utf-8")
        self.assertEqual(config, "self-hosted-runner:\n  labels:\n    - tinyland-nix\n")


if __name__ == "__main__":
    unittest.main()
