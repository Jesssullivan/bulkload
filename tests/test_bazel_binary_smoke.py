from __future__ import annotations

import os
from pathlib import Path
import subprocess
import unittest


class BazelBinarySmokeTest(unittest.TestCase):
    def test_bazel_built_launcher_executes_its_runfiles_closure(self) -> None:
        test_srcdir = os.environ.get("TEST_SRCDIR")
        test_workspace = os.environ.get("TEST_WORKSPACE")
        if not test_srcdir or not test_workspace:
            self.skipTest("Bazel runfiles are unavailable outside bazel test")
        binary = Path(test_srcdir) / test_workspace / "bulkload"
        self.assertTrue(binary.is_file(), binary)
        result = subprocess.run(
            [os.fspath(binary), "--help"],
            check=False,
            capture_output=True,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("usage: bulkload", result.stdout)


if __name__ == "__main__":
    unittest.main()
