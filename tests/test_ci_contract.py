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
CHECKOUT_REV = "3d3c42e5aac5ba805825da76410c181273ba90b1"
LOCAL_ACTION = "./.github/actions/bulkload-public-read-ci"
LOCAL_ACTION_PATH = ".github/actions/bulkload-public-read-ci/action.yml"
GUARD_PATH = "scripts/ci-public-read-guard.sh"
PUBLIC_KEY = "main:eaUydxuDu7xBoy5cCo3MdknYAkVyTIASQ7DGuwxa+XA="
ACTION_SHA256 = "2883a1296e2195a4a9485325d3974dc601feddb0df99a2f016fdff0c29a7e068"
GUARD_SHA256 = "118eb631fb54617111df54980502bd1fbbf4f26ac98d98b0ba22afabe30d2033"
BAZELRC_SHA256 = "f5a7f5116ce0a69471e71b44666fc868e361ed540a40c28a4ee8adc344c87592"
WORKSPACE_BAZELRC_SHA256 = (
    "15aa8306cc530bbc4d143dd7a6a2f0cfd3efbed19c01503d35bbec0c5e7cd357"
)
BAZEL_VERSION_SHA256 = (
    "4fa9948d0ae7007cbd1cc05768bc3e7cc6ec46ad0ea84c87df79e7a0c48d76b4"
)
FLAKE_SHA256 = "971a3040c2c6b44392c793b43d1e82f7d501070e76bb5ed593b6cc72bae39489"
FLAKE_LOCK_SHA256 = "ccd790af791b173623983382a78bd9476760b9fa9e9e617108e2ae3d1040d19d"
EXPECTED_SHA_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.sha || github.sha }}"
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


def validate_workflow(workflow: str) -> None:
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
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", workflow):
        raise ContractError("workflow must not bake a deployment endpoint")
    if re.search(r"type\s*=\s*gha", workflow, re.IGNORECASE):
        raise ContractError("GitHub Actions cache authority is forbidden")

    expected_uses = [f"actions/checkout@{CHECKOUT_REV}", LOCAL_ACTION]
    if extract_uses(workflow) != expected_uses:
        raise ContractError("workflow action inventory drifted")
    if workflow.count(f"ref: {EXPECTED_SHA_EXPRESSION}") != 1:
        raise ContractError("checkout must select the exact PR head or push SHA")
    if workflow.count("persist-credentials: false") != 1:
        raise ContractError("checkout credentials must not persist")
    if workflow.count(f"EXPECTED_SHA: {EXPECTED_SHA_EXPRESSION}") != 1:
        raise ContractError("checkout verification must bind the event SHA")
    if 'run: test "$(git rev-parse HEAD)" = "$EXPECTED_SHA"' not in workflow:
        raise ContractError("CI must verify the checked-out commit")
    if workflow.count('      ATTIC_TOKEN: ""') != 1:
        raise ContractError("workflow must empty ATTIC_TOKEN exactly once")
    if workflow.count('      NIX_ACCESS_TOKENS: ""') != 1:
        raise ContractError("workflow must empty NIX_ACCESS_TOKENS exactly once")

    required_inputs = (
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
    if parse_action_inputs(action) != {
        "event-name",
        "expected-sha",
        "head-repository",
        "ref",
        "repository",
        "upload-bazel-results",
    }:
        raise ContractError("local public-read action input inventory drifted")

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
    ):
        if forbidden in action:
            raise ContractError(
                f"local action contains forbidden authority: {forbidden}"
            )
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", action):
        raise ContractError("local action must not bake a deployment endpoint")
    if re.search(r"type\s*=\s*gha", action, re.IGNORECASE):
        raise ContractError("GitHub Actions cache authority is forbidden")

    required = (
        "        attic-cache: main",
        "      id: guard-snapshot",
        "      id: authority",
        "      id: bazel-build-authority",
        "      id: bazel-test-authority",
        '        /bin/bash "$BULKLOAD_GUARD_PATH" preflight',
        '        /bin/bash "$BULKLOAD_GUARD_PATH" enforce',
        '        test "$(nix config show accept-flake-config)" = false',
        '        test "$(nix config show netrc-file)" = /dev/null',
        '        test -z "$(nix config show access-tokens)"',
        '        test -z "$(nix config show post-build-hook)"',
        '        test -z "$(nix config show secret-key-files)"',
        '        test -z "$(nix config show plugin-files)"',
        "        just flake-check",
        "        nix develop --no-write-lock-file .#default --command just ci-source",
        "        command: build",
        "        targets: //:bulkload",
        "        command: test",
        "        targets: //:tests",
    )
    for declaration in required:
        if action.count(declaration) != 1:
            raise ContractError(f"local action contract drifted: {declaration.strip()}")

    def require_yaml_lines(key: str, expected_values: list[str]) -> None:
        actual = [
            line.strip().removeprefix(f"{key}:").strip()
            for line in action.splitlines()
            if line.strip().startswith(f"{key}:")
        ]
        if actual != expected_values:
            raise ContractError(f"local action {key} environment inventory drifted")

    require_yaml_lines("BASH_ENV", ["/dev/null"] * 8)
    require_yaml_lines("ENV", ["/dev/null"] * 8)
    require_yaml_lines(
        "BULKLOAD_GUARD_PATH",
        ["${{ steps.guard-snapshot.outputs.guard_path }}"] * 4,
    )

    if action.count("      shell: /bin/bash --noprofile --norc {0}") != 5:
        raise ContractError("every guard boundary must use absolute non-profile Bash")
    require_yaml_lines("PATH", ["${{ steps.authority.outputs.trusted_path }}"] * 5)
    require_yaml_lines(
        "HOME",
        [
            "${{ steps.bazel-build-authority.outputs.bazel_home }}",
            "${{ steps.bazel-test-authority.outputs.bazel_home }}",
        ],
    )
    for key in ("ATTIC_TOKEN", "NIX_ACCESS_TOKENS"):
        require_yaml_lines(key, ['""'] * 7)
    require_yaml_lines("NIX_USER_CONF_FILES", ["/dev/null"] * 7)
    require_yaml_lines("NETRC", ["/dev/null"] * 7)
    require_yaml_lines("NIX_CONFIG", ["${{ steps.authority.outputs.nix_config }}"] * 5)
    require_yaml_lines(
        "BAZEL_REMOTE_CACHE",
        ["${{ steps.authority.outputs.bazel_remote_cache }}"] * 4,
    )
    for key in (
        "BAZEL_REMOTE_EXECUTOR",
        "BAZEL_REMOTE_EXEC_HEADER",
        "BAZEL_CREDENTIAL_HELPER",
        "BAZEL_REMOTE_HEADER",
        "BAZEL_REMOTE_CACHE_HEADER",
    ):
        require_yaml_lines(key, ['""'] * 4)
    require_yaml_lines(
        "GF_BAZEL_REMOTE_UPLOAD",
        ["${{ steps.authority.outputs.bazel_remote_upload }}"] * 4,
    )
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
        require_yaml_lines(key, ['""'] * 6)
    require_yaml_lines("BAZELISK_SKIP_WRAPPER", ['"true"'] * 6)
    require_yaml_lines(
        "BAZELISK_HOME",
        [
            '""',
            '""',
            '""',
            "${{ steps.bazel-build-authority.outputs.bazelisk_home }}",
            '""',
            "${{ steps.bazel-test-authority.outputs.bazelisk_home }}",
        ],
    )

    if action.count(GUARD_SHA256) != 5:
        raise ContractError("snapshot guard digest inventory drifted")
    if (
        action.count(
            'test "$(/usr/bin/sha256sum "$BULKLOAD_GUARD_PATH" | /usr/bin/awk \'{print $1}\')" = '
            + GUARD_SHA256
        )
        != 4
    ):
        raise ContractError("every guard execution must have an external digest check")
    if action.count('        /bin/bash "$BULKLOAD_GUARD_PATH" bazel') != 2:
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
    preflight = action.index('/bin/bash "$BULKLOAD_GUARD_PATH" preflight')
    setup = action.index(f"uses: {nix_setup}")
    enforce = action.index('/bin/bash "$BULKLOAD_GUARD_PATH" enforce')
    source = action.index("just flake-check")
    bazel_guards = [
        match.start()
        for match in re.finditer(
            re.escape('/bin/bash "$BULKLOAD_GUARD_PATH" bazel'), action
        )
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
        < source
        < bazel_guards[0]
        < bazel_actions[0]
        < bazel_guards[1]
        < bazel_actions[1]
    ):
        raise ContractError(
            "guard snapshot, discovery, source, and per-Bazel boundaries are misordered"
        )
    if action.count("        config: flywheel") != 2:
        raise ContractError("both Bazel invocations must be cache-only Flywheel calls")


def validate_guard(guard: str, *, exact_digest: bool = True) -> None:
    if exact_digest and sha256(guard) != GUARD_SHA256:
        raise ContractError("public-read guard digest drifted")
    if re.search(r"(?:https?|grpcs?)://[A-Za-z0-9]", guard):
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
        "readonly public_site=bulkload-ci",
        "readonly cache_name=main",
        'require_equal "runner environment" "${BULKLOAD_RUNNER_ENVIRONMENT:-}" self-hosted',
        "*,tinyland-nix,*)",
        'require_equal "event" "${BULKLOAD_EVENT_NAME:-}" "${GITHUB_EVENT_NAME:-}"',
        'require_equal "ref" "${BULKLOAD_REF:-}" "${GITHUB_REF:-}"',
        '"pull-request head repository"',
        '"$(git -C "${GITHUB_WORKSPACE:?}" rev-parse HEAD)"',
        'if [[ "$BULKLOAD_EVENT_NAME" == push && "$BULKLOAD_REF" == refs/heads/main ]]; then',
        'case "$mode" in',
        "preflight | enforce | bazel) ;;",
        'require_empty "Attic token" "${ATTIC_TOKEN:-}"',
        'require_empty "Nix access tokens" "${NIX_ACCESS_TOKENS:-}"',
        'require_equal "Nix user configuration" "${NIX_USER_CONF_FILES:-}" /dev/null',
        'require_equal "netrc environment" "${NETRC:-}" /dev/null',
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
        'require_equal "Bazelisk wrapper skip" "${BAZELISK_SKIP_WRAPPER:-}" true',
        'require_empty "Bazelisk home" "${BAZELISK_HOME:-}"',
        'if [[ "$mode" == preflight ]]; then',
        "printf 'BASH_ENV=/dev/null\\n'",
        "printf 'ENV=/dev/null\\n'",
        'require_equal "GitHub environment directory" "$env_dir" "$runner_temp/_runner_file_commands"',
        "set_env_*) ;;",
        "set_output_*) ;;",
        'if [[ "$mode" == bazel ]]; then',
        '"captured Bazel endpoint"',
        '"active Bazel upload gate"',
        '"captured Nix client configuration"',
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
        "extra-substituters = ${ATTIC_SERVER%/}/${cache_name}",
        "extra-trusted-public-keys = ${public_key}",
        "access-tokens =",
        "netrc-file = /dev/null",
        "accept-flake-config = false",
        "post-build-hook =",
        "secret-key-files =",
        "plugin-files =",
        "printf 'NIX_ACCESS_TOKENS=\\n'",
        "printf 'NIX_USER_CONF_FILES=/dev/null\\n'",
        "printf 'NETRC=/dev/null\\n'",
        "printf 'BAZEL_CREDENTIAL_HELPER=\\n'",
        "printf 'BAZEL_REMOTE_CACHE_HEADER=\\n'",
        "printf 'BAZEL_REMOTE_EXECUTOR=\\n'",
        "printf 'BAZEL_REMOTE_EXEC_HEADER=\\n'",
        "printf 'BAZEL_REMOTE_HEADER=\\n'",
        "printf 'BAZELISK_GITHUB_TOKEN=\\n'",
        "printf 'BAZELISK_HOME=\\n'",
        "printf 'BAZELISK_SKIP_WRAPPER=true\\n'",
        "printf 'BAZELISK_VERIFY_SHA256=\\n'",
        "printf 'trusted_path=%s\\n' \"$PATH\"",
        "printf 'nix_config<<BULKLOAD_NIX_OUTPUT_%s\\n' \"$ci_templates_rev\"",
    )
    for declaration in required:
        if declaration not in guard:
            raise ContractError(f"guard contract drifted: {declaration}")

    nix_config_match = re.search(r'readonly nix_config="([^\"]*)"', guard)
    expected_nix_config = "\n".join(
        (
            "extra-substituters = ${ATTIC_SERVER%/}/${cache_name}",
            "extra-trusted-public-keys = ${public_key}",
            "access-tokens =",
            "netrc-file = /dev/null",
            "accept-flake-config = false",
            "post-build-hook =",
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
        for unsafe in unsafe_variants:
            with self.subTest(unsafe=unsafe[-120:]):
                with self.assertRaises(ContractError):
                    validate_workflow(unsafe)

    def test_checkout_permission_and_upload_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace(
                f"actions/checkout@{CHECKOUT_REV}", "actions/checkout@main"
            ),
            self.workflow.replace("contents: read", "contents: write"),
            self.workflow.replace(
                "persist-credentials: false", "persist-credentials: true"
            ),
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
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_workflow(unsafe)

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
        unsafe_variants = [
            self.action.replace("@" + CI_TEMPLATES_REV, "@v2.13.0", 1),
            self.action.replace(
                "tinyland-inc/ci-templates", "tinyland-inc/GloriousFlywheel", 1
            ),
            self.action.replace("config: flywheel", "config: flywheel-executor", 1),
            self.action.replace("targets: //:tests", "targets: //..."),
            self.action.replace("        just flake-check\n", ""),
            self.action.replace(
                '/bin/bash "$BULKLOAD_GUARD_PATH" preflight',
                '/bin/bash "$GITHUB_WORKSPACE/scripts/ci-public-read-guard.sh" preflight',
                1,
            ),
            self.action.replace(
                '        /bin/bash "$BULKLOAD_GUARD_PATH" bazel\n', "", 1
            ),
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
            setup_before_preflight,
            self.action.replace(
                'accept-flake-config)" = false', 'accept-flake-config)" = true'
            ),
            self.action.replace(
                '        test -z "$(nix config show post-build-hook)"\n', ""
            ),
            self.action.replace(
                '        ATTIC_TOKEN: ""',
                '        ATTIC_TOKEN: ""\n        BAZEL_REMOTE_HEADER: secret',
            ),
            self.action + "\n# type=gha\n",
            self.action + "\n# https://cache.invalid\n",
        ]
        for unsafe in unsafe_variants:
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
        ]
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_guard(unsafe, exact_digest=False)

    def test_guard_executes_pr_and_main_upload_policy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            mock_bin = temporary_path / "mock-bin"
            mock_bin.mkdir()
            git = mock_bin / "git"
            git.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$MOCK_HEAD"\n')
            git.chmod(0o755)
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
            command_files = temporary_path / "_runner_file_commands"
            command_files.mkdir()
            github_env = command_files / "set_env_bulkload"
            github_env.touch()
            github_output = command_files / "set_output_bulkload"
            github_output.touch()
            head = "a" * 40
            nix_config = "\n".join(
                (
                    "extra-substituters = https://cache.example.invalid/main",
                    f"extra-trusted-public-keys = {PUBLIC_KEY}",
                    "access-tokens =",
                    "netrc-file = /dev/null",
                    "accept-flake-config = false",
                    "post-build-hook =",
                    "secret-key-files =",
                    "plugin-files =",
                )
            )
            base_env = {
                "PATH": f"{mock_bin}:/usr/bin:/bin:/sbin",
                "MOCK_HEAD": head,
                "GITHUB_ENV": str(github_env),
                "GITHUB_OUTPUT": str(github_output),
                "RUNNER_TEMP": temporary,
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_REF": "refs/pull/9/merge",
                "GITHUB_WORKSPACE": str(workspace),
                "GITHUB_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_EVENT_NAME": "pull_request",
                "BULKLOAD_EXPECTED_SHA": head,
                "BULKLOAD_REF": "refs/pull/9/merge",
                "BULKLOAD_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_HEAD_REPOSITORY": "Jesssullivan/bulkload",
                "BULKLOAD_UPLOAD_BAZEL_RESULTS": "false",
                "BULKLOAD_RUNNER_ENVIRONMENT": "self-hosted",
                "BULKLOAD_RUNNER_LABELS": "self-hosted,Linux,X64,tinyland-nix",
                "BULKLOAD_ATTIC_REACHABLE": "true",
                "BULKLOAD_BAZEL_CACHE_REACHABLE": "true",
                "ATTIC_TOKEN": "",
                "NIX_ACCESS_TOKENS": "",
                "NIX_USER_CONF_FILES": "/dev/null",
                "NETRC": "/dev/null",
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
            }

            def run_guard(
                mode: str, env: dict[str, str], *, check: bool = True
            ) -> subprocess.CompletedProcess[str]:
                return subprocess.run(
                    ["bash", str(self.root / GUARD_PATH), mode],
                    check=check,
                    env=env,
                    capture_output=True,
                    text=True,
                )

            run_guard("preflight", base_env)
            preflight_emitted = github_env.read_text()
            self.assertIn("BASH_ENV=/dev/null\n", preflight_emitted)
            self.assertIn("ENV=/dev/null\n", preflight_emitted)
            self.assertIn("ATTIC_TOKEN=\n", preflight_emitted)
            self.assertIn("NIX_ACCESS_TOKENS=\n", preflight_emitted)
            self.assertIn("BAZEL_CREDENTIAL_HELPER=\n", preflight_emitted)

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

            for key, value in (
                ("BULKLOAD_HEAD_REPOSITORY", "fork/bulkload"),
                ("BULKLOAD_UPLOAD_BAZEL_RESULTS", "true"),
                ("BULKLOAD_RUNNER_LABELS", "self-hosted,Linux,X64"),
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

    def test_actionlint_knows_only_the_sanctioned_custom_label(self) -> None:
        config = (self.root / ".github/actionlint.yaml").read_text(encoding="utf-8")
        self.assertEqual(config, "self-hosted-runner:\n  labels:\n    - tinyland-nix\n")


if __name__ == "__main__":
    unittest.main()
