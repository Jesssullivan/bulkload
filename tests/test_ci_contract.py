from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys
import unittest

sys.dont_write_bytecode = True

GF_REV = "ba391f344d71bff4ee902ed8d9928b98546d5f06"
HOSTED_RUNNER_PATTERN = re.compile(
    r"(?:ubuntu|macos|windows)-(?:latest|[0-9][A-Za-z0-9.-]*)", re.IGNORECASE
)


class ContractError(ValueError):
    pass


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
    if "runs-on: tinyland-nix" not in workflow:
        raise ContractError("CI must use the tinyland-nix capability class")
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
    if "permissions:\n  contents: read" not in workflow or "id-token:" in workflow:
        raise ContractError("cache-only CI must have contents-read permissions only")
    if re.search(r"(?m)^\s+[A-Za-z-]+:\s*write\s*$", workflow):
        raise ContractError("cache-only CI must not grant write permissions")
    for action_ref in re.findall(r"(?m)^\s*uses:\s*([^\s#]+)", workflow):
        if not re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", action_ref):
            raise ContractError("every external action must use an immutable SHA")
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
                "${{ github.event_name == 'push' && github.ref == "
                "'refs/heads/main' && 'true' || 'false' }}",
                "true",
            ),
            self.workflow.replace("BAZEL_BIN: bazel", "BAZEL_BIN: bazelisk"),
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
