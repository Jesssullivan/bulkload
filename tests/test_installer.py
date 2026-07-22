from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True


def find_workspace() -> Path:
    candidates = [Path.cwd(), Path(__file__).resolve()]
    runfiles = os.environ.get("RUNFILES_DIR")
    if runfiles:
        candidates.append(Path(runfiles) / "_main")
        candidates.append(Path(runfiles) / "bulkload")
    for candidate in candidates:
        for parent in [candidate, *candidate.parents]:
            if (parent / "scripts/install-skill.sh").is_file() and (
                parent / ".agents/skills/bulkload/SKILL.md"
            ).is_file():
                return parent
    raise AssertionError("cannot locate bulkload runfiles workspace")


def materialize_installer(workspace: Path, root: Path) -> Path:
    repository = root / "repository"
    (repository / "scripts").mkdir(parents=True)
    shutil.copytree(
        workspace / ".agents/skills/bulkload",
        repository / ".agents/skills/bulkload",
        symlinks=False,
    )
    for name in ("install-skill.sh", "install_lock.py", "validate_skill.py"):
        shutil.copy2(workspace / "scripts" / name, repository / "scripts" / name)
    return repository / "scripts/install-skill.sh"


def test_environment(home: Path, root: Path) -> dict[str, str]:
    binary_directory = root / "bin"
    binary_directory.mkdir(exist_ok=True)
    python = binary_directory / "python3"
    if not python.exists():
        os.symlink(sys.executable, python)
    return {
        **os.environ,
        "HOME": str(home),
        "PATH": f"{binary_directory}{os.pathsep}{os.environ.get('PATH', '')}",
    }


class InstallerTest(unittest.TestCase):
    def test_workspace_skill_contains_no_generated_cache(self) -> None:
        skill = find_workspace() / ".agents/skills/bulkload"
        generated = [
            path
            for path in skill.rglob("*")
            if "__pycache__" in path.parts or path.suffix in {".pyc", ".pyo"}
        ]
        self.assertEqual(generated, [])

    def test_install_and_doctor_are_idempotent(self) -> None:
        workspace = find_workspace()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = materialize_installer(workspace, root)
            home = root / "home"
            home.mkdir()
            environment = test_environment(home, root)
            first = subprocess.run(
                [str(installer), "--all"],
                env=environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            self.assertIn("INSTALLED", first.stdout)
            canonical = home / ".agents/skills/bulkload"
            claude = home / ".claude/skills/bulkload"
            self.assertTrue((canonical / "SKILL.md").is_file())
            self.assertTrue(claude.is_symlink())
            self.assertEqual(claude.resolve(), canonical.resolve())
            runtime = subprocess.run(
                [sys.executable, str(canonical / "scripts/bulkload.py"), "--version"],
                env=environment,
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertEqual(runtime.returncode, 0, (runtime.stdout, runtime.stderr))
            self.assertEqual(
                [
                    path
                    for path in canonical.rglob("*")
                    if "__pycache__" in path.parts or path.suffix in {".pyc", ".pyo"}
                ],
                [],
            )

            second = subprocess.run(
                [str(installer), "--all"],
                env=environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            self.assertIn("UNCHANGED", second.stdout)
            subprocess.run(
                [str(installer), "--doctor"],
                env=environment,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

    def test_concurrent_installs_converge(self) -> None:
        workspace = find_workspace()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = materialize_installer(workspace, root)
            home = root / "home"
            home.mkdir()
            environment = test_environment(home, root)
            processes = [
                subprocess.Popen(
                    [str(installer), "--all"],
                    env=environment,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                for _ in range(2)
            ]
            results = [process.communicate(timeout=30) for process in processes]
            self.assertEqual(
                [process.returncode for process in processes], [0, 0], results
            )
            canonical = home / ".agents/skills/bulkload"
            self.assertTrue((canonical / "SKILL.md").is_file())

    def test_installer_rejects_symlinked_bundle_content(self) -> None:
        workspace = find_workspace()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = materialize_installer(workspace, root)
            home = root / "home"
            home.mkdir()
            skill = root / "repository/.agents/skills/bulkload"
            os.symlink("SKILL.md", skill / "unexpected-link")
            process = subprocess.run(
                [str(installer), "--all"],
                env=test_environment(home, root),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertNotEqual(process.returncode, 0)
            self.assertIn("must not contain symlinks", process.stderr)

    def test_installer_rejects_generated_bundle_cache(self) -> None:
        workspace = find_workspace()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = materialize_installer(workspace, root)
            home = root / "home"
            home.mkdir()
            cache = (
                root
                / "repository/.agents/skills/bulkload/scripts/bulkload_lib/__pycache__"
            )
            cache.mkdir()
            (cache / "scanner.pyc").write_bytes(b"\x00generated-cache\xff")
            process = subprocess.run(
                [str(installer), "--all"],
                env=test_environment(home, root),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertNotEqual(process.returncode, 0)
            self.assertIn("generated Python cache", process.stderr)
            self.assertFalse((home / ".agents/skills/bulkload").exists())


if __name__ == "__main__":
    unittest.main()
