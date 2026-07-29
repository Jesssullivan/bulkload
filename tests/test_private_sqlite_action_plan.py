from __future__ import annotations

from copy import deepcopy
import hashlib
from pathlib import Path
import shutil
import sqlite3
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest import mock
import uuid

from bulkload_lib.model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
)
from bulkload_lib.cli import _codex_private_sqlite_compose_action_plan
from bulkload_lib import cli as bulkload_cli
from bulkload_lib import private_sqlite_action_plan
from bulkload_lib import private_runtime
from bulkload_lib.private_sqlite_action_plan import (
    PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
    compile_codex_private_sqlite_action_plan,
    validate_codex_private_sqlite_action_plan,
    validate_codex_private_sqlite_action_plan_against_close,
)
from bulkload_lib.private_sqlite_close import PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA
from bulkload_lib.private_sqlite_plan import (
    PRIVATE_SQLITE_PLAN_SCHEMA,
    _classify_table,
    _stable_private_projection,
    _structured_schema,
)


SOURCE_AUTHORITY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DESTINATION_AUTHORITY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
EMPTY_SHA256 = sha256_bytes(canonical_bytes([]))


def _uuid() -> str:
    return str(uuid.uuid4())


def _digest(label: str) -> str:
    return sha256_bytes(label.encode())


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


class CodexPrivateSqliteActionPlanTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.directories = {
            "source_a": self.root / "source-a",
            "source_b": self.root / "source-b",
            "destination_a": self.root / "destination-a",
            "destination_b": self.root / "destination-b",
        }
        for directory in self.directories.values():
            (directory / "sqlite").mkdir(parents=True, mode=0o700)
        self.basename = "logs_1.sqlite"
        self.large_value = b"x" * (8 * 1024 * 1024 + 4096)
        self._write_database(
            self.directories["source_a"] / "sqlite" / self.basename,
            ((1, b"shared"), (2, self.large_value)),
        )
        self._write_database(
            self.directories["destination_a"] / "sqlite" / self.basename,
            ((1, b"shared"), (3, b"destination")),
        )
        for role in ("source", "destination"):
            first = self.directories[f"{role}_a"] / "sqlite" / self.basename
            second = self.directories[f"{role}_b"] / "sqlite" / self.basename
            shutil.copyfile(first, second)
            second.chmod(0o600)

        self.source_schema = self._schema(
            self.directories["source_a"] / "sqlite" / self.basename
        )
        self.destination_schema = self._schema(
            self.directories["destination_a"] / "sqlite" / self.basename
        )
        self.table_rule = {
            "name": "items",
            "merge_class": "keyed-union",
            "identity_columns": ["id"],
        }
        self.registry_family = {
            "basename": self.basename,
            "path_authorities": [],
            "tables": [self.table_rule],
        }
        self.path_map = {
            "source_session_root": "/source/sessions",
            "destination_session_root": "/destination/sessions",
            "rules": [],
        }
        self.opening_relation = self._classify(include_expected_output=False)
        self.captures = self._captures()
        self.runtime_authority = {
            "schema": private_runtime.PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
            "policy_schema": private_runtime.PRIVATE_STATE_POLICY_SCHEMA,
            "policy_sha256": _digest("policy-v5"),
            "runtime_source_sha256": _digest("runtime-v5"),
            "source_digests": {
                path: _digest(path)
                for path in private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS
            },
        }
        self.opening_plan = self._opening_plan()
        self.close_request = self._close_request(self.opening_plan)
        self.session_closes = self._session_closes()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_database(
        self,
        path: Path,
        rows: tuple[tuple[int, bytes], ...],
    ) -> None:
        connection = sqlite3.connect(path)
        try:
            connection.execute(
                "CREATE TABLE items(id INTEGER PRIMARY KEY, value BLOB) STRICT"
            )
            connection.executemany("INSERT INTO items VALUES (?, ?)", rows)
            connection.commit()
        finally:
            connection.close()
        path.chmod(0o600)

    def _schema(self, path: Path) -> dict:
        connection = sqlite3.connect(path)
        try:
            schema, blockers, migrations = _structured_schema(
                connection,
                deadline=time.monotonic() + 30,
            )
        finally:
            connection.close()
        self.assertEqual(blockers, [])
        self.assertEqual(migrations, [])
        return schema

    def _classify(self, *, include_expected_output: bool) -> dict:
        source = sqlite3.connect(
            self.directories["source_a"] / "sqlite" / self.basename
        )
        destination = sqlite3.connect(
            self.directories["destination_a"] / "sqlite" / self.basename
        )
        try:
            relation, blockers = _classify_table(
                source,
                destination,
                basename=self.basename,
                source_contract=self.source_schema,
                destination_contract=self.destination_schema,
                table_rule=self.table_rule,
                registry_family=self.registry_family,
                path_map=self.path_map,
                accepted_session_paths={"source": {}, "destination": {}},
                deadline=time.monotonic() + 30,
                include_expected_output=include_expected_output,
            )
        finally:
            source.close()
            destination.close()
        self.assertEqual(blockers, [])
        return relation

    def _family_metadata(self, path: Path) -> dict:
        size = path.stat().st_size
        digest = _file_sha256(path)
        return {
            "basename": self.basename,
            "sha256": digest,
            "snapshot_size": size,
            "source_sha256": digest,
            "source_size": size,
            "schema_sha256": self.source_schema["raw_schema_sha256"],
            "migrations_sha256": EMPTY_SHA256,
            "application_id": 0,
            "user_version": 0,
        }

    def _capture(self, role: str, pass_name: str) -> dict:
        directory = self.directories[f"{role}_{pass_name}"]
        authority = SOURCE_AUTHORITY if role == "source" else DESTINATION_AUTHORITY
        capture = {
            "role": role,
            "host": "fixture-host",
            "host_authority_id": authority,
            "codex_version": "0.145.0",
            "codex_home": {"resolved_path": f"/{role}/codex"},
            "sqlite_home": {"resolved_path": f"/{role}/sqlite"},
            "selected_state_classes": ["sqlite"],
            "budgets": {"fixture": True},
            "auth": None,
            "sqlite_families": [
                self._family_metadata(directory / "sqlite" / self.basename)
            ],
            "sqlite_live_namespace_sha256": _digest(f"{role}-namespace"),
            "copy_method": {
                "auth": None,
                "sqlite": "sqlite-immutable-backup-api",
                "raw_wal_shm_copy": False,
            },
            "complete": True,
            "ready_for_apply": False,
            "capture_id": _uuid(),
            "captured_at": "2026-07-29T12:00:01Z",
            "quiescence": {
                "attestation_id": _uuid(),
                "attestation_sha256": _digest(f"{role}-{pass_name}-attestation"),
            },
        }
        capture["capture_sha256"] = object_digest(capture, "capture_sha256")
        return capture

    def _captures(self) -> dict[str, dict]:
        return {
            f"{role}_{pass_name}": self._capture(role, pass_name)
            for role in ("source", "destination")
            for pass_name in ("a", "b")
        }

    def _opening_binding(self, role: str) -> dict:
        capture = self.captures[f"{role}_a"]
        return {
            "role": role,
            "host": capture["host"],
            "host_authority_id": capture["host_authority_id"],
            "codex_version": capture["codex_version"],
            "capture_ids": [_uuid(), _uuid()],
            "capture_sha256s": [
                _digest(f"opening-{role}-a"),
                _digest(f"opening-{role}-b"),
            ],
            "quiescence_attestation_ids": [_uuid(), _uuid()],
            "stable_projection": _stable_private_projection(capture),
            "stable_projection_sha256": sha256_bytes(
                canonical_bytes(_stable_private_projection(capture))
            ),
        }

    def _opening_plan(self) -> dict:
        edge_digest = sha256_bytes(canonical_bytes([]))
        family = {
            "basename": self.basename,
            "family_role": "logs",
            "generation": 1,
            "source_artifact": {},
            "destination_artifact": {},
            "source_schema": self.source_schema,
            "destination_schema": self.destination_schema,
            "migration_relation": {
                "relation": "exact",
                "source_count": 0,
                "destination_count": 0,
                "source_latest_migration": None,
                "destination_latest_migration": None,
                "common_prefix_count": 0,
                "common_prefix_sha256": EMPTY_SHA256,
                "source_tail_sha256": EMPTY_SHA256,
                "source_migrations_sha256": EMPTY_SHA256,
                "destination_migrations_sha256": EMPTY_SHA256,
                "adapter": None,
            },
            "path_authorities_sha256": edge_digest,
            "table_relations": [self.opening_relation],
            "edge_closure": {
                "registry_sha256": edge_digest,
                "source_observed_sha256": edge_digest,
                "destination_observed_sha256": edge_digest,
                "registry_matches_observed": True,
                "post_compose_required": False,
                "proven": False,
            },
            "blockers": [],
            "classification_complete": True,
            "composer_implemented": False,
        }
        session_plan_sha256 = _digest("session-plan")
        value = {
            "schema": PRIVATE_SQLITE_PLAN_SCHEMA,
            "created_at": "2026-07-29T11:00:00Z",
            "accepted_inputs": {
                "compatibility_plan_sha256": _digest("compatibility"),
                "adapter_registry_sha256": _digest("registry"),
                "path_map_sha256": _digest("path-map"),
                "session_union_plan_sha256": session_plan_sha256,
            },
            "private_opening": {
                "source": self._opening_binding("source"),
                "destination": self._opening_binding("destination"),
            },
            "session_union": {
                "plan_sha256": session_plan_sha256,
                "source": {"host_authority_id": SOURCE_AUTHORITY},
                "destination": {"host_authority_id": DESTINATION_AUTHORITY},
                "prefix_evidence": {"required": False},
                "ready_for_attended_copy": True,
                "executed": False,
                "verified": False,
            },
            "adapter_registry": {
                "families": [self.registry_family],
            },
            "path_map": self.path_map,
            "sqlite_families": [family],
            "blockers": [
                {"code": "post-plan-private-close-required"},
                {"code": "session-union-execution-and-verification-not-implemented"},
                {"code": "sqlite-composer-not-implemented"},
            ],
        }
        value["plan_sha256"] = object_digest(value, "plan_sha256")
        return value

    def _close_request(self, opening_plan: dict) -> dict:
        accepted = opening_plan["accepted_inputs"]
        value = {
            "schema": PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA,
            "runtime_authority": deepcopy(self.runtime_authority),
            "opening_inputs_revalidated": True,
            "opening_plan": {
                "schema": opening_plan["schema"],
                "plan_sha256": opening_plan["plan_sha256"],
                "body_sha256": sha256_bytes(canonical_bytes(opening_plan)),
                "accepted_inputs": accepted,
                "accepted_inputs_sha256": sha256_bytes(canonical_bytes(accepted)),
            },
            "private_opening": {
                role: {
                    "binding": opening_plan["private_opening"][role],
                    "binding_sha256": sha256_bytes(
                        canonical_bytes(opening_plan["private_opening"][role])
                    ),
                }
                for role in ("source", "destination")
            },
            "session_union": {
                "plan_sha256": opening_plan["session_union"]["plan_sha256"],
                "source": opening_plan["session_union"]["source"],
                "destination": opening_plan["session_union"]["destination"],
                "prefix_evidence": opening_plan["session_union"]["prefix_evidence"],
            },
            "writer_stop_epoch": {
                "epoch_id": _uuid(),
                "stopped_at": "2026-07-29T12:00:00Z",
                "operator_acknowledged_writers_stopped": True,
                "provider_writer_proof": False,
            },
        }
        value["close_request_sha256"] = object_digest(
            value,
            "close_request_sha256",
        )
        return value

    def _session_closes(self) -> dict[str, dict]:
        return {
            f"{role}_{pass_name}": {
                "snapshot": {
                    "capture_id": _uuid(),
                    "snapshot_sha256": _digest(f"session-{role}-{pass_name}-snapshot"),
                    "sessions": [],
                },
                "reclose_capture_sha256": _digest(
                    f"session-{role}-{pass_name}-wrapper"
                ),
            }
            for role in ("source", "destination")
            for pass_name in ("a", "b")
        }

    def _private_bindings(self) -> dict[str, list[dict[str, str]]]:
        return {
            role: [
                {
                    "capture_id": self.captures[f"{role}_{pass_name}"]["capture_id"],
                    "capture_sha256": self.captures[f"{role}_{pass_name}"][
                        "capture_sha256"
                    ],
                    "quiescence_attestation_id": self.captures[f"{role}_{pass_name}"][
                        "quiescence"
                    ]["attestation_id"],
                    "quiescence_attestation_sha256": self.captures[
                        f"{role}_{pass_name}"
                    ]["quiescence"]["attestation_sha256"],
                }
                for pass_name in ("a", "b")
            ]
            for role in ("source", "destination")
        }

    def _session_bindings(self) -> dict[str, list[dict[str, str]]]:
        return {
            role: [
                {
                    "capture_id": self.session_closes[f"{role}_{pass_name}"][
                        "snapshot"
                    ]["capture_id"],
                    "snapshot_sha256": self.session_closes[f"{role}_{pass_name}"][
                        "snapshot"
                    ]["snapshot_sha256"],
                    "reclose_capture_sha256": self.session_closes[
                        f"{role}_{pass_name}"
                    ]["reclose_capture_sha256"],
                }
                for pass_name in ("a", "b")
            ]
            for role in ("source", "destination")
        }

    def _read_bundle(self, path: Path, role: str) -> tuple[dict, Path]:
        for name, directory in self.directories.items():
            if directory == path:
                self.assertEqual(name.split("_", 1)[0], role)
                return self.captures[name], directory
        raise AssertionError(f"unexpected bundle: {path}")

    def _patches(self) -> tuple[mock._patch, ...]:
        return (
            mock.patch.object(
                private_sqlite_action_plan,
                "validate_codex_private_sqlite_compose_plan",
            ),
            mock.patch.object(
                private_sqlite_action_plan,
                "validate_codex_private_sqlite_close_request",
            ),
            mock.patch.object(
                private_sqlite_action_plan,
                "validate_codex_private_sqlite_private_reclose_set",
                return_value=self._private_bindings(),
            ),
            mock.patch.object(
                private_sqlite_action_plan,
                "validate_codex_private_sqlite_session_reclose_set",
                return_value=self._session_bindings(),
            ),
            mock.patch.object(
                private_sqlite_action_plan,
                "read_codex_private_bundle",
                side_effect=self._read_bundle,
            ),
        )

    def _compile(
        self,
        *,
        opening_plan: dict | None = None,
        close_request: dict | None = None,
        runtime_authority: dict | None = None,
    ) -> dict:
        opening = opening_plan or self.opening_plan
        close = close_request or self.close_request
        runtime = runtime_authority or self.runtime_authority
        patches = self._patches()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
        ):
            return compile_codex_private_sqlite_action_plan(
                opening,
                close,
                self.directories["source_a"],
                self.directories["source_b"],
                self.directories["destination_a"],
                self.directories["destination_b"],
                self.session_closes["source_a"],
                self.session_closes["source_b"],
                self.session_closes["destination_a"],
                self.session_closes["destination_b"],
                accept_opening_plan=opening["plan_sha256"],
                accept_close_request=close["close_request_sha256"],
                runtime_authority=runtime,
            )

    def _action_cli_fixture(
        self,
        *,
        include_optional: bool = False,
    ) -> tuple[SimpleNamespace, dict[str, dict], dict]:
        required_documents = {
            "opening_plan": self.opening_plan,
            "close_request": self.close_request,
            "opening_compatibility_plan": {"fixture": "compatibility"},
            "opening_adapter_registry": {"fixture": "registry"},
            "opening_path_map": {"fixture": "path-map"},
            "opening_session_union_plan": {"fixture": "session-plan"},
            "opening_session_source_a": {"fixture": "session-source-a"},
            "opening_session_source_b": {"fixture": "session-source-b"},
            "opening_session_destination_a": {"fixture": "session-destination-a"},
            "opening_session_destination_b": {"fixture": "session-destination-b"},
            "session_source_close_a": self.session_closes["source_a"],
            "session_source_close_b": self.session_closes["source_b"],
            "session_destination_close_a": self.session_closes["destination_a"],
            "session_destination_close_b": self.session_closes["destination_b"],
        }
        arguments = {name: f"/private/{name}.json" for name in required_documents}
        arguments.update(
            {
                "output": "/private/action-plan.json",
                "opening_source_a_bundle": "/private/opening-source-a",
                "opening_source_b_bundle": "/private/opening-source-b",
                "opening_destination_a_bundle": "/private/opening-destination-a",
                "opening_destination_b_bundle": "/private/opening-destination-b",
                "source_close_a_bundle": "/private/close-source-a",
                "source_close_b_bundle": "/private/close-source-b",
                "destination_close_a_bundle": "/private/close-destination-a",
                "destination_close_b_bundle": "/private/close-destination-b",
                "accept_opening_plan": self.opening_plan["plan_sha256"],
                "accept_close_request": self.close_request["close_request_sha256"],
            }
        )
        documents = {
            arguments[name]: value for name, value in required_documents.items()
        }
        for name in (
            "opening_session_prefix_request",
            "opening_session_source_prefix_a",
            "opening_session_source_prefix_b",
            "opening_session_destination_prefix_a",
            "opening_session_destination_prefix_b",
            "opening_session_close_request",
            "opening_session_source_close_a",
            "opening_session_source_close_b",
            "opening_session_destination_close_a",
            "opening_session_destination_close_b",
        ):
            if include_optional:
                path = f"/private/{name}.json"
                arguments[name] = path
                documents[path] = {"fixture": name}
            else:
                arguments[name] = None
        action_plan = {
            "action_plan_sha256": _digest("cli-action-plan"),
            "readiness": {
                "descriptive_action_complete": True,
                "ready_for_offline_compose": False,
            },
        }
        return SimpleNamespace(**arguments), documents, action_plan

    def test_exact_close_emits_deterministic_create_only_union_contract(self) -> None:
        plan = self._compile()
        validate_codex_private_sqlite_action_plan(plan)
        self.assertEqual(plan["schema"], PRIVATE_SQLITE_ACTION_PLAN_SCHEMA)
        self.assertTrue(plan["post_plan_close_proven"])
        self.assertTrue(plan["readiness"]["descriptive_action_complete"])
        self.assertFalse(plan["readiness"]["ready_for_offline_compose"])
        for key in (
            "composer_implemented",
            "sqlite_union_ready",
            "sqlite_compose",
            "sqlite_publish",
            "combined",
            "ready_for_apply",
        ):
            self.assertFalse(plan["readiness"][key])
        table = plan["sqlite_families"][0]["tables"][0]
        self.assertEqual(
            (
                table["shared_equal"],
                table["source_only"],
                table["destination_only"],
                table["conflicts"],
                table["expected_output"]["row_count"],
            ),
            (1, 1, 1, 0, 3),
        )
        self.assertGreater(table["source"]["classified_bytes"], 8 * 1024 * 1024)
        self.assertEqual(
            table["operation"]["baseline"],
            "preserve-destination-exact",
        )
        self.assertFalse(table["operation"]["identity_remap"])
        self.assertFalse(table["operation"]["deduplicate"])
        self.assertFalse(table["operation"]["delete"])
        graph = plan["operation_graph"]
        self.assertTrue(graph["create_only"])
        self.assertTrue(graph["descriptive_only"])
        self.assertFalse(graph["existing_destination_mutation"])
        source_only_node = next(
            node
            for node in graph["nodes"]
            if node["kind"] == "insert-source-only-canonical-identity-order"
        )
        self.assertEqual(
            source_only_node["expected_sha256"],
            table["expected_output"]["partitions"]["source_only"][
                "semantic_rows_sha256"
            ],
        )
        self.assertNotEqual(
            source_only_node["expected_sha256"],
            table["expected_output"]["semantic_rows_sha256"],
        )
        self.assertEqual(
            graph["nodes"][-1]["kind"], "seal-complete-bundle-with-no-replace-rename"
        )

        repeated = self._compile()
        ignored = {"created_at", "action_plan_sha256"}
        self.assertEqual(
            {key: value for key, value in plan.items() if key not in ignored},
            {key: value for key, value in repeated.items() if key not in ignored},
        )

    def test_action_plan_cli_rejects_stdout_custody_bypass(self) -> None:
        with self.assertRaisesRegex(
            BulkloadError,
            "owner-private output file",
        ):
            _codex_private_sqlite_compose_action_plan(SimpleNamespace(output="-"))

    def test_action_plan_cli_recomputes_opening_chain_across_publication(
        self,
    ) -> None:
        arguments, documents, action_plan = self._action_cli_fixture(
            include_optional=True
        )
        events: list[str] = []
        descriptors = iter(range(100, 200))

        def read_document(path: str) -> tuple[dict, int, tuple[int, ...]]:
            return documents[path], next(descriptors), ()

        runtime = SimpleNamespace(
            record=self.runtime_authority,
            revalidate=mock.Mock(),
        )
        runtime_context = mock.MagicMock()
        runtime_context.__enter__.return_value = runtime
        runtime_context.__exit__.return_value = False

        with (
            mock.patch.object(
                bulkload_cli,
                "_read_pinned_codex_json",
                side_effect=read_document,
            ),
            mock.patch.object(
                bulkload_cli.private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=runtime_context,
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_close_request_against_inputs",
                side_effect=lambda *args, **kwargs: events.append("deep"),
            ) as deep_validator,
            mock.patch.object(
                bulkload_cli,
                "compile_codex_private_sqlite_action_plan",
                side_effect=lambda *args, **kwargs: (
                    events.append("compile") or action_plan
                ),
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_action_plan_against_close",
            ),
            mock.patch.object(
                bulkload_cli,
                "_revalidate_pinned_codex_input",
            ),
            mock.patch.object(
                bulkload_cli,
                "_write_pinned_codex_json",
                side_effect=lambda *args, **kwargs: events.append("publish"),
            ) as publish,
            mock.patch.object(bulkload_cli.os, "close"),
        ):
            self.assertEqual(
                _codex_private_sqlite_compose_action_plan(arguments),
                4,
            )

        self.assertEqual(deep_validator.call_count, 3)
        for call in deep_validator.call_args_list:
            for name in (
                "opening_session_prefix_request",
                "opening_session_source_prefix_a",
                "opening_session_source_prefix_b",
                "opening_session_destination_prefix_a",
                "opening_session_destination_prefix_b",
                "opening_session_close_request",
                "opening_session_source_close_a",
                "opening_session_source_close_b",
                "opening_session_destination_close_a",
                "opening_session_destination_close_b",
            ):
                path = getattr(arguments, name)
                self.assertEqual(call.kwargs[name], documents[path])
        self.assertEqual(events[:2], ["deep", "compile"])
        self.assertLess(events.index("publish"), len(events) - 1)
        self.assertEqual(events[-1], "deep")
        publish.assert_called_once()

    def test_action_plan_cli_prepublication_recompute_blocks_output(self) -> None:
        arguments, documents, action_plan = self._action_cli_fixture()
        descriptors = iter(range(200, 300))

        def read_document(path: str) -> tuple[dict, int, tuple[int, ...]]:
            return documents[path], next(descriptors), ()

        runtime = SimpleNamespace(
            record=self.runtime_authority,
            revalidate=mock.Mock(),
        )
        runtime_context = mock.MagicMock()
        runtime_context.__enter__.return_value = runtime
        runtime_context.__exit__.return_value = False

        with (
            mock.patch.object(
                bulkload_cli,
                "_read_pinned_codex_json",
                side_effect=read_document,
            ),
            mock.patch.object(
                bulkload_cli.private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=runtime_context,
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_close_request_against_inputs",
                side_effect=[
                    None,
                    BulkloadError("opening input changed before publication"),
                ],
            ) as deep_validator,
            mock.patch.object(
                bulkload_cli,
                "compile_codex_private_sqlite_action_plan",
                return_value=action_plan,
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_action_plan_against_close",
            ),
            mock.patch.object(
                bulkload_cli,
                "_revalidate_pinned_codex_input",
            ),
            mock.patch.object(
                bulkload_cli,
                "_write_pinned_codex_json",
            ) as publish,
            mock.patch.object(bulkload_cli.os, "close"),
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "opening input changed before publication",
            ):
                _codex_private_sqlite_compose_action_plan(arguments)

        self.assertEqual(deep_validator.call_count, 2)
        publish.assert_not_called()

    def test_registered_prefix_remains_explicitly_blocked(self) -> None:
        opening = deepcopy(self.opening_plan)
        family = opening["sqlite_families"][0]
        family["migration_relation"]["relation"] = "registered-prefix-upgrade"
        family["migration_relation"]["source_migrations_sha256"] = _digest(
            "source-migrations"
        )
        family["migration_relation"]["adapter"] = {
            "adapter_id": "fixture-adapter",
            "adapter_source_sha256": _digest("fixture-adapter-source"),
        }
        opening["plan_sha256"] = object_digest(opening, "plan_sha256")
        close = self._close_request(opening)

        plan = self._compile(opening_plan=opening, close_request=close)
        family_plan = plan["sqlite_families"][0]
        self.assertFalse(plan["readiness"]["ready_for_offline_compose"])
        self.assertEqual(plan["operation_graph"]["nodes"], [])
        self.assertIn(
            "pinned-migration-adapter-not-implemented",
            {blocker["code"] for blocker in family_plan["blockers"]},
        )

    def test_structural_validator_rejects_reauthorized_unsafe_operation(self) -> None:
        plan = self._compile()
        tampered = deepcopy(plan)
        tampered["sqlite_families"][0]["tables"][0]["operation"]["identity_remap"] = (
            True
        )
        tampered["action_plan_sha256"] = object_digest(
            tampered,
            "action_plan_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "operation is unsafe"):
            validate_codex_private_sqlite_action_plan(tampered)

    def test_action_plan_rejects_legacy_v4_runtime_authority(self) -> None:
        with self.assertRaisesRegex(BulkloadError, "requires policy v5"):
            self._compile(
                runtime_authority=private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4
            )

    def test_against_close_recomputes_exact_table_semantics(self) -> None:
        plan = self._compile()
        patches = self._patches()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
        ):
            validate_codex_private_sqlite_action_plan_against_close(
                plan,
                self.opening_plan,
                self.close_request,
                self.directories["source_a"],
                self.directories["source_b"],
                self.directories["destination_a"],
                self.directories["destination_b"],
                self.session_closes["source_a"],
                self.session_closes["source_b"],
                self.session_closes["destination_a"],
                self.session_closes["destination_b"],
            )

        source_path = self.directories["source_a"] / "sqlite" / self.basename
        connection = sqlite3.connect(source_path)
        try:
            connection.execute("INSERT INTO items VALUES (?, ?)", (4, b"late"))
            connection.commit()
        finally:
            connection.close()
        source_path.chmod(0o600)
        patches = self._patches()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "differs from its closing evidence",
            ):
                validate_codex_private_sqlite_action_plan_against_close(
                    plan,
                    self.opening_plan,
                    self.close_request,
                    self.directories["source_a"],
                    self.directories["source_b"],
                    self.directories["destination_a"],
                    self.directories["destination_b"],
                    self.session_closes["source_a"],
                    self.session_closes["source_b"],
                    self.session_closes["destination_a"],
                    self.session_closes["destination_b"],
                )


if __name__ == "__main__":
    unittest.main()
