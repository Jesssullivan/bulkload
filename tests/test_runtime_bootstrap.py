from __future__ import annotations

import ast
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).parents[1]
LAUNCHER = ROOT / ".agents/skills/bulkload/scripts/bulkload.py"


class RuntimeBootstrapTests(unittest.TestCase):
    def test_direct_launcher_is_isolated_and_exposes_only_product_commands(
        self,
    ) -> None:
        result = subprocess.run(
            [str(LAUNCHER), "--help"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for command in (
            "agent-capture",
            "agent-plan",
            "agent-stage",
            "agent-apply",
            "agent-verify",
            "agent-rollback",
            "agent-recover",
        ):
            self.assertIn(command, result.stdout)
        self.assertNotIn("codex-private", result.stdout)

    def test_nonisolated_python_invocation_fails_before_cli(self) -> None:
        result = subprocess.run(
            [sys.executable, str(LAUNCHER), "--version"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires Python -I -S", result.stderr)

    def test_explicit_isolated_invocation_works(self) -> None:
        result = subprocess.run(
            [sys.executable, "-I", "-S", str(LAUNCHER), "--version"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "bulkload 0.2.0")

    def test_pythonpath_cannot_shadow_runtime_or_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            poison = Path(temporary)
            (poison / "json.py").write_text(
                "raise RuntimeError('poison json imported')\n"
            )
            (poison / "bulkload_lib.py").write_text(
                "raise RuntimeError('poison bulkload imported')\n"
            )
            result = subprocess.run(
                [str(LAUNCHER), "--version"],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env={**os.environ, "PYTHONPATH": str(poison)},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "bulkload 0.2.0")

    def test_runtime_closure_is_exact(self) -> None:
        runtime = LAUNCHER.parent / "bulkload_lib"
        self.assertEqual(
            {path.name for path in runtime.glob("*.py")},
            {
                "__init__.py",
                "cli.py",
                "executor.py",
                "model.py",
                "mover.py",
                "planner.py",
                "scanner.py",
            },
        )

    def test_runtime_closure_lists_agree(self) -> None:
        """The drift guard was in two places and only one of them was checked.

        `RUNTIME_SOURCE_NAMES` (`model.py:30-38`) feeds `runtime_source_digest`
        and therefore every pinned evidence digest; the launcher's own
        `RUNTIME_FILES` (`bulkload.py:17-25`) decides what can be imported at
        all; the literal above decides what may exist on disk. Three
        restatements of one closure, and nothing compared them to each other.
        Adding a module to one and not the others is exactly the drift the
        review files as an open decision (BASES-20260828.md F-5).
        """
        from bulkload_lib.model import RUNTIME_SOURCE_NAMES

        tree = ast.parse(LAUNCHER.read_text(encoding="utf-8"))
        declared: set[str] | None = None
        for node in tree.body:
            if (
                isinstance(node, ast.Assign)
                and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == "RUNTIME_FILES"
            ):
                declared = set(ast.literal_eval(node.value))
        self.assertIsNotNone(declared, "launcher declares no RUNTIME_FILES")
        self.assertEqual(set(RUNTIME_SOURCE_NAMES), declared)
        self.assertEqual(
            set(RUNTIME_SOURCE_NAMES),
            {path.name for path in (LAUNCHER.parent / "bulkload_lib").glob("*.py")},
        )


if __name__ == "__main__":
    unittest.main()
