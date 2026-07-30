from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class BazelBinarySmokeTest(unittest.TestCase):
    def test_bazel_built_launcher_executes_its_runfiles_closure(self) -> None:
        test_srcdir = os.environ.get("TEST_SRCDIR")
        test_workspace = os.environ.get("TEST_WORKSPACE")
        if not test_srcdir or not test_workspace:
            self.skipTest("Bazel runfiles are unavailable outside bazel test")
        binary = Path(test_srcdir) / test_workspace / "bulkload"
        self.assertTrue(binary.is_file(), binary)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "startup-hook-ran"
            (root / "sitecustomize.py").write_text(
                "from pathlib import Path\n"
                f"Path({os.fspath(marker)!r}).write_text('executed')\n"
            )
            result = subprocess.run(
                [os.fspath(binary), "--help"],
                check=False,
                capture_output=True,
                env={
                    **os.environ,
                    "PYTHONDONTWRITEBYTECODE": "1",
                    "PYTHONPATH": os.fspath(root),
                },
                text=True,
            )
            self.assertFalse(marker.exists())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("usage: bulkload", result.stdout)


if __name__ == "__main__":
    unittest.main()
