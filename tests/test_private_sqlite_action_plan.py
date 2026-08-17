from __future__ import annotations

from copy import deepcopy
import hashlib
import os
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
from bulkload_lib.cli import (
    _codex_private_sqlite_capacity_observe,
    _codex_private_sqlite_compose_action_plan,
    _codex_private_sqlite_compose_request,
    _revalidate_sqlite_action_chain,
    _sqlite_workspace_roots,
    _write_pinned_codex_json,
)
from bulkload_lib import cli as bulkload_cli
from bulkload_lib import private_sqlite_action_plan
from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_request
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
from bulkload_lib.private_sqlite_request import (
    MAX_CHECKED_BYTES,
    PinnedComposeWorkspace,
    compile_codex_private_sqlite_capacity_observation,
    compile_codex_private_sqlite_compose_request,
    derived_compose_output_leaf,
    validate_codex_private_sqlite_capacity_observation,
    validate_codex_private_sqlite_capacity_observation_against_request,
    validate_codex_private_sqlite_compose_request,
    validate_codex_private_sqlite_compose_request_against_action,
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
        self.runtime_authority = deepcopy(
            private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5
        )
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

    def test_action_plan_rejects_both_legacy_v4_runtime_authorities(self) -> None:
        for authority in (
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4,
            private_runtime.ACCEPTED_H5_PRIVATE_RUNTIME_AUTHORITY_V4,
        ):
            with self.subTest(policy_sha256=authority["policy_sha256"]):
                with self.assertRaisesRegex(BulkloadError, "requires policy v5"):
                    self._compile(runtime_authority=authority)

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

    def _request_runtime_authority(self) -> dict:
        return {
            "schema": private_runtime.PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
            "policy_schema": private_runtime.PRIVATE_STATE_POLICY_SCHEMA,
            "policy_sha256": _digest("policy-v6"),
            "runtime_source_sha256": _digest("runtime-v6"),
            "source_digests": {
                path: _digest(f"v6:{path}")
                for path in private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS
            },
        }

    def test_v6_request_cli_revalidates_full_chain_across_publication(
        self,
    ) -> None:
        action = self._compile()
        workspace_record = self._workspace_record(action)
        request = self._compose_request(action)
        arguments = SimpleNamespace(
            output="/private-evidence/request.json",
            workspace_parent="/private-workspace",
            accept_action_plan=action["action_plan_sha256"],
        )
        documents = {
            "action_plan": action,
            "session_source_a": {"snapshot": {}},
            "session_source_b": {"snapshot": {}},
            "session_destination_a": {"snapshot": {}},
            "session_destination_b": {"snapshot": {}},
        }
        workspace = mock.MagicMock()
        workspace.parent = Path("/private-workspace")
        workspace.record.return_value = workspace_record
        runtime = SimpleNamespace(
            record=self._request_runtime_authority(),
            revalidate=mock.Mock(),
        )
        runtime_context = mock.MagicMock()
        runtime_context.__enter__.return_value = runtime
        runtime_context.__exit__.return_value = False
        events: list[str] = []

        with (
            mock.patch.object(
                bulkload_cli,
                "_read_sqlite_action_chain",
                return_value=({}, {}, {}, {}, documents, []),
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_workspace_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_recorded_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_closing_session_snapshots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_write_protected_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "PinnedComposeWorkspace",
                return_value=workspace,
            ),
            mock.patch.object(
                bulkload_cli.private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=runtime_context,
            ),
            mock.patch.object(
                bulkload_cli,
                "_revalidate_sqlite_action_chain",
                side_effect=lambda *args: events.append("deep"),
            ) as deep,
            mock.patch.object(
                bulkload_cli,
                "compile_codex_private_sqlite_compose_request",
                return_value=request,
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_compose_request_against_action",
            ),
            mock.patch.object(
                bulkload_cli,
                "_write_pinned_codex_json",
                side_effect=lambda *args, **kwargs: events.append("publish"),
            ),
        ):
            self.assertEqual(
                _codex_private_sqlite_compose_request(arguments),
                4,
            )

        self.assertEqual(deep.call_count, 3)
        self.assertEqual(runtime.revalidate.call_count, 3)
        self.assertEqual(workspace.revalidate.call_count, 3)
        self.assertEqual(events, ["deep", "deep", "publish", "deep"])

    def test_v6_capacity_cli_revalidates_chain_capacity_and_request(
        self,
    ) -> None:
        action = self._compile()
        request = self._compose_request(action)
        workspace_record = request["output_intent"]["workspace"]
        requirement = request["capacity_requirement"]
        capacity = {
            "fragment_size": 4096,
            "available_blocks": (requirement["required_bytes"] + 4095) // 4096,
            "available_bytes": ((requirement["required_bytes"] + 4095) // 4096) * 4096,
            "available_inodes": requirement["required_inodes"],
        }
        observation = compile_codex_private_sqlite_capacity_observation(
            request,
            workspace_record,
            capacity,
            accept_request=request["request_sha256"],
            observation_runtime_authority=request["request_runtime_authority"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
            observation_id="99999999-9999-4999-8999-999999999999",
            created_at="2026-07-29T18:01:00Z",
        )
        arguments = SimpleNamespace(
            request="/private-evidence/request.json",
            output="/private-evidence/capacity.json",
            workspace_parent="/private-workspace",
            accept_action_plan=action["action_plan_sha256"],
            accept_request=request["request_sha256"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
        )
        documents = {"action_plan": action}
        workspace = mock.MagicMock()
        workspace.parent = Path("/private-workspace")
        workspace.record.return_value = workspace_record
        workspace.observe_capacity.return_value = capacity
        runtime = SimpleNamespace(
            record=request["request_runtime_authority"],
            revalidate=mock.Mock(),
        )
        runtime_context = mock.MagicMock()
        runtime_context.__enter__.return_value = runtime
        runtime_context.__exit__.return_value = False
        events: list[str] = []

        with (
            mock.patch.object(
                bulkload_cli,
                "_read_sqlite_action_chain",
                return_value=({}, {}, {}, {}, documents, []),
            ),
            mock.patch.object(
                bulkload_cli,
                "_read_pinned_codex_json",
                return_value=(request, 91, ()),
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_workspace_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_recorded_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_closing_session_snapshots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "_sqlite_write_protected_roots",
                return_value=[],
            ),
            mock.patch.object(
                bulkload_cli,
                "PinnedComposeWorkspace",
                return_value=workspace,
            ),
            mock.patch.object(
                bulkload_cli.private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=runtime_context,
            ),
            mock.patch.object(
                bulkload_cli,
                "_revalidate_pinned_codex_input",
            ),
            mock.patch.object(
                bulkload_cli,
                "_revalidate_sqlite_action_chain",
                side_effect=lambda *args: events.append("deep"),
            ) as deep,
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_compose_request",
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_compose_request_against_action",
            ),
            mock.patch.object(
                bulkload_cli,
                "compile_codex_private_sqlite_capacity_observation",
                return_value=observation,
            ),
            mock.patch.object(
                bulkload_cli,
                "validate_codex_private_sqlite_capacity_observation_against_request",
            ),
            mock.patch.object(
                bulkload_cli,
                "_write_pinned_codex_json",
                side_effect=lambda *args, **kwargs: events.append("publish"),
            ),
            mock.patch.object(bulkload_cli.os, "close"),
        ):
            wrong_acceptance = deepcopy(arguments)
            wrong_acceptance.accept_action_plan = "0" * 64
            with self.assertRaisesRegex(
                BulkloadError,
                "accepted private SQLite action-plan digest differs",
            ):
                _codex_private_sqlite_capacity_observe(wrong_acceptance)
            self.assertEqual(
                _codex_private_sqlite_capacity_observe(arguments),
                4,
            )

        self.assertEqual(deep.call_count, 3)
        self.assertEqual(runtime.revalidate.call_count, 3)
        self.assertEqual(workspace.observe_capacity.call_count, 3)
        self.assertEqual(events, ["deep", "deep", "publish", "deep"])

    def test_v6_chain_rejects_an_active_v6_opening_producer(self) -> None:
        documents = {
            "action_plan": {
                "runtime_authority": deepcopy(
                    private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5
                )
            },
            "opening_plan": {
                "runtime_authority": self._request_runtime_authority(),
            },
        }
        with self.assertRaisesRegex(
            BulkloadError,
            "exact reviewed v4 or accepted-H6 v5 producer authority",
        ):
            _revalidate_sqlite_action_chain(documents, {}, {})

    def _workspace_record(self, action_plan: dict) -> dict:
        protected: list[dict] = []
        next_inode = 100
        bindings: dict[str, tuple[dict, dict, list[dict], str]] = {}

        def record(
            namespace_class: str,
            role: str,
            path: str,
            *,
            exists: bool,
            require_private: bool,
        ) -> None:
            nonlocal next_inode
            identity = None
            mount = None
            lineage = None
            lineage_sha256 = None
            if exists:
                binding = bindings.get(path)
                if binding is None:
                    identity = {
                        "device": 10,
                        "inode": next_inode,
                        "uid": os.getuid(),
                        "mode": 0o700 if require_private else 0o755,
                    }
                    mount = {
                        "device": 10,
                        "filesystem_id": 20,
                        "linux_mount_id": None,
                    }
                    lineage = [
                        {
                            "identity": deepcopy(identity),
                            "mount": deepcopy(mount),
                        }
                    ]
                    lineage_sha256 = sha256_bytes(canonical_bytes(lineage))
                    bindings[path] = (
                        identity,
                        mount,
                        lineage,
                        lineage_sha256,
                    )
                    next_inode += 1
                else:
                    identity, mount, lineage, lineage_sha256 = deepcopy(binding)
            protected.append(
                {
                    "class": namespace_class,
                    "role": role,
                    "path": path,
                    "exists": exists,
                    "require_private": require_private,
                    "identity": identity,
                    "mount": mount,
                    "lineage": lineage,
                    "lineage_sha256": lineage_sha256,
                }
            )

        evidence_parent = "/private/evidence"
        for namespace_class in (
            "opening-private-bundle",
            "closing-private-bundle",
        ):
            for role in (
                "source_a",
                "source_b",
                "destination_a",
                "destination_b",
            ):
                record(
                    namespace_class,
                    role,
                    f"{evidence_parent}/{namespace_class}/{role}",
                    exists=True,
                    require_private=True,
                )
        record(
            "protocol-evidence-parent",
            "private-json",
            evidence_parent,
            exists=True,
            require_private=True,
        )
        record(
            "request-output-parent",
            "private-json",
            evidence_parent,
            exists=True,
            require_private=True,
        )
        record(
            "request-runtime-root",
            "consumer",
            "/private/runtime",
            exists=True,
            require_private=False,
        )
        for role in (
            "source:codex_home",
            "source:sqlite_home",
            "destination:codex_home",
            "destination:sqlite_home",
        ):
            record(
                "live-private-root",
                role,
                f"/private/live/{role.replace(':', '-')}",
                exists=False,
                require_private=True,
            )
        for role in (
            "opening_session_source_a",
            "opening_session_source_b",
            "opening_session_destination_a",
            "opening_session_destination_b",
            "session_source_a",
            "session_source_b",
            "session_destination_a",
            "session_destination_b",
        ):
            record(
                "live-session-root",
                role,
                f"/private/live/{role}",
                exists=False,
                require_private=True,
            )
        protected.sort(key=canonical_bytes)
        leaf = derived_compose_output_leaf(action_plan["action_plan_sha256"])
        workspace_identity = {
            "device": 1,
            "inode": 2,
            "uid": os.getuid(),
            "mode": 0o700,
        }
        workspace_mount = {
            "device": 1,
            "filesystem_id": 3,
            "linux_mount_id": None,
        }
        workspace_lineage = [
            {
                "identity": deepcopy(workspace_identity),
                "mount": deepcopy(workspace_mount),
            }
        ]
        return {
            "resolved_parent": "/private/compose-workspace",
            "parent_identity": workspace_identity,
            "mount": workspace_mount,
            "lineage": workspace_lineage,
            "lineage_sha256": sha256_bytes(canonical_bytes(workspace_lineage)),
            "final_leaf": leaf,
            "target_observed_absent": True,
            "staging_prefix": f".{leaf}-staging-",
            "staging_namespace_observed_empty": True,
            "same_parent_staging_required": True,
            "protected_namespaces": protected,
            "protected_namespaces_sha256": sha256_bytes(canonical_bytes(protected)),
        }

    def _compose_request(self, action_plan: dict | None = None) -> dict:
        action = action_plan or self._compile()
        return compile_codex_private_sqlite_compose_request(
            action,
            self._workspace_record(action),
            accept_action_plan=action["action_plan_sha256"],
            request_runtime_authority=self._request_runtime_authority(),
            request_id="77777777-7777-4777-8777-777777777777",
            created_at="2026-07-29T18:00:00Z",
        )

    def test_v6_request_binds_distinct_runtime_and_remains_non_actionable(
        self,
    ) -> None:
        action = self._compile()
        request = self._compose_request(action)
        validate_codex_private_sqlite_compose_request(request)
        validate_codex_private_sqlite_compose_request_against_action(
            request,
            action,
            self._workspace_record(action),
            request["request_runtime_authority"],
        )
        self.assertEqual(
            request["action_plan_producer_runtime_authority"],
            private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5,
        )
        self.assertNotEqual(
            request["action_plan_producer_runtime_authority"],
            request["request_runtime_authority"],
        )
        self.assertTrue(request["claims"]["request_complete"])
        self.assertTrue(request["claims"]["request_runtime_bound"])
        self.assertFalse(request["claims"]["composer_runtime_bound"])
        self.assertIsNone(request["required_composer_runtime_authority"])
        for key, value in request["claims"].items():
            if key not in {"request_complete", "request_runtime_bound"}:
                self.assertFalse(value, key)
        self.assertFalse(request["capacity_observation"]["space_reserved"])
        self.assertFalse(request["capacity_observation"]["future_write_guaranteed"])

    def test_v6_request_rejects_self_redigested_producer_and_readiness(
        self,
    ) -> None:
        action = self._compile()
        changed = deepcopy(action)
        changed["runtime_authority"]["source_digests"][
            "scripts/bulkload_lib/cli.py"
        ] = "0" * 64
        changed["action_plan_sha256"] = object_digest(
            changed,
            "action_plan_sha256",
        )
        with self.assertRaises(BulkloadError):
            self._compose_request(changed)

        request = self._compose_request(action)
        tampered = deepcopy(request)
        tampered["claims"]["ready_for_internal_offline_compose"] = True
        body = {
            key: value
            for key, value in tampered.items()
            if key not in {"request_body_sha256", "request_sha256"}
        }
        tampered["request_body_sha256"] = sha256_bytes(canonical_bytes(body))
        tampered["request_sha256"] = object_digest(
            tampered,
            "request_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "claims differ"):
            validate_codex_private_sqlite_compose_request(tampered)

        forged = self._compose_request(action)
        expected_runtime = deepcopy(forged["request_runtime_authority"])
        forged["request_runtime_authority"]["policy_sha256"] = "0" * 64
        forged["request_runtime_authority"]["runtime_source_sha256"] = "1" * 64
        forged["request_runtime_authority"]["source_digests"] = {
            path: "2" * 64 for path in private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS
        }
        body = {
            key: value
            for key, value in forged.items()
            if key not in {"request_body_sha256", "request_sha256"}
        }
        forged["request_body_sha256"] = sha256_bytes(canonical_bytes(body))
        forged["request_sha256"] = object_digest(
            forged,
            "request_sha256",
        )
        validate_codex_private_sqlite_compose_request(forged)
        with self.assertRaisesRegex(BulkloadError, "runtime authority differs"):
            validate_codex_private_sqlite_compose_request_against_action(
                forged,
                action,
                self._workspace_record(action),
                expected_runtime,
            )

    def test_v6_request_rejects_legacy_consumer_runtime_authority(self) -> None:
        action = self._compile()
        for authority in (
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4,
            private_runtime.ACCEPTED_H5_PRIVATE_RUNTIME_AUTHORITY_V4,
            private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5,
        ):
            with self.subTest(policy_schema=authority["policy_schema"]):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "active v6 consumer policy|must be distinct",
                ):
                    compile_codex_private_sqlite_compose_request(
                        action,
                        self._workspace_record(action),
                        accept_action_plan=action["action_plan_sha256"],
                        request_runtime_authority=authority,
                    )

    def test_v6_capacity_observation_is_volatile_not_reserved(self) -> None:
        request = self._compose_request()
        requirement = request["capacity_requirement"]
        fragment = 4096
        blocks = (requirement["required_bytes"] + fragment - 1) // fragment
        capacity = {
            "fragment_size": fragment,
            "available_blocks": blocks,
            "available_bytes": blocks * fragment,
            "available_inodes": requirement["required_inodes"],
        }
        observation = compile_codex_private_sqlite_capacity_observation(
            request,
            request["output_intent"]["workspace"],
            capacity,
            accept_request=request["request_sha256"],
            observation_runtime_authority=request["request_runtime_authority"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
            observation_id="99999999-9999-4999-8999-999999999999",
            created_at="2026-07-29T18:01:00Z",
        )
        validate_codex_private_sqlite_capacity_observation(observation)
        validate_codex_private_sqlite_capacity_observation_against_request(
            observation,
            request,
            request["output_intent"]["workspace"],
            request["request_runtime_authority"],
        )
        self.assertTrue(observation["claims"]["capacity_sufficient_observed"])
        self.assertFalse(observation["claims"]["space_reserved"])
        self.assertFalse(observation["claims"]["future_write_guaranteed"])
        self.assertFalse(observation["claims"]["ready_for_internal_offline_compose"])

        insufficient = {
            **capacity,
            "available_blocks": blocks - 1,
            "available_bytes": (blocks - 1) * fragment,
        }
        held = compile_codex_private_sqlite_capacity_observation(
            request,
            request["output_intent"]["workspace"],
            insufficient,
            accept_request=request["request_sha256"],
            observation_runtime_authority=request["request_runtime_authority"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
            observation_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            created_at="2026-07-29T18:02:00Z",
        )
        self.assertFalse(held["claims"]["capacity_sufficient_observed"])

    def test_v6_request_and_observation_are_byte_deterministic(self) -> None:
        action = self._compile()
        workspace = self._workspace_record(action)
        request_arguments = {
            "accept_action_plan": action["action_plan_sha256"],
            "request_runtime_authority": self._request_runtime_authority(),
            "request_id": "77777777-7777-4777-8777-777777777777",
            "created_at": "2026-07-29T18:00:00Z",
        }
        request_a = compile_codex_private_sqlite_compose_request(
            action,
            workspace,
            **request_arguments,
        )
        request_b = compile_codex_private_sqlite_compose_request(
            action,
            workspace,
            **request_arguments,
        )
        self.assertEqual(request_a, request_b)
        self.assertEqual(canonical_bytes(request_a), canonical_bytes(request_b))
        self.assertEqual(request_a["request_sha256"], request_b["request_sha256"])

        requirement = request_a["capacity_requirement"]
        fragment = 4096
        blocks = (requirement["required_bytes"] + fragment - 1) // fragment
        capacity = {
            "fragment_size": fragment,
            "available_blocks": blocks,
            "available_bytes": blocks * fragment,
            "available_inodes": requirement["required_inodes"],
        }
        observation_arguments = {
            "accept_request": request_a["request_sha256"],
            "observation_runtime_authority": request_a["request_runtime_authority"],
            "host_authority_id": "88888888-8888-4888-8888-888888888888",
            "observation_id": "99999999-9999-4999-8999-999999999999",
            "created_at": "2026-07-29T18:01:00Z",
        }
        observation_a = compile_codex_private_sqlite_capacity_observation(
            request_a,
            workspace,
            capacity,
            **observation_arguments,
        )
        observation_b = compile_codex_private_sqlite_capacity_observation(
            request_a,
            workspace,
            capacity,
            **observation_arguments,
        )
        self.assertEqual(observation_a, observation_b)
        self.assertEqual(
            canonical_bytes(observation_a),
            canonical_bytes(observation_b),
        )
        self.assertEqual(
            observation_a["observation_sha256"],
            observation_b["observation_sha256"],
        )
        for claims in (request_a["claims"], observation_a["claims"]):
            self.assertFalse(claims["ready_for_internal_offline_compose"])
            self.assertFalse(claims["compose_authorized"])

    def test_v6_request_and_observation_require_canonical_utc_seconds(
        self,
    ) -> None:
        action = self._compile()
        workspace = self._workspace_record(action)
        with self.assertRaisesRegex(BulkloadError, "UTC timestamp"):
            compile_codex_private_sqlite_compose_request(
                action,
                workspace,
                accept_action_plan=action["action_plan_sha256"],
                request_runtime_authority=self._request_runtime_authority(),
                request_id="77777777-7777-4777-8777-777777777777",
                created_at="2026-07-29T18:00:00.500000Z",
            )

        request = self._compose_request(action)
        requirement = request["capacity_requirement"]
        fragment = 4096
        blocks = (requirement["required_bytes"] + fragment - 1) // fragment
        with self.assertRaisesRegex(BulkloadError, "UTC timestamp"):
            compile_codex_private_sqlite_capacity_observation(
                request,
                workspace,
                {
                    "fragment_size": fragment,
                    "available_blocks": blocks,
                    "available_bytes": blocks * fragment,
                    "available_inodes": requirement["required_inodes"],
                },
                accept_request=request["request_sha256"],
                observation_runtime_authority=request["request_runtime_authority"],
                host_authority_id="88888888-8888-4888-8888-888888888888",
                observation_id="99999999-9999-4999-8999-999999999999",
                created_at="2026-07-29T18:01:00.500000Z",
            )

    def test_v6_capacity_observation_rejects_self_redigested_ttl_drift(
        self,
    ) -> None:
        request = self._compose_request()
        requirement = request["capacity_requirement"]
        fragment = 4096
        blocks = (requirement["required_bytes"] + fragment - 1) // fragment
        observation = compile_codex_private_sqlite_capacity_observation(
            request,
            request["output_intent"]["workspace"],
            {
                "fragment_size": fragment,
                "available_blocks": blocks,
                "available_bytes": blocks * fragment,
                "available_inodes": requirement["required_inodes"],
            },
            accept_request=request["request_sha256"],
            observation_runtime_authority=request["request_runtime_authority"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
            observation_id="99999999-9999-4999-8999-999999999999",
            created_at="2026-07-29T18:01:00Z",
        )
        changed = deepcopy(observation)
        changed["expires_at"] = "2026-07-29T18:11:00Z"
        body = {
            key: value
            for key, value in changed.items()
            if key not in {"observation_body_sha256", "observation_sha256"}
        }
        changed["observation_body_sha256"] = sha256_bytes(canonical_bytes(body))
        changed["observation_sha256"] = object_digest(
            changed,
            "observation_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "TTL differs"):
            validate_codex_private_sqlite_capacity_observation(changed)

        fractional = deepcopy(observation)
        fractional["expires_at"] = "2026-07-29T18:06:00.500000Z"
        body = {
            key: value
            for key, value in fractional.items()
            if key not in {"observation_body_sha256", "observation_sha256"}
        }
        fractional["observation_body_sha256"] = sha256_bytes(canonical_bytes(body))
        fractional["observation_sha256"] = object_digest(
            fractional,
            "observation_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "UTC timestamp"):
            validate_codex_private_sqlite_capacity_observation(fractional)

    def test_v6_capacity_arithmetic_handles_large_metadata_without_allocating(
        self,
    ) -> None:
        action = self._compile()
        family = action["sqlite_families"][0]
        family["source_artifact"]["snapshot_size"] = 6 * 1024**3
        family["destination_artifact"]["snapshot_size"] = 5 * 1024**3
        action["action_plan_sha256"] = object_digest(
            action,
            "action_plan_sha256",
        )
        request = self._compose_request(action)
        self.assertGreater(
            request["capacity_requirement"]["required_bytes"],
            3 * 11 * 1024**3,
        )

        overflow = deepcopy(action)
        overflow["sqlite_families"][0]["source_artifact"]["snapshot_size"] = (
            MAX_CHECKED_BYTES
        )
        overflow["action_plan_sha256"] = object_digest(
            overflow,
            "action_plan_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "checked integer range"):
            self._compose_request(overflow)

    def test_v6_workspace_pins_sibling_and_rejects_occupied_or_nested(
        self,
    ) -> None:
        root = Path(self.temporary.name).resolve() / "workspace-contract"
        root.mkdir(mode=0o700)
        protected = root / "protected"
        protected.mkdir(mode=0o700)
        workspace = root / "workspace"
        workspace.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        with PinnedComposeWorkspace(
            workspace,
            leaf,
            existing_roots=[
                ("fixture", "protected", protected, True),
                ("request-output-parent", "private-json", evidence, True),
            ],
            recorded_roots=[],
        ) as pinned:
            pinned.revalidate()
            self.assertTrue(pinned.record()["target_observed_absent"])
            (workspace / leaf).write_bytes(b"occupied")
            with self.assertRaisesRegex(BulkloadError, "already exists"):
                pinned.revalidate()

        nested = protected / "nested"
        nested.mkdir(mode=0o700)
        with self.assertRaisesRegex(BulkloadError, "overlaps"):
            PinnedComposeWorkspace(
                nested,
                leaf,
                existing_roots=[
                    ("fixture", "protected", protected, True),
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[],
            )

        non_private = root / "non-private"
        non_private.mkdir(mode=0o755)
        with self.assertRaisesRegex(BulkloadError, "0700"):
            PinnedComposeWorkspace(
                non_private,
                leaf,
                existing_roots=[
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[],
            )

    def test_v6_workspace_rejects_staging_occupancy_and_symlink_ancestors(
        self,
    ) -> None:
        root = Path(self.temporary.name).resolve() / "workspace-adversarial"
        root.mkdir(mode=0o700)
        workspace = root / "workspace"
        workspace.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        (workspace / f".{leaf}-staging-attacker").write_bytes(b"occupied")
        with self.assertRaisesRegex(BulkloadError, "staging namespace"):
            PinnedComposeWorkspace(
                workspace,
                leaf,
                existing_roots=[
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[],
            )

        clean_workspace = root / "clean-workspace"
        clean_workspace.mkdir(mode=0o700)
        symlink_workspace = root / "workspace-link"
        symlink_workspace.symlink_to(clean_workspace, target_is_directory=True)
        with self.assertRaisesRegex(BulkloadError, "symlink component"):
            PinnedComposeWorkspace(
                symlink_workspace,
                leaf,
                existing_roots=[
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[],
            )

        protected = root / "protected"
        protected.mkdir(mode=0o700)
        protected_link = root / "protected-link"
        protected_link.symlink_to(protected, target_is_directory=True)
        recorded_absent = protected_link / "future-root"
        with self.assertRaisesRegex(BulkloadError, "symlink component"):
            PinnedComposeWorkspace(
                clean_workspace,
                leaf,
                existing_roots=[
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[
                    ("live-private-root", "source", recorded_absent),
                ],
            )

    def test_v6_workspace_capacity_uses_caller_available_blocks(self) -> None:
        root = Path(self.temporary.name).resolve() / "workspace-capacity-contract"
        root.mkdir(mode=0o700)
        workspace_parent = root / "workspace"
        workspace_parent.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        with PinnedComposeWorkspace(
            workspace_parent,
            leaf,
            existing_roots=[
                ("request-output-parent", "private-json", evidence, True),
            ],
            recorded_roots=[],
        ) as workspace:
            filesystem = SimpleNamespace(
                f_frsize=4096,
                f_bsize=4096,
                f_bavail=7,
                f_bfree=10_000_000,
                f_favail=11,
            )
            with (
                mock.patch.object(workspace, "revalidate"),
                mock.patch.object(
                    private_sqlite_request.os,
                    "fstatvfs",
                    return_value=filesystem,
                ),
            ):
                capacity = workspace.observe_capacity()
        self.assertEqual(capacity["available_blocks"], 7)
        self.assertEqual(capacity["available_bytes"], 7 * 4096)
        self.assertEqual(capacity["available_inodes"], 11)

    def test_v6_workspace_capacity_rejects_invalid_or_overflowing_values(
        self,
    ) -> None:
        root = Path(self.temporary.name).resolve() / "workspace-capacity-invalid"
        root.mkdir(mode=0o700)
        workspace_parent = root / "workspace"
        workspace_parent.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        with PinnedComposeWorkspace(
            workspace_parent,
            leaf,
            existing_roots=[
                ("request-output-parent", "private-json", evidence, True),
            ],
            recorded_roots=[],
        ) as workspace:
            fallback = SimpleNamespace(
                f_frsize=0,
                f_bsize=8192,
                f_bavail=3,
                f_favail=2,
            )
            invalid_values = (
                SimpleNamespace(
                    f_frsize=4096,
                    f_bsize=4096,
                    f_bavail=-1,
                    f_favail=2,
                ),
                SimpleNamespace(
                    f_frsize=4096,
                    f_bsize=4096,
                    f_bavail=1,
                    f_favail=-1,
                ),
            )
            with (
                mock.patch.object(workspace, "revalidate"),
                mock.patch.object(
                    private_sqlite_request.os,
                    "fstatvfs",
                    return_value=fallback,
                ),
            ):
                capacity = workspace.observe_capacity()
            self.assertEqual(capacity["fragment_size"], 8192)
            self.assertEqual(capacity["available_bytes"], 3 * 8192)

            for filesystem in invalid_values:
                with (
                    self.subTest(filesystem=filesystem),
                    mock.patch.object(workspace, "revalidate"),
                    mock.patch.object(
                        private_sqlite_request.os,
                        "fstatvfs",
                        return_value=filesystem,
                    ),
                ):
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "filesystem capacity is invalid",
                    ):
                        workspace.observe_capacity()

            overflow = SimpleNamespace(
                f_frsize=MAX_CHECKED_BYTES,
                f_bsize=MAX_CHECKED_BYTES,
                f_bavail=2,
                f_favail=2,
            )
            with (
                mock.patch.object(workspace, "revalidate"),
                mock.patch.object(
                    private_sqlite_request.os,
                    "fstatvfs",
                    return_value=overflow,
                ),
            ):
                with self.assertRaisesRegex(BulkloadError, "checked integer range"):
                    workspace.observe_capacity()

    def test_v6_capacity_observation_reports_inode_shortage_without_authority(
        self,
    ) -> None:
        request = self._compose_request()
        requirement = request["capacity_requirement"]
        fragment = 4096
        blocks = (requirement["required_bytes"] + fragment - 1) // fragment
        observation = compile_codex_private_sqlite_capacity_observation(
            request,
            request["output_intent"]["workspace"],
            {
                "fragment_size": fragment,
                "available_blocks": blocks,
                "available_bytes": blocks * fragment,
                "available_inodes": requirement["required_inodes"] - 1,
            },
            accept_request=request["request_sha256"],
            observation_runtime_authority=request["request_runtime_authority"],
            host_authority_id="88888888-8888-4888-8888-888888888888",
            observation_id="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            created_at="2026-07-29T18:01:00Z",
        )
        self.assertFalse(observation["claims"]["capacity_sufficient_observed"])
        self.assertFalse(observation["claims"]["compose_authorized"])
        self.assertFalse(observation["claims"]["ready_for_internal_offline_compose"])

    def test_v6_output_parent_cannot_descend_from_runtime_or_bundle(self) -> None:
        root = Path(self.temporary.name).resolve() / "output-parent-contract"
        root.mkdir(mode=0o700)
        workspace = root / "workspace"
        workspace.mkdir(mode=0o700)
        protected = root / "protected-runtime"
        protected.mkdir(mode=0o700)
        evidence = protected / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        with self.assertRaisesRegex(
            BulkloadError,
            "storage/evidence namespace overlaps a live/runtime namespace",
        ):
            PinnedComposeWorkspace(
                workspace,
                leaf,
                existing_roots=[
                    ("request-runtime-root", "consumer", protected, True),
                    ("request-output-parent", "private-json", evidence, True),
                ],
                recorded_roots=[],
            )

    def test_v6_bundle_and_live_namespaces_are_pairwise_disjoint(self) -> None:
        root = Path(self.temporary.name).resolve() / "bundle-live-disjoint"
        root.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])

        cases: list[tuple[str, Path, Path]] = []
        exact = root / "exact"
        exact.mkdir(mode=0o700)
        cases.append(("exact", exact, exact))
        storage_parent = root / "storage-parent"
        storage_parent.mkdir(mode=0o700)
        live_child = storage_parent / "live"
        live_child.mkdir(mode=0o700)
        cases.append(("live-descends", storage_parent, live_child))
        live_parent = root / "live-parent"
        live_parent.mkdir(mode=0o700)
        storage_child = live_parent / "bundle"
        storage_child.mkdir(mode=0o700)
        cases.append(("storage-descends", storage_child, live_parent))

        for name, bundle, live in cases:
            workspace = root / f"workspace-{name}"
            workspace.mkdir(mode=0o700)
            with (
                self.subTest(case=name),
                self.assertRaisesRegex(
                    BulkloadError,
                    "storage/evidence namespace overlaps a live/runtime namespace",
                ),
            ):
                PinnedComposeWorkspace(
                    workspace,
                    leaf,
                    existing_roots=[
                        (
                            "opening-private-bundle",
                            "source_a",
                            bundle,
                            True,
                        ),
                        (
                            "live-private-root",
                            "source:codex_home",
                            live,
                            True,
                        ),
                        (
                            "request-output-parent",
                            "private-json",
                            evidence,
                            True,
                        ),
                    ],
                    recorded_roots=[],
                )

        identity = {
            "device": 71,
            "inode": 72,
            "uid": os.getuid(),
            "mode": 0o700,
        }
        mount = {
            "device": 71,
            "filesystem_id": 73,
            "linux_mount_id": None,
        }
        lineage = [{"identity": identity, "mount": mount}]
        synthetic = object.__new__(PinnedComposeWorkspace)
        synthetic._protected = [
            SimpleNamespace(
                namespace_class="opening-private-bundle",
                path=Path("/synthetic/storage"),
                exists=True,
                identity=identity,
                lineage=lineage,
            ),
            SimpleNamespace(
                namespace_class="request-runtime-root",
                path=Path("/synthetic/runtime"),
                exists=True,
                identity=deepcopy(identity),
                lineage=deepcopy(lineage),
            ),
        ]
        with self.assertRaisesRegex(
            BulkloadError,
            "aliases, contains, or descends",
        ):
            synthetic._assert_storage_live_separation()

    def test_v6_serialized_workspace_rejects_live_bundle_overlap(self) -> None:
        action = self._compile()
        baseline = self._workspace_record(action)

        for path, message in (
            ("/private/other/../compose-workspace", "parent traversal"),
            ("/private/\x00compose-workspace", "must not contain NUL"),
        ):
            noncanonical = deepcopy(baseline)
            noncanonical["resolved_parent"] = path
            with (
                self.subTest(path=path),
                self.assertRaisesRegex(BulkloadError, message),
            ):
                private_sqlite_request._validate_workspace(noncanonical)

        def records(workspace: dict) -> tuple[dict, dict]:
            storage = next(
                item
                for item in workspace["protected_namespaces"]
                if item["class"] == "opening-private-bundle"
                and item["role"] == "source_a"
            )
            live = next(
                item
                for item in workspace["protected_namespaces"]
                if item["class"] == "live-private-root"
                and item["role"] == "source:codex_home"
            )
            return storage, live

        def reseal(workspace: dict) -> None:
            workspace["protected_namespaces"].sort(key=canonical_bytes)
            workspace["protected_namespaces_sha256"] = sha256_bytes(
                canonical_bytes(workspace["protected_namespaces"])
            )

        exact = deepcopy(baseline)
        storage, live = records(exact)
        live.update(
            {
                "path": storage["path"],
                "exists": True,
                "identity": deepcopy(storage["identity"]),
                "mount": deepcopy(storage["mount"]),
                "lineage": deepcopy(storage["lineage"]),
                "lineage_sha256": storage["lineage_sha256"],
            }
        )
        reseal(exact)
        with self.assertRaisesRegex(
            BulkloadError,
            "storage/evidence namespace overlaps a live/runtime namespace",
        ):
            private_sqlite_request._validate_workspace(exact)

        for direction in ("storage-descends", "live-descends"):
            nested = deepcopy(baseline)
            storage, live = records(nested)
            if direction == "storage-descends":
                storage["path"] = f"{live['path']}/bundle"
            else:
                live["path"] = f"{storage['path']}/live"
            reseal(nested)
            with (
                self.subTest(direction=direction),
                self.assertRaisesRegex(
                    BulkloadError,
                    "storage/evidence namespace overlaps a live/runtime namespace",
                ),
            ):
                private_sqlite_request._validate_workspace(nested)

        descriptor_alias = deepcopy(baseline)
        storage, live = records(descriptor_alias)
        live.update(
            {
                "exists": True,
                "identity": deepcopy(storage["identity"]),
                "mount": deepcopy(storage["mount"]),
                "lineage": deepcopy(storage["lineage"]),
                "lineage_sha256": storage["lineage_sha256"],
            }
        )
        reseal(descriptor_alias)
        with self.assertRaisesRegex(
            BulkloadError,
            "aliases, contains, or descends",
        ):
            private_sqlite_request._validate_workspace(descriptor_alias)

    def test_v6_writer_refuses_swapped_pinned_output_parent_without_artifact(
        self,
    ) -> None:
        root = Path(self.temporary.name).resolve() / "writer-parent-swap"
        root.mkdir(mode=0o700)
        workspace_parent = root / "workspace"
        workspace_parent.mkdir(mode=0o700)
        evidence = root / "evidence"
        evidence.mkdir(mode=0o700)
        old_evidence = root / "evidence-old"
        action = self._compile()
        leaf = derived_compose_output_leaf(action["action_plan_sha256"])
        with PinnedComposeWorkspace(
            workspace_parent,
            leaf,
            existing_roots=[
                ("request-output-parent", "private-json", evidence, True),
            ],
            recorded_roots=[],
        ) as workspace:
            binding = workspace.output_parent_binding()
            evidence.rename(old_evidence)
            evidence.mkdir(mode=0o700)
            output = evidence / "request.json"
            with self.assertRaisesRegex(
                BulkloadError,
                "pinned output parent path changed",
            ):
                _write_pinned_codex_json(
                    str(output),
                    {"schema": "fixture"},
                    [],
                    [],
                    protected_roots=[workspace_parent],
                    pinned_output_parent=binding,
                )
        self.assertEqual(list(evidence.iterdir()), [])
        self.assertEqual(list(old_evidence.iterdir()), [])

    def test_v6_capacity_reconstructs_request_workspace_across_evidence_dirs(
        self,
    ) -> None:
        root = Path(self.temporary.name).resolve() / "split-evidence"
        input_parent = root / "inputs"
        output_parent = root / "outputs"
        bundles = {
            role: input_parent / f"closing-{role}"
            for role in ("source_a", "source_b", "destination_a", "destination_b")
        }
        opening_bundles = {
            role: input_parent / f"opening-{role}"
            for role in ("source_a", "source_b", "destination_a", "destination_b")
        }
        input_paths = {
            "action_plan": str(input_parent / "action.json"),
            "opening_plan": str(input_parent / "opening.json"),
        }
        optional_paths = {"prefix": None}
        request_roots = _sqlite_workspace_roots(
            bundles=bundles,
            opening_bundles=opening_bundles,
            input_paths=input_paths,
            optional_input_paths=optional_paths,
            evidence_output=str(output_parent / "request.json"),
        )
        capacity_roots = _sqlite_workspace_roots(
            bundles=bundles,
            opening_bundles=opening_bundles,
            input_paths=input_paths,
            optional_input_paths=optional_paths,
            evidence_output=str(output_parent / "capacity.json"),
        )
        self.assertEqual(request_roots, capacity_roots)

    def test_v6_request_requires_the_action_derived_output_leaf(self) -> None:
        action = self._compile()
        workspace = self._workspace_record(action)
        workspace["final_leaf"] = "bulkload-sqlite-compose-" + ("0" * 64)
        workspace["staging_prefix"] = f".{workspace['final_leaf']}-staging-"
        with self.assertRaisesRegex(BulkloadError, "output leaf differs"):
            compile_codex_private_sqlite_compose_request(
                action,
                workspace,
                accept_action_plan=action["action_plan_sha256"],
                request_runtime_authority=self._request_runtime_authority(),
            )


if __name__ == "__main__":
    unittest.main()
