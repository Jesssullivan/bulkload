from __future__ import annotations

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import unittest
from unittest import mock
import uuid

from bulkload_lib.cli import main as cli_main
from bulkload_lib.model import BulkloadError
from bulkload_lib import (
    private_apply,
    private_quiescence,
    private_runtime,
    private_state,
)
from tests.unprivileged_test_main import run_unittest_main


SOURCE_AUTHORITY = "11111111-1111-4111-8111-111111111111"
DESTINATION_AUTHORITY = "22222222-2222-4222-8222-222222222222"
CODEX_VERSION = "0.145.0"


def _write_auth(home: Path, value: str) -> None:
    path = home / private_state.AUTH_BASENAME
    path.write_text(json.dumps({"fixture": value}) + "\n")
    path.chmod(0o600)


def _sqlite_truth(home: Path) -> dict[str, tuple[bytes, int, int, int]]:
    return {
        path.name: (
            path.read_bytes(),
            path.stat().st_ino,
            path.stat().st_mtime_ns,
            path.stat().st_ctime_ns,
        )
        for path in sorted(home.iterdir())
        if path.name.endswith(
            (".sqlite", ".sqlite-wal", ".sqlite-shm", ".sqlite-journal")
        )
    }


def _replace_auth_with_new_inode(home: Path, payload: bytes) -> None:
    staged = home / ".auth.json.test-replacement"
    staged.write_bytes(payload)
    staged.chmod(0o600)
    os.replace(staged, home / private_state.AUTH_BASENAME)


def _append_rehashed_journal_event(path: Path, event: dict) -> None:
    payload = path.read_bytes()
    lines = payload.rstrip(b"\n").split(b"\n")
    previous = json.loads(lines[-1])
    appended = {
        **event,
        "sequence": len(lines),
        "previous_event_sha256": previous["event_sha256"],
    }
    appended["event_sha256"] = private_apply.object_digest(
        appended,
        "event_sha256",
    )
    with path.open("ab") as handle:
        handle.write(private_apply.canonical_bytes(appended) + b"\n")
        handle.flush()
        os.fsync(handle.fileno())


def _rehash_journal_events(events: list[dict]) -> list[dict]:
    rewritten: list[dict] = []
    previous: str | None = None
    for sequence, value in enumerate(events):
        event = json.loads(json.dumps(value))
        event["sequence"] = sequence
        event["previous_event_sha256"] = previous
        event["event_sha256"] = private_apply.object_digest(
            event,
            "event_sha256",
        )
        previous = event["event_sha256"]
        rewritten.append(event)
    return rewritten


def _run_cli(arguments: list[str]) -> int:
    with redirect_stdout(io.StringIO()):
        return cli_main(arguments)


def _create_quiescence_attestation(
    *,
    codex_home: Path,
    sqlite_home: Path | None,
    output: Path,
    operation_output: Path,
    purpose: str,
    host_authority_id: str,
    selected_state_classes: tuple[str, ...],
    capture_role: str | None = None,
    plan_sha256: str | None = None,
    apply_receipt_sha256: str | None = None,
    journal_sha256: str | None = None,
) -> tuple[Path, dict]:
    attestation = private_quiescence.create_codex_private_quiescence_attestation(
        codex_home,
        output,
        purpose=purpose,
        host_authority_id=host_authority_id,
        codex_version=CODEX_VERSION,
        selected_state_classes=selected_state_classes,
        sqlite_home=sqlite_home,
        create_only_output=operation_output,
        acknowledge_writers_quiesced=True,
        capture_role=capture_role,
        accepted_plan_sha256=plan_sha256,
        accepted_apply_receipt_sha256=apply_receipt_sha256,
        accepted_journal_sha256=journal_sha256,
    )
    return output, attestation


def _capture_with_quiescence(
    codex_home: Path,
    output_directory: Path,
    *,
    role: str,
    host_authority_id: str,
    sqlite_home: Path | None,
    include_auth: bool,
    include_sqlite: bool,
    attestation_output: Path,
) -> dict:
    selected_state_classes = tuple(
        state_class
        for state_class, selected in (
            ("auth", include_auth),
            ("sqlite", include_sqlite),
        )
        if selected
    )
    _, attestation = _create_quiescence_attestation(
        codex_home=codex_home,
        sqlite_home=sqlite_home,
        output=attestation_output,
        operation_output=output_directory,
        purpose="capture",
        capture_role=role,
        host_authority_id=host_authority_id,
        selected_state_classes=selected_state_classes,
    )
    return private_state.capture_codex_private_state(
        codex_home,
        output_directory,
        role=role,
        host_authority_id=host_authority_id,
        codex_version=CODEX_VERSION,
        sqlite_home=sqlite_home,
        include_auth=include_auth,
        include_sqlite=include_sqlite,
        acknowledge_private_capture=True,
        quiescence=private_state.private_quiescence_capture_record(attestation),
    )


def _compile_state_class_matrix(
    root: Path,
    *,
    source_auth: bool,
    source_sqlite: bool,
    destination_auth: bool,
    destination_sqlite: bool,
) -> dict:
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    source_home = root / "matrix-source-home"
    destination_home = root / "matrix-destination-home"
    source_home.mkdir(mode=0o700)
    destination_home.mkdir(mode=0o700)
    if source_auth:
        _write_auth(source_home, "matrix-source")
    if destination_auth:
        _write_auth(destination_home, "matrix-destination")
    for home, selected in (
        (source_home, source_sqlite),
        (destination_home, destination_sqlite),
    ):
        if not selected:
            continue
        connection = sqlite3.connect(home / "state_5.sqlite")
        connection.execute("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        connection.commit()
        connection.close()
    source_bundle = root / "matrix-source-bundle"
    destination_bundle = root / "matrix-destination-bundle"
    _capture_with_quiescence(
        source_home,
        source_bundle,
        role="source",
        host_authority_id=SOURCE_AUTHORITY,
        sqlite_home=source_home if source_sqlite else None,
        include_auth=source_auth,
        include_sqlite=source_sqlite,
        attestation_output=root / "matrix-source-attestation.json",
    )
    _capture_with_quiescence(
        destination_home,
        destination_bundle,
        role="destination",
        host_authority_id=DESTINATION_AUTHORITY,
        sqlite_home=destination_home if destination_sqlite else None,
        include_auth=destination_auth,
        include_sqlite=destination_sqlite,
        attestation_output=root / "matrix-destination-attestation.json",
    )
    runtime_authority = private_runtime.current_private_runtime_authority()
    compatibility = private_state.compile_codex_private_state_plan(
        source_bundle,
        destination_bundle,
        runtime_authority=runtime_authority,
    )
    return private_apply.compile_codex_private_install_plan(
        compatibility,
        source_bundle,
        destination_bundle,
        accept_compatibility_plan=compatibility["plan_sha256"],
        runtime_authority=runtime_authority,
    )


class PrivateApplyFixture:
    def __init__(
        self,
        root: Path,
        *,
        divergent_destination_sqlite: bool = False,
        divergent_destination_sqlite_topology: bool = False,
        source_include_sqlite: bool = True,
    ) -> None:
        self.root = root
        self.source_home = root / "source-home"
        self.destination_home = root / "destination-home"
        self.source_home.mkdir(mode=0o700)
        self.destination_home.mkdir(mode=0o700)
        _write_auth(self.source_home, "source-secret")
        _write_auth(self.destination_home, "destination-secret")
        source_database = self.source_home / "state_5.sqlite"
        connection = sqlite3.connect(source_database)
        connection.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT)")
        connection.execute("INSERT INTO items(value) VALUES('shared')")
        connection.commit()
        connection.close()
        destination_database = self.destination_home / source_database.name
        shutil.copy2(source_database, destination_database)
        if divergent_destination_sqlite:
            connection = sqlite3.connect(destination_database)
            connection.execute("INSERT INTO items(value) VALUES('destination-only')")
            connection.commit()
            connection.close()
        if divergent_destination_sqlite_topology:
            connection = sqlite3.connect(destination_database)
            connection.execute(
                "CREATE TABLE destination_only(id INTEGER PRIMARY KEY, value TEXT)"
            )
            connection.execute("PRAGMA user_version = 40")
            connection.commit()
            connection.close()
            destination_only_database = self.destination_home / "goals_1.sqlite"
            connection = sqlite3.connect(destination_only_database)
            connection.execute("CREATE TABLE goals(id INTEGER PRIMARY KEY, value TEXT)")
            connection.execute("INSERT INTO goals(value) VALUES('sting-only')")
            connection.commit()
            connection.close()

        self.source_bundle = root / "source-bundle"
        self.destination_bundle = root / "destination-bundle"
        self.source_capture = _capture_with_quiescence(
            self.source_home,
            self.source_bundle,
            role="source",
            host_authority_id=SOURCE_AUTHORITY,
            sqlite_home=self.source_home if source_include_sqlite else None,
            include_auth=True,
            include_sqlite=source_include_sqlite,
            attestation_output=root / "source-capture-quiescence.json",
        )
        self.destination_capture = _capture_with_quiescence(
            self.destination_home,
            self.destination_bundle,
            role="destination",
            host_authority_id=DESTINATION_AUTHORITY,
            sqlite_home=self.destination_home,
            include_auth=True,
            include_sqlite=True,
            attestation_output=root / "destination-capture-quiescence.json",
        )
        self.compatibility = private_state.compile_codex_private_state_plan(
            self.source_bundle,
            self.destination_bundle,
            runtime_authority=private_runtime.current_private_runtime_authority(),
        )
        self.compatibility_path = root / "compatibility-plan.json"
        private_state.write_private_json_noreplace(
            self.compatibility_path,
            self.compatibility,
            protected_directories=(self.source_bundle, self.destination_bundle),
            recorded_protected_directories=(
                self.source_home,
                self.destination_home,
            ),
        )
        self.install_plan = private_apply.compile_codex_private_install_plan(
            self.compatibility,
            self.source_bundle,
            self.destination_bundle,
            accept_compatibility_plan=self.compatibility["plan_sha256"],
            runtime_authority=private_runtime.current_private_runtime_authority(),
        )
        private_apply.validate_codex_private_install_plan(self.install_plan)
        self.install_plan_path = root / "install-plan.json"
        private_state.write_private_json_noreplace(
            self.install_plan_path,
            self.install_plan,
            protected_directories=(self.source_bundle, self.destination_bundle),
            recorded_protected_directories=(
                self.source_home,
                self.destination_home,
            ),
        )
        self.destination_auth_before = (
            self.destination_home / private_state.AUTH_BASENAME
        ).read_bytes()
        self.source_auth = (self.source_home / private_state.AUTH_BASENAME).read_bytes()
        self.sqlite_before = _sqlite_truth(self.destination_home)

    def operation_attestation(
        self,
        purpose: str,
        *,
        prefix: str,
        receipt_path: Path,
        apply_receipt_sha256: str | None = None,
        journal_sha256: str | None = None,
    ) -> tuple[Path, dict]:
        return _create_quiescence_attestation(
            codex_home=self.destination_home,
            sqlite_home=self.destination_home,
            output=self.root / f"{prefix}-{purpose}-quiescence.json",
            operation_output=receipt_path,
            purpose=purpose,
            host_authority_id=DESTINATION_AUTHORITY,
            selected_state_classes=tuple(self.install_plan["selected_state_classes"]),
            plan_sha256=self.install_plan["plan_sha256"],
            apply_receipt_sha256=apply_receipt_sha256,
            journal_sha256=journal_sha256,
        )

    def apply(
        self,
        prefix: str = "apply",
        *,
        attestation: tuple[Path, dict] | None = None,
    ) -> tuple[dict, dict[str, Path]]:
        paths = {
            "rollback": self.root / f"{prefix}-rollback",
            "post": self.root / f"{prefix}-post",
            "recovery": self.root / f"{prefix}-automatic-recovery",
            "journal": self.root / f"{prefix}-journal.jsonl",
            "receipt": self.root / f"{prefix}-receipt.json",
        }
        if attestation is None:
            attestation = self.operation_attestation(
                "apply",
                prefix=prefix,
                receipt_path=paths["receipt"],
            )
        attestation_path, attestation_value = attestation
        receipt = private_apply.apply_codex_private_install(
            self.install_plan_path,
            self.compatibility_path,
            self.source_bundle,
            self.destination_bundle,
            destination_codex_home=self.destination_home,
            destination_sqlite_home=self.destination_home,
            rollback_directory=paths["rollback"],
            post_capture_directory=paths["post"],
            recovery_capture_directory=paths["recovery"],
            journal_path=paths["journal"],
            receipt_path=paths["receipt"],
            accept_plan=self.install_plan["plan_sha256"],
            destination_host_authority_id=DESTINATION_AUTHORITY,
            codex_version=CODEX_VERSION,
            quiescence_attestation_path=attestation_path,
            accept_quiescence_attestation=attestation_value["attestation_sha256"],
            acknowledge_private_apply=True,
        )
        return receipt, paths

    def recover(
        self,
        journal: Path,
        *,
        prefix: str,
        apply_receipt_path: Path | None = None,
        accept_apply_receipt: str | None = None,
        preflight_capture_directory: Path | None = None,
        post_capture_directory: Path | None = None,
        receipt_path: Path | None = None,
    ) -> dict:
        journal_sha256, _ = private_state._hash_regular_file(journal)
        preflight_capture_directory = (
            self.root / f"{prefix}-preflight"
            if preflight_capture_directory is None
            else preflight_capture_directory
        )
        post_capture_directory = (
            self.root / f"{prefix}-post"
            if post_capture_directory is None
            else post_capture_directory
        )
        receipt_path = (
            self.root / f"{prefix}-receipt.json"
            if receipt_path is None
            else receipt_path
        )
        attestation_path, attestation = self.operation_attestation(
            "recover",
            prefix=prefix,
            receipt_path=receipt_path,
            apply_receipt_sha256=accept_apply_receipt,
            journal_sha256=journal_sha256,
        )
        return private_apply.recover_codex_private_mutation(
            self.install_plan_path,
            self.compatibility_path,
            self.source_bundle,
            self.destination_bundle,
            journal,
            apply_receipt_path=apply_receipt_path,
            destination_codex_home=self.destination_home,
            destination_sqlite_home=self.destination_home,
            preflight_capture_directory=preflight_capture_directory,
            post_capture_directory=post_capture_directory,
            receipt_path=receipt_path,
            accept_plan=self.install_plan["plan_sha256"],
            accept_journal=journal_sha256,
            accept_apply_receipt=accept_apply_receipt,
            destination_host_authority_id=DESTINATION_AUTHORITY,
            codex_version=CODEX_VERSION,
            quiescence_attestation_path=attestation_path,
            accept_quiescence_attestation=attestation["attestation_sha256"],
            acknowledge_private_recovery=True,
        )

    def verify(
        self,
        apply_receipt: dict,
        apply_paths: dict[str, Path],
        *,
        prefix: str,
    ) -> tuple[dict, dict[str, Path]]:
        paths = {
            "capture": self.root / f"{prefix}-capture",
            "receipt": self.root / f"{prefix}-receipt.json",
        }
        attestation_path, attestation = self.operation_attestation(
            "verify",
            prefix=prefix,
            receipt_path=paths["receipt"],
            apply_receipt_sha256=apply_receipt["receipt_sha256"],
        )
        receipt = private_apply.verify_codex_private_install(
            self.install_plan_path,
            self.compatibility_path,
            self.source_bundle,
            self.destination_bundle,
            apply_paths["receipt"],
            destination_codex_home=self.destination_home,
            destination_sqlite_home=self.destination_home,
            capture_directory=paths["capture"],
            receipt_path=paths["receipt"],
            accept_plan=self.install_plan["plan_sha256"],
            accept_apply_receipt=apply_receipt["receipt_sha256"],
            destination_host_authority_id=DESTINATION_AUTHORITY,
            codex_version=CODEX_VERSION,
            quiescence_attestation_path=attestation_path,
            accept_quiescence_attestation=attestation["attestation_sha256"],
            acknowledge_private_verify=True,
        )
        return receipt, paths

    def rollback(
        self,
        apply_receipt: dict,
        apply_paths: dict[str, Path],
        *,
        prefix: str,
        attestation: tuple[Path, dict] | None = None,
    ) -> tuple[dict, dict[str, Path]]:
        paths = {
            "preflight": self.root / f"{prefix}-preflight",
            "post": self.root / f"{prefix}-post",
            "recovery": self.root / f"{prefix}-recovery",
            "journal": self.root / f"{prefix}-journal.jsonl",
            "receipt": self.root / f"{prefix}-receipt.json",
        }
        if attestation is None:
            attestation = self.operation_attestation(
                "rollback",
                prefix=prefix,
                receipt_path=paths["receipt"],
                apply_receipt_sha256=apply_receipt["receipt_sha256"],
            )
        attestation_path, attestation_value = attestation
        receipt = private_apply.rollback_codex_private_install(
            self.install_plan_path,
            self.compatibility_path,
            self.source_bundle,
            self.destination_bundle,
            apply_paths["receipt"],
            apply_paths["rollback"],
            destination_codex_home=self.destination_home,
            destination_sqlite_home=self.destination_home,
            preflight_capture_directory=paths["preflight"],
            post_capture_directory=paths["post"],
            recovery_capture_directory=paths["recovery"],
            journal_path=paths["journal"],
            receipt_path=paths["receipt"],
            accept_plan=self.install_plan["plan_sha256"],
            accept_apply_receipt=apply_receipt["receipt_sha256"],
            destination_host_authority_id=DESTINATION_AUTHORITY,
            codex_version=CODEX_VERSION,
            quiescence_attestation_path=attestation_path,
            accept_quiescence_attestation=attestation_value["attestation_sha256"],
            acknowledge_private_rollback=True,
            acknowledge_no_post_apply_writes=True,
        )
        return receipt, paths


class CodexPrivateApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.process_runtime_authority = (
            private_runtime._open_disk_private_runtime_authority_for_tests()
        )
        private_runtime.bind_process_private_runtime_authority(
            self.process_runtime_authority
        )

    def tearDown(self) -> None:
        private_runtime._unbind_process_private_runtime_authority_for_tests(
            self.process_runtime_authority
        )
        self.process_runtime_authority.close()

    def assertChildExit(self, pid: int, expected: int) -> None:
        waited, status = os.waitpid(pid, 0)
        self.assertEqual(waited, pid)
        self.assertTrue(os.WIFEXITED(status))
        self.assertEqual(os.WEXITSTATUS(status), expected)

    def test_private_apply_refuses_root_before_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            auth_before = (
                fixture.destination_home / private_state.AUTH_BASENAME
            ).read_bytes()
            with (
                mock.patch.object(private_apply.os, "geteuid", return_value=0),
                self.assertRaisesRegex(
                    BulkloadError,
                    "private Codex apply refuses root",
                ),
            ):
                fixture.apply(prefix="root-refusal")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )
            for suffix in (
                "rollback",
                "post",
                "automatic-recovery",
                "journal.jsonl",
                "receipt.json",
            ):
                self.assertFalse((fixture.root / f"root-refusal-{suffix}").exists())

    def test_compiler_round_trip_and_full_apply_verify_rollback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            receipt, paths = fixture.apply()
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )
            verify_receipt_path = fixture.root / "verify-receipt.json"
            verify_attestation_path, verify_attestation = fixture.operation_attestation(
                "verify",
                prefix="verify",
                receipt_path=verify_receipt_path,
                apply_receipt_sha256=receipt["receipt_sha256"],
            )
            verify = private_apply.verify_codex_private_install(
                fixture.install_plan_path,
                fixture.compatibility_path,
                fixture.source_bundle,
                fixture.destination_bundle,
                paths["receipt"],
                destination_codex_home=fixture.destination_home,
                destination_sqlite_home=fixture.destination_home,
                capture_directory=fixture.root / "verify-capture",
                receipt_path=verify_receipt_path,
                accept_plan=fixture.install_plan["plan_sha256"],
                accept_apply_receipt=receipt["receipt_sha256"],
                destination_host_authority_id=DESTINATION_AUTHORITY,
                codex_version=CODEX_VERSION,
                quiescence_attestation_path=verify_attestation_path,
                accept_quiescence_attestation=verify_attestation["attestation_sha256"],
                acknowledge_private_verify=True,
            )
            self.assertTrue(verify["offline_verified"])
            rollback, _ = fixture.rollback(
                receipt,
                paths,
                prefix="rollback",
            )
            self.assertTrue(rollback["offline_restored"])
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_apply_receipt_and_journal_bind_full_attestation_and_lock(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            receipt_path = fixture.root / "bound-receipt.json"
            attestation_path, attestation = fixture.operation_attestation(
                "apply",
                prefix="bound",
                receipt_path=receipt_path,
            )
            receipt, paths = fixture.apply(
                prefix="bound",
                attestation=(attestation_path, attestation),
            )
            prepared = json.loads(paths["journal"].read_bytes().splitlines()[0])
            expected_binding = {
                "path": os.fspath(attestation_path.resolve()),
                "attestation": attestation,
            }
            for record in (prepared, receipt):
                self.assertEqual(
                    record["quiescence_attestation"],
                    expected_binding,
                )
                self.assertEqual(
                    record["quiescence_id"],
                    attestation["attestation_id"],
                )
                self.assertEqual(
                    record["bulkload_lock"]["scope_sha256"],
                    attestation["bulkload_lock_scope_sha256"],
                )
                self.assertEqual(
                    record["bulkload_lock"]["scope"],
                    private_quiescence.PRIVATE_BULKLOAD_LOCK_SCOPE,
                )
                self.assertEqual(
                    record["bulkload_lock"]["method"],
                    private_quiescence.PRIVATE_BULKLOAD_LOCK_METHOD,
                )
                self.assertFalse(record["bulkload_lock"]["provider_writer_proof"])
                self.assertEqual(
                    record["plan_sha256"],
                    fixture.install_plan["plan_sha256"],
                )
            self.assertEqual(
                fixture.install_plan["runtime_authority"],
                fixture.compatibility["runtime_authority"],
            )
            runtime_tampered_plan = json.loads(json.dumps(fixture.install_plan))
            runtime_tampered_plan["runtime_authority"]["policy_sha256"] = "0" * 64
            runtime_tampered_plan["plan_sha256"] = private_apply.object_digest(
                runtime_tampered_plan,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "receipt differs from accepted plan",
            ):
                private_apply._validate_apply_receipt_against_plan(
                    receipt,
                    runtime_tampered_plan,
                )

    def test_apply_receipt_rejects_attestation_substitution_and_moved_path(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            receipt_path = fixture.root / "bound-receipt.json"
            foreign_home = fixture.root / "foreign-destination-home"
            foreign_home.mkdir(mode=0o700)
            _write_auth(foreign_home, "foreign")
            shutil.copy2(
                fixture.destination_home / "state_5.sqlite",
                foreign_home / "state_5.sqlite",
            )
            foreign_attestation_path, foreign_attestation = (
                _create_quiescence_attestation(
                    codex_home=foreign_home,
                    sqlite_home=foreign_home,
                    output=fixture.root / "foreign-root-quiescence.json",
                    operation_output=receipt_path,
                    purpose="apply",
                    host_authority_id=DESTINATION_AUTHORITY,
                    selected_state_classes=tuple(
                        fixture.install_plan["selected_state_classes"]
                    ),
                    plan_sha256=fixture.install_plan["plan_sha256"],
                )
            )
            receipt, paths = fixture.apply(
                prefix="bound",
            )
            self.assertEqual(paths["receipt"], receipt_path)

            different_output_path, different_output = fixture.operation_attestation(
                "apply",
                prefix="different-output",
                receipt_path=fixture.root / "different-receipt.json",
            )
            forged_output = json.loads(json.dumps(receipt))
            forged_output["quiescence_id"] = different_output["attestation_id"]
            forged_output["quiescence_attestation"] = {
                "path": os.fspath(different_output_path.resolve()),
                "attestation": different_output,
            }
            forged_output["receipt_sha256"] = private_apply.object_digest(
                forged_output,
                "receipt_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "claims differ",
            ):
                private_apply.validate_codex_private_apply_receipt(forged_output)

            forged_root = json.loads(json.dumps(receipt))
            forged_root["quiescence_id"] = foreign_attestation["attestation_id"]
            forged_root["quiescence_attestation"] = {
                "path": os.fspath(foreign_attestation_path.resolve()),
                "attestation": foreign_attestation,
            }
            forged_root["receipt_sha256"] = private_apply.object_digest(
                forged_root,
                "receipt_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "claims differ",
            ):
                private_apply.validate_codex_private_apply_receipt(forged_root)

            moved = fixture.root / "moved-apply-receipt.json"
            paths["receipt"].rename(moved)
            with self.assertRaisesRegex(
                BulkloadError,
                "actual authority",
            ):
                private_apply.read_codex_private_apply_receipt(moved)

    def test_apply_journal_rejects_same_plan_different_output_attestation(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply(prefix="journal-binding")
            attestation_path, attestation = fixture.operation_attestation(
                "apply",
                prefix="journal-foreign-output",
                receipt_path=fixture.root / "journal-foreign-receipt.json",
            )
            events = [
                json.loads(line) for line in paths["journal"].read_text().splitlines()
            ]
            events[0]["quiescence_id"] = attestation["attestation_id"]
            events[0]["quiescence_attestation"] = {
                "path": os.fspath(attestation_path.resolve()),
                "attestation": attestation,
            }
            with self.assertRaisesRegex(
                BulkloadError,
                "claims differ",
            ):
                private_apply._validate_private_journal_events(
                    _rehash_journal_events(events),
                )

    def test_manual_rollback_rejects_same_plan_rehashed_foreign_journal(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            foreign_receipt, foreign_paths = fixture.apply(prefix="foreign-run")
            fixture.rollback(
                foreign_receipt,
                foreign_paths,
                prefix="foreign-run-rollback",
            )
            current_receipt, current_paths = fixture.apply(prefix="current-run")

            foreign_journal = fixture.root / "rehashed-foreign-journal.jsonl"
            foreign_events = [
                json.loads(line)
                for line in foreign_paths["journal"].read_text().splitlines()
            ]
            foreign_events[0]["journal_path"] = os.fspath(foreign_journal.resolve())
            foreign_events = _rehash_journal_events(foreign_events)
            private_apply._validate_private_journal_events(foreign_events)
            foreign_journal.write_bytes(
                b"".join(
                    private_apply.canonical_bytes(event) + b"\n"
                    for event in foreign_events
                )
            )
            foreign_journal.chmod(0o600)
            foreign_sha256, foreign_size = private_state._hash_regular_file(
                foreign_journal
            )

            forged_receipt = json.loads(json.dumps(current_receipt))
            forged_receipt["journal"] = {
                "path": os.fspath(foreign_journal.resolve()),
                "sha256": foreign_sha256,
                "size": foreign_size,
            }
            forged_receipt["receipt_sha256"] = private_apply.object_digest(
                forged_receipt,
                "receipt_sha256",
            )
            with current_paths["receipt"].open("wb") as handle:
                handle.write(private_apply.canonical_bytes(forged_receipt) + b"\n")
                handle.flush()
                os.fsync(handle.fileno())
            private_apply.read_codex_private_apply_receipt(current_paths["receipt"])

            auth_path = fixture.destination_home / private_state.AUTH_BASENAME
            auth_before = (
                auth_path.read_bytes(),
                private_state._identity(auth_path.stat()),
            )
            sqlite_before = _sqlite_truth(fixture.destination_home)
            with self.assertRaisesRegex(
                BulkloadError,
                "private apply journal authority differs from receipt",
            ):
                fixture.rollback(
                    forged_receipt,
                    current_paths,
                    prefix="reject-foreign-journal",
                )
            self.assertEqual(
                (
                    auth_path.read_bytes(),
                    private_state._identity(auth_path.stat()),
                ),
                auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                sqlite_before,
            )

    def test_operation_receipts_reject_attestation_substitution_and_moved_path(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            apply_receipt, apply_paths = fixture.apply(prefix="binding-apply")
            verify_receipt, verify_paths = fixture.verify(
                apply_receipt,
                apply_paths,
                prefix="binding-verify",
            )
            rollback_receipt, rollback_paths = fixture.rollback(
                apply_receipt,
                apply_paths,
                prefix="binding-rollback",
            )
            recovery_receipt = fixture.recover(
                rollback_paths["journal"],
                prefix="binding-recovery",
                apply_receipt_path=apply_paths["receipt"],
                accept_apply_receipt=apply_receipt["receipt_sha256"],
            )
            recovery_path = fixture.root / "binding-recovery-receipt.json"

            cases = (
                (
                    "verify",
                    verify_receipt,
                    verify_paths["receipt"],
                    private_apply.validate_codex_private_verify_receipt,
                    private_apply.read_codex_private_verify_receipt,
                ),
                (
                    "rollback",
                    rollback_receipt,
                    rollback_paths["receipt"],
                    private_apply.validate_codex_private_rollback_receipt,
                    private_apply.read_codex_private_rollback_receipt,
                ),
                (
                    "recover",
                    recovery_receipt,
                    recovery_path,
                    private_apply.validate_codex_private_recovery_receipt,
                    private_apply.read_codex_private_recovery_receipt,
                ),
            )
            for purpose, receipt, path, validator, reader in cases:
                with self.subTest(purpose=purpose, case="substitution"):
                    accepted = receipt["quiescence_attestation"]["attestation"][
                        "accepted_inputs"
                    ]
                    foreign_path, foreign = _create_quiescence_attestation(
                        codex_home=fixture.destination_home,
                        sqlite_home=fixture.destination_home,
                        output=fixture.root / f"{purpose}-foreign-quiescence.json",
                        operation_output=(
                            fixture.root / f"{purpose}-foreign-receipt.json"
                        ),
                        purpose=purpose,
                        host_authority_id=DESTINATION_AUTHORITY,
                        selected_state_classes=tuple(
                            fixture.install_plan["selected_state_classes"]
                        ),
                        plan_sha256=accepted["plan_sha256"],
                        apply_receipt_sha256=accepted["apply_receipt_sha256"],
                        journal_sha256=accepted["journal_sha256"],
                    )
                    forged = json.loads(json.dumps(receipt))
                    forged["quiescence_id"] = foreign["attestation_id"]
                    forged["quiescence_attestation"] = {
                        "path": os.fspath(foreign_path.resolve()),
                        "attestation": foreign,
                    }
                    forged["receipt_sha256"] = private_apply.object_digest(
                        forged,
                        "receipt_sha256",
                    )
                    with self.assertRaisesRegex(BulkloadError, "claims differ"):
                        validator(forged)

                with self.subTest(purpose=purpose, case="moved-path"):
                    moved = fixture.root / f"{purpose}-moved-receipt.json"
                    path.rename(moved)
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "actual authority",
                    ):
                        reader(moved)

    def test_stale_attestation_and_cross_purpose_reuse_fail_closed(self) -> None:
        with (
            self.subTest(case="stale-observation"),
            tempfile.TemporaryDirectory() as directory,
        ):
            fixture = PrivateApplyFixture(Path(directory))
            stale = fixture.operation_attestation(
                "apply",
                prefix="stale",
                receipt_path=fixture.root / "stale-receipt.json",
            )
            _write_auth(fixture.destination_home, "changed-after-attestation")
            with self.assertRaisesRegex(
                BulkloadError,
                "quiescence auth observation no longer matches",
            ):
                fixture.apply(prefix="stale", attestation=stale)
            self.assertFalse((fixture.root / "stale-receipt.json").exists())
            self.assertFalse((fixture.root / "stale-journal.jsonl").exists())

        with (
            self.subTest(case="cross-purpose-reuse"),
            tempfile.TemporaryDirectory() as directory,
        ):
            fixture = PrivateApplyFixture(Path(directory))
            apply_attestation = fixture.operation_attestation(
                "apply",
                prefix="reuse",
                receipt_path=fixture.root / "reuse-receipt.json",
            )
            apply_receipt, apply_paths = fixture.apply(
                prefix="reuse",
                attestation=apply_attestation,
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "purpose differs from expected",
            ):
                private_apply.verify_codex_private_install(
                    fixture.install_plan_path,
                    fixture.compatibility_path,
                    fixture.source_bundle,
                    fixture.destination_bundle,
                    apply_paths["receipt"],
                    destination_codex_home=fixture.destination_home,
                    destination_sqlite_home=fixture.destination_home,
                    capture_directory=fixture.root / "reuse-verify-capture",
                    receipt_path=fixture.root / "reuse-verify-receipt.json",
                    accept_plan=fixture.install_plan["plan_sha256"],
                    accept_apply_receipt=apply_receipt["receipt_sha256"],
                    destination_host_authority_id=DESTINATION_AUTHORITY,
                    codex_version=CODEX_VERSION,
                    quiescence_attestation_path=apply_attestation[0],
                    accept_quiescence_attestation=apply_attestation[1][
                        "attestation_sha256"
                    ],
                    acknowledge_private_verify=True,
                )
            self.assertFalse((fixture.root / "reuse-verify-receipt.json").exists())

    def test_auth_apply_preserves_divergent_destination_sqlite_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(
                Path(directory),
                divergent_destination_sqlite=True,
            )
            self.assertTrue(fixture.install_plan["ready_for_apply"])
            self.assertEqual(fixture.install_plan["blockers"], [])
            self.assertEqual(
                [item["action"] for item in fixture.install_plan["sqlite_families"]],
                ["preserve-destination-exact"],
            )
            receipt, _ = fixture.apply(prefix="divergent-sqlite")
            self.assertTrue(receipt["offline_verified"])
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_auth_only_source_and_auth_sqlite_destination_are_ready_and_preserved(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(
                Path(directory),
                source_include_sqlite=False,
                divergent_destination_sqlite_topology=True,
            )
            self.assertEqual(
                fixture.source_capture["selected_state_classes"],
                ["auth"],
            )
            self.assertEqual(
                fixture.destination_capture["selected_state_classes"],
                ["auth", "sqlite"],
            )
            self.assertEqual(
                fixture.install_plan["selected_state_classes"],
                ["auth", "sqlite"],
            )
            self.assertTrue(fixture.install_plan["ready_for_apply"])
            self.assertEqual(fixture.install_plan["blockers"], [])
            self.assertEqual(
                fixture.install_plan["input_state_classes"],
                {
                    "source": ["auth"],
                    "destination": ["auth", "sqlite"],
                },
            )
            self.assertEqual(
                fixture.source_capture["quiescence"]["selected_state_classes"],
                ["auth"],
            )
            sqlite_before = _sqlite_truth(fixture.destination_home)
            receipt, paths = fixture.apply(prefix="split-state-classes")
            self.assertTrue(receipt["offline_verified"])
            self.assertFalse(fixture.install_plan["sqlite_union_ready"])
            self.assertEqual(
                receipt["quiescence_attestation"]["attestation"][
                    "selected_state_classes"
                ],
                ["auth", "sqlite"],
            )
            self.assertTrue(
                all(
                    "source_sha256" not in family
                    for family in receipt["sqlite_families"]
                )
            )
            prepared = json.loads(paths["journal"].read_bytes().splitlines()[0])
            self.assertEqual(prepared["sqlite_mutations"], 0)
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(_sqlite_truth(fixture.destination_home), sqlite_before)

    def test_auth_apply_ignores_source_sqlite_topology_for_destination_preservation(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(
                Path(directory),
                divergent_destination_sqlite_topology=True,
            )
            compatibility_codes = {
                blocker["code"] for blocker in fixture.compatibility["blockers"]
            }
            self.assertIn("sqlite-family-set-mismatch", compatibility_codes)
            self.assertIn("sqlite-composer-not-implemented", compatibility_codes)
            self.assertTrue(fixture.install_plan["ready_for_apply"])
            self.assertEqual(fixture.install_plan["blockers"], [])
            self.assertEqual(
                [
                    (item["basename"], item["action"])
                    for item in fixture.install_plan["sqlite_families"]
                ],
                [
                    ("goals_1.sqlite", "preserve-destination-exact"),
                    ("state_5.sqlite", "preserve-destination-exact"),
                ],
            )
            receipt, _ = fixture.apply(prefix="destination-topology")
            self.assertTrue(receipt["offline_verified"])
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_auth_only_to_auth_only_mutation_is_rejected(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(BulkloadError, "matrix is unsupported"):
                _compile_state_class_matrix(
                    Path(directory),
                    source_auth=True,
                    source_sqlite=False,
                    destination_auth=True,
                    destination_sqlite=False,
                )

    def test_inverse_and_destination_without_auth_matrices_are_rejected(
        self,
    ) -> None:
        cases = (
            {
                "source_auth": True,
                "source_sqlite": True,
                "destination_auth": True,
                "destination_sqlite": False,
            },
            {
                "source_auth": True,
                "source_sqlite": False,
                "destination_auth": False,
                "destination_sqlite": True,
            },
        )
        for index, case in enumerate(cases):
            with (
                self.subTest(case=case),
                tempfile.TemporaryDirectory() as directory,
                self.assertRaisesRegex(BulkloadError, "matrix is unsupported"),
            ):
                _compile_state_class_matrix(
                    Path(directory) / f"case-{index}",
                    **case,
                )

    def test_input_state_class_claim_tampering_is_rejected_against_inputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(
                Path(directory),
                source_include_sqlite=False,
            )
            tampered = json.loads(json.dumps(fixture.install_plan))
            tampered["input_state_classes"]["source"] = ["auth", "sqlite"]
            tampered["plan_sha256"] = private_apply.object_digest(
                tampered,
                "plan_sha256",
            )
            private_apply.validate_codex_private_install_plan(tampered)
            with self.assertRaisesRegex(
                BulkloadError,
                "differs from exact input semantics",
            ):
                private_apply.validate_codex_private_install_plan_against_inputs(
                    tampered,
                    fixture.compatibility,
                    fixture.source_bundle,
                    fixture.destination_bundle,
                )
            digest_tampered = json.loads(json.dumps(fixture.install_plan))
            digest_tampered["input_state_classes"]["source"] = ["auth", "sqlite"]
            with self.assertRaisesRegex(BulkloadError, "digest mismatch"):
                private_apply.validate_codex_private_install_plan(digest_tampered)

    def test_install_plan_publication_is_runtime_pinned(self) -> None:
        class RuntimeGate:
            def __init__(self, record: dict, fail_at: int) -> None:
                self.record = json.loads(json.dumps(record))
                self.fail_at = fail_at
                self.calls = 0

            def revalidate(self) -> None:
                self.calls += 1
                if self.calls == self.fail_at:
                    raise BulkloadError("injected runtime publication drift")

            def __enter__(self):
                return self

            def __exit__(self, *_: object) -> None:
                return None

        for fail_at, published in ((1, False), (2, True)):
            with (
                self.subTest(fail_at=fail_at),
                tempfile.TemporaryDirectory() as directory,
            ):
                fixture = PrivateApplyFixture(Path(directory))
                output = fixture.root / f"runtime-plan-{fail_at}.json"
                gate = RuntimeGate(
                    fixture.compatibility["runtime_authority"],
                    fail_at,
                )
                with mock.patch.object(
                    private_runtime,
                    "open_pinned_private_runtime_authority",
                    return_value=gate,
                ):
                    result = _run_cli(
                        [
                            "codex-private-install-plan",
                            "--compatibility-plan",
                            os.fspath(fixture.compatibility_path),
                            "--source-bundle",
                            os.fspath(fixture.source_bundle),
                            "--destination-bundle",
                            os.fspath(fixture.destination_bundle),
                            "--output",
                            os.fspath(output),
                            "--accept-compatibility-plan",
                            fixture.compatibility["plan_sha256"],
                        ]
                    )
                self.assertEqual(result, 2)
                self.assertEqual(output.exists(), published)

    def test_runtime_drift_before_or_after_apply_commit_restores_before_state(
        self,
    ) -> None:
        original_revalidate = private_runtime.PinnedPrivateRuntimeAuthority.revalidate
        for fail_at in (3, 4, 5):
            with (
                self.subTest(fail_at=fail_at),
                tempfile.TemporaryDirectory() as directory,
            ):
                fixture = PrivateApplyFixture(Path(directory))
                calls = 0

                def gated_revalidate(context) -> None:
                    nonlocal calls
                    calls += 1
                    if calls == fail_at:
                        raise BulkloadError("injected live runtime drift")
                    original_revalidate(context)

                with mock.patch.object(
                    private_runtime.PinnedPrivateRuntimeAuthority,
                    "revalidate",
                    gated_revalidate,
                ):
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "injected live runtime drift",
                    ):
                        fixture.apply(prefix=f"runtime-drift-{fail_at}")
                self.assertEqual(
                    (
                        fixture.destination_home / private_state.AUTH_BASENAME
                    ).read_bytes(),
                    fixture.destination_auth_before,
                )
                self.assertEqual(
                    _sqlite_truth(fixture.destination_home),
                    fixture.sqlite_before,
                )
                self.assertFalse(
                    (fixture.root / f"runtime-drift-{fail_at}-receipt.json").exists()
                )

    def test_runtime_drift_after_rollback_commit_reinstalls_applied_state(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            apply_receipt, apply_paths = fixture.apply(prefix="runtime-rollback-apply")
            original_revalidate = (
                private_runtime.PinnedPrivateRuntimeAuthority.revalidate
            )
            calls = 0

            def gated_revalidate(context) -> None:
                nonlocal calls
                calls += 1
                if calls == 4:
                    raise BulkloadError("injected rollback runtime drift")
                original_revalidate(context)

            with mock.patch.object(
                private_runtime.PinnedPrivateRuntimeAuthority,
                "revalidate",
                gated_revalidate,
            ):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "injected rollback runtime drift",
                ):
                    fixture.rollback(
                        apply_receipt,
                        apply_paths,
                        prefix="runtime-rollback",
                    )
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )
            self.assertFalse((fixture.root / "runtime-rollback-receipt.json").exists())

    def test_transport_reconstructed_source_bundle_remains_portable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            transported_source = fixture.root / "transported-source-bundle"
            shutil.copytree(fixture.source_bundle, transported_source)
            transported_source.chmod(0o700)
            compatibility = private_state.compile_codex_private_state_plan(
                transported_source,
                fixture.destination_bundle,
                runtime_authority=(private_runtime.current_private_runtime_authority()),
            )
            compatibility_path = fixture.root / "transported-compatibility.json"
            private_state.write_private_json_noreplace(
                compatibility_path,
                compatibility,
                protected_directories=(
                    transported_source,
                    fixture.destination_bundle,
                ),
                recorded_protected_directories=(
                    fixture.source_home,
                    fixture.destination_home,
                ),
            )
            install_plan = private_apply.compile_codex_private_install_plan(
                compatibility,
                transported_source,
                fixture.destination_bundle,
                accept_compatibility_plan=compatibility["plan_sha256"],
                runtime_authority=(private_runtime.current_private_runtime_authority()),
            )
            install_plan_path = fixture.root / "transported-install-plan.json"
            private_state.write_private_json_noreplace(
                install_plan_path,
                install_plan,
                protected_directories=(
                    transported_source,
                    fixture.destination_bundle,
                ),
                recorded_protected_directories=(
                    fixture.source_home,
                    fixture.destination_home,
                ),
            )
            paths = {
                "rollback": fixture.root / "transported-rollback",
                "post": fixture.root / "transported-post",
                "recovery": fixture.root / "transported-recovery",
                "journal": fixture.root / "transported-journal.jsonl",
                "receipt": fixture.root / "transported-receipt.json",
            }
            attestation_path, attestation = _create_quiescence_attestation(
                codex_home=fixture.destination_home,
                sqlite_home=fixture.destination_home,
                output=fixture.root / "transported-apply-quiescence.json",
                operation_output=paths["receipt"],
                purpose="apply",
                host_authority_id=DESTINATION_AUTHORITY,
                selected_state_classes=tuple(install_plan["selected_state_classes"]),
                plan_sha256=install_plan["plan_sha256"],
            )
            receipt = private_apply.apply_codex_private_install(
                install_plan_path,
                compatibility_path,
                transported_source,
                fixture.destination_bundle,
                destination_codex_home=fixture.destination_home,
                destination_sqlite_home=fixture.destination_home,
                rollback_directory=paths["rollback"],
                post_capture_directory=paths["post"],
                recovery_capture_directory=paths["recovery"],
                journal_path=paths["journal"],
                receipt_path=paths["receipt"],
                accept_plan=install_plan["plan_sha256"],
                destination_host_authority_id=DESTINATION_AUTHORITY,
                codex_version=CODEX_VERSION,
                quiescence_attestation_path=attestation_path,
                accept_quiescence_attestation=attestation["attestation_sha256"],
                acknowledge_private_apply=True,
            )
            self.assertTrue(receipt["offline_verified"])
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

    def test_cli_full_private_auth_lifecycle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(
                Path(directory),
                divergent_destination_sqlite=True,
            )
            install_plan_path = fixture.root / "cli-install-plan.json"
            self.assertEqual(
                _run_cli(
                    [
                        "codex-private-install-plan",
                        "--compatibility-plan",
                        str(fixture.compatibility_path),
                        "--source-bundle",
                        str(fixture.source_bundle),
                        "--destination-bundle",
                        str(fixture.destination_bundle),
                        "--accept-compatibility-plan",
                        fixture.compatibility["plan_sha256"],
                        "--output",
                        str(install_plan_path),
                    ]
                ),
                0,
            )
            install_plan = private_apply.read_codex_private_install_plan(
                install_plan_path
            )
            apply_paths = {
                "rollback": fixture.root / "cli-apply-rollback",
                "post": fixture.root / "cli-apply-post",
                "recovery": fixture.root / "cli-apply-recovery",
                "journal": fixture.root / "cli-apply-journal.jsonl",
                "receipt": fixture.root / "cli-apply-receipt.json",
            }
            apply_attestation_path, apply_attestation = _create_quiescence_attestation(
                codex_home=fixture.destination_home,
                sqlite_home=fixture.destination_home,
                output=fixture.root / "cli-apply-quiescence.json",
                operation_output=apply_paths["receipt"],
                purpose="apply",
                host_authority_id=DESTINATION_AUTHORITY,
                selected_state_classes=tuple(install_plan["selected_state_classes"]),
                plan_sha256=install_plan["plan_sha256"],
            )
            self.assertEqual(
                _run_cli(
                    [
                        "codex-private-apply",
                        "--install-plan",
                        str(install_plan_path),
                        "--compatibility-plan",
                        str(fixture.compatibility_path),
                        "--source-bundle",
                        str(fixture.source_bundle),
                        "--destination-before-bundle",
                        str(fixture.destination_bundle),
                        "--destination-codex-home",
                        str(fixture.destination_home),
                        "--destination-sqlite-home",
                        str(fixture.destination_home),
                        "--rollback-directory",
                        str(apply_paths["rollback"]),
                        "--post-capture-directory",
                        str(apply_paths["post"]),
                        "--recovery-capture-directory",
                        str(apply_paths["recovery"]),
                        "--journal",
                        str(apply_paths["journal"]),
                        "--receipt",
                        str(apply_paths["receipt"]),
                        "--accept-plan",
                        install_plan["plan_sha256"],
                        "--destination-host-authority-id",
                        DESTINATION_AUTHORITY,
                        "--codex-version",
                        CODEX_VERSION,
                        "--quiescence-attestation",
                        str(apply_attestation_path),
                        "--accept-quiescence-attestation",
                        apply_attestation["attestation_sha256"],
                        "--acknowledge-private-apply",
                    ]
                ),
                0,
            )
            apply_receipt = private_apply.read_codex_private_apply_receipt(
                apply_paths["receipt"]
            )
            verify_receipt_path = fixture.root / "cli-verify-receipt.json"
            verify_attestation_path, verify_attestation = (
                _create_quiescence_attestation(
                    codex_home=fixture.destination_home,
                    sqlite_home=fixture.destination_home,
                    output=fixture.root / "cli-verify-quiescence.json",
                    operation_output=verify_receipt_path,
                    purpose="verify",
                    host_authority_id=DESTINATION_AUTHORITY,
                    selected_state_classes=tuple(
                        install_plan["selected_state_classes"]
                    ),
                    plan_sha256=install_plan["plan_sha256"],
                    apply_receipt_sha256=apply_receipt["receipt_sha256"],
                )
            )
            self.assertEqual(
                _run_cli(
                    [
                        "codex-private-verify",
                        "--install-plan",
                        str(install_plan_path),
                        "--compatibility-plan",
                        str(fixture.compatibility_path),
                        "--source-bundle",
                        str(fixture.source_bundle),
                        "--destination-before-bundle",
                        str(fixture.destination_bundle),
                        "--apply-receipt",
                        str(apply_paths["receipt"]),
                        "--destination-codex-home",
                        str(fixture.destination_home),
                        "--destination-sqlite-home",
                        str(fixture.destination_home),
                        "--capture-directory",
                        str(fixture.root / "cli-verify-capture"),
                        "--receipt",
                        str(verify_receipt_path),
                        "--accept-plan",
                        install_plan["plan_sha256"],
                        "--accept-apply-receipt",
                        apply_receipt["receipt_sha256"],
                        "--destination-host-authority-id",
                        DESTINATION_AUTHORITY,
                        "--codex-version",
                        CODEX_VERSION,
                        "--quiescence-attestation",
                        str(verify_attestation_path),
                        "--accept-quiescence-attestation",
                        verify_attestation["attestation_sha256"],
                        "--acknowledge-private-verify",
                    ]
                ),
                0,
            )
            journal_sha256, _ = private_state._hash_regular_file(apply_paths["journal"])
            recovery_receipt_path = fixture.root / "cli-recover-receipt.json"
            recovery_attestation_path, recovery_attestation = (
                _create_quiescence_attestation(
                    codex_home=fixture.destination_home,
                    sqlite_home=fixture.destination_home,
                    output=fixture.root / "cli-recover-quiescence.json",
                    operation_output=recovery_receipt_path,
                    purpose="recover",
                    host_authority_id=DESTINATION_AUTHORITY,
                    selected_state_classes=tuple(
                        install_plan["selected_state_classes"]
                    ),
                    plan_sha256=install_plan["plan_sha256"],
                    journal_sha256=journal_sha256,
                )
            )
            self.assertEqual(
                _run_cli(
                    [
                        "codex-private-recover",
                        "--install-plan",
                        str(install_plan_path),
                        "--compatibility-plan",
                        str(fixture.compatibility_path),
                        "--source-bundle",
                        str(fixture.source_bundle),
                        "--destination-before-bundle",
                        str(fixture.destination_bundle),
                        "--journal",
                        str(apply_paths["journal"]),
                        "--destination-codex-home",
                        str(fixture.destination_home),
                        "--destination-sqlite-home",
                        str(fixture.destination_home),
                        "--preflight-capture-directory",
                        str(fixture.root / "cli-recover-preflight"),
                        "--post-capture-directory",
                        str(fixture.root / "cli-recover-post"),
                        "--receipt",
                        str(recovery_receipt_path),
                        "--accept-plan",
                        install_plan["plan_sha256"],
                        "--accept-journal",
                        journal_sha256,
                        "--destination-host-authority-id",
                        DESTINATION_AUTHORITY,
                        "--codex-version",
                        CODEX_VERSION,
                        "--quiescence-attestation",
                        str(recovery_attestation_path),
                        "--accept-quiescence-attestation",
                        recovery_attestation["attestation_sha256"],
                        "--acknowledge-private-recovery",
                    ]
                ),
                0,
            )
            rollback_paths = {
                "preflight": fixture.root / "cli-rollback-preflight",
                "post": fixture.root / "cli-rollback-post",
                "recovery": fixture.root / "cli-rollback-recovery",
                "journal": fixture.root / "cli-rollback-journal.jsonl",
                "receipt": fixture.root / "cli-rollback-receipt.json",
            }
            rollback_attestation_path, rollback_attestation = (
                _create_quiescence_attestation(
                    codex_home=fixture.destination_home,
                    sqlite_home=fixture.destination_home,
                    output=fixture.root / "cli-rollback-quiescence.json",
                    operation_output=rollback_paths["receipt"],
                    purpose="rollback",
                    host_authority_id=DESTINATION_AUTHORITY,
                    selected_state_classes=tuple(
                        install_plan["selected_state_classes"]
                    ),
                    plan_sha256=install_plan["plan_sha256"],
                    apply_receipt_sha256=apply_receipt["receipt_sha256"],
                )
            )
            self.assertEqual(
                _run_cli(
                    [
                        "codex-private-rollback",
                        "--install-plan",
                        str(install_plan_path),
                        "--compatibility-plan",
                        str(fixture.compatibility_path),
                        "--source-bundle",
                        str(fixture.source_bundle),
                        "--destination-before-bundle",
                        str(fixture.destination_bundle),
                        "--apply-receipt",
                        str(apply_paths["receipt"]),
                        "--rollback-directory",
                        str(apply_paths["rollback"]),
                        "--destination-codex-home",
                        str(fixture.destination_home),
                        "--destination-sqlite-home",
                        str(fixture.destination_home),
                        "--preflight-capture-directory",
                        str(rollback_paths["preflight"]),
                        "--post-capture-directory",
                        str(rollback_paths["post"]),
                        "--recovery-capture-directory",
                        str(rollback_paths["recovery"]),
                        "--journal",
                        str(rollback_paths["journal"]),
                        "--receipt",
                        str(rollback_paths["receipt"]),
                        "--accept-plan",
                        install_plan["plan_sha256"],
                        "--accept-apply-receipt",
                        apply_receipt["receipt_sha256"],
                        "--destination-host-authority-id",
                        DESTINATION_AUTHORITY,
                        "--codex-version",
                        CODEX_VERSION,
                        "--quiescence-attestation",
                        str(rollback_attestation_path),
                        "--accept-quiescence-attestation",
                        rollback_attestation["attestation_sha256"],
                        "--acknowledge-private-rollback",
                        "--acknowledge-no-post-apply-writes",
                    ]
                ),
                0,
            )
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

    def test_apply_recovery_restores_unreceipted_after_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply()
            paths["receipt"].unlink()
            recovered = fixture.recover(paths["journal"], prefix="recover-after")
            self.assertEqual(recovered["decision"], "restored-before")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_apply_recovery_confirms_unreceipted_before_state_and_cleans_stage(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            with mock.patch.object(
                private_apply,
                "_commit_staged_auth",
                side_effect=BulkloadError("injected pre-rename stop"),
            ):
                with self.assertRaises(BulkloadError):
                    fixture.apply(prefix="interrupted")
            journal = fixture.root / "interrupted-journal.jsonl"
            recovered = fixture.recover(journal, prefix="recover-before")
            self.assertEqual(recovered["decision"], "confirmed-before-noop")
            self.assertTrue(recovered["cleaned_staging_leaves"])
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_recovery_confirms_valid_committed_receipt_without_inversion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            receipt, paths = fixture.apply()
            recovered = fixture.recover(paths["journal"], prefix="recover-committed")
            self.assertEqual(recovered["decision"], "confirmed-committed")
            self.assertEqual(
                recovered["original_receipt"]["sha256"],
                receipt["receipt_sha256"],
            )
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_rollback_recovery_reinstalls_unreceipted_before_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            apply_receipt, apply_paths = fixture.apply()
            _, rollback_paths = fixture.rollback(
                apply_receipt,
                apply_paths,
                prefix="rollback",
            )
            rollback_paths["receipt"].unlink()
            recovered = fixture.recover(
                rollback_paths["journal"],
                prefix="recover-rollback",
                apply_receipt_path=apply_paths["receipt"],
                accept_apply_receipt=apply_receipt["receipt_sha256"],
            )
            self.assertEqual(recovered["decision"], "restored-before")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_before_apply_rename_recovers_without_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            attestation = fixture.operation_attestation(
                "apply",
                prefix="fork-before",
                receipt_path=fixture.root / "fork-before-receipt.json",
            )
            pid = os.fork()
            if pid == 0:
                private_apply._commit_staged_auth = lambda *args, **kwargs: os._exit(92)
                fixture.apply(prefix="fork-before", attestation=attestation)
                os._exit(99)
            self.assertChildExit(pid, 92)
            journal = fixture.root / "fork-before-journal.jsonl"
            recovered = fixture.recover(journal, prefix="recover-fork-before")
            self.assertEqual(recovered["decision"], "confirmed-before-noop")
            self.assertTrue(recovered["cleaned_staging_leaves"])
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_after_apply_rename_restores_before_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            attestation = fixture.operation_attestation(
                "apply",
                prefix="fork-after",
                receipt_path=fixture.root / "fork-after-receipt.json",
            )
            pid = os.fork()
            if pid == 0:
                original = private_apply._commit_staged_auth

                def commit_then_exit(*args, **kwargs) -> None:
                    original(*args, **kwargs)
                    os._exit(91)

                private_apply._commit_staged_auth = commit_then_exit
                fixture.apply(prefix="fork-after", attestation=attestation)
                os._exit(99)
            self.assertChildExit(pid, 91)
            journal = fixture.root / "fork-after-journal.jsonl"
            recovered = fixture.recover(journal, prefix="recover-fork-after")
            self.assertEqual(recovered["decision"], "restored-before")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_after_recovery_rename_retries_without_inversion(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply(prefix="recovery-rename")
            paths["receipt"].unlink()
            pid = os.fork()
            if pid == 0:
                original = private_apply._commit_staged_auth

                def commit_then_exit(*args, **kwargs) -> None:
                    original(*args, **kwargs)
                    os._exit(90)

                private_apply._commit_staged_auth = commit_then_exit
                fixture.recover(
                    paths["journal"],
                    prefix="recovery-rename-crash",
                )
                os._exit(99)
            self.assertChildExit(pid, 90)
            events = [
                json.loads(line) for line in paths["journal"].read_text().splitlines()
            ]
            self.assertEqual(events[-1]["event"], "recovery-mutation-intent")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

            recovered = fixture.recover(
                paths["journal"],
                prefix="recovery-rename-retry",
            )
            self.assertEqual(recovered["decision"], "confirmed-before-noop")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_during_journal_append_recovers_from_bound_tail(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            attestation = fixture.operation_attestation(
                "apply",
                prefix="fork-torn-journal",
                receipt_path=fixture.root / "fork-torn-journal-receipt.json",
            )
            pid = os.fork()
            if pid == 0:
                original = private_apply._write_all
                calls = 0

                def write_partial_second_event(
                    descriptor: int,
                    payload: bytes,
                ) -> None:
                    nonlocal calls
                    calls += 1
                    if calls == 2:
                        written = os.write(
                            descriptor,
                            payload[: max(1, len(payload) // 2)],
                        )
                        if written < 1:
                            os._exit(98)
                        os.fsync(descriptor)
                        os._exit(95)
                    original(descriptor, payload)

                private_apply._write_all = write_partial_second_event
                fixture.apply(
                    prefix="fork-torn-journal",
                    attestation=attestation,
                )
                os._exit(99)
            self.assertChildExit(pid, 95)
            journal = fixture.root / "fork-torn-journal-journal.jsonl"
            self.assertFalse(journal.read_bytes().endswith(b"\n"))
            recovered = fixture.recover(
                journal,
                prefix="recover-fork-torn-journal",
            )
            self.assertEqual(recovered["decision"], "confirmed-before-noop")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_during_recovery_event_append_repairs_bound_tail(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply(prefix="recovery-torn-journal")
            paths["receipt"].unlink()
            pid = os.fork()
            if pid == 0:
                original = private_apply._write_all

                def write_partial_recovery_event(
                    descriptor: int,
                    payload: bytes,
                ) -> None:
                    if b'"event":"recovery-restored-before"' in payload:
                        written = os.write(
                            descriptor,
                            payload[: max(1, len(payload) // 2)],
                        )
                        if written < 1:
                            os._exit(98)
                        os.fsync(descriptor)
                        os._exit(89)
                    original(descriptor, payload)

                private_apply._write_all = write_partial_recovery_event
                fixture.recover(
                    paths["journal"],
                    prefix="recovery-torn-crash",
                )
                os._exit(99)
            self.assertChildExit(pid, 89)
            self.assertFalse(paths["journal"].read_bytes().endswith(b"\n"))
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

            recovered = fixture.recover(
                paths["journal"],
                prefix="recovery-torn-retry",
            )
            self.assertEqual(recovered["decision"], "confirmed-before-noop")
            repaired_events = [
                json.loads(line) for line in paths["journal"].read_text().splitlines()
            ]
            self.assertIn(
                "journal-tail-quarantined",
                [event["event"] for event in repaired_events],
            )
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.destination_auth_before,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home),
                fixture.sqlite_before,
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_after_receipt_commit_does_not_invert_install(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            attestation = fixture.operation_attestation(
                "apply",
                prefix="fork-receipt",
                receipt_path=fixture.root / "fork-receipt-receipt.json",
            )
            pid = os.fork()
            if pid == 0:
                original = private_apply._publish_mutation_receipt

                def publish_then_exit(*args, **kwargs) -> None:
                    original(*args, **kwargs)
                    os._exit(94)

                private_apply._publish_mutation_receipt = publish_then_exit
                fixture.apply(prefix="fork-receipt", attestation=attestation)
                os._exit(99)
            self.assertChildExit(pid, 94)
            journal = fixture.root / "fork-receipt-journal.jsonl"
            receipt_path = fixture.root / "fork-receipt-receipt.json"
            receipt = private_apply.read_codex_private_apply_receipt(receipt_path)
            recovered = fixture.recover(journal, prefix="recover-fork-receipt")
            self.assertEqual(recovered["decision"], "confirmed-committed")
            self.assertEqual(
                recovered["original_receipt"]["sha256"],
                receipt["receipt_sha256"],
            )
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_process_exit_after_rollback_rename_reinstalls_apply_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            apply_receipt, apply_paths = fixture.apply()
            attestation = fixture.operation_attestation(
                "rollback",
                prefix="fork-rollback",
                receipt_path=fixture.root / "fork-rollback-receipt.json",
                apply_receipt_sha256=apply_receipt["receipt_sha256"],
            )
            pid = os.fork()
            if pid == 0:
                original = private_apply._commit_staged_auth

                def commit_then_exit(*args, **kwargs) -> None:
                    original(*args, **kwargs)
                    os._exit(93)

                private_apply._commit_staged_auth = commit_then_exit
                fixture.rollback(
                    apply_receipt,
                    apply_paths,
                    prefix="fork-rollback",
                    attestation=attestation,
                )
                os._exit(99)
            self.assertChildExit(pid, 93)
            recovered = fixture.recover(
                fixture.root / "fork-rollback-journal.jsonl",
                prefix="recover-fork-rollback",
                apply_receipt_path=apply_paths["receipt"],
                accept_apply_receipt=apply_receipt["receipt_sha256"],
            )
            self.assertEqual(recovered["decision"], "restored-before")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )

    def test_repeated_recovery_protects_prior_attempt_evidence_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply(prefix="repeated-recovery")
            paths["receipt"].unlink()
            original_capture = private_apply._capture_destination
            capture_calls = 0

            def fail_second_capture(*args, **kwargs):
                nonlocal capture_calls
                capture_calls += 1
                if capture_calls == 2:
                    raise BulkloadError("injected recovery post-capture failure")
                return original_capture(*args, **kwargs)

            with mock.patch.object(
                private_apply,
                "_capture_destination",
                side_effect=fail_second_capture,
            ):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "injected recovery post-capture failure",
                ):
                    fixture.recover(
                        paths["journal"],
                        prefix="prior-attempt",
                    )

            prior_preflight = fixture.root / "prior-attempt-preflight"
            self.assertTrue(prior_preflight.is_dir())
            with self.assertRaisesRegex(
                BulkloadError,
                # Linux catches the lexical ancestor first. macOS canonicalizes
                # /var through /private/var and catches the same directory in
                # the descriptor-backed protected-artifact pass instead.
                "^recovery preflight capture overlaps "
                "(?:original recovery evidence|protected original evidence 3)$",
            ):
                fixture.recover(
                    paths["journal"],
                    prefix="later-attempt",
                    preflight_capture_directory=(prior_preflight / "nested-preflight"),
                )

    def test_recovery_rejects_same_bytes_with_unknown_inode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply()
            paths["receipt"].unlink()
            _replace_auth_with_new_inode(
                fixture.destination_home,
                fixture.source_auth,
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "unknown live auth state",
            ):
                fixture.recover(paths["journal"], prefix="unknown-inode")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )
            for suffix in ("preflight", "post", "receipt.json"):
                self.assertFalse((fixture.root / f"unknown-inode-{suffix}").exists())

    def test_recovery_rejects_unknown_third_auth_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply()
            paths["receipt"].unlink()
            _replace_auth_with_new_inode(
                fixture.destination_home,
                b'{"fixture":"foreign-secret"}\n',
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "unknown live auth state",
            ):
                fixture.recover(paths["journal"], prefix="unknown-third")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                b'{"fixture":"foreign-secret"}\n',
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )
            for suffix in ("preflight", "post", "receipt.json"):
                self.assertFalse((fixture.root / f"unknown-third-{suffix}").exists())

    def test_recovery_rejects_unknown_rehashed_journal_event(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = PrivateApplyFixture(Path(directory))
            _, paths = fixture.apply()
            paths["receipt"].unlink()
            _append_rehashed_journal_event(
                paths["journal"],
                {
                    "event": "foreign-untyped-event",
                    "recorded_at": private_apply.utc_now(),
                    "payload": "must-not-be-ignored",
                },
            )
            with self.assertRaises(BulkloadError):
                fixture.recover(paths["journal"], prefix="foreign-event")
            self.assertEqual(
                (fixture.destination_home / private_state.AUTH_BASENAME).read_bytes(),
                fixture.source_auth,
            )
            self.assertEqual(
                _sqlite_truth(fixture.destination_home), fixture.sqlite_before
            )
            for suffix in ("preflight", "post", "receipt.json"):
                self.assertFalse((fixture.root / f"foreign-event-{suffix}").exists())

    def test_recovery_rejects_rehashed_known_events_outside_the_dfa(self) -> None:
        cases = (
            (
                "extra-field",
                lambda prepared, auth: {
                    "event": "auth-installed",
                    "recorded_at": private_apply.utc_now(),
                    "run_id": prepared["run_id"],
                    "auth_sha256": auth["sha256"],
                    "auth_identity": auth["identity"],
                    "unexpected": True,
                },
            ),
            (
                "duplicate-terminal-event",
                lambda prepared, auth: {
                    "event": "auth-installed",
                    "recorded_at": private_apply.utc_now(),
                    "run_id": prepared["run_id"],
                    "auth_sha256": auth["sha256"],
                    "auth_identity": auth["identity"],
                },
            ),
            (
                "recovery-event-without-opener",
                lambda prepared, auth: {
                    "event": "recovery-confirmed-before",
                    "recorded_at": private_apply.utc_now(),
                    "operation": "apply",
                    "operation_id": prepared["run_id"],
                    "recovery_id": str(uuid.uuid4()),
                    "auth_sha256": auth["sha256"],
                    "auth_identity": auth["identity"],
                },
            ),
        )
        for label, event_factory in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                fixture = PrivateApplyFixture(Path(directory))
                _, paths = fixture.apply(prefix=label)
                paths["receipt"].unlink()
                prepared = json.loads(paths["journal"].read_bytes().splitlines()[0])
                auth_path = fixture.destination_home / private_state.AUTH_BASENAME
                auth_sha256, auth_size = private_state._hash_regular_file(auth_path)
                auth = {
                    "sha256": auth_sha256,
                    "size": auth_size,
                    "identity": private_state._identity(auth_path.stat()),
                }
                _append_rehashed_journal_event(
                    paths["journal"],
                    event_factory(prepared, auth),
                )
                with self.assertRaises(BulkloadError):
                    fixture.recover(
                        paths["journal"],
                        prefix=f"reject-{label}",
                    )
                self.assertEqual(auth_path.read_bytes(), fixture.source_auth)
                self.assertEqual(
                    _sqlite_truth(fixture.destination_home),
                    fixture.sqlite_before,
                )


if __name__ == "__main__":
    run_unittest_main()
