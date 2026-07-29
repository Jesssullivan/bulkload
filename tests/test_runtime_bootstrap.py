from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import ModuleType
import unittest
from unittest import mock


BOOTSTRAP_PATH = (
    Path(__file__).parents[1] / ".agents/skills/bulkload/scripts/bulkload.py"
)


def _load_bootstrap() -> ModuleType:
    spec = importlib.util.spec_from_file_location(
        "bulkload_runtime_bootstrap_test",
        BOOTSTRAP_PATH,
    )
    if spec is None or spec.loader is None:
        raise AssertionError("cannot load Bulkload runtime bootstrap")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RuntimeBootstrapTest(unittest.TestCase):
    def test_runtime_authority_requires_the_pinned_isolated_launcher(self) -> None:
        bootstrap = _load_bootstrap()
        with self.assertRaisesRegex(
            bootstrap.BootstrapError,
            "requires the pinned isolated launcher",
        ):
            bootstrap._open_runtime_authority(BOOTSTRAP_PATH.parent)

    def test_direct_launcher_isolates_stdlib_from_scripts_shadowing(self) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            skill_root = Path(directory) / "bulkload"
            shutil.copytree(BOOTSTRAP_PATH.parents[1], skill_root)
            sentinel = "UNPINNED_IMPORT_EXECUTED"
            (skill_root / "scripts/hashlib.py").write_text(
                f"print({sentinel!r})\nraise RuntimeError('shadow imported')\n"
            )
            runtime_payloads = {
                path.relative_to(skill_root).as_posix(): path.read_bytes()
                for path in sorted((skill_root / "scripts").rglob("*.py"))
            }
            policy_path = (
                skill_root / "references" / "codex-private-state-policy.v3.json"
            )
            policy = json.loads(policy_path.read_text())
            policy["runtime_source_sha256"] = bootstrap._runtime_digest(
                runtime_payloads
            )
            policy_path.write_bytes(bootstrap._canonical_bytes(policy) + b"\n")

            result = subprocess.run(
                [
                    sys.executable,
                    str(skill_root / "scripts/bulkload.py"),
                    "--help",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn(sentinel, result.stdout)
            self.assertNotIn(sentinel, result.stderr)

    def test_symlink_launcher_promotes_one_pinned_real_closure(self) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill_root = root / "bulkload"
            shutil.copytree(BOOTSTRAP_PATH.parents[1], skill_root)
            policy_path = (
                skill_root / "references" / "codex-private-state-policy.v3.json"
            )
            runtime_payloads = {
                path.relative_to(skill_root).as_posix(): path.read_bytes()
                for path in sorted((skill_root / "scripts").rglob("*.py"))
            }
            policy = json.loads(policy_path.read_text())
            policy["runtime_source_sha256"] = bootstrap._runtime_digest(
                runtime_payloads
            )
            policy_path.write_bytes(bootstrap._canonical_bytes(policy) + b"\n")
            launcher = root / "bulkload-runfiles-link"
            launcher.symlink_to(skill_root / "scripts/bulkload.py")

            result = subprocess.run(
                [sys.executable, str(launcher), "--help"],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("usage: bulkload", result.stdout)

    def test_runfiles_symlink_forest_pins_every_runtime_leaf(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            runfiles = root / "runfiles"
            shutil.copytree(BOOTSTRAP_PATH.parents[1], source)
            launcher_relative = Path("scripts/bulkload.py")
            policy_relative = Path("references/codex-private-state-policy.v3.json")
            leaves = [
                *(
                    path.relative_to(source)
                    for path in sorted((source / "scripts").rglob("*.py"))
                ),
                policy_relative,
            ]
            for relative in leaves:
                destination = runfiles / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                if relative == launcher_relative:
                    shutil.copy2(source / relative, destination)
                else:
                    destination.symlink_to(source / relative)

            result = subprocess.run(
                [
                    sys.executable,
                    str(runfiles / launcher_relative),
                    "--help",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("usage: bulkload", result.stdout)

    def test_symlink_swap_to_foreign_target_then_restore_fails_closed(
        self,
    ) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target_a = root / "a.py"
            target_b = root / "b.py"
            target_a.write_text("A = 1\n")
            target_b.write_text("B = 2\n")
            link = root / "runtime.py"
            link.symlink_to(target_a)
            real_open = bootstrap.os.open

            def racing_open(path, flags):
                if Path(path) != link:
                    return real_open(path, flags)
                link.unlink()
                link.symlink_to(target_b)
                try:
                    return real_open(path, flags)
                finally:
                    link.unlink()
                    link.symlink_to(target_a)

            with mock.patch.object(
                bootstrap.os,
                "open",
                side_effect=racing_open,
            ):
                with self.assertRaisesRegex(
                    bootstrap.BootstrapError,
                    "path entry changed|descriptor target differs",
                ):
                    bootstrap._open_file(
                        link,
                        maximum=1024,
                        label="raced runtime",
                    )

    def test_actual_launcher_swap_to_foreign_closure_then_restore_fails_closed(
        self,
    ) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill_a = root / "skill-a"
            skill_b = root / "skill-b"
            shutil.copytree(BOOTSTRAP_PATH.parents[1], skill_a)
            shutil.copytree(BOOTSTRAP_PATH.parents[1], skill_b)
            launcher = root / "bulkload-link"
            launcher_a = skill_a / "scripts/bulkload.py"
            launcher_b = skill_b / "scripts/bulkload.py"
            launcher.symlink_to(launcher_a)
            source = launcher_a.read_text()
            insertion = (
                "    _bootstrap_real_open = _bootstrap_posix.open\n"
                "    def _bootstrap_racing_open(path, flags):\n"
                "        _bootstrap_posix.unlink(path)\n"
                f"        _bootstrap_posix.symlink({str(launcher_b)!r}, path)\n"
                "        try:\n"
                "            return _bootstrap_real_open(path, flags)\n"
                "        finally:\n"
                "            _bootstrap_posix.unlink(path)\n"
                f"            _bootstrap_posix.symlink({str(launcher_a)!r}, path)\n"
            )
            source = source.replace(
                "    try:\n        _bootstrap_descriptor = _bootstrap_posix.open(",
                insertion + "    try:\n"
                "        _bootstrap_descriptor = _bootstrap_racing_open(",
                1,
            ).replace(
                "        _bootstrap_descriptor = _bootstrap_posix.open(",
                "        _bootstrap_descriptor = _bootstrap_racing_open(",
                1,
            )
            launcher_a.write_text(source)
            runtime_payloads = {
                path.relative_to(skill_a).as_posix(): path.read_bytes()
                for path in sorted((skill_a / "scripts").rglob("*.py"))
            }
            policy_path = skill_a / "references" / "codex-private-state-policy.v3.json"
            policy = json.loads(policy_path.read_text())
            policy["runtime_source_sha256"] = bootstrap._runtime_digest(
                runtime_payloads
            )
            policy_path.write_bytes(bootstrap._canonical_bytes(policy) + b"\n")

            result = subprocess.run(
                [sys.executable, str(launcher), "--help"],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertRegex(
                result.stderr,
                "Bulkload launcher (entry changed while pinning|"
                "descriptor differs from requested target)",
            )

    def test_runtime_link_and_target_replacement_fail_revalidation(self) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for replace_link in (True, False):
                with self.subTest(replace_link=replace_link):
                    target = root / f"target-{replace_link}.py"
                    replacement = root / f"replacement-{replace_link}.py"
                    target.write_text("VALUE = 'pinned'\n")
                    replacement.write_text("VALUE = 'replacement'\n")
                    link = root / f"runtime-{replace_link}.py"
                    link.symlink_to(target)
                    descriptor, _, binding = bootstrap._open_file(
                        link,
                        maximum=1024,
                        label="runtime fixture",
                    )
                    try:
                        if replace_link:
                            link.unlink()
                            link.symlink_to(replacement)
                        else:
                            retained = root / f"retained-{replace_link}.py"
                            target.rename(retained)
                            replacement.rename(target)
                        with self.assertRaisesRegex(
                            bootstrap.BootstrapError,
                            "path entry changed|path binding changed",
                        ):
                            bootstrap._revalidate_path_binding(
                                link,
                                descriptor,
                                binding,
                                label="runtime fixture",
                            )
                    finally:
                        bootstrap.os.close(descriptor)

    def test_unknown_bulkload_module_never_falls_through(self) -> None:
        bootstrap = _load_bootstrap()
        finder = bootstrap.PinnedSourceFinder({})
        with self.assertRaises(ModuleNotFoundError):
            finder.find_spec("bulkload_lib.unpinned")
        self.assertIsNone(finder.find_spec("json"))

    def test_loader_executes_pinned_bytes_after_path_replacement(self) -> None:
        bootstrap = _load_bootstrap()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "module.py"
            path.write_text("VALUE = 'pinned'\n")
            loader = bootstrap.PinnedSourceLoader(
                path,
                path.read_bytes(),
                False,
            )
            path.write_text("VALUE = 'replacement'\n")
            module = ModuleType("bulkload_lib.fixture")
            loader.exec_module(module)
            self.assertEqual(module.VALUE, "pinned")

    def test_authority_closes_when_module_map_construction_fails(self) -> None:
        bootstrap = _load_bootstrap()

        class Authority:
            record = {
                "schema": bootstrap.RUNTIME_SCHEMA,
                "policy_schema": bootstrap.POLICY_SCHEMA,
                "policy_sha256": "0" * 64,
                "runtime_source_sha256": "1" * 64,
                "source_digests": {
                    path: "2" * 64 for path in bootstrap.CORE_SOURCE_KEYS
                },
            }

            def __init__(self) -> None:
                self.closed = False
                self.revalidations = 0

            def module_sources(self):
                raise RuntimeError("stop after source-path setup")

            def revalidate(self) -> None:
                self.revalidations += 1

            def close(self) -> None:
                self.closed = True

        authority = Authority()
        with mock.patch.object(
            bootstrap,
            "_open_runtime_authority",
            return_value=authority,
        ):
            with self.assertRaisesRegex(RuntimeError, "stop"):
                bootstrap._run()
        self.assertTrue(authority.closed)

    def test_policy_parser_rejects_duplicates_and_noncanonical_json(self) -> None:
        bootstrap = _load_bootstrap()
        for payload in (
            b'{"schema":"one","schema":"two"}\n',
            b'{"value":NaN}\n',
            json.dumps({"value": 1}, indent=2).encode("utf-8") + b"\n",
        ):
            with self.subTest(payload=payload):
                with self.assertRaises(bootstrap.BootstrapError):
                    bootstrap._strict_object(payload)


if __name__ == "__main__":
    unittest.main()
