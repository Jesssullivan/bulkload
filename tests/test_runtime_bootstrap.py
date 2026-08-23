from __future__ import annotations

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
                "planner.py",
                "scanner.py",
            },
        )


if __name__ == "__main__":
    unittest.main()
