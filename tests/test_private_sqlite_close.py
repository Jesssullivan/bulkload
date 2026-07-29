from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import uuid

from bulkload_lib.model import (
    BulkloadError,
    object_digest,
    sha256_bytes,
    canonical_bytes,
    utc_now,
)
from bulkload_lib.cli import (
    _codex_private_sqlite_close_request,
    _codex_private_sqlite_session_reclose,
)
from bulkload_lib import cli as bulkload_cli
from bulkload_lib import private_runtime, private_sqlite_close
from bulkload_lib.private_sqlite_close import (
    capture_codex_private_sqlite_session_reclose,
    compile_codex_private_sqlite_close_request,
    validate_codex_private_sqlite_close_request,
    validate_codex_private_sqlite_close_request_against_openings,
    validate_codex_private_sqlite_private_reclose_capture,
    validate_codex_private_sqlite_private_reclose_set,
    validate_codex_private_sqlite_session_reclose_against_live,
    validate_codex_private_sqlite_session_reclose_capture,
    validate_codex_private_sqlite_session_reclose_set,
)
from bulkload_lib.private_sqlite_plan import PRIVATE_SQLITE_PLAN_SCHEMA
from bulkload_lib.sessions import (
    capture_codex_sessions,
    compile_codex_session_union_plan,
)


SOURCE_AUTHORITY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DESTINATION_AUTHORITY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
SESSION_ID = "11111111-1111-4111-8111-111111111111"


def active_runtime_authority() -> dict:
    return {
        "schema": private_runtime.PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
        "policy_schema": private_runtime.PRIVATE_STATE_POLICY_SCHEMA,
        "policy_sha256": "4" * 64,
        "runtime_source_sha256": "5" * 64,
        "source_digests": {
            path: "6" * 64 for path in private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS
        },
    }


def write_rollout(root: Path) -> Path:
    directory = root / "2026" / "07" / "29"
    directory.mkdir(parents=True, mode=0o700)
    current = root
    current.chmod(0o700)
    for component in ("2026", "07", "29"):
        current /= component
        current.chmod(0o700)
    path = directory / f"rollout-2026-07-29T00-00-00-{SESSION_ID}.jsonl"
    path.write_text(
        json.dumps(
            {
                "timestamp": "2026-07-29T00:00:00Z",
                "type": "session_meta",
                "payload": {"id": SESSION_ID},
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    path.chmod(0o600)
    return path


def capture_pair(
    root: Path,
    *,
    role: str,
    authority: str,
) -> tuple[dict, dict]:
    return (
        capture_codex_sessions(
            root,
            role=role,
            acknowledge_writers_quiesced=True,
            host_authority_id=authority,
        ),
        capture_codex_sessions(
            root,
            role=role,
            acknowledge_writers_quiesced=True,
            host_authority_id=authority,
        ),
    )


def private_opening_binding(
    *,
    role: str,
    host: str,
    authority: str,
) -> dict:
    stable_projection = {
        "role": role,
        "host": host,
        "host_authority_id": authority,
        "codex_version": "0.145.0",
        "codex_home": {
            "resolved_path": f"/private/{role}/codex",
            "identity": {"device": 1, "inode": 2, "uid": 3, "mode": 0o700},
        },
        "sqlite_home": {
            "resolved_path": f"/private/{role}/sqlite",
            "identity": {"device": 1, "inode": 4, "uid": 3, "mode": 0o700},
            "authority_source": "explicit",
        },
        "selected_state_classes": ["sqlite"],
        "budgets": {"fixture": "bounded"},
        "auth": None,
        "sqlite_families": [{"basename": "state_5.sqlite"}],
        "sqlite_live_namespace_sha256": sha256_bytes(f"{role}-namespace".encode()),
        "copy_method": {
            "auth": None,
            "sqlite": "sqlite-immutable-backup-api",
            "raw_wal_shm_copy": False,
        },
        "complete": True,
        "ready_for_apply": False,
    }
    binding = {
        "role": role,
        "host": host,
        "host_authority_id": authority,
        "codex_version": "0.145.0",
        "capture_ids": [str(uuid.uuid4()), str(uuid.uuid4())],
        "capture_sha256s": [
            sha256_bytes(f"{role}-capture-a".encode()),
            sha256_bytes(f"{role}-capture-b".encode()),
        ],
        "quiescence_attestation_ids": [
            str(uuid.uuid4()),
            str(uuid.uuid4()),
        ],
        "stable_projection": stable_projection,
        "stable_projection_sha256": sha256_bytes(canonical_bytes(stable_projection)),
    }
    return binding


def fake_opening_plan(session_plan: dict, *, host: str) -> dict:
    accepted_inputs = {
        "compatibility_plan_sha256": "1" * 64,
        "adapter_registry_sha256": "2" * 64,
        "path_map_sha256": "3" * 64,
        "session_union_plan_sha256": session_plan["plan_sha256"],
    }
    value = {
        "schema": PRIVATE_SQLITE_PLAN_SCHEMA,
        "created_at": "2026-07-29T00:00:00Z",
        "accepted_inputs": accepted_inputs,
        "private_opening": {
            "source": private_opening_binding(
                role="source",
                host=host,
                authority=SOURCE_AUTHORITY,
            ),
            "destination": private_opening_binding(
                role="destination",
                host=host,
                authority=DESTINATION_AUTHORITY,
            ),
        },
        "session_union": {
            "plan_sha256": session_plan["plan_sha256"],
            "source": session_plan["source"],
            "destination": session_plan["destination"],
            "prefix_evidence": session_plan["prefix_evidence"],
            "ready_for_attended_copy": True,
            "executed": False,
            "verified": False,
        },
        "fixture": "validated-v4-body-authority",
    }
    value["plan_sha256"] = object_digest(value, "plan_sha256")
    return value


class CodexPrivateSqliteCloseTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source_root = self.root / "source-sessions"
        self.destination_root = self.root / "destination-sessions"
        self.source_rollout = write_rollout(self.source_root)
        self.destination_rollout = write_rollout(self.destination_root)
        self.source_a, self.source_b = capture_pair(
            self.source_root,
            role="source",
            authority=SOURCE_AUTHORITY,
        )
        self.destination_a, self.destination_b = capture_pair(
            self.destination_root,
            role="destination",
            authority=DESTINATION_AUTHORITY,
        )
        self.session_plan = compile_codex_session_union_plan(
            self.source_a,
            self.source_b,
            self.destination_a,
            self.destination_b,
        )
        self.opening_plan = fake_opening_plan(
            self.session_plan,
            host=self.source_a["host"],
        )
        self.epoch_id = str(uuid.uuid4())

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def opening_revalidation_inputs(self) -> dict:
        return {
            "opening_compatibility_plan": {},
            "opening_source_a_directory": self.root / "private-source-a",
            "opening_source_b_directory": self.root / "private-source-b",
            "opening_destination_a_directory": (self.root / "private-destination-a"),
            "opening_destination_b_directory": (self.root / "private-destination-b"),
            "opening_adapter_registry": {},
            "opening_path_map": {},
        }

    def test_private_close_cli_rejects_stdout_custody_bypass(self) -> None:
        for handler in (
            _codex_private_sqlite_close_request,
            _codex_private_sqlite_session_reclose,
        ):
            with self.subTest(handler=handler.__name__):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "owner-private output file",
                ):
                    handler(SimpleNamespace(output="-"))

    def test_reclose_cli_requires_the_close_requests_exact_runtime(self) -> None:
        request = self.compile_request()
        for handler, arguments in (
            (
                bulkload_cli._codex_private_sqlite_session_reclose,
                SimpleNamespace(
                    output="/private/session-close.json",
                    close_request="/private/close.json",
                ),
            ),
            (
                bulkload_cli._codex_private_sqlite_private_reclose,
                SimpleNamespace(close_request="/private/close.json"),
            ),
        ):
            with self.subTest(handler=handler.__name__):
                with (
                    mock.patch.object(
                        bulkload_cli,
                        "_read_pinned_codex_json",
                        return_value=(request, 99, ()),
                    ),
                    mock.patch.object(
                        bulkload_cli.private_runtime,
                        "open_pinned_private_runtime_authority",
                        side_effect=BulkloadError("runtime mismatch"),
                    ) as opener,
                    mock.patch.object(bulkload_cli.os, "close"),
                ):
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "runtime mismatch",
                    ):
                        handler(arguments)
                opener.assert_called_once_with(request["runtime_authority"])

    def compile_request(self) -> dict:
        with (
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan",
            ),
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan_against_inputs",
            ),
        ):
            return compile_codex_private_sqlite_close_request(
                self.opening_plan,
                self.session_plan,
                self.source_a,
                self.source_b,
                self.destination_a,
                self.destination_b,
                active_runtime_authority(),
                accept_opening_plan=self.opening_plan["plan_sha256"],
                accept_session_union_plan=self.session_plan["plan_sha256"],
                writer_stop_epoch_id=self.epoch_id,
                writer_stop_epoch_at=utc_now(),
                acknowledge_provider_writers_stopped=True,
                **self.opening_revalidation_inputs(),
            )

    def test_request_binds_exact_bodies_projections_catalogs_and_epoch(self) -> None:
        request = self.compile_request()
        validate_codex_private_sqlite_close_request(request)
        self.assertEqual(
            request["opening_plan"]["body_sha256"],
            sha256_bytes(canonical_bytes(self.opening_plan)),
        )
        self.assertEqual(
            request["session_union"]["body_sha256"],
            sha256_bytes(canonical_bytes(self.session_plan)),
        )
        self.assertEqual(
            request["session_opening"]["source"]["stable_projection"]["catalog"],
            {
                "directories": self.source_a["directories"],
                "sessions": self.source_a["sessions"],
                "non_private_file_count": self.source_a["non_private_file_count"],
                "non_private_directory_count": self.source_a[
                    "non_private_directory_count"
                ],
            },
        )
        self.assertEqual(
            request["writer_stop_epoch"]["epoch_id"],
            self.epoch_id,
        )
        self.assertFalse(request["writer_stop_epoch"]["provider_writer_proof"])
        self.assertEqual(
            request["required_closing"],
            {
                "planes": ["private", "session"],
                "roles": ["source", "destination"],
                "passes_per_role": 2,
            },
        )

        tampered = deepcopy(request)
        source = tampered["private_opening"]["source"]["binding"]
        source["codex_version"] = "0.146.0"
        source["stable_projection"]["codex_version"] = "0.146.0"
        source["stable_projection_sha256"] = sha256_bytes(
            canonical_bytes(source["stable_projection"])
        )
        tampered["private_opening"]["source"]["binding_sha256"] = sha256_bytes(
            canonical_bytes(source)
        )
        tampered["close_request_sha256"] = object_digest(
            tampered,
            "close_request_sha256",
        )
        validate_codex_private_sqlite_close_request(tampered)
        with (
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan",
            ),
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan_against_inputs",
            ),
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "differs from its opening bodies",
            ):
                validate_codex_private_sqlite_close_request_against_openings(
                    tampered,
                    self.opening_plan,
                    self.session_plan,
                    self.source_a,
                    self.source_b,
                    self.destination_a,
                    self.destination_b,
                    active_runtime_authority(),
                )

    def test_request_rejects_digest_epoch_and_custody_mismatch(self) -> None:
        with (
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan",
            ),
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_sqlite_compose_plan_against_inputs",
            ),
        ):
            with self.assertRaisesRegex(BulkloadError, "opening-plan digest"):
                compile_codex_private_sqlite_close_request(
                    self.opening_plan,
                    self.session_plan,
                    self.source_a,
                    self.source_b,
                    self.destination_a,
                    self.destination_b,
                    active_runtime_authority(),
                    accept_opening_plan="0" * 64,
                    accept_session_union_plan=self.session_plan["plan_sha256"],
                    writer_stop_epoch_id=self.epoch_id,
                    writer_stop_epoch_at=utc_now(),
                    acknowledge_provider_writers_stopped=True,
                    **self.opening_revalidation_inputs(),
                )
            with self.assertRaisesRegex(BulkloadError, "canonical UTC-seconds"):
                compile_codex_private_sqlite_close_request(
                    self.opening_plan,
                    self.session_plan,
                    self.source_a,
                    self.source_b,
                    self.destination_a,
                    self.destination_b,
                    active_runtime_authority(),
                    accept_opening_plan=self.opening_plan["plan_sha256"],
                    accept_session_union_plan=self.session_plan["plan_sha256"],
                    writer_stop_epoch_id=self.epoch_id,
                    writer_stop_epoch_at="2026-07-29T00:00:00+00:00",
                    acknowledge_provider_writers_stopped=True,
                    **self.opening_revalidation_inputs(),
                )
            crossed = deepcopy(self.opening_plan)
            crossed["session_union"]["source"] = self.session_plan["destination"]
            crossed["plan_sha256"] = object_digest(crossed, "plan_sha256")
            with self.assertRaisesRegex(BulkloadError, "exact session union"):
                compile_codex_private_sqlite_close_request(
                    crossed,
                    self.session_plan,
                    self.source_a,
                    self.source_b,
                    self.destination_a,
                    self.destination_b,
                    active_runtime_authority(),
                    accept_opening_plan=crossed["plan_sha256"],
                    accept_session_union_plan=self.session_plan["plan_sha256"],
                    writer_stop_epoch_id=self.epoch_id,
                    writer_stop_epoch_at=utc_now(),
                    acknowledge_provider_writers_stopped=True,
                    **self.opening_revalidation_inputs(),
                )

    def capture_reclose(self, request: dict, root: Path, role: str) -> dict:
        return capture_codex_private_sqlite_session_reclose(
            root,
            role=role,
            close_request=request,
            accept_close_request=request["close_request_sha256"],
            writer_stop_epoch_id=self.epoch_id,
            acknowledge_writers_quiesced=True,
        )

    def private_reclose(self, request: dict, role: str, suffix: str) -> dict:
        projection = deepcopy(
            request["private_opening"][role]["binding"]["stable_projection"]
        )
        return {
            **projection,
            "captured_at": request["created_at"],
            "capture_id": str(uuid.uuid4()),
            "capture_sha256": sha256_bytes(f"{role}-{suffix}-capture".encode()),
            "quiescence": {
                "purpose": "close",
                "capture_role": role,
                "provider_writer_proof": False,
                "accepted_inputs": {
                    "plan_sha256": request["close_request_sha256"],
                    "apply_receipt_sha256": None,
                    "journal_sha256": None,
                },
                "attestation_id": str(uuid.uuid4()),
                "attestation_sha256": sha256_bytes(
                    f"{role}-{suffix}-attestation".encode()
                ),
            },
        }

    def test_private_reclose_set_binds_request_projection_and_distinct_ids(
        self,
    ) -> None:
        request = self.compile_request()
        captures = [
            self.private_reclose(request, "source", "a"),
            self.private_reclose(request, "source", "b"),
            self.private_reclose(request, "destination", "a"),
            self.private_reclose(request, "destination", "b"),
        ]
        with mock.patch.object(
            private_sqlite_close,
            "validate_codex_private_capture",
        ):
            binding = validate_codex_private_sqlite_private_reclose_capture(
                captures[0],
                request,
                role="source",
            )
        self.assertEqual(binding["capture_id"], captures[0]["capture_id"])
        with (
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_capture",
            ),
            mock.patch.object(
                private_sqlite_close,
                "read_codex_private_bundle",
                side_effect=[
                    (captures[0], Path("/bundle/source-a")),
                    (captures[1], Path("/bundle/source-b")),
                    (captures[2], Path("/bundle/destination-a")),
                    (captures[3], Path("/bundle/destination-b")),
                ],
            ),
        ):
            bindings = validate_codex_private_sqlite_private_reclose_set(
                request,
                Path("/bundle/source-a"),
                Path("/bundle/source-b"),
                Path("/bundle/destination-a"),
                Path("/bundle/destination-b"),
            )
        self.assertEqual(
            [len(bindings["source"]), len(bindings["destination"])],
            [2, 2],
        )

        captures[1]["capture_id"] = captures[0]["capture_id"]
        with (
            mock.patch.object(
                private_sqlite_close,
                "validate_codex_private_capture",
            ),
            mock.patch.object(
                private_sqlite_close,
                "read_codex_private_bundle",
                side_effect=[
                    (captures[0], Path("/bundle/source-a")),
                    (captures[1], Path("/bundle/source-b")),
                    (captures[2], Path("/bundle/destination-a")),
                    (captures[3], Path("/bundle/destination-b")),
                ],
            ),
            self.assertRaisesRegex(BulkloadError, "globally distinct"),
        ):
            validate_codex_private_sqlite_private_reclose_set(
                request,
                Path("/bundle/source-a"),
                Path("/bundle/source-b"),
                Path("/bundle/destination-a"),
                Path("/bundle/destination-b"),
            )

    def test_direct_live_reclose_set_is_fresh_distinct_and_exact(self) -> None:
        request = self.compile_request()
        source_a = self.capture_reclose(request, self.source_root, "source")
        source_b = self.capture_reclose(request, self.source_root, "source")
        destination_a = self.capture_reclose(
            request,
            self.destination_root,
            "destination",
        )
        destination_b = self.capture_reclose(
            request,
            self.destination_root,
            "destination",
        )
        bindings = validate_codex_private_sqlite_session_reclose_set(
            request,
            source_a,
            source_b,
            destination_a,
            destination_b,
        )
        self.assertEqual(set(bindings), {"source", "destination"})
        self.assertTrue(
            all(len(role_bindings) == 2 for role_bindings in bindings.values())
        )
        capture_ids = {
            item["capture_id"]
            for role_bindings in bindings.values()
            for item in role_bindings
        }
        self.assertEqual(len(capture_ids), 4)
        self.assertTrue(
            capture_ids.isdisjoint(request["session_union"]["evidence_capture_ids"])
        )
        self.assertTrue(
            all(
                capture["writer_stop_epoch"] == request["writer_stop_epoch"]
                for capture in (
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )
            )
        )
        with self.assertRaisesRegex(BulkloadError, "globally distinct"):
            validate_codex_private_sqlite_session_reclose_set(
                request,
                source_a,
                source_a,
                destination_a,
                destination_b,
            )

    def test_reclose_never_accepts_stale_snapshot_and_rejects_live_drift(self) -> None:
        request = self.compile_request()
        with self.assertRaises(TypeError):
            capture_codex_private_sqlite_session_reclose(
                self.source_root,
                role="source",
                close_request=request,
                accept_close_request=request["close_request_sha256"],
                writer_stop_epoch_id=self.epoch_id,
                acknowledge_writers_quiesced=True,
                snapshot=self.source_a,
            )
        with self.assertRaisesRegex(BulkloadError, "close-request digest"):
            capture_codex_private_sqlite_session_reclose(
                self.source_root,
                role="source",
                close_request=request,
                accept_close_request="0" * 64,
                writer_stop_epoch_id=self.epoch_id,
                acknowledge_writers_quiesced=True,
            )
        with self.source_rollout.open("a", encoding="utf-8") as output:
            output.write("{}\n")
        with self.assertRaisesRegex(BulkloadError, "differs from opening custody"):
            self.capture_reclose(request, self.source_root, "source")

    def test_post_publication_live_revalidation_rejects_session_drift(self) -> None:
        request = self.compile_request()
        capture = self.capture_reclose(request, self.source_root, "source")
        validate_codex_private_sqlite_session_reclose_against_live(
            capture,
            self.source_root,
            request,
            role="source",
            accept_close_request=request["close_request_sha256"],
            writer_stop_epoch_id=self.epoch_id,
            acknowledge_writers_quiesced=True,
        )
        with self.source_rollout.open("a", encoding="utf-8") as output:
            output.write("{}\n")
        with self.assertRaisesRegex(BulkloadError, "differs from opening custody"):
            validate_codex_private_sqlite_session_reclose_against_live(
                capture,
                self.source_root,
                request,
                role="source",
                accept_close_request=request["close_request_sha256"],
                writer_stop_epoch_id=self.epoch_id,
                acknowledge_writers_quiesced=True,
            )

    def test_reclose_structural_validation_rejects_false_provider_proof(self) -> None:
        request = self.compile_request()
        capture = self.capture_reclose(request, self.source_root, "source")
        tampered = deepcopy(capture)
        tampered["writer_stop_epoch"]["provider_writer_proof"] = True
        tampered["reclose_capture_sha256"] = object_digest(
            tampered,
            "reclose_capture_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "writer-stop epoch"):
            validate_codex_private_sqlite_session_reclose_capture(tampered)


if __name__ == "__main__":
    unittest.main()
