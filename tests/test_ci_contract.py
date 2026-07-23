from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys
import unittest

sys.dont_write_bytecode = True

GF_REV = "eb50ca7da6cce315867de963bef2184cfd924b26"
CHECKOUT_REV = "3d3c42e5aac5ba805825da76410c181273ba90b1"
EXPECTED_SHA_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.sha || github.sha }}"
)
AUDITED_JOB_RUNNERS = {"test": "tinyland-nix"}
HOSTED_RUNNER_PATTERN = re.compile(
    r"(?:ubuntu|macos|windows)-(?:latest|[0-9][A-Za-z0-9.-]*)", re.IGNORECASE
)
PERMISSIONS_DECLARATION_PATTERN = re.compile(
    r"""^(?:    )?(?:permissions|["']permissions["']):(?:\s*.*)?$"""
)


class ContractError(ValueError):
    pass


def parse_job_contract(workflow: str) -> dict[str, dict[str, list[str]]]:
    """Parse the closed-world subset of workflow YAML used for job routing."""
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
        unexpected = sorted(set(jobs) - set(AUDITED_JOB_RUNNERS))
        missing = sorted(set(AUDITED_JOB_RUNNERS) - set(jobs))
        raise ContractError(
            f"workflow job inventory is not audited (unexpected={unexpected}, missing={missing})"
        )

    for job, expected_runner in AUDITED_JOB_RUNNERS.items():
        properties = jobs[job]
        if properties.get("uses"):
            raise ContractError(f"job-level reusable workflow is forbidden: {job}")
        if properties.get("runs-on") != [expected_runner]:
            raise ContractError(
                f"job {job} must declare exactly runs-on: {expected_runner}"
            )

    declarations = [
        line
        for line in workflow.splitlines()
        if "runs-on:" in line and not line.lstrip().startswith("#")
    ]
    expected_declarations = [
        f"    runs-on: {runner}" for runner in AUDITED_JOB_RUNNERS.values()
    ]
    if declarations != expected_declarations:
        raise ContractError("every runs-on declaration must be an audited exact scalar")


def validate_permissions(workflow: str) -> None:
    lines = workflow.splitlines()
    declarations = [
        (index, line)
        for index, line in enumerate(lines)
        if PERMISSIONS_DECLARATION_PATTERN.fullmatch(line) is not None
    ]
    if len(declarations) != 1 or declarations[0][1] != "permissions:":
        raise ContractError(
            "cache-only CI must declare one top-level permissions block only"
        )

    start = declarations[0][0]
    block: list[str] = []
    for line in lines[start + 1 :]:
        if line and not line.startswith((" ", "\t", "#")):
            break
        if line.strip() and not line.lstrip().startswith("#"):
            block.append(line)
    if block != ["  contents: read"]:
        raise ContractError("cache-only CI permissions must be exactly contents: read")


def parse_action_refs(workflow: str) -> list[str]:
    """Parse the closed-world block-style step subset and return action refs."""
    lines = workflow.splitlines()
    step_blocks = [index for index, line in enumerate(lines) if line == "    steps:"]
    if len(step_blocks) != len(AUDITED_JOB_RUNNERS):
        raise ContractError("every audited job must declare one block-style steps list")

    action_refs: list[str] = []
    for start in step_blocks:
        current_properties: set[str] | None = None
        for line in lines[start + 1 :]:
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            if "\t" in line:
                raise ContractError("workflow steps must not use tab indentation")
            indentation = len(line) - len(line.lstrip(" "))
            if indentation <= 4:
                break
            if indentation == 6:
                if re.fullmatch(r"      - name: \S.*", line) is None:
                    raise ContractError(
                        "workflow steps must begin with a canonical named block mapping"
                    )
                current_properties = {"name"}
                continue
            if indentation == 8:
                if current_properties is None:
                    raise ContractError("workflow step property precedes a named step")
                property_match = re.fullmatch(
                    r"        ([A-Za-z_][A-Za-z0-9_-]*):(?:\s*(.*))?", line
                )
                if property_match is None:
                    raise ContractError(
                        "workflow step properties must use literal block mapping keys"
                    )
                key = property_match.group(1)
                value = property_match.group(2) or ""
                if key not in {"env", "run", "uses", "with"}:
                    raise ContractError(f"unaudited workflow step property: {key}")
                if key in current_properties:
                    raise ContractError(f"duplicate workflow step property: {key}")
                current_properties.add(key)
                if key == "uses":
                    action_ref = value.split(" #", 1)[0].strip()
                    if not action_ref:
                        raise ContractError("action reference must be an exact scalar")
                    action_refs.append(action_ref)
                continue
            if indentation < 10 or current_properties is None:
                raise ContractError("workflow steps use unaudited indentation")
    return action_refs


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


def validate_workflow(workflow: str) -> None:
    validate_job_routing(workflow)
    validate_permissions(workflow)
    if HOSTED_RUNNER_PATTERN.search(workflow):
        raise ContractError("GitHub-hosted runner label is forbidden")
    if re.search(r"(?m)^\s*runs-on:\s*\$\{\{", workflow):
        raise ContractError("dynamic runner selection is forbidden")
    if re.search(r"(?m)^\s*runs-on:\s*\[", workflow):
        raise ContractError("runner label arrays are forbidden")
    if "self-hosted" in workflow:
        raise ContractError("bare self-hosted routing is forbidden")
    if (
        "bazel-contrib/setup-bazel" in workflow
        or "cachix/install-nix-action" in workflow
    ):
        raise ContractError("hosted bootstrap/cache actions are forbidden")
    if "id-token:" in workflow:
        raise ContractError("cache-only CI must not request an OIDC token")
    action_refs = parse_action_refs(workflow)
    for action_ref in action_refs:
        if not re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", action_ref):
            raise ContractError("every external action must use an immutable SHA")
    checkout = f"actions/checkout@{CHECKOUT_REV}"
    if workflow.count(checkout) != 1:
        raise ContractError("CI must use exactly one pinned checkout step")
    if workflow.count(f"ref: {EXPECTED_SHA_EXPRESSION}") != 1:
        raise ContractError("checkout must select the exact PR head or push SHA")
    if workflow.count("persist-credentials: false") != 1:
        raise ContractError("checkout credentials must not persist")
    if workflow.count(f"EXPECTED_SHA: {EXPECTED_SHA_EXPRESSION}") != 1:
        raise ContractError("checkout verification must bind the same event SHA")
    if 'run: test "$(git rev-parse HEAD)" = "$EXPECTED_SHA"' not in workflow:
        raise ContractError("CI must verify the checked-out commit before execution")
    action = "tinyland-inc/GloriousFlywheel/.github/actions/nix-job@" + GF_REV
    flake = "github:tinyland-inc/GloriousFlywheel/" + GF_REV + "#ci"
    if action not in workflow or flake not in workflow:
        raise ContractError("GloriousFlywheel action and devshell must share one pin")
    if re.search(r"(?m)^\s+BAZEL_BIN:\s*bazel\s*$", workflow) is None:
        raise ContractError("CI must select the Bazel shim exposed by GF #ci")
    if 'push-cache: "false"' not in workflow:
        raise ContractError("Attic publication must remain disabled")
    upload_gate = (
        "GF_BAZEL_REMOTE_UPLOAD: "
        "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' "
        "&& 'true' || 'false' }}"
    )
    if upload_gate not in workflow:
        raise ContractError("Bazel uploads must be restricted to trusted main pushes")
    if 'test -n "${BAZEL_REMOTE_CACHE:-}"' not in workflow:
        raise ContractError("CI must fail closed when the Bazel cache is absent")
    if 'test "${GF_BAZEL_SUBSTRATE_MODE:-}" = shared-cache-backed' not in workflow:
        raise ContractError("CI must require the declared cache-only substrate")
    if "just ci" not in workflow:
        raise ContractError("CI must use the repo-owned Flywheel gate")


class CiContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = find_workspace()
        cls.workflow = (cls.root / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )

    def test_workflow_is_flywheel_only(self) -> None:
        validate_workflow(self.workflow)

    def test_hosted_and_dynamic_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace("runs-on: tinyland-nix", "runs-on: ubuntu-latest"),
            self.workflow.replace(
                "runs-on: tinyland-nix", "runs-on: ${{ vars.RUNNER || 'tinyland-nix' }}"
            ),
            self.workflow.replace(
                "runs-on: tinyland-nix", "runs-on: [self-hosted, tinyland-nix]"
            ),
        ]
        for unsafe in unsafe_variants:
            with self.subTest(unsafe=unsafe.split("runs-on:", 1)[1].splitlines()[0]):
                with self.assertRaises(ContractError):
                    validate_workflow(unsafe)

    def test_unaudited_jobs_and_reusable_workflows_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow
            + "\n  unrelated:\n    runs-on: tinyland-nix\n    steps: []\n",
            self.workflow.replace(
                "    runs-on: tinyland-nix",
                "    uses: example/workflows/.github/workflows/test.yml@" + "0" * 40,
            ),
            self.workflow.replace(
                "    runs-on: tinyland-nix", '    runs-on: "tinyland-nix"'
            ),
        ]
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_workflow(unsafe)

    def test_hosted_bootstrap_regression_fails_closed(self) -> None:
        unsafe = self.workflow + "\n# uses: bazel-contrib/setup-bazel@deadbeef\n"
        with self.assertRaises(ContractError):
            validate_workflow(unsafe)

    def test_mutable_action_and_broad_write_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace(
                "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "actions/checkout@main",
            ),
            self.workflow.replace("contents: read", "contents: write"),
            self.workflow.replace(
                "    runs-on: tinyland-nix",
                "    runs-on: tinyland-nix\n    permissions: write-all",
            ),
            self.workflow.replace(
                "    runs-on: tinyland-nix",
                "    runs-on: tinyland-nix\n    permissions: {contents: write}",
            ),
            self.workflow.replace(
                "    runs-on: tinyland-nix",
                '    runs-on: tinyland-nix\n    "permissions": write-all',
            ),
            self.workflow.replace(
                "      - name: Verify exact checked out revision",
                "      - uses: example/action@main\n"
                "      - name: Verify exact checked out revision",
            ),
            self.workflow.replace(
                "      - name: Verify exact checked out revision",
                "      - uses : example/action@main\n"
                "      - name: Verify exact checked out revision",
            ),
            self.workflow.replace(
                "      - name: Verify exact checked out revision",
                "      - 'uses' : example/action@main\n"
                "      - name: Verify exact checked out revision",
            ),
            self.workflow.replace(
                "      - name: Verify exact checked out revision",
                "      - {uses: example/action@main}\n"
                "      - name: Verify exact checked out revision",
            ),
            self.workflow.replace(
                "${{ github.event_name == 'push' && github.ref == "
                "'refs/heads/main' && 'true' || 'false' }}",
                "true",
            ),
            self.workflow.replace("BAZEL_BIN: bazel", "BAZEL_BIN: bazelisk"),
        ]
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_workflow(unsafe)

    def test_checkout_identity_and_credentials_regressions_fail_closed(self) -> None:
        unsafe_variants = [
            self.workflow.replace(
                f"ref: {EXPECTED_SHA_EXPRESSION}", "ref: ${{ github.sha }}"
            ),
            self.workflow.replace(
                "persist-credentials: false", "persist-credentials: true"
            ),
            self.workflow.replace(
                'run: test "$(git rev-parse HEAD)" = "$EXPECTED_SHA"',
                "run: git rev-parse HEAD",
            ),
        ]
        for unsafe in unsafe_variants:
            with self.assertRaises(ContractError):
                validate_workflow(unsafe)

    def test_frontdoor_kit_is_imported_and_endpoint_free(self) -> None:
        bazelrc = (self.root / ".bazelrc").read_text(encoding="utf-8")
        flywheel_bazelrc = (self.root / ".bazelrc.flywheel").read_text(encoding="utf-8")
        justfile = (self.root / "justfile").read_text(encoding="utf-8")
        self.assertIn("try-import %workspace%/.bazelrc.flywheel", bazelrc)
        self.assertIn('import? "justfile.flywheel"', justfile)
        self.assertNotRegex(flywheel_bazelrc, r"(?:grpc|grpcs|http|https)://")
        self.assertNotIn("ubuntu-latest", flywheel_bazelrc)
        self.assertIn("just flywheel-build //:bulkload", justfile)
        self.assertIn("just flywheel-test //:tests", justfile)
        self.assertIn("just flywheel-verify", justfile)
        self.assertIn("nix develop .#default --command just ci-source", justfile)

    def test_manifest_declares_cache_first_owner_overlay(self) -> None:
        manifest = json.loads(
            (self.root / "tinyland.repo.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            manifest["enrollment"],
            {
                "forgeScope": "Jesssullivan",
                "operatorOverlay": "jesssullivan-infra",
                "executionPool": "tinyland-nix",
                "substrateMode": "shared-cache-backed",
            },
        )

    def test_actionlint_knows_only_the_sanctioned_custom_label(self) -> None:
        config = (self.root / ".github/actionlint.yaml").read_text(encoding="utf-8")
        self.assertEqual(
            config,
            "self-hosted-runner:\n  labels:\n    - tinyland-nix\n",
        )


if __name__ == "__main__":
    unittest.main()
