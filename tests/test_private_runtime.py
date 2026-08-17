from __future__ import annotations

from contextlib import redirect_stderr
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from bulkload_lib.cli import main as cli_main
from bulkload_lib.model import BulkloadError, canonical_bytes
from bulkload_lib import private_runtime


def _write_runtime_fixture(root: Path) -> dict[str, bytes]:
    scripts = root / "scripts"
    library = scripts / "bulkload_lib"
    references = root / "references"
    library.mkdir(parents=True, exist_ok=True)
    references.mkdir(exist_ok=True)
    payloads = {
        "scripts/bulkload.py": b"print('fixture')\n",
        "scripts/bulkload_lib/__init__.py": b'"""fixture"""\n',
        "scripts/bulkload_lib/cli.py": b"def main(): return 0\n",
        "scripts/bulkload_lib/private_apply.py": b"def apply(): return None\n",
        "scripts/bulkload_lib/private_quiescence.py": (b"def attest(): return None\n"),
        "scripts/bulkload_lib/private_sqlite_action_plan.py": (
            b"def compile_action_plan(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_close.py": (b"def close(): return None\n"),
        "scripts/bulkload_lib/private_sqlite_plan.py": (
            b"def compile_plan(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_request.py": (
            b"def compile_request(): return None\n"
        ),
        "scripts/bulkload_lib/private_state.py": b"def capture(): return None\n",
        "scripts/bulkload_lib/sessions.py": b"def sessions(): return None\n",
        "scripts/bulkload_lib/other.py": b"VALUE = 1\n",
    }
    for relative, payload in payloads.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
    source_digests = {
        relative: hashlib.sha256(payloads[relative]).hexdigest()
        for relative in private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS
    }
    policy = {
        "schema": private_runtime.PRIVATE_STATE_POLICY_SCHEMA,
        "implementation": private_runtime.PRIVATE_INSTALL_IMPLEMENTATION,
        "readiness": {
            "auth_install": True,
            "combined": False,
            "sqlite_compose": False,
            "sqlite_compose_action_plan": False,
            "sqlite_compose_request": True,
            "sqlite_capacity_observation": True,
            "sqlite_compose_plan": False,
            "sqlite_publish": False,
        },
        "source_digests": source_digests,
        "runtime_source_sha256": private_runtime._runtime_digest(payloads),
        "allowed_codex_cli_commands": (
            private_runtime.PRIVATE_ALLOWED_CODEX_CLI_COMMANDS
        ),
        "state_classes": private_runtime.PRIVATE_STATE_CLASSES,
        "forbidden_commands": private_runtime.PRIVATE_FORBIDDEN_COMMANDS,
    }
    (references / private_runtime.PRIVATE_STATE_POLICY_NAME).write_bytes(
        canonical_bytes(policy) + b"\n"
    )
    return payloads


class PrivateRuntimeAuthorityTest(unittest.TestCase):
    def test_operational_runtime_requires_preimport_bootstrap_binding(self) -> None:
        with self.assertRaisesRegex(
            BulkloadError,
            "pre-import bootstrap binding",
        ):
            private_runtime.open_pinned_private_runtime_authority()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "codex-home"
            home.mkdir(mode=0o700)
            output = root / "quiescence.json"
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                result = cli_main(
                    [
                        "codex-private-quiescence-attest",
                        "--codex-home",
                        str(home),
                        "--output",
                        str(output),
                        "--operation-output",
                        str(root / "capture"),
                        "--purpose",
                        "capture",
                        "--capture-role",
                        "source",
                        "--host-authority-id",
                        "11111111-1111-4111-8111-111111111111",
                        "--codex-version",
                        "0.145.0",
                        "--include-auth",
                        "--acknowledge-writers-quiesced",
                    ]
                )
            self.assertEqual(result, 2)
            self.assertIn("pre-import bootstrap binding", stderr.getvalue())
            self.assertFalse(output.exists())

    def test_round_trip_and_expected_authority_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _write_runtime_fixture(root)
            with mock.patch.object(
                private_runtime,
                "_skill_root",
                return_value=root,
            ):
                with (
                    private_runtime._open_disk_private_runtime_authority_for_tests() as pinned
                ):
                    private_runtime.validate_private_runtime_authority(pinned.record)
                    expected = pinned.record
                    pinned.revalidate()
                with private_runtime._open_disk_private_runtime_authority_for_tests(
                    expected
                ) as reopened:
                    self.assertEqual(reopened.record, expected)

    def test_only_the_two_exact_legacy_v4_authorities_are_accepted(self) -> None:
        authorities = (
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4,
            private_runtime.ACCEPTED_H5_PRIVATE_RUNTIME_AUTHORITY_V4,
        )
        self.assertEqual(len(authorities), 2)
        self.assertNotEqual(authorities[0], authorities[1])
        for index, authority in enumerate(authorities):
            with self.subTest(authority=index):
                exact = json.loads(json.dumps(authority))
                self.assertEqual(
                    private_runtime.validate_private_runtime_authority(exact),
                    exact,
                )

        for mask in range(16):
            bits = tuple((mask >> index) & 1 for index in range(4))
            mixed = json.loads(json.dumps(authorities[0]))
            mixed["policy_sha256"] = authorities[bits[0]]["policy_sha256"]
            mixed["runtime_source_sha256"] = authorities[bits[1]][
                "runtime_source_sha256"
            ]
            for offset, path in enumerate(
                (
                    "scripts/bulkload_lib/cli.py",
                    "scripts/bulkload_lib/sessions.py",
                ),
                start=2,
            ):
                mixed["source_digests"][path] = authorities[bits[offset]][
                    "source_digests"
                ][path]
            with self.subTest(authority_mix=bits):
                if bits in ((0, 0, 0, 0), (1, 1, 1, 1)):
                    self.assertEqual(
                        private_runtime.validate_private_runtime_authority(mixed),
                        mixed,
                    )
                else:
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "exact v4 closures",
                    ):
                        private_runtime.validate_private_runtime_authority(mixed)

        unknown = json.loads(json.dumps(authorities[1]))
        unknown["policy_sha256"] = "f" * 64
        unknown["runtime_source_sha256"] = "e" * 64
        unknown["source_digests"] = {
            path: "d" * 64 for path in unknown["source_digests"]
        }
        with self.assertRaisesRegex(BulkloadError, "exact v4 closures"):
            private_runtime.validate_private_runtime_authority(unknown)

        extra = json.loads(json.dumps(authorities[1]))
        extra["unexpected"] = "field"
        missing = json.loads(json.dumps(authorities[1]))
        missing.pop("runtime_source_sha256")
        wrong_schema = json.loads(json.dumps(authorities[1]))
        wrong_schema["schema"] += ".unknown"
        extra_source = json.loads(json.dumps(authorities[1]))
        extra_source["source_digests"]["scripts/bulkload_lib/unknown.py"] = "c" * 64
        missing_source = json.loads(json.dumps(authorities[1]))
        missing_source["source_digests"].pop("scripts/bulkload_lib/cli.py")
        for label, malformed in (
            ("extra", extra),
            ("missing", missing),
            ("schema", wrong_schema),
            ("extra-source", extra_source),
            ("missing-source", missing_source),
        ):
            with self.subTest(malformed=label):
                with self.assertRaises(BulkloadError):
                    private_runtime.validate_private_runtime_authority(malformed)

    def test_only_the_exact_accepted_h6_v5_authority_is_accepted(
        self,
    ) -> None:
        legacy = json.loads(
            json.dumps(private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5)
        )
        self.assertEqual(
            private_runtime.validate_private_runtime_authority(legacy),
            legacy,
        )
        for field in (
            "policy_sha256",
            "runtime_source_sha256",
        ):
            with self.subTest(field=field):
                changed = json.loads(json.dumps(legacy))
                changed[field] = "0" * 64
                with self.assertRaisesRegex(
                    BulkloadError,
                    "exact accepted-H6 v5 closure",
                ):
                    private_runtime.validate_private_runtime_authority(changed)
        changed = json.loads(json.dumps(legacy))
        changed["source_digests"]["scripts/bulkload_lib/cli.py"] = "0" * 64
        with self.assertRaisesRegex(BulkloadError, "exact accepted-H6 v5 closure"):
            private_runtime.validate_private_runtime_authority(changed)

    def test_path_replacement_and_inventory_drift_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _write_runtime_fixture(root)
            with mock.patch.object(
                private_runtime,
                "_skill_root",
                return_value=root,
            ):
                pinned = (
                    private_runtime._open_disk_private_runtime_authority_for_tests()
                )
                try:
                    cli = root / "scripts/bulkload_lib/cli.py"
                    replacement = cli.with_name("cli.py.replacement")
                    replacement.write_bytes(cli.read_bytes())
                    os.replace(replacement, cli)
                    with self.assertRaisesRegex(BulkloadError, "binding changed"):
                        pinned.revalidate()
                finally:
                    pinned.close()

            _write_runtime_fixture(root)
            with mock.patch.object(
                private_runtime,
                "_skill_root",
                return_value=root,
            ):
                pinned = (
                    private_runtime._open_disk_private_runtime_authority_for_tests()
                )
                try:
                    (root / "scripts/bulkload_lib/added.py").write_text("VALUE = 2\n")
                    with self.assertRaisesRegex(BulkloadError, "inventory changed"):
                        pinned.revalidate()
                finally:
                    pinned.close()

    def test_exact_policy_semantics_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payloads = _write_runtime_fixture(root)
            runtime_sha256 = private_runtime._runtime_digest(payloads)
            policy_path = (
                root / "references" / private_runtime.PRIVATE_STATE_POLICY_NAME
            )
            policy = json.loads(policy_path.read_text())
            mutations = (
                lambda value: value["readiness"].__setitem__("combined", True),
                lambda value: value["allowed_codex_cli_commands"].append(
                    "codex-private-combined-apply"
                ),
                lambda value: value["forbidden_commands"].pop(),
                lambda value: value["state_classes"]["sqlite_families"].__setitem__(
                    "composer_implemented",
                    True,
                ),
            )
            for mutate in mutations:
                with self.subTest(mutation=mutate):
                    changed = json.loads(json.dumps(policy))
                    mutate(changed)
                    with self.assertRaises(BulkloadError):
                        private_runtime._validate_policy(
                            changed,
                            payloads=payloads,
                            runtime_sha256=runtime_sha256,
                        )

    def test_expected_record_and_core_or_noncore_drift_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _write_runtime_fixture(root)
            with mock.patch.object(
                private_runtime,
                "_skill_root",
                return_value=root,
            ):
                with (
                    private_runtime._open_disk_private_runtime_authority_for_tests() as pinned
                ):
                    expected = pinned.record
                changed = json.loads(json.dumps(expected))
                changed["runtime_source_sha256"] = "0" * 64
                with self.assertRaisesRegex(BulkloadError, "differs"):
                    private_runtime._open_disk_private_runtime_authority_for_tests(
                        changed
                    )

                for relative in (
                    "scripts/bulkload_lib/private_apply.py",
                    "scripts/bulkload_lib/other.py",
                ):
                    with self.subTest(relative=relative):
                        path = root / relative
                        original = path.read_bytes()
                        path.write_bytes(original + b"# drift\n")
                        try:
                            with self.assertRaises(BulkloadError):
                                (
                                    private_runtime._open_disk_private_runtime_authority_for_tests()
                                )
                        finally:
                            path.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
