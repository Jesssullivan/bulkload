from __future__ import annotations

from datetime import UTC, datetime, timedelta
import json
import os
from pathlib import Path
import sqlite3
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from bulkload_lib.model import BulkloadError
from bulkload_lib import private_quiescence


HOST_AUTHORITY_ID = "22222222-2222-4222-8222-222222222222"
CODEX_VERSION = "0.145.0"
PLAN_SHA256 = "a" * 64
RECEIPT_SHA256 = "b" * 64
JOURNAL_SHA256 = "c" * 64


class _Unset:
    pass


UNSET = _Unset()


class QuiescenceFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.codex_home = root / "codex"
        self.sqlite_home = root / "sqlite"
        self.evidence = root / "evidence"
        for directory in (self.codex_home, self.sqlite_home, self.evidence):
            directory.mkdir(mode=0o700)
        auth = self.codex_home / "auth.json"
        auth.write_text(json.dumps({"fixture_secret": "never-record-this"}) + "\n")
        auth.chmod(0o600)
        connection = sqlite3.connect(self.sqlite_home / "state_5.sqlite")
        connection.execute("CREATE TABLE threads(id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO threads VALUES('fixture-thread')")
        connection.commit()
        connection.close()
        self.attestation = self.evidence / "quiescence.json"
        self.operation_output = self.evidence / "operation.json"

    def create(
        self,
        *,
        output: Path | None = None,
        purpose: str = "apply",
        acknowledge: bool = True,
        capture_role: str | None = None,
        plan_sha256: str | None = PLAN_SHA256,
        receipt_sha256: str | None = None,
        journal_sha256: str | None = None,
        selected: tuple[str, ...] = ("auth", "sqlite"),
        sqlite_home: Path | None | _Unset = UNSET,
        ttl_seconds: int = 300,
    ) -> dict:
        effective_sqlite = (
            self.sqlite_home if isinstance(sqlite_home, _Unset) else sqlite_home
        )
        return private_quiescence.create_codex_private_quiescence_attestation(
            self.codex_home,
            self.attestation if output is None else output,
            purpose=purpose,
            host_authority_id=HOST_AUTHORITY_ID,
            codex_version=CODEX_VERSION,
            selected_state_classes=selected,
            sqlite_home=effective_sqlite,
            create_only_output=self.operation_output,
            acknowledge_writers_quiesced=acknowledge,
            capture_role=capture_role,
            accepted_plan_sha256=plan_sha256,
            accepted_apply_receipt_sha256=receipt_sha256,
            accepted_journal_sha256=journal_sha256,
            ttl_seconds=ttl_seconds,
        )

    def open(
        self,
        value: dict,
        *,
        purpose: str = "apply",
        operation_output: Path | None = None,
        capture_role: str | None = None,
        plan_sha256: str | None = PLAN_SHA256,
        receipt_sha256: str | None = None,
        journal_sha256: str | None = None,
        codex_home: Path | None = None,
        selected: tuple[str, ...] = ("auth", "sqlite"),
        sqlite_home: Path | None | _Unset = UNSET,
    ) -> private_quiescence.PinnedCodexPrivateQuiescenceAttestation:
        effective_sqlite = (
            self.sqlite_home if isinstance(sqlite_home, _Unset) else sqlite_home
        )
        return private_quiescence.open_codex_private_quiescence_attestation(
            self.attestation,
            accept_attestation=value["attestation_sha256"],
            expected_purpose=purpose,
            expected_host_authority_id=HOST_AUTHORITY_ID,
            expected_codex_version=CODEX_VERSION,
            expected_selected_state_classes=selected,
            expected_codex_home=(self.codex_home if codex_home is None else codex_home),
            expected_sqlite_home=effective_sqlite,
            expected_operation_output=(
                self.operation_output if operation_output is None else operation_output
            ),
            expected_capture_role=capture_role,
            expected_plan_sha256=plan_sha256,
            expected_apply_receipt_sha256=receipt_sha256,
            expected_journal_sha256=journal_sha256,
        )


class CodexPrivateQuiescenceTest(unittest.TestCase):
    def test_round_trip_is_operator_claim_not_provider_proof(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()

            self.assertEqual(
                value["claim"],
                "operator-attested-procedural-fence",
            )
            self.assertIs(value["provider_writer_proof"], False)
            self.assertNotIn(
                "never-record-this",
                fixture.attestation.read_text(),
            )
            self.assertEqual(
                private_quiescence.read_codex_private_quiescence_attestation(
                    fixture.attestation
                ),
                value,
            )
            with fixture.open(value) as pinned:
                pinned.revalidate(require_operation_output_absent=True)
                with private_quiescence.acquire_codex_private_bulkload_lock(
                    fixture.codex_home,
                    fixture.sqlite_home,
                ) as lock:
                    pinned.assert_bulkload_lock(lock)
                    self.assertEqual(
                        lock.record["scope"],
                        "cooperating-bulkload-processes-only",
                    )
                    self.assertIs(lock.record["provider_writer_proof"], False)

    def test_acknowledgement_is_required_and_no_artifact_is_created(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            with self.assertRaisesRegex(BulkloadError, "acknowledgement"):
                fixture.create(acknowledge=False)
            self.assertFalse(fixture.attestation.exists())

    def test_attestation_publication_is_create_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            fixture.create()
            original = fixture.attestation.read_bytes()
            with self.assertRaises(BulkloadError):
                fixture.create()
            self.assertEqual(fixture.attestation.read_bytes(), original)

    def test_world_readable_attestation_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            fixture.create()
            fixture.attestation.chmod(0o644)
            with self.assertRaisesRegex(BulkloadError, "custody"):
                private_quiescence.read_codex_private_quiescence_attestation(
                    fixture.attestation
                )

    def test_hardlinked_attestation_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            fixture.create()
            os.link(
                fixture.attestation,
                fixture.evidence / "second-link.json",
            )
            with self.assertRaisesRegex(BulkloadError, "custody"):
                private_quiescence.read_codex_private_quiescence_attestation(
                    fixture.attestation
                )

    def test_symlink_attestation_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()
            symlink = fixture.evidence / "symlink.json"
            symlink.symlink_to(fixture.attestation)
            with self.assertRaises(BulkloadError):
                private_quiescence.open_codex_private_quiescence_attestation(
                    symlink,
                    accept_attestation=value["attestation_sha256"],
                    expected_purpose="apply",
                    expected_host_authority_id=HOST_AUTHORITY_ID,
                    expected_codex_version=CODEX_VERSION,
                    expected_selected_state_classes=("auth", "sqlite"),
                    expected_codex_home=fixture.codex_home,
                    expected_sqlite_home=fixture.sqlite_home,
                    expected_operation_output=fixture.operation_output,
                    expected_plan_sha256=PLAN_SHA256,
                )

    def test_expired_attestation_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            created = datetime(2026, 7, 29, 12, 0, tzinfo=UTC)
            with mock.patch.object(
                private_quiescence,
                "_now_utc",
                return_value=created,
            ):
                value = fixture.create(ttl_seconds=30)
            with (
                mock.patch.object(
                    private_quiescence,
                    "_now_utc",
                    return_value=created + timedelta(seconds=31),
                ),
                self.assertRaisesRegex(BulkloadError, "expired"),
            ):
                fixture.open(value)

    def test_host_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()
            with (
                mock.patch.object(
                    private_quiescence.os,
                    "uname",
                    return_value=SimpleNamespace(nodename="not-this-host"),
                ),
                self.assertRaisesRegex(BulkloadError, "another host"),
            ):
                fixture.open(value)

    def test_replaced_live_root_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()
            fixture.codex_home.rename(fixture.root / "old-codex")
            fixture.codex_home.mkdir(mode=0o700)
            with self.assertRaisesRegex(BulkloadError, "differs"):
                fixture.open(value)

    def test_purpose_and_input_contracts_are_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            with self.assertRaisesRegex(BulkloadError, "requires a plan"):
                fixture.create(plan_sha256=None)
            self.assertFalse(fixture.attestation.exists())
            value = fixture.create()
            with self.assertRaisesRegex(BulkloadError, "purpose differs"):
                fixture.open(
                    value,
                    purpose="verify",
                    receipt_sha256=RECEIPT_SHA256,
                )

    def test_capture_input_matrix_is_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create(
                purpose="capture",
                capture_role="source",
                plan_sha256=None,
            )
            with fixture.open(
                value,
                purpose="capture",
                capture_role="source",
                plan_sha256=None,
            ) as pinned:
                pinned.revalidate(require_operation_output_absent=True)
            private_quiescence.validate_codex_private_quiescence_attestation(value)

            legacy = json.loads(json.dumps(value))
            legacy["schema"] = (
                private_quiescence.LEGACY_PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA
            )
            legacy["attestation_sha256"] = private_quiescence.object_digest(
                legacy,
                "attestation_sha256",
            )
            private_quiescence.validate_codex_private_quiescence_attestation(legacy)

    def test_close_binds_one_request_digest_and_capture_role(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create(
                purpose="close",
                capture_role="source",
            )
            with fixture.open(
                value,
                purpose="close",
                capture_role="source",
            ) as pinned:
                pinned.revalidate(require_operation_output_absent=True)
            self.assertEqual(
                value["accepted_inputs"],
                {
                    "plan_sha256": PLAN_SHA256,
                    "apply_receipt_sha256": None,
                    "journal_sha256": None,
                },
            )
            self.assertIs(value["provider_writer_proof"], False)

            legacy = json.loads(json.dumps(value))
            legacy["schema"] = (
                private_quiescence.LEGACY_PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA
            )
            legacy["attestation_sha256"] = private_quiescence.object_digest(
                legacy,
                "attestation_sha256",
            )
            with self.assertRaisesRegex(BulkloadError, "v2 attestation"):
                private_quiescence.validate_codex_private_quiescence_attestation(legacy)

            other_root = Path(temporary) / "other"
            other_root.mkdir(mode=0o700)
            other = QuiescenceFixture(other_root)
            with self.assertRaisesRegex(BulkloadError, "only the exact"):
                other.create(
                    purpose="close",
                    capture_role="destination",
                    receipt_sha256=RECEIPT_SHA256,
                )

    def test_output_binding_mismatch_and_existing_output_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()
            with self.assertRaisesRegex(BulkloadError, "differs from expected"):
                fixture.open(
                    value,
                    operation_output=fixture.evidence / "other.json",
                )
            fixture.operation_output.write_text("{}\n")
            fixture.operation_output.chmod(0o600)
            with self.assertRaisesRegex(BulkloadError, "create-only and absent"):
                fixture.open(value)

    @unittest.skipUnless(hasattr(os, "fork"), "requires POSIX fork")
    def test_lock_contention_fails_nonblocking(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            ready_read, ready_write = os.pipe()
            release_read, release_write = os.pipe()
            process = os.fork()
            if process == 0:
                os.close(ready_read)
                os.close(release_write)
                status = 0
                try:
                    with private_quiescence.acquire_codex_private_bulkload_lock(
                        fixture.codex_home,
                        fixture.sqlite_home,
                    ):
                        os.write(ready_write, b"1")
                        os.read(release_read, 1)
                except BaseException:
                    status = 1
                    try:
                        os.write(ready_write, b"0")
                    except OSError:
                        pass
                finally:
                    os.close(ready_write)
                    os.close(release_read)
                os._exit(status)

            os.close(ready_write)
            os.close(release_read)
            try:
                self.assertEqual(os.read(ready_read, 1), b"1")
                with self.assertRaisesRegex(BulkloadError, "contended"):
                    private_quiescence.acquire_codex_private_bulkload_lock(
                        fixture.codex_home,
                        fixture.sqlite_home,
                    )
            finally:
                os.write(release_write, b"1")
                os.close(release_write)
                os.close(ready_read)
                _, status = os.waitpid(process, 0)
            self.assertTrue(os.WIFEXITED(status))
            self.assertEqual(os.WEXITSTATUS(status), 0)

    def test_lock_scope_deduplicates_same_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            with private_quiescence.acquire_codex_private_bulkload_lock(
                fixture.codex_home,
                fixture.codex_home,
            ) as lock:
                self.assertEqual(len(lock.record["roots"]), 1)
                self.assertEqual(
                    lock.record["method"],
                    "posix-flock-exclusive-nonblocking-directory-descriptors",
                )

    def test_live_sqlite_sidecar_rejects_attestation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            sidecar = fixture.sqlite_home / "state_5.sqlite-wal"
            sidecar.write_bytes(b"not-a-live-database")
            sidecar.chmod(0o600)
            with self.assertRaisesRegex(BulkloadError, "absent live sidecars"):
                fixture.create()
            self.assertFalse(fixture.attestation.exists())

    def test_pinned_context_detects_auth_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QuiescenceFixture(Path(temporary))
            value = fixture.create()
            with fixture.open(value) as pinned:
                replacement = fixture.codex_home / ".replacement"
                replacement.write_text(json.dumps({"changed": True}) + "\n")
                replacement.chmod(0o600)
                os.replace(replacement, fixture.codex_home / "auth.json")
                with self.assertRaisesRegex(BulkloadError, "no longer matches"):
                    pinned.revalidate()


if __name__ == "__main__":
    unittest.main()
